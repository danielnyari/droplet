// crates/droplet-core/src/security/error_safety.rs
//! Error-safety / REPL poisoning / panic-safety — adversarial angles. seam: `session.rs` settle()/REPL survival, no host panic across the dispatch loop.
#![allow(unused_imports)]
use super::{catch_dispatch, dispatch, list_len, sales_parquet, tmp_dir, write_parquet};
use crate::DropletError;
use crate::engine_duckdb::{DEFAULT_MAX_RESULT_ROWS, Dataset, DuckEngine};
use crate::registry::Registry;
use crate::session::Session;
use crate::tool::{Tool, ToolCx};
use monty::MontyObject;

/// `HOLDS` — Baseline recoverable run-time NameError path through settle's ReplStartError restore.
/// seam: session.rs settle(): feed_start raises NameError -> ReplStartError carries REPL back, repl restored
#[test]
fn recoverable_undefined_name_errs_and_repl_survives() {
    let mut s = Session::new("err-undef").unwrap();
    let e = s.run_code("undefined_name_here + 1");
    let after = s.run_code("1 + 2");
    assert!(
        matches!(e, Err(DropletError::Monty(_))),
        "undefined name must fold to Monty err, got {e:?}"
    );
    assert_eq!(
        after.unwrap(),
        MontyObject::Int(3),
        "REPL must survive a recoverable error"
    );
}

/// `HOLDS` — Parse-time failure distinct from run-time NameError; both must restore REPL but enter settle via a different monty path.
/// seam: session.rs settle() on feed_start PARSE failure (SyntaxError) vs run-time
#[test]
fn syntax_error_errs_and_repl_survives() {
    let mut s = Session::new("err-syntax").unwrap();
    let e = s.run_code("def (:");
    let after = s.run_code("6 + 6");
    assert!(
        matches!(e, Err(DropletError::Monty(_))),
        "syntax error must fold to Monty err, got {e:?}"
    );
    assert_eq!(
        after.unwrap(),
        MontyObject::Int(12),
        "a parse error must not poison the session"
    );
}

/// `HOLDS` — Explicit `raise` statement gadget — distinct from implicit name/syntax/runtime errors.
/// seam: session.rs run_code: explicit `raise` propagates as MontyException through settle
#[test]
fn agent_raise_custom_exception_errs_and_survives() {
    let mut s = Session::new("err-raise").unwrap();
    let e = s.run_code("raise ValueError('boom')");
    let after = s.run_code("4 + 4");
    assert!(
        matches!(e, Err(DropletError::Monty(_))),
        "agent-raised exception must fold to Monty err, got {e:?}"
    );
    assert_eq!(
        after.unwrap(),
        MontyObject::Int(8),
        "an agent-raised exception must not poison the session"
    );
}

/// `HOLDS` — Inverse of a raised error: sandbox self-recovers, run_code stays Ok — proves host non-involvement on self-handled exceptions. Pairs with agent_cannot_catch_host_tool_error to bound the try/except reach.
/// seam: monty sandbox handles its own exception fully inside feed_start; host run_code never sees Err
#[test]
fn agent_try_except_self_handled_completes_ok() {
    let mut s = Session::new("err-tryexcept").unwrap();
    let v = s.run_code("x = 0\ntry:\n    raise ValueError('x')\nexcept ValueError:\n    x = 42\nx");
    assert_eq!(
        v.unwrap(),
        MontyObject::Int(42),
        "agent-handled exception must complete to Ok with the host uninvolved"
    );
}

/// `HOLDS` — STATE continuity, not just survival — asserts the restored REPL preserves the prior namespace. Distinct from the bare survive tests.
/// seam: session.rs persistent MontyRepl: bindings survive a settle-restored REPL (same repl object handed back)
#[test]
fn namespace_persists_across_recoverable_error() {
    let mut s = Session::new("err-persist").unwrap();
    s.run_code("g = 7").unwrap();
    let _ = s.run_code("undefined_name_here");
    let after = s.run_code("g + 1");
    assert_eq!(
        after.unwrap(),
        MontyObject::Int(8),
        "a binding from before a recoverable error must still resolve afterward"
    );
}

/// `HOLDS` — Exercises the ExtFunctionResult::NotFound branch (inventory miss) which monty turns into a recoverable NameError + resumes — distinct from a tool that errors DURING dispatch (those consume the REPL).
/// seam: session.rs FunctionCall arm: inventory miss -> ExtFunctionResult::NotFound -> sandbox NameError -> settle restores REPL
#[test]
fn unknown_tool_name_surfaces_nameerror_and_survives() {
    let mut s = Session::new("err-unktool").unwrap();
    let e = s.run_code("no_such_tool(1)");
    let after = s.run_code("5 + 5");
    assert!(
        matches!(e, Err(DropletError::Monty(_))),
        "calling an unregistered tool must fold to a Monty err (NameError), got {e:?}"
    );
    assert_eq!(
        after.unwrap(),
        MontyObject::Int(10),
        "an unknown-tool call must not poison the session"
    );
}

