// crates/droplet-core/src/security/mod.rs
//! Adversarial security test suite for Droplet's V1 agent surface.
//!
//! The agent program is HOSTILE. Each submodule attacks one class of the boundary. Every test
//! carries a CONTRACT label (HOLDS / PROBE / CANARY / LIMIT) — see
//! docs/superpowers/plans/2026-06-25-adversarial-test-suite.md for the protocol.
#![cfg(test)]
#![allow(unused_imports, dead_code)]

use monty::MontyObject;

use crate::DropletError;
use crate::engine_duckdb::DuckEngine;
use crate::registry::Registry;
use crate::session::Session;
use crate::tool::{Tool, ToolCx};

mod exfiltration;
mod dos_limits;
mod sandbox_escape;
mod egress;
mod writes_ddl;
mod sql_injection;
mod handles_args;
mod result_cap;
mod error_safety;

/// A unique temp dir per tag so fixtures never collide.
pub(crate) fn tmp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("droplet-sec-{tag}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Write a parquet from a SELECT via a throwaway connection (NOT the hardened session engine).
pub(crate) fn write_parquet(path: &str, select_sql: &str) {
    let conn = duckdb::Connection::open_in_memory().unwrap();
    conn.execute_batch(&format!("COPY ({select_sql}) TO '{path}' (FORMAT PARQUET)"))
        .unwrap();
}

/// A valid 1-row `sales` parquet (region:str, amt:DOUBLE) a legitimate path can point at.
pub(crate) fn sales_parquet(dir: &std::path::Path) -> String {
    let p = dir.join("sales.parquet").to_str().unwrap().to_string();
    write_parquet(&p, "SELECT 'EU' AS region, CAST(1.0 AS DOUBLE) AS amt");
    p
}

/// List length of a MontyObject, else 0 (for capped read-out size assertions).
pub(crate) fn list_len(v: &MontyObject) -> usize {
    match v {
        MontyObject::List(items) => items.len(),
        _ => 0,
    }
}

/// Drive ONE registered `#[droplet_tool]` against a throwaway context (fresh engine + empty
/// registry) — the unit-level dispatch path, resolving the tool by name and calling its thunk.
pub(crate) fn dispatch(name: &str, args: &[MontyObject]) -> Result<MontyObject, DropletError> {
    let tool = inventory::iter::<Tool>()
        .find(|t| t.name == name)
        .unwrap_or_else(|| panic!("tool {name} must be registered"));
    let mut engine = DuckEngine::new_in_memory().unwrap();
    let mut handles = Registry::new();
    let mut cx = ToolCx { engine: &mut engine, handles: &mut handles };
    (tool.dispatch)(&mut cx, args, &[])
}

/// `dispatch` under `catch_unwind` so a PROBE can assert "must NOT panic". `Ok(inner)` = the thunk
/// returned (inner is the tool Result); `Err(_)` = it PANICKED (an `args[i]` OOB abort) — the finding.
pub(crate) fn catch_dispatch(
    name: &str,
    args: &[MontyObject],
) -> std::thread::Result<Result<MontyObject, DropletError>> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| dispatch(name, args)))
}

/// Like `dispatch` but passes explicit kwargs — exercises the `_kwargs` path (thunk ignores kwargs).
/// Also wrapped in `catch_unwind` (same semantics as `catch_dispatch`).
pub(crate) fn catch_dispatch_kw(
    name: &str,
    args: &[MontyObject],
    kwargs: &[(MontyObject, MontyObject)],
) -> std::thread::Result<Result<MontyObject, DropletError>> {
    let tool = inventory::iter::<Tool>()
        .find(|t| t.name == name)
        .unwrap_or_else(|| panic!("tool {name} must be registered"));
    let kwargs_owned = kwargs.to_vec();
    let args_owned = args.to_vec();
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let mut engine = DuckEngine::new_in_memory().unwrap();
        let mut handles = Registry::new();
        let mut cx = ToolCx { engine: &mut engine, handles: &mut handles };
        (tool.dispatch)(&mut cx, &args_owned, &kwargs_owned)
    }))
}

/// Run a closure on a thread with a large (16 MiB) stack and propagate its panics, so a deep-recursion
/// test cannot SIGABRT the default-stack test thread in debug builds. `join().unwrap()` re-panics on
/// the calling thread if `f` panicked, so `assert!`s inside `f` still fail the test normally.
pub(crate) fn run_big_stack(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}
