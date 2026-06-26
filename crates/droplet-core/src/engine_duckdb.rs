//! The local analyze engine (M1) — DuckDB behind the boundary.
//!
//! DuckDB is an always-on core dependency (no longer feature-gated), and the `droplet-py` wheel
//! binds straight to this surface. DuckDB is an in-process OLAP SQL engine (like SQLite, but
//! column-oriented for analytics): it runs inside
//! our Rust process — no server, no port, no network. Each `Session` owns one ephemeral
//! in-memory connection that dies with the process (invariant #3 isolation, invariant #5 never
//! serialize the engine). The sandbox never sees a `Connection`, only opaque `Dataset` handles
//! (invariant #6).

use duckdb::Connection;
// Arrow types come THROUGH DuckDB's re-export (`duckdb` does `pub use arrow;`), so this is
// the exact same Arrow that DuckDB returns. Never add a top-level `arrow` dep — two Arrow
// majors in the tree produce the infamous `expected RecordBatch, found RecordBatch` (invariant #10).
use duckdb::arrow::record_batch::RecordBatch;

/// The default cap on how many rows any tool may move into the sandbox in one result
/// (invariant #6). It bounds every `to_rows` the agent sees, which keeps snapshots small later
/// (M8). It is now a *default*, not a hard limit: each `DuckEngine` carries its own
/// `max_result_rows` (see `set_max_result_rows`) so a caller can tune the boundary per session.
pub const DEFAULT_MAX_RESULT_ROWS: usize = 1000;

/// One run's local analyze engine — a host-side wrapper around a single ephemeral,
/// in-memory DuckDB connection. Lives behind the boundary (invariant #6): the sandbox
/// never holds a `DuckEngine` or a `Connection`, only the `Dataset` handles it hands back.
///
/// A `Connection` is `!Sync` (must not be *shared* across threads — it may still be *moved*
/// to one, which is what `spawn_blocking` does). Owning exactly one per `Session` matches
/// both DuckDB's threading rule and Droplet's per-session isolation (invariant #3): one run =
/// one Session = one ephemeral local DuckDB. Never pool or share connections between sessions.
pub struct DuckEngine {
    conn: Connection,
    /// Monotonic counter naming the next dataset view `ds_{n}`. Only ever increases, so
    /// `ds_0`, `ds_1`, … are unique within a session and never reused — the same trick as
    /// M0's handle `Registry`.
    next_id: u64,
    /// The per-engine cap on rows crossing the boundary in one `to_rows` (invariant #6).
    /// Defaults to `DEFAULT_MAX_RESULT_ROWS`; tunable via `set_max_result_rows`.
    max_result_rows: usize,
}

impl DuckEngine {
    /// One ephemeral in-memory DuckDB per Session. `open_in_memory()` (== path ":memory:")
    /// dies with the process; we never persist or serialize the engine itself (invariant #5).
    ///
    /// Invariant #3 is enforced STRUCTURALLY here, not by convention: the engine is locked so
    /// it cannot reach a network data source, while still reading LOCAL files (which the
    /// analyze surface needs). Stock DuckDB auto-installs/auto-loads `httpfs` on the first
    /// remote path and would egress to S3/HTTP; we (a) turn that autoload off, and (b) disable
    /// the HTTP/S3 filesystems outright. `disabled_filesystems` is a one-way latch — DuckDB
    /// rejects clearing it on a running database — so even if arbitrary `local_sql` explicitly
    /// `LOAD`s httpfs, an `s3://`/`https://` read still fails instantly with a permission
    /// error and no network round-trip. (We deliberately do NOT use `enable_external_access`,
    /// which would also block local-file reads. Local-filesystem sandboxing — e.g. reading
    /// `/etc/passwd` via `read_csv` — is a separate, ACCEPTED V1a gap (host-data exfiltration),
    /// closed at the V3 load boundary by scoping reads to the host-controlled cache dir
    /// (`allowed_directories` + `enable_external_access=false`). Full writeup:
    /// `docs/security/2026-06-24-v1a-local-fs-read-gap.md`.)
    pub fn new_in_memory() -> Result<Self, crate::DropletError> {
        let conn = Connection::open_in_memory()?; // duckdb::Error -> DropletError via #[from]
        conn.execute_batch(
            "SET autoinstall_known_extensions=false; \
             SET autoload_known_extensions=false; \
             SET disabled_filesystems='HTTPFileSystem,S3FileSystem';",
        )?;
        Ok(Self {
            conn,
            next_id: 0,
            max_result_rows: DEFAULT_MAX_RESULT_ROWS,
        })
    }

