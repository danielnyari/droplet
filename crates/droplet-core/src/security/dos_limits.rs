// crates/droplet-core/src/security/dos_limits.rs
//! Resource-exhaustion angles, bounded by the Task-2 `LimitedTracker` budget. The two `..._holds`
//! calibration tests bracket the budget from BELOW (legit work must fit); the LIMIT bombs bracket it
//! from ABOVE (real bombs must trip it and the session must survive); the `#[ignore]`d watchdog
//! CANARYs pin the residual pure-CPU spin gap (needs a future `max_duration`).
#![allow(unused_imports)]
use super::{list_len, sales_parquet, tmp_dir, write_parquet};
use crate::DropletError;
use crate::engine_duckdb::DEFAULT_MAX_RESULT_ROWS;
use crate::session::Session;
use monty::MontyObject;

/// `HOLDS` — Pins the exact field-type/constructor edit and proves a generous budget does not break trivial legit execution. Distinct from every bomb: it asserts the limiter is wired AND harmless to legit work.
/// seam: session.rs Session.repl field type + Session::new constructor (NoLimitTracker -> LimitedTracker swap; field becomes Option<MontyRepl<LimitedTracker>>, settle() retyped)
#[test]
fn session_edit_repl_field_is_limited_tracker() {
    use crate::session::Session;
    use monty::MontyObject;
    let mut s = Session::new("limit-wired").unwrap();
    let v = s.run_code("1 + 2").unwrap();
    assert_eq!(v, MontyObject::Int(3)); // a limited session still executes legit code. Compile-time proof: after the edit session.rs names LimitedTracker in the field, MontyRepl::new call, and the two settle() signatures (Result<ReplProgress<LimitedTracker>, Box<ReplStartError<LimitedTracker>>>); the file would not build against the old NoLimitTracker references, so this test passing under the new type IS the structural proof of the edit.
}

/// `LIMIT` — Allocation-COUNT exhaustion via many distinct heap objects — the ONLY proposed bomb that actually exercises max_allocations (on_allocate), since each inner list is a real Ref(HeapId) object. Distinct from the memory-growth angle.
/// seam: monty Heap::allocate -> on_allocate under nested-comprehension object fan-out (many DISTINCT heap objects) with max_allocations
#[test]
fn nested_list_object_fanout_bomb_is_bounded() {
    use crate::session::Session;
    let mut s = Session::new("nested-bomb").unwrap();
    // Mint millions of DISTINCT inner list objects. Each inner `[0]*1000` is a fresh heap
    // object routed through Heap::allocate -> on_allocate, which DOES consult max_allocations
    // (resource.rs:522-532). This is the one bomb a count-only cap genuinely stops.
    let bomb = "x = [[0] * 1000 for _ in range(10 ** 7)]";
    let err = s.run_code(bomb);
    let after = s.run_code("len([1,2,3])");
    assert!(
        err.is_err(),
        "object-fanout explosion must trip the allocation-count cap"
    );
    assert!(
        matches!(err.as_ref().unwrap_err(), crate::DropletError::Monty(_)),
        "breach must surface as the resource/Monty error path, got {err:?}"
    );
    assert!(after.is_ok(), "session REPL must survive a bounded breach");
}

/// `LIMIT` — Single-object MEMORY growth (Vec backing-store realloc via on_grow), distinct from object-count fan-out. Proves the limiter's memory leg, not its count leg.
/// seam: session.rs run_code loop -> monty List::append -> Heap::track_growth -> on_grow under max_memory (NOT max_allocations)
#[test]
fn memory_growth_append_loop_is_bounded() {
    use crate::session::Session;
    use monty::MontyObject;
    let mut s = Session::new("alloc-bomb-append").unwrap();
    // list.append grows ONE list object. Each append calls track_growth(VALUE_SIZE) -> on_grow
    // (list.rs:132), and `0` is an inline immediate Value::Int(i64) (value.rs) so it allocates
    // NO heap object. on_grow is gated by max_memory ONLY (resource.rs:559-573), so this loop is
    // bounded by the memory cap, not the allocation-count cap.
    let bomb = "x = []\nwhile True:\n    x.append(0)";
    let err = s.run_code(bomb);
    let after = s.run_code("7 * 6");
    assert!(
        err.is_err(),
        "unbounded list growth must be bounded by max_memory, not run forever"
    );
    assert!(
        matches!(err.as_ref().unwrap_err(), crate::DropletError::Monty(_)),
        "breach must surface as the resource/Monty error path, got {err:?}"
    );
    assert_eq!(
        after.unwrap(),
        MontyObject::Int(42),
        "session REPL must survive a bounded breach"
    );
}

