//! Fixed analyze primitives exposed to sandboxed agent code via `#[droplet_tool]`.
//!
//! V1a ships exactly one: `query`. V1b adds the handle-based surface (filter_rows/group_agg/...).

use droplet_macros::droplet_tool;

use crate::DropletError;
use crate::convert::Rows;
use crate::engine_duckdb::Dataset;
use crate::tool::ToolCx;

/// Run read-only SQL over a single local Parquet file, returning the (capped) result rows.
///
/// The agent writes `FROM data` in `sql`; `data` is bound to the file at `path`. The engine's cap
/// (invariant #6) bounds how many rows cross back. Local file only — the engine has the network
/// filesystems disabled (invariant #3), so a remote path fails instantly with no egress.
///
/// SECURITY — ACCEPTED V1a GAP: the agent controls both `path` and `sql`, and the local filesystem
/// is not sandboxed, so agent SQL can read arbitrary host files via `read_csv`/`read_blob`/`glob`
/// (host-data exfiltration). Network egress and writes are blocked; local read is not. Closed at the
/// V3 load boundary. Full writeup: `docs/security/2026-06-24-v1a-local-fs-read-gap.md`.
#[droplet_tool]
pub fn query(cx: &mut ToolCx, path: String, sql: String) -> Result<Rows, DropletError> {
    let ds = cx.engine.register_parquet(&path)?;
    let result = cx.engine.local_sql(&sql, &[("data", &ds)])?;
    Ok(Rows(cx.engine.to_rows_values(&result)?))
}

// --- The handle-based local analyze surface (V1b) -------------------------------------------------
//
// These return opaque `Dataset` handles (invariant #6): the agent chains them without rows ever
// crossing, until it calls `to_rows`/`scalar`. All SQL fragments are local & ephemeral (invariant
// #3). Same accepted local-file-read gap as `query` (agent-supplied path/SQL) — see the SECURITY
// note above and docs/security/2026-06-24-v1a-local-fs-read-gap.md; closed at the V3 load boundary.

/// Register a LOCAL Parquet file as a `Dataset` handle (no rows copied). The agent's entry point to
/// the analyze surface in V1b; the governed `load(dataset, …)` replaces it in V3.
#[droplet_tool]
pub fn register(cx: &mut ToolCx, path: String) -> Result<Dataset, DropletError> {
    cx.engine.register_parquet(&path)
}

/// `WHERE` over a handle → a new handle. `where_sql` is a local predicate (e.g. `"amt > 100"`).
#[droplet_tool]
pub fn filter_rows(
    cx: &mut ToolCx,
    ds: Dataset,
    where_sql: String,
) -> Result<Dataset, DropletError> {
    cx.engine.filter_rows(&ds, &where_sql)
}

/// `GROUP BY` over a handle → a new handle. `by` is the grouping columns; `metrics` is a list of
/// `(alias, sql_expr)` pairs, e.g. `[("total", "SUM(amt)")]`.
#[droplet_tool]
pub fn group_agg(
    cx: &mut ToolCx,
    ds: Dataset,
    by: Vec<String>,
    metrics: Vec<(String, String)>,
) -> Result<Dataset, DropletError> {
    let by_refs: Vec<&str> = by.iter().map(String::as_str).collect();
    let metric_refs: Vec<(&str, &str)> = metrics
        .iter()
        .map(|(a, e)| (a.as_str(), e.as_str()))
        .collect();
    cx.engine.group_agg(&ds, &by_refs, &metric_refs)
}

/// Add a derived column → a new handle. `expr` is a local SQL expression aliased as `name`.
#[droplet_tool]
pub fn with_column(
    cx: &mut ToolCx,
    ds: Dataset,
    name: String,
    expr: String,
) -> Result<Dataset, DropletError> {
    cx.engine.local_sql(
        &format!("SELECT *, {expr} AS {name} FROM base"),
        &[("base", &ds)],
    )
}

/// Inner-join two handles on a local predicate → a new handle. `on` references aliases `l`/`r`,
/// e.g. `"l.account_id = r.account_id"`.
#[droplet_tool]
pub fn join(
    cx: &mut ToolCx,
    left: Dataset,
    right: Dataset,
    on: String,
) -> Result<Dataset, DropletError> {
    cx.engine.local_sql(
        &format!("SELECT * FROM l JOIN r ON {on}"),
        &[("l", &left), ("r", &right)],
    )
}