    /// The current row cap a `to_rows` read-out clamps to (invariant #6).
    pub fn max_result_rows(&self) -> usize {
        self.max_result_rows
    }

    /// Set the per-engine row cap. The `droplet-py` wheel surfaces this as
    /// `Engine(max_result_rows=...)`; lower it to shrink the boundary crossing for a session.
    pub fn set_max_result_rows(&mut self, max_result_rows: usize) {
        self.max_result_rows = max_result_rows;
    }

    /// Define a fresh lazy `ds_{n}` view over `select_sql` and return its handle (a `CREATE
    /// VIEW` stores SQL text — it does not copy rows). Every handle-producing op
    /// (`register_parquet`, `filter_rows`, `group_agg`, `local_sql`) funnels through here —
    /// one place that mints names and creates views (DRY).
    fn new_view(&mut self, select_sql: &str) -> Result<Dataset, crate::DropletError> {
        let table = format!("ds_{}", self.next_id);
        self.next_id += 1;
        // SINGLE-statement guard — the seam that contains agent SQL. Every handle-producing op
        // splices agent text into `CREATE VIEW {table} AS {select_sql}`. We must NOT hand a
        // multi-statement string to the engine: the duckdb driver's `prepare`/`execute` calls
        // `duckdb_extract_statements` and RUNS every `;`-separated statement (it does not reject
        // them), so a `;`-smuggled second statement — `…; COPY (…) TO '<path>'` (arbitrary local
        // write), `…; CREATE OR REPLACE VIEW ds_0 …` (silent handle poisoning), smuggled CREATE
        // TABLE / ATTACH / INSTALL — would execute past the wrapper. `is_single_statement` rejects
        // any composed SQL that holds more than one statement (a lone trailing `;` is allowed).
        // Pinned by crates/droplet-core/src/security/writes_ddl.rs.
        let sql = format!("CREATE VIEW {table} AS {select_sql}");
        if !is_single_statement(&sql) {
            return Err(crate::DropletError::BadArg(
                "SQL contains more than one statement; only a single statement is allowed".into(),
            ));
        }
        self.conn.execute(&sql, [])?;
        Ok(Dataset { table })
    }

    /// Register a LOCAL Parquet file as a `Dataset` handle (a DuckDB view). No data is
    /// copied — `read_parquet` is a lazy table function and `CREATE VIEW` just gives the
    /// file a stable name; a query only touches what it needs.
    ///
    /// Invariant #3: callers pass only LOCAL paths, and the HTTP/S3 filesystems are disabled on
    /// the connection (see `new_in_memory`), so even a remote-looking path cannot egress — a
    /// `read_parquet('s3://…')` fails instantly with a permission error, no network.
    pub fn register_parquet(&mut self, path: &str) -> Result<Dataset, crate::DropletError> {
        // TODO(M2): bind the path. In M1 `path` comes from host-controlled tests, so a
        // format!-built SQL string is fine. Once a path can be derived from agent input,
        // switch to a bind parameter to avoid SQL-injection via the filename.
        self.new_view(&format!("SELECT * FROM read_parquet('{path}')"))
    }

    /// `WHERE` over a handle's table → a NEW handle (invariant #6: no rows cross). For M1 the
    /// predicate is a raw SQL string; the typed `eq`/`gt`/`between` builders land in M2.
    ///
    /// Invariant #3: the predicate may be any local SQL — safe precisely because it can only
    /// touch the local, ephemeral copy, never a source.
    pub fn filter_rows(
        &mut self,
        ds: &Dataset,
        where_sql: &str,
    ) -> Result<Dataset, crate::DropletError> {
        self.new_view(&format!("SELECT * FROM {} WHERE {}", ds.table(), where_sql))
    }

    /// `GROUP BY` over a handle → a NEW handle. `by` is the grouping columns; `metrics` is a
    /// list of `(alias, sql_expr)` spliced into the SELECT (e.g. `("total", "SUM(amount)")`).
    /// The aggregate result stays a handle (invariant #6); the agent calls `to_rows` when it
    /// actually wants the (small) numbers.
    pub fn group_agg(
        &mut self,
        ds: &Dataset,
        by: &[&str],
        metrics: &[(&str, &str)],
    ) -> Result<Dataset, crate::DropletError> {
        // SELECT = grouping columns first, then each "expr AS alias".
        let mut cols: Vec<String> = by.iter().map(|c| c.to_string()).collect();
        for (alias, expr) in metrics {
            cols.push(format!("{expr} AS {alias}"));
        }
        let select_cols = cols.join(", ");
        // An empty `by` is a grand-total over the whole dataset: omit GROUP BY entirely
        // (DuckDB treats a pure-aggregate SELECT with no GROUP BY as a single group). A
        // trailing `GROUP BY ` with no columns would be a parser error.
        let sql = if by.is_empty() {
            format!("SELECT {select_cols} FROM {}", ds.table())
        } else {
            format!(
                "SELECT {select_cols} FROM {} GROUP BY {}",
                ds.table(),
                by.join(", ")
            )
        };
        self.new_view(&sql)
    }

