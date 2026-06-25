// crates/droplet-core/src/security/result_cap.rs
//! Result-cap / boundary-volume (invariant #6) — adversarial angles. seam: `engine_duckdb.rs` row cap + `run_code`'s uncapped final return value.
#![allow(unused_imports)]
use monty::MontyObject;
use crate::DropletError;
use crate::session::Session;
use crate::engine_duckdb::{DuckEngine, Dataset, DEFAULT_MAX_RESULT_ROWS};
use crate::registry::Registry;
use crate::tool::{Tool, ToolCx};
use super::{dispatch, catch_dispatch, tmp_dir, sales_parquet, write_parquet, list_len};

/// `HOLDS` — Cap on the pure handle/to_rows value-move seam where rows never touch a parquet file (distinct from the register_parquet/query path).
/// seam: engine_duckdb.rs to_rows LIMIT clamp + cap_batches over a pure handle; tools.rs to_rows/local_sql; session.rs run_code suspend/resume
#[test]
fn cap_holds_via_to_rows_handle_path_2500_to_1000() {
    let mut s = Session::new("rc-torows-2500").unwrap();
    // 2500-row dataset built entirely as a HANDLE (range view, no parquet file), then crossed via to_rows.
    let out = s.run_code("to_rows(local_sql('SELECT * FROM range(2500)', []))").unwrap();
    assert_eq!(list_len(&out), DEFAULT_MAX_RESULT_ROWS, "to_rows() over a pure handle must clamp 2500 -> 1000 just like query()"); // expected: 1000
}

/// `HOLDS` — Boundary arithmetic: probes the precise off-by-one at the cap edge (LIMIT 1000 vs the cap_batches .min()), a mechanism a bulk 2500-row test cannot expose.
/// seam: engine_duckdb.rs to_rows 'SELECT ... LIMIT {cap}' arithmetic + cap_batches at the exact clamp edge
#[test]
fn cap_off_by_one_boundary_1001_clamps_to_1000() {
    let dir = tmp_dir("rc-offbyone");
    let p = sales_parquet(&dir);
    let mut s = Session::new("rc-offbyone").unwrap();
    let out = s.run_code(&format!("query({p:?}, 'SELECT * FROM range(1001)')")).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(list_len(&out), DEFAULT_MAX_RESULT_ROWS, "cap+1 rows must clamp to exactly the cap, never let one extra row through"); // expected: 1000 not 1001
}

/// `CANARY` (PROBE→CANARY conversion) — FINDING: result cap is row-count only; a 1-row x 10000-column result crosses fully.
/// Column width is an unbounded boundary-volume channel — invariant #6 bounds rows, not cells.
/// seam: engine_duckdb.rs to_rows_values column loop (no width bound) + convert.rs Rows::into_monty (one Dict per row, one pair per column)
/// Observed: 10000 columns cross. To fix: add a per-result cells/column-count cap at the engine read-out seam.
#[test]
fn known_gap_cap_is_on_rows_not_cells_wide_row_crosses_fully() {
    let dir = tmp_dir("rc-wide");
    let p = sales_parquet(&dir);
    let mut s = Session::new("rc-wide").unwrap();
    let cols = (0..10000).map(|i| format!("{i} AS c{i}")).collect::<Vec<_>>().join(", ");
    let out = s.run_code(&format!("query({p:?}, 'SELECT {cols}')")).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    // FINDING: wide row bypasses the row-count cap; 10000 columns cross whole.
    assert_eq!(list_len(&out), 1, "one row crosses (unbounded column width)");
    let MontyObject::List(items) = &out else { panic!("expected list") };
    let MontyObject::Dict(pairs) = &items[0] else { panic!("expected dict row") };
    let ncols = pairs.clone().into_iter().count();
    assert_eq!(ncols, 10000, "CANARY: 10000 columns cross; column-count cap not enforced (invariant #6 gap)");
}