/// `ORDER BY` over a handle → a new handle. `by` is a local SQL order list, e.g. `"total DESC"`.
#[droplet_tool]
pub fn sort(cx: &mut ToolCx, ds: Dataset, by: String) -> Result<Dataset, DropletError> {
    cx.engine.local_sql(
        &format!("SELECT * FROM base ORDER BY {by}"),
        &[("base", &ds)],
    )
}

/// Arbitrary local DuckDB SQL over named handles → a new handle. `datasets` maps the names used in
/// the SQL to handles, e.g. `[("usage", h)]`. Unrestricted because it is local & ephemeral.
#[droplet_tool]
pub fn local_sql(
    cx: &mut ToolCx,
    sql: String,
    datasets: Vec<(String, Dataset)>,
) -> Result<Dataset, DropletError> {
    let refs: Vec<(&str, &Dataset)> = datasets.iter().map(|(a, d)| (a.as_str(), d)).collect();
    cx.engine.local_sql(&sql, &refs)
}

/// Move up to the cap (invariant #6) of a handle's rows into the sandbox as `list[dict]`. One of the
/// only two ops that move values (the other is `scalar`); everything else stays a handle.
#[droplet_tool]
pub fn to_rows(cx: &mut ToolCx, ds: Dataset) -> Result<Rows, DropletError> {
    Ok(Rows(cx.engine.to_rows_values(&ds)?))
}

