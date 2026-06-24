//! `Session` — the per-run context (PRODUCT.md §14 isolation).
//!
//! One run = one `Session`. It owns a unique working directory (wiped on close)
//! and the handle registry. The ephemeral DuckDB connection, the read-only
//! Surreal handle, and the deferred store backends get added in later milestones.

use std::fs;
use std::path::{Path, PathBuf};

use monty::{
    ExtFunctionResult, MontyObject, MontyRepl, NameLookupResult, NoLimitTracker, PrintWriter,
    ReplProgress, ReplStartError,
};

use crate::DropletError;
use crate::registry::Registry;
use crate::source::{LocalParquetSource, Source};
use crate::tool::Tool;

pub struct Session {
    run_id: String,
    work_dir: PathBuf,
    // One registry per session. The stored type is a placeholder for now;
    // it becomes the real engine-handle type when DuckDB lands in M1.
    handles: Registry<()>,
    // The connector seam (invariant #1): any backend plugs in unchanged.
    // Dev impl now; Athena/S3 later (M6).
    source: Box<dyn Source>,
    // The per-session local analyze engine (M1). Host-side behind the boundary — the sandbox
    // never sees it (invariant #6). One ephemeral in-memory DuckDB per Session (invariant #3).
    duck: crate::engine_duckdb::DuckEngine,
    // The session's persistent Monty REPL — agent code across run_code steps shares this namespace
    // (invariant #8: monty is fine in core; only pyo3 is barred). Held in an Option so run_code can
    // take it out for the duration of a step and put it back on Complete.
    repl: Option<MontyRepl<NoLimitTracker>>,
    // Later milestones add (NOT in M0/M1):
    //   surreal: read-only Surreal<Mem>      // M9 — schema-derived field search (read-only)
    //   artifacts: Box<dyn ArtifactStore>    // M5 — content-addressed load cache
    //   coord:     Box<dyn CoordinationStore>// M7 — run registry / leases / cache index
    //   snapshots: Box<dyn SnapshotStore>    // M8 — REPL+manifest blobs
}

impl Session {
    pub fn new(run_id: &str) -> Result<Self, DropletError> {
        // Unique per run so two sessions never collide (§14 isolation).
        let work_dir = std::env::temp_dir().join(format!("droplet-{run_id}"));
        // Wipe any stale dir from a previous run, then recreate it empty.
        let _ = fs::remove_dir_all(&work_dir); // ignore "not found"
        fs::create_dir_all(&work_dir)?; // io::Error -> DropletError via #[from]
        // Default the connector to the local-Parquet dev impl, looking in the
        // session's own work_dir. M2's catalog decides this properly.
        let source: Box<dyn Source> = Box::new(LocalParquetSource::new(work_dir.clone()));
        // One ephemeral in-memory DuckDB per Session, built right after the work dir.
        // `?` folds duckdb::Error into DropletError (invariant #10).
        let duck = crate::engine_duckdb::DuckEngine::new_in_memory()?;
        // One persistent REPL per session. NoLimitTracker for now; a real resource limiter is a
        // later milestone (// SWAP: LimitedTracker for prod).
        let repl = Some(MontyRepl::new("session.py", NoLimitTracker));
        Ok(Self {
            run_id: run_id.to_string(),
            work_dir,
            handles: Registry::new(),
            source,
            duck,
            repl,
        })
    }

    /// Borrow the session's local analyze engine (host-side; invariant #6). Use this for the
    /// read-out primitives (`to_rows`, `scalar_*`), which take `&self`.
    pub fn duck(&self) -> &crate::engine_duckdb::DuckEngine {
        &self.duck
    }

    /// Mutably borrow the session's analyze engine. Required for the dataset-producing
    /// primitives (`register_parquet`, `filter_rows`, `group_agg`, `local_sql`), which take
    /// `&mut self` because they mint a new `ds_{n}` view. Without this the session-owned engine
    /// would be unusable for its actual purpose.
    pub fn duck_mut(&mut self) -> &mut crate::engine_duckdb::DuckEngine {
        &mut self.duck
    }