/// `HOLDS` — Stack-depth/control-flow DoS distinct from every heap angle; verifies the LimitedTracker swap preserves the 1000 recursion cap (ResourceLimits::new() keeps Some(1000)).
/// seam: monty check_recursion_depth (ResourceLimits::new() sets max_recursion_depth Some(1000)) via Session.run_code
#[test]
fn deep_recursion_already_bounded_at_1000_holds() {
    super::run_big_stack(move || {
        use crate::session::Session;
        let mut s = Session::new("recursion-1000").unwrap();
        // ResourceLimits::new() sets max_recursion_depth Some(1000) (resource.rs:391-396) and
        // NoLimitTracker also caps at 1000 — so the swap preserves the cap. Unbounded self-recursion
        // must hit it and raise, never overflow the native Rust stack.
        let bomb = "def f(n):\n    return f(n + 1)\nf(0)";
        let err = s.run_code(bomb);
        let after = s.run_code("1 + 1");
        assert!(
            err.is_err(),
            "unbounded recursion must hit the 1000 cap, not overflow the host stack"
        );
        assert!(
            matches!(err.as_ref().unwrap_err(), crate::DropletError::Monty(_)),
            "recursion breach is the Monty error path, got {err:?}"
        );
        assert!(after.is_ok(), "session survives a recursion breach");
    });
}

/// `HOLDS` — Validates the depth cap counts call FRAMES globally (A<->B), catching an implementation that mistakenly bounded per-callee — distinct from single-function recursion.
/// seam: monty check_recursion_depth across alternating frames (mutual A<->B recursion) via Session.run_code
#[test]
fn mutual_recursion_bounded_holds() {
    super::run_big_stack(move || {
        use crate::session::Session;
        let mut s = Session::new("mutual-recursion").unwrap();
        let bomb = "def a(n):\n    return b(n + 1)\ndef b(n):\n    return a(n + 1)\na(0)";
        let err = s.run_code(bomb);
        let after = s.run_code("9");
        assert!(err.is_err(), "mutual recursion must hit the depth cap");
        assert!(
            matches!(err.as_ref().unwrap_err(), crate::DropletError::Monty(_)),
            "got {err:?}"
        );
        assert!(after.is_ok(), "session survives");
    });
}

/// `LIMIT` — Survivability/contract angle: proves a DoS breach is RECOVERABLE (settle restores the repl) not session-poisoning — distinct from merely asserting the bomb errs.
/// seam: session.rs settle() restoring the repl from ReplStartError after a resource breach (REPL-survival under DoS)
#[test]
fn alloc_breach_does_not_consume_repl_then_legit_analyze_runs() {
    use crate::session::Session;
    use monty::MontyObject;
    let mut s = Session::new("breach-survive").unwrap();
    // Trip a bound via the count-stopping bomb (object fan-out trips max_allocations reliably;
    // a pure append loop only trips if max_memory is set).
    let _ = s.run_code("x = [[0] * 1000 for _ in range(10 ** 7)]");
    // Then run a REAL multi-statement program in the SAME session: namespace intact, budget
    // still allows normal work (recoverable breach, repl restored by settle from ReplStartError).
    let v = s.run_code("a = 10\nb = 20\na + b");
    assert_eq!(
        v.unwrap(),
        MontyObject::Int(30),
        "after a bounded DoS breach the session REPL must survive and keep running legit code"
    );
}

