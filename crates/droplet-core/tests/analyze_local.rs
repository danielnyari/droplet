//! M1 integration test — the full LOCAL analyze chain over a local Parquet fixture, driven
//! through droplet-core's PUBLIC API exactly as an outside caller would (only `pub` items are
//! reachable here). This locks the spec's build-order step 3 "Done when": *a local analyze
//! engine runs the dataframe primitives and `local_sql` over a local Parquet `Dataset`, capped.*
//!
//! The async/`spawn_blocking` half of the "Done when" is covered by the feature-gated
//! `#[tokio::test] analyze_local_parquet_runs_the_chain_in_spawn_blocking` unit test.
//!
//! Values are asserted via `scalar_i64` over filtered sub-views — no Arrow downcasting, no
//! `duckdb`/`arrow` dependency in this test crate. The Arrow payload of `to_rows` is verified
//! directly in the engine's unit tests (`group_agg_sums_per_category`, `local_sql_…`).
#![cfg(feature = "duckdb")]

use droplet_core::engine_duckdb::DuckEngine;

/// The known-answer fixture: (a,50),(a,150),(b,200),(b,90),(c,300).
/// By category: a→200, b→290, c→300; total SUM=790; rows with amount>100 → 3.
fn fixture_path() -> String {
    format!("{}/tests/data/sample.parquet", env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn full_local_analyze_chain() -> Result<(), droplet_core::DropletError> {
    let mut eng = DuckEngine::new_in_memory()?;

    // 1. register the LOCAL Parquet → an opaque Dataset handle (a DuckDB view).
    let usage = eng.register_parquet(&fixture_path())?;

    // 2. filter_rows (handle → handle) → 3. to_rows (capped read-out). amount>100 keeps 150,200,300.
    //    `.num_rows()` is an inherent method on the returned batches, so no Arrow type is named.
    let big = eng.filter_rows(&usage, "amount > 100")?;
    let big_rows: usize = eng.to_rows(&big)?.iter().map(|b| b.num_rows()).sum();
    assert_eq!(big_rows, 3);
    assert_eq!(eng.scalar_i64(&big, "SUM(amount)")?, 650);

    // 4. group_agg (handle → handle) → to_rows: 3 groups, asserting a→200, b→290, c→300.
    let agg = eng.group_agg(&usage, &["category"], &[("total", "SUM(amount)")])?;
    let agg_rows: usize = eng.to_rows(&agg)?.iter().map(|b| b.num_rows()).sum();
    assert_eq!(agg_rows, 3);
    for (cat, want) in [("a", 200), ("b", 290), ("c", 300)] {
        let one = eng.filter_rows(&agg, &format!("category = '{cat}'"))?;
        assert_eq!(eng.scalar_i64(&one, "total")?, want);
    }

    // 5. scalar over the whole dataset → the single SUM value.
    assert_eq!(eng.scalar_i64(&usage, "SUM(amount)")?, 790);

    // 6. local_sql (UNRESTRICTED, but local & ephemeral) over a named dataset → same aggregate.
    let via_sql = eng.local_sql(
        "SELECT category, SUM(amount) AS total FROM u GROUP BY category",
        &[("u", &usage)],
    )?;
    let sql_rows: usize = eng.to_rows(&via_sql)?.iter().map(|b| b.num_rows()).sum();
    assert_eq!(sql_rows, 3);
    for (cat, want) in [("a", 200), ("b", 290), ("c", 300)] {
        let one = eng.filter_rows(&via_sql, &format!("category = '{cat}'"))?;
        assert_eq!(eng.scalar_i64(&one, "total")?, want);
    }

    Ok(())
}