/// `PROBE` — The dispatch-time hard-error path: error escapes via ? AFTER repl.take(), so REPL is consumed and the next call is a defined clean NotFound. Asserts the required contract (no panic). Distinct from settle-restored recoverable errors.
/// seam: session.rs: (tool.dispatch)(...)? propagates Duckdb err AFTER repl.take() -> repl stays None
#[test]
fn hard_sql_error_consumes_repl_then_clean_err_not_panic() {
    let dir = tmp_dir("err-hardsql");
    let p = sales_parquet(&dir);
    let mut s = Session::new("err-hardsql").unwrap();
    let e1 = s.run_code(&format!("query({p:?}, 'SELECT nonesuch FROM data')"));
    let e2 = s.run_code("1 + 1");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        matches!(e1, Err(DropletError::Duckdb(_))),
        "bad SQL must fold to a Duckdb err, got {e1:?}"
    );
    assert!(
        matches!(e2, Err(DropletError::NotFound(_))),
        "a consumed REPL must yield a CLEAN NotFound, not a panic or a second engine error, got {e2:?}"
    );
}

/// `PROBE` — Idempotence/stability of the poisoned state across repeated calls — distinct from the single-shot consume test; proves degradation is predictable, not diverging.
/// seam: session.rs: repl.take().ok_or_else(NotFound) idempotent once repl=None
#[test]
fn consumed_repl_stays_clean_err_on_repeated_calls() {
    let dir = tmp_dir("err-consumed");
    let p = sales_parquet(&dir);
    let mut s = Session::new("err-consumed").unwrap();
    let _ = s.run_code(&format!("query({p:?}, 'SELECT nonesuch FROM data')"));
    let e2 = s.run_code("1");
    let e3 = s.run_code("2");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(matches!(e2, Err(DropletError::NotFound(_))), "got {e2:?}");
    assert!(
        matches!(e3, Err(DropletError::NotFound(_))),
        "every call against a consumed REPL must stay a clean NotFound, got {e3:?}"
    );
}

/// `PROBE` — Conversion-layer (FromArg) error path rather than engine — a type mismatch mid-thunk also propagates via ? and consumes the REPL. Distinct seam from Duckdb/BadHandle.
/// seam: macros thunk FromArg::from_arg on args[i] -> BadArg propagates via ? from dispatch (conversion seam, not engine)
#[test]
fn bad_arg_type_consumes_repl_cleanly() {
    let mut s = Session::new("err-badarg").unwrap();
    let e1 = s.run_code("query(123, 'SELECT 1')");
    let e2 = s.run_code("2 + 2");
    assert!(
        matches!(e1, Err(DropletError::BadArg(_))),
        "wrong arg type must fold to BadArg, got {e1:?}"
    );
    assert!(
        matches!(e2, Err(DropletError::NotFound(_))),
        "a BadArg from the thunk consumes the REPL -> next call is a clean NotFound, got {e2:?}"
    );
}

/// `PROBE` — Handle-forgery path (registry.require miss) for an in-range u64 — distinct from BadArg (type) and the negative-handle (try_from) edge.
/// seam: convert.rs Dataset::from_arg -> cx.handles.require(handle) -> BadHandle propagates from dispatch
#[test]
fn bad_handle_forgery_consumes_repl_cleanly() {
    let mut s = Session::new("err-badhandle").unwrap();
    let e1 = s.run_code("to_rows(999)");
    let e2 = s.run_code("3 + 3");
    assert!(
        matches!(e1, Err(DropletError::BadHandle(999))),
        "a forged in-range handle must be BadHandle(999), got {e1:?}"
    );
    assert!(
        matches!(e2, Err(DropletError::NotFound(_))),
        "a BadHandle consumes the REPL -> clean NotFound next, got {e2:?}"
    );
}

/// `PROBE` — Targets the i64->u64 try_from guard specifically — distinct gadget from the in-range-but-missing BadHandle path; an unguarded conversion here would panic on overflow.
/// seam: convert.rs Dataset::from_arg: u64::try_from(i64) guard on a negative int (the arithmetic-edge that could underflow)
#[test]
fn negative_handle_is_bad_arg_not_panic() {
    let mut s = Session::new("err-neg").unwrap();
    let e1 = s.run_code("to_rows(-1)");
    let e2 = s.run_code("9 + 9");
    assert!(
        matches!(e1, Err(DropletError::BadArg(_))),
        "a negative handle must be a clean BadArg ('dataset handle must be non-negative'), not an arithmetic panic, got {e1:?}"
    );
    assert!(
        matches!(e2, Err(DropletError::NotFound(_))),
        "the negative-handle BadArg consumes the REPL -> clean NotFound, got {e2:?}"
    );
}