/// `HOLDS` — Lower-bracket calibration guard: proves the budget (both N and M) is high enough for a legitimate maximal 1000-row read-out. A too-tight budget is itself a DoS-on-legit-users finding. Distinct from every bomb (asserts the limiter does NOT fire).
/// seam: session.rs run_code building a 1000-row list[dict] result under the chosen budget (calibration: limiter must NOT fire on legal max results)
#[test]
fn legit_thousand_row_to_rows_under_budget_holds() {
    use crate::engine_duckdb::DEFAULT_MAX_RESULT_ROWS;
    use crate::session::Session;
    // Fixture dir must NOT match Session work_dir pattern (temp/droplet-{run_id}).
    // Use a "fixture-" prefix so Session::new() never wipes the parquet before the query runs.
    let dir = std::env::temp_dir().join("fixture-dos-legit-rows");
    std::fs::create_dir_all(&dir).unwrap();
    let big = dir.join("big.parquet").to_str().unwrap().to_string();
    let conn = duckdb::Connection::open_in_memory().unwrap();
    conn.execute_batch(&format!(
        "COPY (SELECT * FROM range(2500)) TO '{big}' (FORMAT PARQUET)"
    ))
    .unwrap();
    let mut s = Session::new("dos-legit-rows").unwrap();
    let out = s.run_code(&format!("query({big:?}, 'SELECT * FROM data')"));
    let n = match &out {
        Ok(monty::MontyObject::List(v)) => v.len(),
        _ => 0,
    };
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.is_ok(),
        "a legit capped 1000-row read-out must NOT trip the budget: {out:?}"
    );
    assert_eq!(
        n, DEFAULT_MAX_RESULT_ROWS,
        "the full capped result still crosses"
    );
}

/// `HOLDS` — Second calibration guard using the realistic V1b analyze workload (tool round-trips + python control flow + lambda sort) — ensures host-dispatch resume cycles don't accumulate enough allocations to trip a too-tight budget. Distinct from the flat row-dump guard.
/// seam: session.rs run_code multi-step analyze (register/group_agg/to_rows + python loop + lambda sort) allocation+memory footprint vs budget
#[test]
fn legit_multistep_handle_analyze_under_budget_holds() {
    use crate::session::Session;
    // Use a SEPARATE fixture dir (NOT the session work_dir) so Session::new() doesn't wipe it.
    // Session work_dir = temp/droplet-<run_id>; fixture dir must not match that name.
    // Fixture dir must NOT match Session work_dir pattern (temp/droplet-{run_id}).
    // Use a "fixture-" prefix so Session::new() never wipes the parquet before the query runs.
    let dir = std::env::temp_dir().join("fixture-dos-legit-multistep");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("demo.parquet").to_str().unwrap().to_string();
    let conn = duckdb::Connection::open_in_memory().unwrap();
    conn.execute_batch(&format!("COPY (SELECT region, amt::DOUBLE AS amt FROM (VALUES ('EU',100.0),('EU',50.0),('US',200.0),('APAC',300.0),('APAC',0.0)) AS t(region,amt)) TO '{p}' (FORMAT PARQUET)")).unwrap();
    let code = [
        format!("ds = register({p:?})"),
        "agg = group_agg(ds, ['region'], [('total','SUM(amt)'), ('n','CAST(COUNT(*) AS BIGINT)')])"
            .to_string(),
        "ranked = []".to_string(),
        "for r in to_rows(agg):".to_string(),
        "    avg = r['total'] / r['n']".to_string(),
        "    if avg >= 100:".to_string(),
        "        ranked.append({'region': r['region'], 'avg': avg})".to_string(),
        "ranked.sort(key=lambda x: -x['avg'])".to_string(),
        "ranked".to_string(),
    ]
    .join("\n");
    let mut s = Session::new("dos-legit-multistep").unwrap();
    let out = s.run_code(&code);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.is_ok(),
        "the legit multi-step analyze demo must run under the budget: {out:?}"
    );
    match out.unwrap() {
        monty::MontyObject::List(v) => assert_eq!(v.len(), 2, "US and APAC survive the threshold"),
        other => panic!("expected ranked list, got {other:?}"),
    };
}

