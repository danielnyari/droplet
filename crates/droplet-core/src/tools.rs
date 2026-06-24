//! Fixed analyze primitives exposed to sandboxed agent code via `#[droplet_tool]`.
//!
//! V1a ships exactly one: `query`. V1b adds the handle-based surface (filter_rows/group_agg/...).

use droplet_macros::droplet_tool;

use crate::DropletError;
use crate::convert::Rows;
use crate::engine_duckdb::DuckEngine;

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
pub fn query(eng: &mut DuckEngine, path: String, sql: String) -> Result<Rows, DropletError> {
    let ds = eng.register_parquet(&path)?;
    let result = eng.local_sql(&sql, &[("data", &ds)])?;
    Ok(Rows(eng.to_rows_values(&result)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use monty::MontyObject;

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

        let tool = inventory::iter::<crate::tool::Tool>()
            .find(|t| t.name == "query")
            .unwrap();
        let mut eng = DuckEngine::new_in_memory().unwrap();
        let out = (tool.dispatch)(
            &mut eng,
            &[
                MontyObject::String(path),
                MontyObject::String(
                    "SELECT region, SUM(amt) AS t FROM data GROUP BY region".into(),
                ),
            ],
            &[],
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
}