/// `PROBE` — Asymmetry contract: the sandbox can catch its OWN python exceptions but CANNOT catch a host-dispatch error because ? returns before call.resume re-injects anything. A future change that let the sandbox swallow tool errors would flip this. Security-relevant (an agent must not mask host failures).
/// seam: session.rs: dispatch ? short-circuits BEFORE call.resume, so a host error never re-enters the sandbox for try/except to catch
#[test]
fn agent_cannot_catch_host_tool_error() {
    let mut s = Session::new("err-toolcatch").unwrap();
    let v = s.run_code("ok=0\ntry:\n    to_rows(999)\nexcept Exception:\n    ok=1\nok");
    let after = s.run_code("12 + 12");
    assert!(
        matches!(v, Err(DropletError::BadHandle(999))),
        "a hard host-tool error must escape past the agent's try/except as a host DropletError, not be swallowed in-sandbox, got {v:?}"
    );
    assert!(
        matches!(after, Err(DropletError::NotFound(_))),
        "and it still consumes the REPL, got {after:?}"
    );
}

/// `PROBE` — Error mid-suspension with another tool call on the stack (inner-as-argument) — checks the suspend/resume loop unwinds without panicking the pending outer call. Distinct control-flow shape from single-call errors.
/// seam: session.rs suspend/resume across NESTED FunctionCalls; inner dispatch err propagates via ? while outer call is pending
#[test]
fn nested_failing_tool_call_in_args_is_clean() {
    let mut s = Session::new("err-nested").unwrap();
    let e1 = s.run_code("to_rows(to_rows(999))");
    let e2 = s.run_code("11 + 11");
    assert!(
        matches!(e1, Err(DropletError::BadHandle(999))),
        "the inner failing tool call must surface BadHandle, not a panic from a half-applied outer call, got {e1:?}"
    );
    assert!(
        matches!(e2, Err(DropletError::NotFound(_))),
        "REPL consumed cleanly, got {e2:?}"
    );
}

/// `HOLDS` — Two distinct builtin-raise gadgets (assert, integer //0) in sequence — proves multiple recoverable errors each restore the REPL with no cumulative poisoning. Different failure family from name/syntax.
/// seam: session.rs settle: AssertionError / ZeroDivisionError raised in feed_start are REPL-restoring; two in a row = no cumulative poisoning
#[test]
fn assertion_and_zerodiv_are_recoverable_and_survive() {
    let mut s = Session::new("err-assert").unwrap();
    let e1 = s.run_code("assert False, 'nope'");
    let e2 = s.run_code("1 // 0");
    let after = s.run_code("8 + 8");
    assert!(
        matches!(e1, Err(DropletError::Monty(_))),
        "assert must fold to Monty err, got {e1:?}"
    );
    assert!(
        matches!(e2, Err(DropletError::Monty(_))),
        "zero-division must fold to Monty err, got {e2:?}"
    );
    assert_eq!(
        after.unwrap(),
        MontyObject::Int(16),
        "two consecutive recoverable runtime errors must not poison the session"
    );
}

/// `HOLDS` — Degenerate-input robustness: empty / comment-only code is a parser edge that must map to None and not trip the Complete arm. Distinct from all error-raising angles.
/// seam: session.rs Complete arm with no last expression -> MontyObject::None (degenerate parser inputs)
#[test]
fn empty_and_comment_only_code_is_ok_none_and_survives() {
    let mut s = Session::new("err-empty").unwrap();
    let e1 = s.run_code("");
    let e2 = s.run_code("# just a comment");
    let after = s.run_code("10 + 10");
    assert_eq!(
        e1.unwrap(),
        MontyObject::None,
        "empty program must complete to None, not error or panic"
    );
    assert_eq!(
        e2.unwrap(),
        MontyObject::None,
        "comment-only program must complete to None"
    );
    assert_eq!(
        after.unwrap(),
        MontyObject::Int(20),
        "degenerate inputs must leave the session usable"
    );
}

/// `PROBE` — catch_unwind umbrella asserting ABSENCE of unwinding across the whole recoverable-error family — distinct from any single error's value assertion. Fresh Session per input so a consumed REPL never confounds the next.
/// seam: session.rs run_code overall panic-safety across heterogeneous error inputs (catch_unwind umbrella)
#[test]
fn run_code_never_panics_under_a_burst_of_mixed_errors() {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    let inputs = [
        "undefined_x",
        "raise RuntimeError('x')",
        "1//0",
        "def (:",
        "no_such_tool(1)",
        "assert False",
        "",
        "# c",
    ];
    let res = catch_unwind(AssertUnwindSafe(|| {
        for (i, code) in inputs.iter().enumerate() {
            let mut s = Session::new(&format!("err-burst-{i}")).unwrap();
            let _ = s.run_code(code);
        }
    }));
    assert!(
        res.is_ok(),
        "run_code must never panic/process-abort across a burst of heterogeneous error inputs"
    );
}