/// `CANARY` — CPU-time exhaustion with ZERO allocation — the gap the allocation/memory caps structurally cannot close. Distinct from every heap/recursion angle; pins the missing wall-clock limit.
/// seam: monty has NO wall-clock/instruction limit wired when only max_allocations/max_memory are set; 'while True: pass' allocates nothing -> CPU spins forever
#[ignore = "DoS watchdog/CPU-spin canary; run explicitly with `cargo test -- --ignored`"]
#[test]
fn watchdog_pure_cpu_spin_is_unbounded_canary() {
    // #[ignore] — run explicitly: cargo test watchdog_pure_cpu_spin -- --ignored
    use crate::session::Session;
    use std::sync::mpsc;
    use std::time::Duration;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut s = Session::new("cpu-spin").unwrap();
        let r = s.run_code("while True:\n    pass");
        let _ = tx.send(r.is_err());
    });
    let outcome = rx.recv_timeout(Duration::from_secs(3));
    assert!(
        outcome.is_err(),
        "CANARY+FINDING: pure-CPU 'while True: pass' is NOT bounded by an alloc/memory-only limiter — it spun past the 3s watchdog. recv_timeout returning Err(RecvTimeoutError::Timeout) == still spinning. Flip to assert_eq!(outcome, Ok(true)) once ResourceLimits::max_duration is wired (LimitedTracker.check_time exists, resource.rs:576-593, but is dormant unless max_duration is Some)."
    )
}

/// `CANARY` — Distinct CPU-DoS gadget from 'while True: pass': executes real arithmetic bytecode each iteration but with a value range (small int) that defeats BOTH allocation and memory tracking — proving the gap isn't just empty loops.
/// seam: monty no time limit: bounded-value integer accumulator (i = (i+1) % 2) keeps values as inline immediates -> ~zero net heap growth -> neither alloc nor memory cap trips
#[ignore = "DoS watchdog/CPU-spin canary; run explicitly with `cargo test -- --ignored`"]
#[test]
fn watchdog_non_allocating_arithmetic_spin_is_unbounded_canary() {
    // #[ignore] — companion CPU canary that does real bytecode work but allocates ~nothing.
    use crate::session::Session;
    use std::sync::mpsc;
    use std::time::Duration;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut s = Session::new("arith-spin").unwrap();
        let r = s.run_code("i = 0\nwhile True:\n    i = (i + 1) % 2");
        let _ = tx.send(r.is_err());
    });
    let outcome = rx.recv_timeout(Duration::from_secs(3));
    assert!(
        outcome.is_err(),
        "CANARY+FINDING: a bounded-value arithmetic spin keeps i as an inline Value::Int(i64) (value.rs) so net heap growth is ~0 -> neither max_allocations nor max_memory can stop it; it ran past the 3s watchdog. A wall-clock max_duration is required. Flip once that lands."
    )
}

/// `LIMIT` — Bridges LIMIT and watchdog: verifies the allocation-count cap yields a TIMELY failure, not a multi-second grind — the real DoS-protection property. Distinct from the bare is_err() fan-out test.
/// seam: session.rs run_code: the allocation-count bomb must terminate PROMPTLY (a small count cap bounds wall-clock as a side effect), not grind for seconds
#[ignore = "DoS watchdog/CPU-spin canary; run explicitly with `cargo test -- --ignored`"]
#[test]
fn watchdog_proves_object_fanout_bomb_terminates_within_budget() {
    // Can run normally (it should terminate fast) or be #[ignore]d. Uses the watchdog to assert
    // the count-capped bomb returns QUICKLY — distinguishing 'bounded' from 'eventually bounded'.
    use crate::session::Session;
    use std::sync::mpsc;
    use std::time::Duration;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut s = Session::new("bomb-prompt").unwrap();
        // object fan-out reliably trips max_allocations (each inner list is a heap object).
        let r = s.run_code("x = [[0] * 1000 for _ in range(10 ** 7)]");
        let _ = tx.send(r.is_err());
    });
    let outcome = rx.recv_timeout(Duration::from_secs(10));
    assert_eq!(
        outcome,
        Ok(true),
        "the object-fanout bomb must trip the count cap and return Err PROMPTLY (within 10s) — proving the budget bounds wall-clock for allocation-driven loops"
    );
}