/// `CANARY` (PROBE→CANARY conversion) — FINDING: result cap bounds row COUNT but not per-cell BYTE size.
/// A single 50MB cell inside 1 capped row defeats invariant #6's size intent (boundary-volume / snapshot-size channel).
/// seam: engine_duckdb.rs cell_value StringArray -> Value::Str(.to_string()) (no length bound); convert.rs Value::Str -> MontyObject::String
/// Observed: 50_000_000-byte cell crosses whole inside a 1-row result. To fix: add a per-cell byte-size cap at the cell_value seam.
#[test]
fn known_gap_cap_does_not_bound_per_cell_byte_size() {
    let dir = tmp_dir("rc-bigstr");
    let p = sales_parquet(&dir);
    let mut s = Session::new("rc-bigstr").unwrap();
    // One row, one column, but the single cell is a ~50MB string built in SQL via repeat().
    let out = s.run_code(&format!("query({p:?}, \"SELECT repeat('x', 50000000) AS big\")")).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let MontyObject::List(items) = &out else { panic!("expected list") };
    let MontyObject::Dict(pairs) = &items[0] else { panic!("expected dict") };
    let mut cell_len = 0usize;
    for (_k, v) in pairs.clone().into_iter() { if let MontyObject::String(big) = v { cell_len = big.len(); } }
    // FINDING: 50MB cell crosses; per-cell byte cap not enforced.
    assert_eq!(cell_len, 50_000_000, "CANARY: a 50MB cell crosses whole; per-cell byte size is unbounded (invariant #6 gap)");
}

/// `CANARY` (PROBE→CANARY conversion) — FINDING: run_code's final return value is uncapped.
/// Agent-built [0]*1_000_000 crosses whole (1_000_000 elements). Invariant #6's cap only guards engine read-outs (to_rows/query), not the program's own return value.
/// seam: session.rs run_code ReplProgress::Complete value path (the cap lives in the ENGINE read-out, not on the run_code return)
/// Observed: 1_000_000-element list returned without hitting the Task-2 limiter (list fits under 256 MiB). Not a limiter FLIP — it's a genuine open gap (different seam from the engine cap).
/// To fix: add a run_code return-value size cap in session.rs at the ReplProgress::Complete arm.
#[test]
fn known_gap_cap_does_not_bound_agent_built_final_return_value() {
    let mut s = Session::new("rc-agent-ret").unwrap();
    // The agent fabricates a giant list in its OWN Monty code (no tool, no engine) and returns it.
    let out = s.run_code("[0]*1000000").unwrap();
    // FINDING: 1_000_000-element list crosses; run_code return-value cap not enforced.
    assert_eq!(list_len(&out), 1_000_000, "CANARY: agent-built list of 1M elements crosses run_code boundary whole; run_code return-value is uncapped (invariant #6 gap — different seam from engine cap)");
}

/// `HOLDS` — Numeric boundary correctness: a scalar read-out that overflows i64 must fail loudly, not silently truncate a value crossing the boundary.
/// seam: engine_duckdb.rs scalar_i64 'CAST({expr} AS BIGINT)' where expr overflows INT64 during evaluation; tools.rs scalar
#[test]
fn scalar_i64_addition_overflow_surfaces_err_not_silent() {
    let dir = tmp_dir("rc-scalovf");
    let p = sales_parquet(&dir);
    let mut s = Session::new("rc-scalovf").unwrap();
    // Addition of two INT64-max literals overflows INT64 inside DuckDB.
    let err = s.run_code(&format!("scalar(register({p:?}), '9223372036854775807 + 9223372036854775807')")).unwrap_err();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(matches!(err, DropletError::Duckdb(_)), "i64 overflow in scalar() must surface a contained DuckDB error, never wrap/truncate silently; got {err:?}"); // expected: Err(Duckdb(... Out of Range ...))
}

/// `HOLDS` — Distinct from the addition-overflow angle: here the overflow is in the i128->i64 narrowing CAST (scalar_i64's load-bearing cast), a different DuckDB code path (Conversion vs Out-of-Range).
/// seam: engine_duckdb.rs scalar_i64 CAST(expr AS BIGINT) where expr is HUGEINT(i128) > i64::MAX (the i128->i64 narrowing cast itself)
#[test]
fn scalar_hugeint_to_i64_cast_overflow_surfaces_err() {
    let dir = tmp_dir("rc-hugecast");
    let p = sales_parquet(&dir);
    let mut s = Session::new("rc-hugecast").unwrap();
    // A HUGEINT value just past i64::MAX; the scalar_i64 BIGINT cast must reject it, not wrap to negative.
    let err = s.run_code(&format!("scalar(register({p:?}), 'CAST(9223372036854775808::HUGEINT AS BIGINT)')")).unwrap_err();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(matches!(err, DropletError::Duckdb(_)), "HUGEINT(i128) exceeding i64 must fail the BIGINT cast in scalar(), not silently wrap; got {err:?}"); // expected: Err(Duckdb(... Conversion Error ... out of range for ... INT64 ...))
}