    /// The UNRESTRICTED escape hatch — arbitrary local DuckDB SQL over named datasets → a NEW
    /// handle. `datasets` maps the readable names the agent writes in the SQL (e.g. `usage`) to
    /// the real `ds_{n}` views. This is safe *because* it is local and ephemeral (invariant #3):
    /// wide open, yet unable to reach a network source — the HTTP/S3 filesystems are disabled
    /// on the connection (see `new_in_memory`), so even `read_parquet('s3://…')`/`COPY … TO
    /// 's3://…'` fails instantly with no network. It must NEVER leak to the load side, where
    /// SQL is never arbitrary (invariant #2).
    pub fn local_sql(
        &mut self,
        sql: &str,
        datasets: &[(&str, &Dataset)],
    ) -> Result<Dataset, crate::DropletError> {
        // Bind each alias to its real `ds_{n}` view as a CTE, so the resulting view is
        // SELF-CONTAINED: its stored definition references the stable `ds_{n}` names, never a
        // session-scoped temp view. This keeps every returned handle stable even if a later
        // call reuses the same alias for a different dataset (CTE scope is the query, so it
        // also cannot shadow a real `ds_{n}` handle).
        // TODO(M2): aliases are host-controlled in M1; when they can derive from agent input,
        // validate each as a bare SQL identifier before splicing it here.
        let full = if datasets.is_empty() {
            sql.to_string()
        } else {
            let ctes = datasets
                .iter()
                .map(|(alias, ds)| format!("{alias} AS (SELECT * FROM {})", ds.table()))
                .collect::<Vec<_>>()
                .join(", ");
            format!("WITH {ctes} {sql}")
        };
        self.new_view(&full)
    }

    /// SYNC: run a SELECT and collect the results as Arrow batches. Internal — callers cap.
    /// A prepared statement compiles the SQL once; `query_arrow([])` runs it (no bind params)
    /// and yields an iterator of `RecordBatch` that `.collect()` gathers. DuckDB blocks the
    /// OS thread here, which is why the public async entrypoint wraps callers in spawn_blocking.
    fn query_arrow_blocking(&self, sql: &str) -> Result<Vec<RecordBatch>, crate::DropletError> {
        let mut stmt = self.conn.prepare(sql)?; // query_arrow takes &mut self
        let batches: Vec<RecordBatch> = stmt.query_arrow([])?.collect();
        Ok(batches)
    }

    /// Move up to `self.max_result_rows` rows of a dataset into the caller as Arrow — one of only
    /// two functions allowed to cross the boundary, and it is capped (invariant #6). The SQL
    /// `LIMIT` lets DuckDB stop early (it never materializes more than the cap); `cap_batches`
    /// is a second, code-side guard.
    pub fn to_rows(&self, ds: &Dataset) -> Result<Vec<RecordBatch>, crate::DropletError> {
        let cap = self.max_result_rows;
        let sql = format!("SELECT * FROM {} LIMIT {}", ds.table(), cap);
        let batches = self.query_arrow_blocking(&sql)?;
        Ok(cap_batches(batches, cap))
    }

    /// Capped read-out as **plain Rust rows** — one `Vec<(column_name, Value)>` per row, in the
    /// dataset's column order. This is the Arrow-free face of `to_rows`: it lets a caller that
    /// must not depend on Arrow (the `droplet-py` wheel — invariant #10's "use the `duckdb::arrow`
    /// re-export, never a top-level arrow dep" trap stays contained here — and, later, M3's Monty
    /// driver building a `list[dict]` `MontyObject`) read results without naming an Arrow type.
    /// Same cap as `to_rows` (invariant #6).
    pub fn to_rows_values(
        &self,
        ds: &Dataset,
    ) -> Result<Vec<Vec<(String, Value)>>, crate::DropletError> {
        let batches = self.to_rows(ds)?;
        let mut rows = Vec::new();
        for batch in &batches {
            let names: Vec<String> = batch
                .schema()
                .fields()
                .iter()
                .map(|f| f.name().clone())
                .collect();
            for r in 0..batch.num_rows() {
                let mut row = Vec::with_capacity(names.len());
                for (c, name) in names.iter().enumerate() {
                    row.push((name.clone(), cell_value(batch.column(c), r)?));
                }
                rows.push(row);
            }
        }
        Ok(rows)
    }

