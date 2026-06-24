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