/// `HOLDS` — Type-confusion at the value-move boundary: an out-of-vocabulary cell type must be a clean Err, not reach the infallible-by-assumption down!() .expect() downcast.
/// seam: engine_duckdb.rs cell_value match -> a DataType (Decimal128) not in the supported arm set -> DropletError::UnsupportedType; convert.rs Rows path; guards the down!() .expect() downcasts
#[test]
fn hugeint_column_via_to_rows_is_unsupported_type_not_panic() {
    let mut s = Session::new("rc-hugecol").unwrap();
    // A HUGEINT (Arrow Decimal128) column read via to_rows: cell_value has no Decimal128 arm.
    let res = s.run_code("to_rows(local_sql('SELECT 170141183460469231731687303715884105727::HUGEINT AS h', []))");
    let err = res.unwrap_err();
    assert!(matches!(err, DropletError::UnsupportedType(_)), "a HUGEINT/Decimal128 result column must surface a contained UnsupportedType at the read-out, never a downcast panic or garbage; got {err:?}"); // expected: Err(UnsupportedType("Decimal128(38, 0)"))
}

/// `HOLDS` — Boundary-correctness sibling of the cap: a degenerate single-value crossing (NULL) must fail loudly so a zero-row analysis can't masquerade as a real 0 — a different seam (query_row NULL decode) than overflow.
/// seam: engine_duckdb.rs scalar_i64 conn.query_row::<i64> decoding a NULL aggregate (empty filter) -> InvalidColumnType folded into DropletError::Duckdb
#[test]
fn empty_aggregate_null_scalar_surfaces_err_not_silent_zero() {
    let dir = tmp_dir("rc-nullscal");
    let p = sales_parquet(&dir);
    let mut s = Session::new("rc-nullscal").unwrap();
    // Filter matches nothing, so SUM(amt) is NULL; scalar() must NOT silently coerce NULL to 0.
    let err = s.run_code(&format!("scalar(filter_rows(register({p:?}), 'amt > 1e9'), 'SUM(amt)')")).unwrap_err();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(matches!(err, DropletError::Duckdb(_)), "a NULL aggregate (empty group) must surface a contained engine error, not silently cross as 0/garbage; got {err:?}"); // expected: Err(Duckdb(InvalidColumnType(.. Null)))
}

/// `HOLDS` — The cap is a PER-SESSION configurable field, not a global const; verifies the default is 1000 AND that lowering it is actually honored on the value-move path.
/// seam: engine_duckdb.rs max_result_rows field + set_max_result_rows/max_result_rows; to_rows clamp honors the per-session knob (the field droplet-py surfaces as Engine(max_result_rows=...))
#[test]
fn lowered_per_session_cap_is_honored_on_readout() {
    use crate::engine_duckdb::DuckEngine;
    let mut eng = DuckEngine::new_in_memory().unwrap();
    assert_eq!(eng.max_result_rows(), DEFAULT_MAX_RESULT_ROWS); // default preserved
    eng.set_max_result_rows(2);
    let dir = tmp_dir("rc-cap-cfg");
    let big = dir.join("b.parquet").to_str().unwrap().to_string();
    write_parquet(&big, "SELECT * FROM range(100)");
    let ds = eng.register_parquet(&big).unwrap();
    let n: usize = eng.to_rows(&ds).unwrap().iter().map(|b| b.num_rows()).sum();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(n, 2, "a lowered per-session cap (2) must clamp every read-out; the default (1000) must be the starting value"); // expected: 2 rows out of 100
}

/// `PROBE` — Pre-allocation / huge-int bomb reached purely through the run_code value path (no engine). Verifies Droplet contains a Monty bignum blowup as Ok-or-Err, never a panic across the boundary.
/// seam: monty BigInt construction reached through session.rs run_code (Droplet's surface) -> ReplProgress::Complete value; pre-allocation safety of a single huge object
#[test]
fn huge_int_literal_construction_does_not_panic_session() {
    let mut s = Session::new("rc-bigint").unwrap();
    // A huge int built in pure Monty; Droplet must contain whatever Monty does (result or Err), never panic.
    let res = s.run_code("2**100000");
    // CONTRACT (Droplet's surface, not Monty internals): reached through run_code it must NOT panic/UAF; it must surface a contained DropletError OR a correct MontyObject. Ok or Err both acceptable; a crash is not.
    match res {
      Ok(v) => assert!(matches!(v, MontyObject::BigInt(_) | MontyObject::Int(_)), "huge int must come back as a (Big)Int, got {v:?}"),
      Err(e) => assert!(matches!(e, DropletError::Monty(_) | DropletError::Duckdb(_) | DropletError::BadArg(_)), "huge int must fail as a contained DropletError, got {e:?}"),
    }
}