    /// Pull exactly one numeric value out (e.g. a COUNT or SUM) — the narrowest boundary
    /// crossing, inherently capped at one value (invariant #6). `CAST(... AS BIGINT)` is
    /// load-bearing: `SUM` over an INTEGER column is a DuckDB HUGEINT (i128), and reading
    /// that as `i64` raises a runtime `InvalidColumnType`. The CAST makes the column type
    /// unambiguously i64. (M1 hard-codes i64; a typed value enum lands later.)
    ///
    /// Requires a single, non-NULL value: an aggregate over zero rows (e.g. `SUM` after a
    /// filter that matches nothing) yields NULL and surfaces a `DropletError` rather than a
    /// clean result. A defined empty/NULL contract (`Option`/`COALESCE`) lands with the typed
    /// value enum.
    pub fn scalar_i64(&self, ds: &Dataset, expr: &str) -> Result<i64, crate::DropletError> {
        let sql = format!("SELECT CAST({expr} AS BIGINT) FROM {} LIMIT 1", ds.table());
        let v: i64 = self.conn.query_row(&sql, [], |r| r.get(0))?;
        Ok(v)
    }
}

/// One async boundary over a whole unit of local analyze work (invariant #9). DuckDB blocks
/// the OS thread while a query runs, so the synchronous primitives all run *inside* one
/// `spawn_blocking` — the async runtime's worker threads stay free.
///
/// Because a `Connection` is `!Sync`, the engine is created and OWNED INSIDE the task (never
/// shared across threads). The closure is `move` + `Send + 'static`: owned data in (a `String`,
/// not a `&str`), owned data out (`Vec<RecordBatch>`).
///
/// The `.await??` is two folds: `spawn_blocking(...).await` yields
/// `Result<Result<T, DropletError>, JoinError>`. The FIRST `?` unwraps Tokio's `JoinError` (did
/// the blocking task panic?); the SECOND `?` unwraps the inner `Result` (did the query fail?).
///
// GIL (droplet-py): when an analyze primitive is called via PyO3, the thin wrapper in
// droplet-py must release the GIL around the call (py.detach(...)) so other Python threads run
// while DuckDB works. droplet-core itself has NO pyo3 (invariant #8) — the GIL release lives
// ONLY in droplet-py. This comment is the seam, not an implementation.
pub async fn analyze_local_parquet(path: String) -> Result<Vec<RecordBatch>, crate::DropletError> {
    let rows =
        tokio::task::spawn_blocking(move || -> Result<Vec<RecordBatch>, crate::DropletError> {
            let mut eng = DuckEngine::new_in_memory()?; // owned here, on the blocking thread
            let ds = eng.register_parquet(&path)?;
            // CAST the metric so the result column is a clean BIGINT (i64), readable without
            // HUGEINT/Decimal handling — see scalar_i64's note on SUM-over-INTEGER.
            let agg = eng.group_agg(
                &ds,
                &["category"],
                &[("total", "CAST(SUM(amount) AS BIGINT)")],
            )?;
            eng.to_rows(&agg) // capped Arrow back
        })
        .await??; // outer ? = JoinError (task panic?); inner ? = DropletError (query fail?)
    Ok(rows)
}

/// Trim a batch list to at most `max_rows` total, slicing the boundary batch. `RecordBatch::slice`
/// is a zero-copy view (it shares the underlying buffers) and panics if `offset + len > num_rows`,
/// which is why we clamp `take` with `.min(num_rows)`.
fn cap_batches(batches: Vec<RecordBatch>, max_rows: usize) -> Vec<RecordBatch> {
    let mut out = Vec::new();
    let mut remaining = max_rows;
    for b in batches {
        if remaining == 0 {
            break;
        }
        let take = remaining.min(b.num_rows());
        out.push(b.slice(0, take)); // zero-copy view of the first `take` rows
        remaining -= take;
    }
    out
}