/// `HOLDS` (was CANARY → FINDING; arity gap FIXED 2026-06-26)
// FIX: the #[droplet_tool] macro thunk now validates arity BEFORE any `&args[i]`. A zero-arg call
// to `to_rows` (expects 1 arg) is a contained BadArg, not an out-of-bounds host panic. The BadArg
// propagates through the dispatch path exactly like a type mismatch, consuming the REPL (the next
// call is then a clean NotFound). A run_code call that previously panicked would fail this test by
// unwinding it, so a direct call (no catch_unwind) is sufficient to prove the panic is gone.
/// seam: macros/src/lib.rs thunk arity guard -> BadArg through session.rs dispatch (no host panic)
#[test]
fn under_arity_through_run_code_is_contained_bad_arg() {
    let mut s = Session::new("err-underarity").unwrap();
    // to_rows expects (ds); call it with ZERO args -> arity guard returns BadArg, no OOB panic.
    let e1 = s.run_code("to_rows()");
    let e2 = s.run_code("1 + 1");
    assert!(
        matches!(e1, Err(DropletError::BadArg(_))),
        "under-arity tool call must be a contained BadArg, not a host panic, got {e1:?}"
    );
    assert!(
        matches!(e2, Err(DropletError::NotFound(_))),
        "the arity BadArg consumes the REPL -> next call is a clean NotFound, got {e2:?}"
    );
}

/// `PROBE` — The mirror of under-arity: extra args. The thunk indexes only [0..declared) so extras are silently dropped at the indexing layer (monty may TypeError first). Distinct gadget — tests the OPPOSITE arity edge for panic-safety / argument-smuggling.
/// seam: macros thunk: extra positional args beyond declared params are simply not indexed; verify no panic and REPL stays usable
#[test]
fn over_arity_extra_args_ignored_or_clean_err_no_panic() {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    let res = catch_unwind(AssertUnwindSafe(|| {
        let mut s = Session::new("err-overarity").unwrap();
        // register expects (path); pass an extra trailing arg.
        let e1 = s.run_code("register('x.parquet', 'extra', 999)");
        let e2 = s.run_code("2 + 2");
        (format!("{e1:?}"), format!("{e2:?}"))
    }));
    let (e1, _e2) = res.expect("extra positional args to a tool must not panic the dispatch thunk");
    assert!(
        e1.contains("Err("),
        "over-arity call should surface a contained Err (Duckdb missing-file or Monty TypeError), not Ok with silent arg drop ideally — but at minimum must not panic; got {e1}"
    );
}

/// `HOLDS` (was CANARY → ⭐ HIGH-SEVERITY FINDING; arity gap FIXED 2026-06-26)
// ⭐ ARITY-THROUGH-RUN_CODE: was a host panic, now a contained BadArg.
// FIX: the #[droplet_tool] macro thunk validates arity before any `&args[i]`. `query('/tmp/x.parquet')`
// (1 arg to a 2-arg tool) now reaches the thunk, fails the arity check, and returns BadArg, which
// propagates through the session.rs FunctionCall arm as an ordinary DropletError — no out-of-bounds
// panic crosses run_code (and thus none reaches the PyO3 boundary). The BadArg consumes the REPL, so
// the next call is a clean NotFound. Had run_code still panicked, it would unwind this test directly.
/// seam: macros/src/lib.rs thunk arity guard -> BadArg through session.rs FunctionCall arm (no host panic)
#[test]
fn wrong_arity_through_run_code_is_contained_bad_arg() {
    let mut s = crate::session::Session::new("err-arity-runcode").unwrap();
    // Agent code calls the 2-arg `query` tool with ONE positional arg, THROUGH the real suspend/resume
    // FunctionCall arm. The arity guard returns BadArg before `&args[1]` is ever indexed.
    let e1 = s.run_code("query('/tmp/x.parquet')");
    let e2 = s.run_code("1 + 1");
    assert!(
        matches!(e1, Err(DropletError::BadArg(_))),
        "wrong-arity tool call through run_code must be a contained BadArg, not a host panic, got {e1:?}"
    );
    assert!(
        matches!(e2, Err(DropletError::NotFound(_))),
        "the arity BadArg consumes the REPL -> next call is a clean NotFound, got {e2:?}"
    );
}