/// Pull a single integer out of a handle (e.g. `SUM`/`COUNT`) — the narrowest value crossing.
#[droplet_tool]
pub fn scalar(cx: &mut ToolCx, ds: Dataset, expr: String) -> Result<i64, DropletError> {
    cx.engine.scalar_i64(&ds, &expr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use monty::MontyObject;

    use crate::engine_duckdb::DuckEngine;
    use crate::registry::Registry;
    use crate::tool::Tool;

    /// Drive a dispatch fn against a throwaway context (fresh engine + empty handle registry).
    fn dispatch(name: &str, args: &[MontyObject]) -> Result<MontyObject, DropletError> {
        let tool = inventory::iter::<Tool>()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("tool {name} must be registered"));
        let mut engine = DuckEngine::new_in_memory().unwrap();
        let mut handles = Registry::new();
        let mut cx = ToolCx {
            engine: &mut engine,
            handles: &mut handles,
        };
        (tool.dispatch)(&mut cx, args, &[])
    }

    /// Write a tiny `sales.parquet` (region:str, amt:DOUBLE) via a throwaway DuckDB connection.
    /// `amt` is cast to DOUBLE on purpose: a decimal literal like `100.0` is DECIMAL in DuckDB, and
    /// `SUM` over DECIMAL/INTEGER widens to DECIMAL/HUGEINT (Arrow Decimal128) which the capped
    /// read-out does not yet decode; DOUBLE -> Float64 crosses cleanly. (HUGEINT/DECIMAL decoding
    /// is a later engine refinement.)
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

    #[test]
    fn query_tool_is_registered_with_stub() {
        let tool = inventory::iter::<crate::tool::Tool>()
            .find(|t| t.name == "query")
            .expect("query must be registered");
        assert_eq!(
            tool.stub,
            "def query(path: str, sql: str) -> list[dict]: ..."
        );
    }

    #[test]
    fn query_returns_aggregates_via_dispatch() {
        let dir = std::env::temp_dir().join("droplet-v1a-query-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = write_sales_parquet(&dir);

        let out = dispatch(
            "query",
            &[
                MontyObject::String(path),
                MontyObject::String(
                    "SELECT region, SUM(amt) AS t FROM data GROUP BY region".into(),
                ),
            ],
        )
        .unwrap();

        // list[dict] back: {region -> t}.
        let MontyObject::List(items) = out else {
            panic!("expected a list");
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
    }

    #[test]
    fn analyze_tools_have_correct_stubs() {
        // Also validates the macro's compound-type mapping: Vec<T> -> list[T], tuples, Dataset.
        let want = [
            ("register", "def register(path: str) -> Dataset: ..."),
            (
                "filter_rows",
                "def filter_rows(ds: Dataset, where_sql: str) -> Dataset: ...",
            ),
            (
                "group_agg",
                "def group_agg(ds: Dataset, by: list[str], metrics: list[tuple[str, str]]) -> Dataset: ...",
            ),
            (
                "with_column",
                "def with_column(ds: Dataset, name: str, expr: str) -> Dataset: ...",
            ),
            (
                "join",
                "def join(left: Dataset, right: Dataset, on: str) -> Dataset: ...",
            ),
            ("sort", "def sort(ds: Dataset, by: str) -> Dataset: ..."),
            (
                "local_sql",
                "def local_sql(sql: str, datasets: list[tuple[str, Dataset]]) -> Dataset: ...",
            ),
            ("to_rows", "def to_rows(ds: Dataset) -> list[dict]: ..."),
            ("scalar", "def scalar(ds: Dataset, expr: str) -> int: ..."),
        ];
        for (name, stub) in want {
            let tool = inventory::iter::<Tool>().find(|t| t.name == name).unwrap();
            assert_eq!(tool.stub, stub, "stub mismatch for {name}");
        }
    }

    /// Threads `Dataset` handles through several tools within ONE context: register -> filter_rows
    /// -> group_agg -> to_rows. Proves handles cross as opaque ints and resolve back to host-side
    /// datasets (invariant #6) — no rows move until `to_rows`.
    #[test]
    fn handle_tools_chain_within_a_context() {
        let dir = std::env::temp_dir().join("droplet-v1b-chain");
        std::fs::create_dir_all(&dir).unwrap();
        let path = write_sales_parquet(&dir);

        let mut engine = DuckEngine::new_in_memory().unwrap();
        let mut handles = Registry::new();
        let mut cx = ToolCx {
            engine: &mut engine,
            handles: &mut handles,
        };
        let run = |cx: &mut ToolCx, name: &str, args: &[MontyObject]| {
            let tool = inventory::iter::<Tool>().find(|t| t.name == name).unwrap();
            (tool.dispatch)(cx, args, &[]).unwrap()
        };

        let h0 = run(&mut cx, "register", &[MontyObject::String(path)]);
        assert!(
            matches!(h0, MontyObject::Int(_)),
            "register returns a handle, not rows"
        );
        let h1 = run(
            &mut cx,
            "filter_rows",
            &[h0, MontyObject::String("amt > 60".into())],
        );
        let by = MontyObject::List(vec![MontyObject::String("region".into())]);
        let metrics = MontyObject::List(vec![MontyObject::Tuple(vec![
            MontyObject::String("total".into()),
            MontyObject::String("SUM(amt)".into()),
        ])]);
        let h2 = run(&mut cx, "group_agg", &[h1, by, metrics]);
        let rows = run(&mut cx, "to_rows", &[h2]);

        // amt>60 keeps EU 100 and US 200 (EU's 50 dropped); grouped SUM per region.
        let MontyObject::List(items) = rows else {
            panic!("to_rows returns a list")
        };
        let mut got = std::collections::BTreeMap::new();
        for it in items {
            let MontyObject::Dict(pairs) = it else {
                panic!()
            };
            let (mut region, mut total) = (None, None);
            for (k, v) in pairs.clone() {
                if let MontyObject::String(k) = k {
                    match (k.as_str(), v) {
                        ("region", MontyObject::String(s)) => region = Some(s),
                        ("total", MontyObject::Float(f)) => total = Some(f),
                        _ => {}
                    }
                }
            }
            got.insert(region.unwrap(), total.unwrap());
        }
        assert_eq!(got.get("EU"), Some(&100.0));
        assert_eq!(got.get("US"), Some(&200.0));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bad_handle_is_a_clean_error() {
        // Passing a handle that was never registered must surface BadHandle, not panic.
        let err = dispatch("to_rows", &[MontyObject::Int(999)]).unwrap_err();
        assert!(matches!(err, DropletError::BadHandle(999)), "got {err:?}");
    }
}