/// True iff `sql` is at most ONE statement (a lone trailing `;` is allowed). The single-statement
/// guard for agent SQL in `new_view` — see the long note there for *why* the engine itself can't be
/// trusted to reject multi-statement input.
///
/// FAIL-CLOSED by construction. It skips `;` only inside the lexical constructs where DuckDB closes
/// **no earlier** than this scanner does — single-quoted strings (`''` escape), double-quoted
/// identifiers (`""` escape), `--` line comments, and `/* */` block comments. Anything else (e.g.
/// `E'…'` escape strings or `$tag$…$tag$` dollar quotes, which this scanner does not model) stays
/// "code", so a `;` there is treated as a statement separator and the SQL is REJECTED. The result:
/// it can over-reject an exotic literal that contains `;`, but it can NEVER let a real second
/// statement slip past as "inside a string".
fn is_single_statement(sql: &str) -> bool {
    let b = sql.as_bytes();
    let mut i = 0;
    let mut terminated = false; // we have passed a top-level ';'
    while i < b.len() {
        let c = b[i];
        // Comments first, so a '-'/'/' that opens one is never mistaken for code.
        if c == b'-' && b.get(i + 1) == Some(&b'-') {
            i += 2;
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                i += 1;
            }
            i += 2; // step past the closing */ (may overshoot b.len() if unterminated; the `i < b.len()` loop guard ends the scan)
            continue;
        }
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        // Any non-whitespace, non-comment byte is real content. If we already passed a top-level
        // ';', this byte starts a second statement -> not a single statement.
        if terminated {
            return false;
        }
        match c {
            b'\'' => {
                i += 1;
                while i < b.len() {
                    if b[i] == b'\'' {
                        if b.get(i + 1) == Some(&b'\'') {
                            i += 2; // '' escape: stay inside the string
                            continue;
                        }
                        i += 1; // closing quote
                        break;
                    }
                    i += 1;
                }
            }
            b'"' => {
                i += 1;
                while i < b.len() {
                    if b[i] == b'"' {
                        if b.get(i + 1) == Some(&b'"') {
                            i += 2; // "" escape: stay inside the identifier
                            continue;
                        }
                        i += 1; // closing quote
                        break;
                    }
                    i += 1;
                }
            }
            b';' => {
                terminated = true;
                i += 1;
            }
            _ => i += 1,
        }
    }
    true
}

/// One cell of a capped read-out, as a plain Rust value the boundary can carry without Arrow.
/// M1 covers the column types the analyze surface actually produces (bools, integers, floats,
/// strings) and maps SQL `NULL` to `Null`; anything else surfaces a `DropletError::UnsupportedType`
/// rather than guessing. (The "typed value enum" the `scalar_i64` docs anticipate starts here.)
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
}

/// Read a single Arrow cell `(col, row)` into a plain `Value`. Downcasting is keyed on the
/// column's `DataType`, so each `downcast_ref` is infallible (the type was just matched). Arrow
/// types stay **inside this module** (via the `duckdb::arrow` re-export) — callers see only `Value`.
fn cell_value(
    col: &dyn duckdb::arrow::array::Array,
    row: usize,
) -> Result<Value, crate::DropletError> {
    use duckdb::arrow::array::{
        BooleanArray, Float32Array, Float64Array, Int8Array, Int16Array, Int32Array, Int64Array,
        LargeStringArray, StringArray, UInt8Array, UInt16Array, UInt32Array, UInt64Array,
    };
    use duckdb::arrow::datatypes::DataType;

    if col.is_null(row) {
        return Ok(Value::Null);
    }
    // `down::<T>()` downcasts to the concrete array we already matched on, then reads `row`.
    macro_rules! down {
        ($ty:ty) => {
            col.as_any()
                .downcast_ref::<$ty>()
                .expect("datatype matched")
        };
    }
    let value = match col.data_type() {
        DataType::Boolean => Value::Bool(down!(BooleanArray).value(row)),
        DataType::Int8 => Value::Int(down!(Int8Array).value(row) as i64),
        DataType::Int16 => Value::Int(down!(Int16Array).value(row) as i64),
        DataType::Int32 => Value::Int(down!(Int32Array).value(row) as i64),
        DataType::Int64 => Value::Int(down!(Int64Array).value(row)),
        DataType::UInt8 => Value::Int(down!(UInt8Array).value(row) as i64),
        DataType::UInt16 => Value::Int(down!(UInt16Array).value(row) as i64),
        DataType::UInt32 => Value::Int(down!(UInt32Array).value(row) as i64),
        DataType::UInt64 => Value::Int(down!(UInt64Array).value(row) as i64),
        DataType::Float32 => Value::Float(down!(Float32Array).value(row) as f64),
        DataType::Float64 => Value::Float(down!(Float64Array).value(row)),
        DataType::Utf8 => Value::Str(down!(StringArray).value(row).to_string()),
        DataType::LargeUtf8 => Value::Str(down!(LargeStringArray).value(row).to_string()),
        other => return Err(crate::DropletError::UnsupportedType(format!("{other:?}"))),
    };
    Ok(value)
}

/// An opaque handle to a table living inside the host's DuckDB. The sandbox holds these;
/// it never holds rows (invariant #6). Cheap to clone, cheap to pass, cheap to snapshot —
/// the actual columns never travel with it.
#[derive(Clone, Debug)]
pub struct Dataset {
    /// The DuckDB view/table name this handle resolves to, e.g. "ds_0".
    table: String,
}