    /// Run one agent program in the session's Monty sandbox to completion, returning the value of
    /// its last expression. External-function calls suspend the sandbox; the host dispatches them by
    /// name to the `#[droplet_tool]`-registered tools (run against this session's local engine) and
    /// resumes (PRODUCT.md §8 execution model; invariant #6 keeps results capped & data host-side).
    ///
    /// On a tool error the run aborts (the error folds into `DropletError`) and the session's REPL
    /// is consumed; create a new `Session` to continue. (Graceful in-sandbox error resume is later.)
    pub fn run_code(&mut self, code: &str) -> Result<MontyObject, DropletError> {
        let repl = self.repl.take().expect("session REPL present");
        let mut progress = repl
            .feed_start(code, vec![], PrintWriter::Disabled)
            .map_err(start_err)?;
        loop {
            match progress {
                ReplProgress::Complete { repl, value } => {
                    self.repl = Some(repl); // put it back for the next run_code step
                    return Ok(value);
                }
                ReplProgress::FunctionCall(call) => {
                    let reply: ExtFunctionResult =
                        match inventory::iter::<Tool>().find(|t| t.name == call.function_name) {
                            Some(tool) => {
                                (tool.dispatch)(&mut self.duck, &call.args, &call.kwargs)?.into()
                            }
                            None => ExtFunctionResult::NotFound(call.function_name.clone()),
                        };
                    progress = call
                        .resume(reply, PrintWriter::Disabled)
                        .map_err(start_err)?;
                }
                // Safe defaults for suspension kinds V1a doesn't use (carried from the M0 seam).
                ReplProgress::OsCall(c) => {
                    progress = c
                        .resume(MontyObject::None, PrintWriter::Disabled)
                        .map_err(start_err)?;
                }
                ReplProgress::NameLookup(l) => {
                    progress = l
                        .resume(NameLookupResult::Undefined, PrintWriter::Disabled)
                        .map_err(start_err)?;
                }
                ReplProgress::ResolveFutures(f) => {
                    let results: Vec<(u32, ExtFunctionResult)> = f
                        .pending_call_ids()
                        .iter()
                        .map(|&id| (id, ExtFunctionResult::Return(MontyObject::None)))
                        .collect();
                    progress = f
                        .resume(results, PrintWriter::Disabled)
                        .map_err(start_err)?;
                }
            }
        }
    }

