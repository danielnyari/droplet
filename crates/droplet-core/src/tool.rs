//! The tool registry. Each `#[droplet_tool]` submits one `Tool` into the `inventory` collection at
//! link time; the `run_code` driver (session.rs) iterates them to dispatch sandbox calls by name.
//! This IS the auto-bootstrapped tool surface (PRODUCT.md invariant #4) — no hand-maintained table.

use monty::MontyObject;

use crate::DropletError;
use crate::engine_duckdb::{Dataset, DuckEngine};
use crate::registry::Registry;

/// The host-side context a tool runs against: the session's local analyze engine plus the handle
/// registry that keeps `Dataset`s host-side (invariant #6) while the sandbox holds only opaque
/// integer handles. `#[droplet_tool]` functions take `&mut ToolCx` as their first parameter when
/// they need either; the generated dispatch thunk always receives it (to convert handle args/rets).
pub struct ToolCx<'a> {
    pub engine: &'a mut DuckEngine,
    pub handles: &'a mut Registry<Dataset>,
}

/// A tool's host implementation: unpack args (resolving handles via `cx`), run against the engine,
/// pack the return (registering handles via `cx`). `#[droplet_tool]` generates one of these per tool.
pub type DispatchFn = fn(
    &mut ToolCx,
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

    /// Build a throwaway context (fresh engine + empty handle registry) to drive a dispatch fn.
    fn with_cx<R>(f: impl FnOnce(&mut ToolCx) -> R) -> R {
        let mut engine = DuckEngine::new_in_memory().unwrap();
        let mut handles = Registry::new();
        let mut cx = ToolCx {
            engine: &mut engine,
            handles: &mut handles,
        };
        f(&mut cx)
    }

    /// Trivial tool with NO context parameter — exercises the macro's arg conversion + stub gen +
    /// submission path without touching engine/handles. (Real tools are in tools.rs.)
    #[droplet_tool]
    pub fn echo(text: String) -> Result<String, DropletError> {
        Ok(text)
    }

    #[test]
    fn macro_generates_stub_and_dispatch() {
        let tool = inventory::iter::<Tool>()
            .find(|t| t.name == "echo")
            .expect("echo tool must be registered by #[droplet_tool]");
        // Stub fragment is generated from the signature; the context param (none here) is omitted.
        assert_eq!(tool.stub, "def echo(text: str) -> str: ...");
        // Dispatch converts MontyObject args -> Rust, runs, converts the return back.
        let out =
            with_cx(|cx| (tool.dispatch)(cx, &[MontyObject::String("hi".into())], &[])).unwrap();
        assert_eq!(out, MontyObject::String("hi".into()));
    }

    #[test]
    fn dispatch_reports_bad_argument_type() {
        let tool = inventory::iter::<Tool>()
            .find(|t| t.name == "echo")
            .unwrap();
        // Passing an int where echo wants a str must surface as BadArg (a retryable boundary error).
        let err = with_cx(|cx| (tool.dispatch)(cx, &[MontyObject::Int(1)], &[])).unwrap_err();
        assert!(matches!(err, DropletError::BadArg(_)));
    }

    // A test-only tool, submitted by hand HERE ONLY to prove the inventory plumbing collects and
    // iterates. Real tools are submitted by #[droplet_tool], never by hand.
    fn dispatch_noop(
        _cx: &mut ToolCx,
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