impl Dataset {
    pub fn table(&self) -> &str {
        &self.table
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duckdb::Connection;

    /// The known-answer Parquet fixture, addressed from the crate root at compile time
    /// (`CARGO_MANIFEST_DIR`) so the path holds no matter the test's working directory.
    /// Data: (a,50),(a,150),(b,200),(b,90),(c,300) — so SUM=790; by category a→200,b→290,c→300.
    fn fixture_path() -> String {
        format!("{}/tests/data/sample.parquet", env!("CARGO_MANIFEST_DIR"))
    }

    /// Smallest possible proof that the DuckDB dependency + feature wiring links and runs.
    #[test]
    fn duckdb_links_and_answers() -> duckdb::Result<()> {
        let conn = Connection::open_in_memory()?;
        let answer: i64 = conn.query_row("SELECT 42", [], |r| r.get(0))?;
        assert_eq!(answer, 42);
        Ok(())
    }

    #[test]
    fn register_parquet_returns_a_handle() -> Result<(), crate::DropletError> {
        let mut eng = DuckEngine::new_in_memory()?;
        let ds = eng.register_parquet(&fixture_path())?;
        assert_eq!(ds.table(), "ds_0"); // first registration is named ds_0
        Ok(())
    }

    /// Total rows across all returned batches — the capped boundary crossing's size.
    fn total_rows(batches: &[duckdb::arrow::record_batch::RecordBatch]) -> usize {
        batches.iter().map(|b| b.num_rows()).sum()
    }

    #[test]
    fn to_rows_returns_every_row_under_the_cap() -> Result<(), crate::DropletError> {
        let mut eng = DuckEngine::new_in_memory()?;
        let ds = eng.register_parquet(&fixture_path())?;
        // The fixture has 5 rows, well under MAX_RESULT_ROWS, so all 5 come back.
        assert_eq!(total_rows(&eng.to_rows(&ds)?), 5);
        Ok(())
    }

    #[test]
    fn to_rows_clamps_to_the_max() -> Result<(), crate::DropletError> {
        // `eng` is not `mut` here: this test never calls `register_parquet` (the only
        // &mut method). `conn.execute_batch` and `to_rows` are both &self.
        let eng = DuckEngine::new_in_memory()?;
        // White-box: a 5000-row view (no fixture big enough). The tests module is a child
        // of engine_duckdb, so it may touch the private `conn` and build a `Dataset` directly.
        eng.conn
            .execute_batch("CREATE VIEW big AS SELECT * FROM range(5000)")?;
        let ds = Dataset {
            table: "big".to_string(),
        };
        assert_eq!(total_rows(&eng.to_rows(&ds)?), DEFAULT_MAX_RESULT_ROWS); // clamped to 1000
        Ok(())
    }

    /// The cap is no longer a hard-coded const: a fresh engine starts at the default, and a caller
    /// (e.g. the `droplet-py` wheel via `Engine(max_result_rows=...)`) can lower it. (Fails before
    /// the `max_result_rows` field + setter exist.)
    #[test]
    fn result_cap_is_configurable() -> Result<(), crate::DropletError> {
        let mut eng = DuckEngine::new_in_memory()?;
        assert_eq!(eng.max_result_rows(), DEFAULT_MAX_RESULT_ROWS); // default preserved

        eng.set_max_result_rows(3);
        // A 100-row view; with the cap lowered to 3, only 3 rows may cross.
        eng.conn
            .execute_batch("CREATE VIEW capped AS SELECT * FROM range(100)")?;
        let ds = Dataset {
            table: "capped".to_string(),
        };
        assert_eq!(total_rows(&eng.to_rows(&ds)?), 3);
        Ok(())
    }

    #[test]
    fn scalar_sums_the_column() -> Result<(), crate::DropletError> {
        let mut eng = DuckEngine::new_in_memory()?;
        let ds = eng.register_parquet(&fixture_path())?;
        assert_eq!(eng.scalar_i64(&ds, "SUM(amount)")?, 790);
        Ok(())
    }

    /// Read a 2-column (VARCHAR, BIGINT) Arrow result into a sorted Vec — used to assert the
    /// actual aggregate values that cross the boundary, not just the row count.
    fn str_i64_rows(batches: &[RecordBatch]) -> Vec<(String, i64)> {
        use duckdb::arrow::array::{Array, Int64Array, StringArray};
        let mut out = Vec::new();
        for b in batches {
            let keys = b
                .column(0)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("col 0 is VARCHAR");
            let vals = b
                .column(1)
                .as_any()
                .downcast_ref::<Int64Array>()
                .expect("col 1 is BIGINT");
            for i in 0..b.num_rows() {
                out.push((keys.value(i).to_string(), vals.value(i)));
            }
        }
        out.sort();
        out
    }

    #[test]
    fn filter_rows_keeps_only_matching() -> Result<(), crate::DropletError> {
        let mut eng = DuckEngine::new_in_memory()?;
        let ds = eng.register_parquet(&fixture_path())?;
        let kept = eng.filter_rows(&ds, "amount > 100")?; // 150, 200, 300
        assert_eq!(total_rows(&eng.to_rows(&kept)?), 3);
        assert_eq!(eng.scalar_i64(&kept, "SUM(amount)")?, 650);
        Ok(())
    }

    #[test]
    fn group_agg_sums_per_category() -> Result<(), crate::DropletError> {
        let mut eng = DuckEngine::new_in_memory()?;
        let ds = eng.register_parquet(&fixture_path())?;
        let agg = eng.group_agg(
            &ds,
            &["category"],
            &[("total", "CAST(SUM(amount) AS BIGINT)")],
        )?;
        let got = str_i64_rows(&eng.to_rows(&agg)?);
        assert_eq!(
            got,
            vec![("a".into(), 200), ("b".into(), 290), ("c".into(), 300)]
        );
        Ok(())
    }

    /// `to_rows_values` is the Arrow-free, capped read-out the `droplet-py` wheel binds to: it
    /// turns a `Dataset` into plain `Vec<(column, Value)>` rows so a non-Arrow caller can build a
    /// `list[dict]` without ever naming an Arrow type. (Fails before `Value` + `to_rows_values`.)
    #[test]
    fn to_rows_values_returns_plain_typed_rows() -> Result<(), crate::DropletError> {
        let mut eng = DuckEngine::new_in_memory()?;
        let ds = eng.register_parquet(&fixture_path())?;
        let agg = eng.group_agg(
            &ds,
            &["category"],
            &[("total", "CAST(SUM(amount) AS BIGINT)")],
        )?;

        let mut got: Vec<(String, i64)> = eng
            .to_rows_values(&agg)?
            .iter()
            .map(|row| {
                // Each row is the column order of the SELECT: (category VARCHAR, total BIGINT).
                let cat = match &row[0] {
                    (name, Value::Str(s)) if name == "category" => s.clone(),
                    other => panic!("col 0 should be category VARCHAR, got {other:?}"),
                };
                let total = match &row[1] {
                    (name, Value::Int(n)) if name == "total" => *n,
                    other => panic!("col 1 should be total BIGINT, got {other:?}"),
                };
                (cat, total)
            })
            .collect();
        got.sort();
        assert_eq!(
            got,
            vec![("a".into(), 200), ("b".into(), 290), ("c".into(), 300)]
        );
        Ok(())
    }

    /// The plain read-out honors the same cap as `to_rows` (it is built on top of it).
    #[test]
    fn to_rows_values_respects_the_cap() -> Result<(), crate::DropletError> {
        let mut eng = DuckEngine::new_in_memory()?;
        eng.set_max_result_rows(2);
        let ds = eng.register_parquet(&fixture_path())?; // 5 rows
        assert_eq!(eng.to_rows_values(&ds)?.len(), 2);
        Ok(())
    }

    #[test]
    fn local_sql_runs_arbitrary_sql_over_named_datasets() -> Result<(), crate::DropletError> {
        let mut eng = DuckEngine::new_in_memory()?;
        let ds = eng.register_parquet(&fixture_path())?;
        let agg = eng.local_sql(
            "SELECT category, CAST(SUM(amount) AS BIGINT) AS total FROM usage GROUP BY category",
            &[("usage", &ds)],
        )?;
        let got = str_i64_rows(&eng.to_rows(&agg)?);
        assert_eq!(
            got,
            vec![("a".into(), 200), ("b".into(), 290), ("c".into(), 300)]
        );
        Ok(())
    }

    #[tokio::test]
    async fn analyze_local_parquet_runs_the_chain_in_spawn_blocking()
    -> Result<(), crate::DropletError> {
        // The whole multi-step analyze runs inside one spawn_blocking; `.await??` folds both
        // JoinError (did the task panic?) and DropletError (did the query fail?).
        let batches = analyze_local_parquet(fixture_path()).await?;
        assert_eq!(
            str_i64_rows(&batches),
            vec![("a".into(), 200), ("b".into(), 290), ("c".into(), 300)]
        );
        Ok(())
    }

    // --- Regression tests from the M1 adversarial review ---

    /// Invariant #3 must be STRUCTURAL, not convention: a fresh engine must NOT autoload httpfs,
    /// so a remote path can't silently egress to S3/HTTP. Stock DuckDB defaults this ON, so the
    /// assertion fails before the hardening in `new_in_memory`. (`disabled_filesystems` is the
    /// belt-and-suspenders second layer, but it is write-only — `current_setting` returns "" —
    /// so it can't be asserted directly; its effect is verified out-of-band. Local reads still
    /// work, as every other test here reads the local fixture.)
    #[test]
    fn httpfs_autoload_is_disabled_on_a_fresh_engine() -> Result<(), crate::DropletError> {
        let eng = DuckEngine::new_in_memory()?;
        let autoload: bool = eng.conn.query_row(
            "SELECT current_setting('autoload_known_extensions')::BOOLEAN",
            [],
            |r| r.get(0),
        )?;
        assert!(
            !autoload,
            "httpfs autoload must be off so a remote path can't egress"
        );
        Ok(())
    }

    /// A `Dataset` handle returned by `local_sql` must be self-contained: reusing the same
    /// agent alias for a DIFFERENT dataset in a later call must NOT change what an earlier
    /// handle yields. (Fails before the CTE rewrite, where lingering TEMP VIEWs cross-talk.)
    #[test]
    fn local_sql_handles_survive_alias_reuse() -> Result<(), crate::DropletError> {
        let mut eng = DuckEngine::new_in_memory()?;
        let ds = eng.register_parquet(&fixture_path())?; // 5 rows

        let h1 = eng.local_sql("SELECT * FROM u", &[("u", &ds)])?;
        assert_eq!(total_rows(&eng.to_rows(&h1)?), 5);

        // Reuse alias "u" bound to a different, smaller dataset.
        let filtered = eng.filter_rows(&ds, "amount > 100")?; // 3 rows
        let _h2 = eng.local_sql("SELECT * FROM u", &[("u", &filtered)])?;

        // The FIRST handle must be untouched by the later alias reuse.
        assert_eq!(
            total_rows(&eng.to_rows(&h1)?),
            5,
            "old handle must be unaffected by later alias reuse"
        );
        Ok(())
    }

    /// The single-statement guard `new_view` uses to contain agent SQL. Must ACCEPT one statement
    /// (with a lone trailing `;` and embedded `;` inside strings/comments) and REJECT a real second
    /// statement. Fail-closed: anything it can't lex (dollar/E-strings with `;`) is over-rejected.
    #[test]
    fn is_single_statement_accepts_one_rejects_smuggled_second() {
        // Single statement — accepted.
        assert!(is_single_statement("SELECT 1"));
        assert!(is_single_statement("SELECT 1;")); // lone trailing ';'
        assert!(is_single_statement("SELECT 1 ;  \n  ")); // trailing ws after ';'
        assert!(is_single_statement("SELECT ';' AS x")); // ';' inside a string literal
        assert!(is_single_statement("SELECT 'it''s; ok' AS x")); // '' escape + ';' in string
        assert!(is_single_statement("SELECT 1 -- a ; b\n")); // ';' inside a line comment
        assert!(is_single_statement("SELECT 1 /* a ; b */")); // ';' inside a block comment
        assert!(is_single_statement("SELECT 1; -- trailing comment only")); // benign trailing
        assert!(is_single_statement(r#"SELECT "a;b" AS x"#)); // ';' inside a quoted identifier

        // A real second statement — rejected.
        assert!(!is_single_statement(
            "SELECT 1; CREATE TABLE evil AS SELECT 1"
        ));
        assert!(!is_single_statement(
            "SELECT 1; COPY (SELECT 1) TO '/tmp/x'"
        ));
        assert!(!is_single_statement(
            "SELECT * FROM data /* hide */ ; CREATE TABLE evil2 AS SELECT 1"
        ));
        assert!(!is_single_statement("SELECT 'a' ; DROP TABLE t")); // ';' after a closed string
    }

    /// An empty `by` is a natural call — a grand-total over the whole dataset, one row.
    /// (Fails before the fix: it builds a trailing `GROUP BY ` and DuckDB rejects it.)
    #[test]
    fn group_agg_with_no_grouping_is_a_grand_total() -> Result<(), crate::DropletError> {
        let mut eng = DuckEngine::new_in_memory()?;
        let ds = eng.register_parquet(&fixture_path())?;
        let total = eng.group_agg(&ds, &[], &[("total", "SUM(amount)")])?;
        assert_eq!(total_rows(&eng.to_rows(&total)?), 1); // exactly one grand-total row
        assert_eq!(eng.scalar_i64(&total, "total")?, 790);
        Ok(())
    }
}