    pub fn work_dir(&self) -> &Path {
        &self.work_dir
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Borrow the session's handle registry.
    pub fn handles(&self) -> &Registry<()> {
        &self.handles
    }

    /// Borrow the session's connector (the only thing that touches a source).
    pub fn source(&self) -> &dyn Source {
        self.source.as_ref()
    }

    /// Consume the session and surface a teardown error, for callers who want
    /// the wipe to be loud rather than best-effort. `Drop` still runs as a
    /// backstop if you don't call this.
    pub fn close(self) -> Result<(), DropletError> {
        fs::remove_dir_all(&self.work_dir)?;
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Best-effort cleanup; never panic in a destructor.
        let _ = fs::remove_dir_all(&self.work_dir);
    }
}

/// Fold monty's boxed start/resume error (which carries the surviving REPL + the exception) into
/// the one boundary error type (invariant #10). The surviving REPL is dropped — see run_code's note.
fn start_err(e: Box<ReplStartError<NoLimitTracker>>) -> DropletError {
    DropletError::Monty(e.error)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write `sales.parquet` (region:str, amt:DOUBLE). `amt` is cast to DOUBLE: a decimal literal is
    /// DECIMAL in DuckDB and `SUM` widens to Decimal128, which the capped read-out can't decode yet.
    fn write_sales_parquet(dir: &std::path::Path) -> String {
        let path = dir.join("sales.parquet");
        let p = path.to_str().unwrap().to_string();
        let conn = duckdb::Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "COPY (SELECT region, amt::DOUBLE AS amt \
             FROM (VALUES ('EU', 100.0), ('EU', 50.0), ('US', 200.0)) AS t(region, amt)) \
             TO '{p}' (FORMAT PARQUET)"
        ))
        .unwrap();
        p
    }

    /// FIRST WORKING DROPLET (pure Rust): agent code in the Monty sandbox calls the macro-generated
    /// `query` tool and gets the real aggregates back into its own code. This is V1a's "Done when".
    #[test]
    fn run_code_runs_agent_program_against_local_parquet() -> Result<(), DropletError> {
        let dir = std::env::temp_dir().join("droplet-v1a-runcode-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = write_sales_parquet(&dir);

        let mut session = Session::new("run-v1a")?;
        // The agent's program: query -> print -> leave `rows` as the final expression so its value
        // crosses back as the run_code result (proving the aggregates reached the agent's code).
        let code = format!(
            "rows = query({path:?}, 'SELECT region, SUM(amt) AS t FROM data GROUP BY region')\n\
             print(rows)\n\
             rows"
        );
        let value = session.run_code(&code)?;

        let MontyObject::List(items) = value else {
            panic!("expected list[dict], got {value:?}");
        };
        let mut got = std::collections::BTreeMap::new();
        for it in items {
            let MontyObject::Dict(pairs) = it else {
                panic!()
            };
            let (mut region, mut t) = (None, None);
            for (k, v) in pairs.clone() {
                if let MontyObject::String(k) = k {
                    match (k.as_str(), v) {
                        ("region", MontyObject::String(s)) => region = Some(s),
                        ("t", MontyObject::Float(f)) => t = Some(f),
                        _ => {}
                    }
                }
            }
            got.insert(region.unwrap(), t.unwrap());
        }
        assert_eq!(got.get("EU"), Some(&150.0));
        assert_eq!(got.get("US"), Some(&200.0));

        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    /// A call to a name that is not a registered tool must surface an error, not panic.
    #[test]
    fn run_code_unknown_tool_errors() {
        let mut session = Session::new("run-v1a-unknown").unwrap();
        let err = session.run_code("not_a_real_tool(1)");
        assert!(err.is_err(), "unknown tool must produce an error");
    }

    #[test]
    fn new_creates_a_fresh_work_dir() {
        let s = Session::new("run-123").unwrap();
        assert!(s.work_dir().is_dir()); // the dir exists on disk
    }

    #[test]
    fn drop_wipes_the_work_dir() {
        let path = {
            let s = Session::new("run-drop").unwrap();
            s.work_dir().to_path_buf()
        }; // session dropped here
        assert!(!path.exists(), "Drop should have wiped {path:?}");
    }

    #[test]
    fn session_owns_a_live_duck_engine() {
        // Session::new returning Ok already proves Connection::open_in_memory()
        // succeeded (it is built with `?` inside new). Reaching `s.duck()` proves
        // the engine lives behind the session boundary (invariant #6: host-side).
        let s = Session::new("run-duck").unwrap();
        let _engine: &crate::engine_duckdb::DuckEngine = s.duck();
    }

    /// The session-owned engine must be USABLE for analysis, not just readable: the
    /// dataset-producing primitives take `&mut self`, so the session needs `duck_mut()`.
    /// (Fails before `duck_mut` exists — the engine would be effectively write-only.)
    #[test]
    fn session_engine_is_usable_for_analysis() -> Result<(), DropletError> {
        let mut s = Session::new("run-duck-mut")?;
        let path = format!("{}/tests/data/sample.parquet", env!("CARGO_MANIFEST_DIR"));
        let ds = s.duck_mut().register_parquet(&path)?;
        assert_eq!(s.duck().scalar_i64(&ds, "SUM(amount)")?, 790);
        Ok(())
    }

    #[tokio::test]
    async fn session_carries_a_working_connector() {
        use crate::source::LoadRequest;

        let s = Session::new("run-src").unwrap();
        // The dev connector looks in work_dir for <dataset>.parquet.
        let file = s.work_dir().join("orders.parquet");
        std::fs::write(&file, b"PAR1...not-real-parquet...").unwrap();

        let got = s
            .source()
            .load(&LoadRequest {
                dataset: "orders".into(),
            })
            .await
            .unwrap();
        assert_eq!(got, file);
        assert!(got.exists());
    }
}
