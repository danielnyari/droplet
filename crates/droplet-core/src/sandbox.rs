//! The Monty sandbox seam — the suspend/resume boundary.
//!
//! Sandboxed Python runs in a persistent `MontyRepl`. When it calls an external
//! (host) function, execution *pauses* and hands the host a `ReplProgress`
//! state machine; the host computes a result and `resume`s. The sandbox only
//! ever sees a function name + `MontyObject` args; real engine state stays
//! host-side (invariant #6). M3 promotes this loop into the real `run_code`
//! driver; M0 just proves the seam, so the driver + smoke tests live under
//! `#[cfg(test)]`.
//!
//! verified against monty `v0.0.18` source (crates/monty/src/{repl,run_progress,
//! object,io,resource,exception_public}.rs).

#[cfg(test)]
mod tests {
    use monty::{
        ExtFunctionResult, MontyObject, MontyRepl, NameLookupResult, NoLimitTracker, PrintWriter,
        ReplProgress, ReplStartError,
    };

    use crate::DropletError;

    /// M0 uses the unbounded resource tracker; M3/later wire real limits.
    type Repl = MontyRepl<NoLimitTracker>;

    fn new_repl() -> Repl {
        MontyRepl::new("session.py", NoLimitTracker)
    }

    /// feed_start/resume return `Box<ReplStartError<T>>` (which carries the
    /// surviving REPL + the `MontyException`). Fold the exception into the one
    /// boundary error type (invariant #10).
    fn start_err(e: Box<ReplStartError<NoLimitTracker>>) -> DropletError {
        let ReplStartError { error, .. } = *e;
        DropletError::Monty(error)
    }

    /// Run one snippet that may call host functions, mutating shared `counter`,
    /// and hand the REPL back so the session can keep feeding snippets.
    fn drive(
        repl: Repl,
        code: &str,
        counter: &mut i64,
    ) -> Result<(MontyObject, Repl), DropletError> {
        let mut progress = repl
            .feed_start(code, vec![], PrintWriter::Disabled)
            .map_err(start_err)?;
        loop {
            match progress {
                ReplProgress::Complete { repl, value } => return Ok((value, repl)),
                ReplProgress::FunctionCall(call) => {
                    // The sandbox sees only the name + args; host state stays here.
                    let reply: ExtFunctionResult = match call.function_name.as_str() {
                        "host_get" => MontyObject::Int(123).into(),
                        "host_add" => {
                            if let Some(MontyObject::Int(n)) = call.args.first() {
                                *counter += *n;
                            }
                            MontyObject::Int(*counter).into()
                        }
                        other => ExtFunctionResult::NotFound(other.to_string()),
                    };
                    progress = call
                        .resume(reply, PrintWriter::Disabled)
                        .map_err(start_err)?;
                }
                // Safe defaults for the other suspension kinds (M0 tests don't hit them).
                ReplProgress::OsCall(call) => {
                    progress = call
                        .resume(MontyObject::None, PrintWriter::Disabled)
                        .map_err(start_err)?;
                }
                ReplProgress::NameLookup(lookup) => {
                    progress = lookup
                        .resume(NameLookupResult::Undefined, PrintWriter::Disabled)
                        .map_err(start_err)?;
                }
                ReplProgress::ResolveFutures(futures) => {
                    let results: Vec<(u32, ExtFunctionResult)> = futures
                        .pending_call_ids()
                        .iter()
                        .map(|&id| (id, ExtFunctionResult::Return(MontyObject::None)))
                        .collect();
                    progress = futures
                        .resume(results, PrintWriter::Disabled)
                        .map_err(start_err)?;
                }
            }
        }
    }

    #[test]
    fn repl_runs_trivial_expression() {
        let mut repl = new_repl();
        let v = repl
            .feed_run("1 + 2", vec![], PrintWriter::Disabled)
            .unwrap();
        assert_eq!(v, MontyObject::Int(3));
    }

    #[test]
    fn repl_state_persists() {
        let mut r = new_repl();
        r.feed_run("x = 10", vec![], PrintWriter::Disabled).unwrap();
        r.feed_run("y = 20", vec![], PrintWriter::Disabled).unwrap();
        let v = r.feed_run("x + y", vec![], PrintWriter::Disabled).unwrap();
        assert_eq!(v, MontyObject::Int(30));
    }

    #[test]
    fn host_function_returns_value() {
        let mut counter = 0;
        let (v, _repl) = drive(new_repl(), "host_get(5)", &mut counter).unwrap();
        assert_eq!(v, MontyObject::Int(123));
    }

    #[test]
    fn host_function_mutates_shared_state() {
        // The literal M0 goal: call one host function over shared session state.
        let mut counter = 0;
        let (_v, repl) = drive(new_repl(), "host_add(5)", &mut counter).unwrap();
        let (v2, _repl) = drive(repl, "host_add(7)", &mut counter).unwrap();
        assert_eq!(counter, 12);
        assert_eq!(v2, MontyObject::Int(12));
    }

    #[test]
    fn monty_error_folds_into_droplet_error() {
        fn run(code: &str) -> Result<MontyObject, DropletError> {
            let mut r = new_repl();
            // feed_run returns MontyException; `?` folds it via #[from] (invariant #10).
            Ok(r.feed_run(code, vec![], PrintWriter::Disabled)?)
        }
        // A bare undefined name raises NameError inside feed_run.
        assert!(matches!(
            run("undefined_name + 1"),
            Err(DropletError::Monty(_))
        ));
    }
}
