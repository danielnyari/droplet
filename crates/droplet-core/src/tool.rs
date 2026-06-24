//! The tool registry. Each `#[droplet_tool]` submits one `Tool` into the `inventory` collection at
//! link time; the `run_code` driver (session.rs) iterates them to dispatch sandbox calls by name.
//! This IS the auto-bootstrapped tool surface (PRODUCT.md invariant #4) — no hand-maintained table.

use monty::MontyObject;

use crate::DropletError;
use crate::engine_duckdb::DuckEngine;

/// A tool's host implementation: unpack args, run against the local engine, pack the return value.
/// `#[droplet_tool]` generates one of these per tool (Task 4).
pub type DispatchFn = fn(
    &mut DuckEngine,
    &[MontyObject],
    &[(MontyObject, MontyObject)],
) -> Result<MontyObject, DropletError>;

/// One registered tool: its sandbox-visible name, its generated `.pyi` stub line, and its dispatch.
pub struct Tool {
    pub name: &'static str,
    pub stub: &'static str,
    pub dispatch: DispatchFn,
}

inventory::collect!(Tool);

#[cfg(test)]
mod tests {
    use super::*;
    use droplet_macros::droplet_tool;

    /// Trivial tool with NO engine parameter — exercises the macro's arg conversion + stub gen +
    /// submission path without DuckDB. (The real engine-using tool is `query`, Task 5.)
    #[droplet_tool]
    pub fn echo(text: String) -> Result<String, DropletError> {
        Ok(text)
    }

    #[test]
    fn macro_generates_stub_and_dispatch() {
        let tool = inventory::iter::<Tool>()
            .find(|t| t.name == "echo")
            .expect("echo tool must be registered by #[droplet_tool]");
        // Stub fragment is generated from the signature; the engine param (none here) is omitted.
        assert_eq!(tool.stub, "def echo(text: str) -> str: ...");
        // Dispatch converts MontyObject args -> Rust, runs, converts the return back.
        let mut eng = DuckEngine::new_in_memory().unwrap();
        let out = (tool.dispatch)(&mut eng, &[MontyObject::String("hi".into())], &[]).unwrap();
        assert_eq!(out, MontyObject::String("hi".into()));
    }

    #[test]
    fn dispatch_reports_bad_argument_type() {
        let tool = inventory::iter::<Tool>()
            .find(|t| t.name == "echo")
            .unwrap();
        let mut eng = DuckEngine::new_in_memory().unwrap();
        // Passing an int where echo wants a str must surface as BadArg (a retryable boundary error).
        let err = (tool.dispatch)(&mut eng, &[MontyObject::Int(1)], &[]).unwrap_err();
        assert!(matches!(err, DropletError::BadArg(_)));
    }

    // A test-only tool, submitted by hand HERE ONLY to prove the inventory plumbing collects and
    // iterates. Real tools are submitted by #[droplet_tool] (Task 4), never by hand.
    fn dispatch_noop(
        _eng: &mut DuckEngine,
        _args: &[MontyObject],
        _kwargs: &[(MontyObject, MontyObject)],
    ) -> Result<MontyObject, DropletError> {
        Ok(MontyObject::None)
    }
    inventory::submit! { Tool { name: "__test_noop", stub: "def __test_noop() -> None: ...", dispatch: dispatch_noop } }

    #[test]
    fn inventory_collects_submitted_tools() {
        let found = inventory::iter::<Tool>().any(|t| t.name == "__test_noop");
        assert!(found, "inventory must collect the test tool");
    }
}
