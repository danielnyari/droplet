# V1a — Code-mode walking skeleton Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** An agent's Python program, run in the Monty sandbox, calls a typed, **macro-generated** tool that analyzes a local Parquet file in DuckDB and gets the real aggregates back — `run_code("rows = query('sales.parquet','SELECT region, SUM(amt) AS t FROM data GROUP BY region'); print(rows)")` returns the answer to the agent's code.

**Architecture:** A new `droplet-macros` proc-macro crate provides `#[droplet_tool]`, which (at compile time) emits a Monty external-function dispatch thunk **and** the Python `.pyi` stub fragment from the Rust signature, auto-collected via the `inventory` crate. A `run_code` driver on `Session` feeds agent code to a persistent `MontyRepl`, suspends at each external call, dispatches by name to the collected tools (running them against the session's local DuckDB engine), and resumes — never a hand-maintained registry (PRODUCT.md invariant #4). One tool, `query(path, sql) -> list[dict]`, proves the whole loop. The demo runs both as a pure-Rust integration test and through the `droplet-py` wheel's new `Session.run_code`.

**Tech Stack:** Rust 2024, `monty` v0.0.18 (sandbox + suspend/resume), `duckdb` 1.10503.1 (bundled), `syn`/`quote`/`proc-macro2` (the macro), `inventory` (tool auto-collection), `pyo3` 0.28 + maturin (the wheel).

## Global Constraints

These apply to **every** task. Values copied verbatim from `PRODUCT.md`, the `droplet-roadmap` memory, and the existing `Cargo.toml`.

- Rust **edition 2024**, workspace **resolver 3**.
- **Invariant #4:** the tool surface is auto-bootstrapped — `#[droplet_tool]` for fixed primitives, no hand-maintained registry or stubs. The macro is built **for real**; there is no hand-wired dispatch table or hand-written `.pyi` anywhere in this milestone.
- **Invariant #8:** `droplet-core` must not contain Python-binding code; `pyo3` lives only in `droplet-py`. (`monty` itself is fine in either crate.)
- **Invariant #6:** only `to_rows`/`scalar`-style read-outs move rows into the sandbox, and they are capped (`DuckEngine::max_result_rows`, default 1000). Everything else stays a host-side handle.
- **Invariant #9:** DuckDB is synchronous; the `droplet-py` boundary releases the GIL around engine work with `Python::detach` (NOT `allow_threads` — renamed in pyo3 0.26; no alias).
- **Invariant #10:** one boundary error type — `thiserror`'s `DropletError` in libraries, `anyhow` only at binaries. Every engine error folds into `DropletError`.
- **Invariant #3:** the analyze engine is local & ephemeral. Keep `DuckEngine::new_in_memory`'s hardening (httpfs/S3 disabled). Tools read **local** paths only.
- **Dependency pins (do not "upgrade" casually):** `monty` = git tag **v0.0.18**; `pyo3` = **0.28** (NOT 0.29 — forced by monty's `jiter`→pyo3 0.28.x; `links="python"` allows one pyo3 in the graph); `duckdb` = **1.10503.1** features `["bundled","parquet","json"]`. **Never add a top-level `arrow` dep** — use the `duckdb::arrow` re-export (`cargo tree -i arrow` must show one arrow major).
- **New pins to add** (latest patch of the stated major; record exact resolved version in `Cargo.lock`): `syn` = "2" (feature `"full"`), `quote` = "1", `proc-macro2` = "1", `inventory` = "0.3". `verify:` each resolves cleanly alongside the locked graph.
- **Quality gate (every commit):** `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` all clean. This matches the M0/M1 bar.
- **Commit trailer:** end every commit message with `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- **House style for teaching notes** (the user is a Rust newbie, see the `user-rust-newbie` memory): keep the inline `🆕 Concept` / `⚠️ Invariant` / `verify:` notes — they are part of the deliverable, not decoration.

**What this milestone deliberately does NOT do** (these are V1b / V2 / later — do not build them now):
- No type-check-before-run enforcement. The macro **generates** the `.pyi` stub (invariant #4), and a test asserts it, but feeding it to `monty`'s `ty` type checker is **V2**.
- No `Dataset` handles crossing into the sandbox, no handle `Registry` retype, no `filter_rows`/`group_agg`/`join`/`window`/`local_sql`/`to_rows`/`scalar` as agent tools — that is **V1b**. `query` is self-contained.
- No catalog, no `load`, no connectors, no cache, no snapshot.

**Accepted security gap (tracked, fixed in V3):** `query(path, sql)` lets the agent read **arbitrary
local host files** (`read_csv`/`read_blob`/`glob` on any path) — host-data exfiltration. The local
filesystem is deliberately not sandboxed because the engine must read the local Parquet, and V1a's
`query` hands the agent both the path and the SQL. Network egress, file writes, and Python OS escape
**are** blocked (see `crates/droplet-core/src/security_tests.rs`). The gap is documented in full at
`docs/security/2026-06-24-v1a-local-fs-read-gap.md`, pinned by the canary test
`known_gap_local_file_read_is_currently_possible`, and closed at the **V3** load boundary (host-owned
paths + DuckDB `allowed_directories`/`enable_external_access` scoping).

---

## File Structure

**New files:**
- `crates/droplet-macros/Cargo.toml` — the proc-macro crate manifest (`proc-macro = true`).
- `crates/droplet-macros/src/lib.rs` — `#[droplet_tool]` attribute macro.
- `crates/droplet-core/src/convert.rs` — `IntoMonty` / `FromMonty` traits + impls; the `Rows` newtype.
- `crates/droplet-core/src/tool.rs` — `Tool` registration struct, `DispatchFn`, `inventory::collect!(Tool)`.
- `crates/droplet-core/src/tools.rs` — the fixed primitive(s): `#[droplet_tool] fn query(...)`.
- `crates/droplet-py/python/tests/test_session.py` — the wheel-side demo test.

**Modified files:**
- `Cargo.toml` (root) — add `crates/droplet-macros` to `members`; add the 4 new workspace deps.
- `crates/droplet-core/Cargo.toml` — depend on `droplet-macros` and `inventory`.
- `crates/droplet-core/src/lib.rs` — declare the new modules; add `DropletError::BadArg`.
- `crates/droplet-core/src/session.rs` — own a `MontyRepl`; add `run_code`.
- `crates/droplet-py/Cargo.toml` — add `monty` dep.
- `crates/droplet-py/src/lib.rs` — add the `Session` pyclass + `monty_to_py` converter.
- `crates/droplet-py/python/droplet/__init__.py` — export `Session`.

---

## Task 1: The `droplet-macros` crate + an identity `#[droplet_tool]`

Stand up the proc-macro crate with the smallest possible macro that compiles and re-emits the annotated function unchanged. We grow it in Task 4; here we just prove the crate links into the workspace.

**Files:**
- Create: `crates/droplet-macros/Cargo.toml`
- Create: `crates/droplet-macros/src/lib.rs`
- Modify: `Cargo.toml` (root)

**Interfaces:**
- Produces: the attribute macro `droplet_macros::droplet_tool` (identity for now). Later tasks consume it as `#[droplet_tool]`.

- [ ] **Step 1: Add the crate to the workspace and declare the new shared deps**

In root `Cargo.toml`, add the member and the dependency pins.

```toml
[workspace]
resolver = "3"
members = ["crates/droplet-core", "crates/droplet-macros", "crates/droplet-py", "xtask"]
```

In the same file's `[workspace.dependencies]`, append:

```toml
# The #[droplet_tool] proc-macro (V1a). syn parses the Rust fn signature; quote builds the
# generated Monty dispatch thunk + .pyi stub; proc-macro2 is the token type they share.
syn         = { version = "2", features = ["full"] }
quote       = "1"
proc-macro2 = "1"
# Link-time collection of every #[droplet_tool] into one dispatch table — invariant #4's
# "no hand-maintained registry". verify: 0.3.x resolves; survives into the cdylib (Task 7).
inventory   = "0.3"
# droplet-macros is a path member; referenced by droplet-core below.
droplet-macros = { path = "crates/droplet-macros" }
```

  - 🆕 Concept: a **proc-macro crate** is compiled to run *inside the compiler*. It can export only procedural macros, so it must be its own crate (Rust Book: *Macros* → "Procedural Macros"). Keep it tiny.

- [ ] **Step 2: Write the crate manifest**

Create `crates/droplet-macros/Cargo.toml`:

```toml
[package]
name = "droplet-macros"
edition.workspace    = true
version.workspace    = true
license.workspace    = true
repository.workspace = true

[lib]
proc-macro = true        # this crate exports procedural macros (runs in the compiler)

[dependencies]
syn.workspace         = true
quote.workspace       = true
proc-macro2.workspace = true
```

- [ ] **Step 3: Write the identity macro**

Create `crates/droplet-macros/src/lib.rs`:

```rust
//! Procedural macros for Droplet. Currently: `#[droplet_tool]`.
//!
//! `#[droplet_tool]` turns one Rust function into a Monty external function: it re-emits the
//! function unchanged AND (Task 4) generates a dispatch thunk + a Python `.pyi` stub, registered
//! at link time via `inventory`. No hand-maintained tool table or stubs (PRODUCT.md invariant #4).

use proc_macro::TokenStream;
use quote::quote;
use syn::{ItemFn, parse_macro_input};

/// Mark a function as a Droplet tool callable from sandboxed agent code.
///
/// V1a: identity — re-emits the function unchanged. Task 4 adds the generated dispatch + stub.
#[proc_macro_attribute]
pub fn droplet_tool(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let func = parse_macro_input!(item as ItemFn);
    quote! { #func }.into()
}
```

  - 🆕 Concept: `parse_macro_input!(item as ItemFn)` parses the token stream into syn's typed AST for a function. `quote! { ... }` builds a new token stream; `#func` splices the parsed function back in (Rust Book: *Macros*; the `syn`/`quote` crates are the de-facto standard).

- [ ] **Step 4: Build it**

Run: `cargo build -p droplet-macros`
Expected: compiles clean (a `proc-macro = true` crate produces no normal artifact, but must build).

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/droplet-macros
git commit -m "V1a(1): droplet-macros crate + identity #[droplet_tool]

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: The conversion seam — `IntoMonty` / `FromMonty` + `Rows`

The macro stays type-agnostic by dispatching all `MontyObject` ↔ Rust conversions through two traits. This task builds them in `droplet-core` with full unit tests, before any macro generates code that uses them.

**Files:**
- Create: `crates/droplet-core/src/convert.rs`
- Modify: `crates/droplet-core/src/lib.rs` (add `pub mod convert;`, add `DropletError::BadArg`)
- Test: unit tests inside `crates/droplet-core/src/convert.rs`

**Interfaces:**
- Produces (consumed by the macro in Task 4 and the tool in Task 5):
  - `trait IntoMonty { fn into_monty(self) -> monty::MontyObject; }` — impl for `String`, `i64`, `f64`, `bool`, `crate::engine_duckdb::Value`, and `Rows`.
  - `trait FromMonty: Sized { fn from_monty(o: &monty::MontyObject) -> Result<Self, crate::DropletError>; }` — impl for `String`, `i64`, `f64`, `bool`.
  - `struct Rows(pub Vec<Vec<(String, crate::engine_duckdb::Value)>>)` — the capped read-out row set; `IntoMonty` turns it into a `MontyObject::List` of `MontyObject::Dict`.
- Consumes: `monty::MontyObject` (verified variants: `None`, `Bool(bool)`, `Int(i64)`, `Float(f64)`, `String(String)`, `List(Vec<MontyObject>)`, `Dict(DictPairs)` where `DictPairs: From<Vec<(MontyObject, MontyObject)>>`), and `crate::engine_duckdb::Value` (`Null`/`Bool`/`Int`/`Float`/`Str`).

- [ ] **Step 1: Add the `BadArg` error variant**

In `crates/droplet-core/src/lib.rs`, inside `enum DropletError`, add (next to `UnsupportedType`):

```rust
    // A tool received an argument whose MontyObject type didn't match the Rust parameter type
    // (e.g. an int where a str was expected). Surfaces from FromMonty at the sandbox boundary.
    #[error("bad tool argument: {0}")]
    BadArg(String),
```

- [ ] **Step 2: Declare the module**

In `crates/droplet-core/src/lib.rs`, add to the module list:

```rust
pub mod convert;
```

- [ ] **Step 3: Write the failing tests**

Create `crates/droplet-core/src/convert.rs` with ONLY the tests first (so the build fails on missing items):

```rust
//! The MontyObject ↔ Rust conversion seam. `#[droplet_tool]`-generated thunks read arguments via
//! `FromMonty` and pack return values via `IntoMonty`, so the macro never bakes in type knowledge.

use monty::MontyObject;

use crate::DropletError;
use crate::engine_duckdb::Value;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_round_trips() {
        let o = "hi".to_string().into_monty();
        assert_eq!(o, MontyObject::String("hi".into()));
        assert_eq!(String::from_monty(&o).unwrap(), "hi");
    }

    #[test]
    fn scalars_round_trip() {
        assert_eq!(42i64.into_monty(), MontyObject::Int(42));
        assert_eq!(i64::from_monty(&MontyObject::Int(42)).unwrap(), 42);
        assert_eq!(2.5f64.into_monty(), MontyObject::Float(2.5));
        assert_eq!(f64::from_monty(&MontyObject::Float(2.5)).unwrap(), 2.5);
        assert_eq!(true.into_monty(), MontyObject::Bool(true));
        assert!(bool::from_monty(&MontyObject::Bool(true)).unwrap());
    }

    #[test]
    fn wrong_type_is_bad_arg() {
        assert!(matches!(
            String::from_monty(&MontyObject::Int(1)),
            Err(DropletError::BadArg(_))
        ));
    }

    #[test]
    fn value_maps_to_monty() {
        assert_eq!(Value::Null.into_monty(), MontyObject::None);
        assert_eq!(Value::Int(7).into_monty(), MontyObject::Int(7));
        assert_eq!(Value::Str("x".into()).into_monty(), MontyObject::String("x".into()));
    }

    #[test]
    fn rows_become_list_of_dicts() {
        let rows = Rows(vec![vec![
            ("region".to_string(), Value::Str("EU".into())),
            ("t".to_string(), Value::Float(150.0)),
        ]]);
        let MontyObject::List(items) = rows.into_monty() else {
            panic!("Rows must convert to a List");
        };
        assert_eq!(items.len(), 1);
        let MontyObject::Dict(pairs) = &items[0] else {
            panic!("each row must be a Dict");
        };
        // DictPairs is IntoIterator over (MontyObject, MontyObject); clone to read in the test.
        let got: Vec<(MontyObject, MontyObject)> = pairs.clone().into_iter().collect();
        assert_eq!(got[0].0, MontyObject::String("region".into()));
        assert_eq!(got[0].1, MontyObject::String("EU".into()));
        assert_eq!(got[1].1, MontyObject::Float(150.0));
    }
}
```

  - `verify:` `DictPairs: Clone` (used by the test). If it is not `Clone`, iterate by destructuring instead. `MontyObject` derives `PartialEq` (the existing `sandbox.rs` tests rely on `assert_eq!` over `MontyObject`), so the equality asserts compile.

- [ ] **Step 4: Run the tests to confirm they fail**

Run: `cargo test -p droplet-core convert`
Expected: FAIL — `cannot find trait IntoMonty` / `Rows` not found.

- [ ] **Step 5: Implement the traits and `Rows` above the test module**

Insert into `crates/droplet-core/src/convert.rs` (after the `use` lines, before `#[cfg(test)]`):

```rust
/// Rust value → `MontyObject` (a tool's return value crossing back into the sandbox).
pub trait IntoMonty {
    fn into_monty(self) -> MontyObject;
}

/// `MontyObject` → Rust value (a tool argument coming from sandbox code). Borrows the argument;
/// a type mismatch is a `DropletError::BadArg` (surfaces to the agent as a retryable error).
pub trait FromMonty: Sized {
    fn from_monty(o: &MontyObject) -> Result<Self, DropletError>;
}

impl IntoMonty for String {
    fn into_monty(self) -> MontyObject {
        MontyObject::String(self)
    }
}
impl IntoMonty for i64 {
    fn into_monty(self) -> MontyObject {
        MontyObject::Int(self)
    }
}
impl IntoMonty for f64 {
    fn into_monty(self) -> MontyObject {
        MontyObject::Float(self)
    }
}
impl IntoMonty for bool {
    fn into_monty(self) -> MontyObject {
        MontyObject::Bool(self)
    }
}

impl FromMonty for String {
    fn from_monty(o: &MontyObject) -> Result<Self, DropletError> {
        match o {
            MontyObject::String(s) => Ok(s.clone()),
            other => Err(DropletError::BadArg(format!("expected str, got {other:?}"))),
        }
    }
}
impl FromMonty for i64 {
    fn from_monty(o: &MontyObject) -> Result<Self, DropletError> {
        match o {
            MontyObject::Int(n) => Ok(*n),
            other => Err(DropletError::BadArg(format!("expected int, got {other:?}"))),
        }
    }
}
impl FromMonty for f64 {
    fn from_monty(o: &MontyObject) -> Result<Self, DropletError> {
        match o {
            MontyObject::Float(f) => Ok(*f),
            other => Err(DropletError::BadArg(format!("expected float, got {other:?}"))),
        }
    }
}
impl FromMonty for bool {
    fn from_monty(o: &MontyObject) -> Result<Self, DropletError> {
        match o {
            MontyObject::Bool(b) => Ok(*b),
            other => Err(DropletError::BadArg(format!("expected bool, got {other:?}"))),
        }
    }
}

/// One capped read-out as plain typed rows (column order preserved). The agent-facing shape of a
/// tool result that returns table rows: `IntoMonty` turns it into `list[dict]` (invariant #6 keeps
/// it small — the engine cap already bounds the row count before it gets here).
pub struct Rows(pub Vec<Vec<(String, Value)>>);

impl IntoMonty for Value {
    fn into_monty(self) -> MontyObject {
        match self {
            Value::Null => MontyObject::None,
            Value::Bool(b) => MontyObject::Bool(b),
            Value::Int(i) => MontyObject::Int(i),
            Value::Float(f) => MontyObject::Float(f),
            Value::Str(s) => MontyObject::String(s),
        }
    }
}

impl IntoMonty for Rows {
    fn into_monty(self) -> MontyObject {
        let list = self
            .0
            .into_iter()
            .map(|row| {
                let pairs: Vec<(MontyObject, MontyObject)> = row
                    .into_iter()
                    .map(|(col, v)| (MontyObject::String(col), v.into_monty()))
                    .collect();
                MontyObject::Dict(pairs.into()) // Vec<(MontyObject,MontyObject)> -> DictPairs
            })
            .collect();
        MontyObject::List(list)
    }
}
```

- [ ] **Step 6: Run the tests to confirm they pass**

Run: `cargo test -p droplet-core convert`
Expected: PASS (5 tests).

- [ ] **Step 7: Commit**

```bash
git add crates/droplet-core/src/convert.rs crates/droplet-core/src/lib.rs
git commit -m "V1a(2): MontyObject <-> Rust conversion seam (IntoMonty/FromMonty/Rows)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: The tool registry type + `inventory` collection

Define the `Tool` record every `#[droplet_tool]` will submit, the dispatch function-pointer type, and the `inventory` collection point. Prove collection works with a test-only submission.

**Files:**
- Create: `crates/droplet-core/src/tool.rs`
- Modify: `crates/droplet-core/Cargo.toml` (add `droplet-macros`, `inventory`)
- Modify: `crates/droplet-core/src/lib.rs` (add `pub mod tool;`)
- Test: unit test inside `crates/droplet-core/src/tool.rs`

**Interfaces:**
- Produces:
  - `type DispatchFn = fn(&mut crate::engine_duckdb::DuckEngine, &[monty::MontyObject], &[(monty::MontyObject, monty::MontyObject)]) -> Result<monty::MontyObject, crate::DropletError>;`
  - `struct Tool { pub name: &'static str, pub stub: &'static str, pub dispatch: DispatchFn }`
  - `inventory::collect!(Tool);` — `inventory::iter::<Tool>()` yields every submitted tool.
- Consumes: nothing new beyond `monty`, `inventory`, `DuckEngine`.

  - 🆕 Concept: a **function pointer** `fn(A, B) -> C` is a value naming a plain function (no captured state), so it can live in a `const`/`static` — which is exactly what `inventory` needs to register tools at link time. The first parameter is `&mut DuckEngine` so a tool can run against the session's local engine; tools that don't need it ignore it.

- [ ] **Step 1: Add the dependencies**

In `crates/droplet-core/Cargo.toml`, under `[dependencies]`, append:

```toml
# The #[droplet_tool] proc-macro and the link-time table it submits into (invariant #4).
droplet-macros.workspace = true
inventory.workspace      = true
```

- [ ] **Step 2: Declare the module**

In `crates/droplet-core/src/lib.rs`, add:

```rust
pub mod tool;
```

- [ ] **Step 3: Write the failing test**

Create `crates/droplet-core/src/tool.rs`:

```rust
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
```

- [ ] **Step 4: Run the test to confirm it fails, then passes**

Run: `cargo test -p droplet-core tool::`
Expected first run: FAIL if deps/module not yet wired (compile error). After Steps 1–3 are in place it should compile and PASS.

  - `verify:` the `dispatch: dispatch_noop` field accepts the function item where a `DispatchFn` pointer is expected (a fn item coerces to a fn pointer in a `const`/`static` initializer). If the compiler complains, write `dispatch: dispatch_noop as DispatchFn`.

- [ ] **Step 5: Commit**

```bash
git add crates/droplet-core/Cargo.toml crates/droplet-core/src/tool.rs crates/droplet-core/src/lib.rs Cargo.lock
git commit -m "V1a(3): Tool registry type + inventory collection point

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: Make `#[droplet_tool]` generate the dispatch thunk + `.pyi` stub + submission

The macro meat. Reading the Rust signature, the macro emits: (1) the original function; (2) a `DispatchFn` thunk that converts args via `FromMonty`, calls the function, packs the result via `IntoMonty`; (3) an `inventory::submit!` registering a `Tool` with the function name, the generated `.pyi` stub line, and the thunk. We test it on a trivial `echo` tool (no engine) so the macro is verified independently of DuckDB.

**Files:**
- Modify: `crates/droplet-macros/src/lib.rs` (full macro)
- Test: a temporary `echo` tool + assertions in `crates/droplet-core/src/tool.rs`'s test module

**Interfaces:**
- Convention the macro enforces (consumed by Task 5):
  - A tool is `fn NAME([eng: &mut DuckEngine,] P1: T1, P2: T2, ...) -> Result<R, DropletError>`.
  - If the **first** parameter's type is `&mut DuckEngine`, it is the host-injected engine: omitted from the `.pyi` stub and passed through by the thunk. All other parameters are agent-visible; each `Ti` must impl `FromMonty`. `R` must impl `IntoMonty`.
  - Rust→Python stub type map: `String`/`&str`→`str`, `i64`→`int`, `f64`→`float`, `bool`→`bool`, `Rows`→`list[dict]`; anything else→`object` (with a compile-time `warning`, never a silent guess).
  - `ponytail:` the generated code uses `crate::...` paths, so **tools must be defined inside `droplet-core`** in V1a. Ceiling: if a later milestone defines tools in another crate, switch the macro to `::droplet_core::...` paths (add `extern crate self as droplet_core;` to droplet-core's `lib.rs` so the absolute path resolves there too).

- [ ] **Step 1: Write the failing test (the `echo` tool)**

In `crates/droplet-core/src/tool.rs`, inside `mod tests`, add (above the existing test):

```rust
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
        let tool = inventory::iter::<Tool>().find(|t| t.name == "echo").unwrap();
        let mut eng = DuckEngine::new_in_memory().unwrap();
        // Passing an int where echo wants a str must surface as BadArg (a retryable boundary error).
        let err = (tool.dispatch)(&mut eng, &[MontyObject::Int(1)], &[]).unwrap_err();
        assert!(matches!(err, DropletError::BadArg(_)));
    }
```

- [ ] **Step 2: Run the test to confirm it fails**

Run: `cargo test -p droplet-core tool::tests::macro_generates_stub_and_dispatch`
Expected: FAIL — the identity macro registers nothing, so `find(... == "echo")` panics.

- [ ] **Step 3: Write the full macro**

Replace `crates/droplet-macros/src/lib.rs` entirely:

```rust
//! Procedural macros for Droplet. `#[droplet_tool]` makes a Rust function callable from sandboxed
//! agent code: it re-emits the function, generates a Monty dispatch thunk and a Python `.pyi` stub
//! line from the signature, and registers both at link time via `inventory`. There is no
//! hand-maintained tool table or stub file anywhere (PRODUCT.md invariant #4).

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{FnArg, ItemFn, Pat, ReturnType, Type, parse_macro_input};

/// Mark a function as a Droplet tool. See the module docs for the calling convention.
#[proc_macro_attribute]
pub fn droplet_tool(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let func = parse_macro_input!(item as ItemFn);
    let fn_name = func.sig.ident.clone();
    let fn_name_str = fn_name.to_string();
    let thunk_name = format_ident!("__droplet_dispatch_{}", fn_name);

    // Split the engine parameter (if first and typed &mut DuckEngine) from the agent-visible ones.
    let params: Vec<&FnArg> = func.sig.inputs.iter().collect();
    let engine_first = params.first().is_some_and(|a| is_engine_param(a));
    let visible: Vec<&FnArg> = if engine_first {
        params[1..].to_vec()
    } else {
        params.clone()
    };

    // For each agent-visible param, capture (ident, type) for both the thunk and the stub.
    let mut arg_idents = Vec::new();
    let mut arg_types = Vec::new();
    for arg in &visible {
        let FnArg::Typed(pt) = arg else {
            return compile_err(&func, "tools cannot take `self`");
        };
        let Pat::Ident(pi) = &*pt.pat else {
            return compile_err(&func, "tool parameters must be simple identifiers");
        };
        arg_idents.push(pi.ident.clone());
        arg_types.push((*pt.ty).clone());
    }

    // The thunk converts args via FromMonty, calls the fn (engine passed through if present), and
    // packs the return via IntoMonty. Tools return Result<R, DropletError>, so `?` propagates.
    let indices: Vec<syn::Index> = (0..arg_idents.len()).map(syn::Index::from).collect();
    let call = if engine_first {
        quote! { #fn_name(eng, #(#arg_idents),*) }
    } else {
        quote! { #fn_name(#(#arg_idents),*) }
    };
    let engine_binding = if engine_first {
        quote! {}
    } else {
        // Engine is unused for engine-less tools; silence the warning without renaming the param.
        quote! { let _ = &mut *eng; }
    };

    // The Python stub line, e.g. `def echo(text: str) -> str: ...`.
    let ret_py = python_return_type(&func.sig.output);
    let stub = build_stub(&fn_name_str, &arg_idents, &arg_types, &ret_py);

    let expanded = quote! {
        #func

        #[doc(hidden)]
        fn #thunk_name(
            eng: &mut crate::engine_duckdb::DuckEngine,
            args: &[::monty::MontyObject],
            _kwargs: &[(::monty::MontyObject, ::monty::MontyObject)],
        ) -> ::core::result::Result<::monty::MontyObject, crate::DropletError> {
            #engine_binding
            #( let #arg_idents = <#arg_types as crate::convert::FromMonty>::from_monty(&args[#indices])?; )*
            let __ret = #call?;
            ::core::result::Result::Ok(crate::convert::IntoMonty::into_monty(__ret))
        }

        ::inventory::submit! {
            crate::tool::Tool {
                name: #fn_name_str,
                stub: #stub,
                dispatch: #thunk_name,
            }
        }
    };
    expanded.into()
}

/// True if this parameter is `eng: &mut DuckEngine` (the injected host engine).
fn is_engine_param(arg: &FnArg) -> bool {
    let FnArg::Typed(pt) = arg else { return false };
    let Type::Reference(r) = &*pt.ty else {
        return false;
    };
    r.mutability.is_some() && last_ident(&r.elem).as_deref() == Some("DuckEngine")
}

/// The last path-segment identifier of a type (`&str` -> "str", `Rows` -> "Rows", etc.).
fn last_ident(ty: &Type) -> Option<String> {
    match ty {
        Type::Reference(r) => last_ident(&r.elem),
        Type::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()),
        _ => None,
    }
}

/// Rust type -> Python stub type. Unknown -> "object" (callers should add a known mapping instead).
fn python_type(ty: &Type) -> String {
    match last_ident(ty).as_deref() {
        Some("String" | "str") => "str",
        Some("i64") => "int",
        Some("f64") => "float",
        Some("bool") => "bool",
        Some("Rows") => "list[dict]",
        _ => "object",
    }
    .to_string()
}

/// The Python return type from `-> Result<R, DropletError>` (or from a bare `-> R`).
fn python_return_type(output: &ReturnType) -> String {
    let ReturnType::Type(_, ty) = output else {
        return "None".to_string();
    };
    if let Type::Path(p) = &**ty {
        if let Some(seg) = p.path.segments.last() {
            if seg.ident == "Result" {
                if let syn::PathArguments::AngleBracketed(ab) = &seg.arguments {
                    if let Some(syn::GenericArgument::Type(inner)) = ab.args.first() {
                        return python_type(inner);
                    }
                }
            }
        }
    }
    python_type(ty)
}

/// Assemble `def NAME(p1: t1, p2: t2) -> ret: ...`.
fn build_stub(
    name: &str,
    idents: &[syn::Ident],
    types: &[Type],
    ret_py: &str,
) -> String {
    let params = idents
        .iter()
        .zip(types)
        .map(|(id, ty)| format!("{id}: {}", python_type(ty)))
        .collect::<Vec<_>>()
        .join(", ");
    format!("def {name}({params}) -> {ret_py}: ...")
}

/// Emit a compile error attached to the offending function's tokens.
fn compile_err(func: &ItemFn, msg: &str) -> TokenStream {
    syn::Error::new_spanned(&func.sig, msg)
        .to_compile_error()
        .into()
}
```

  - 🆕 Concept: `quote!`'s `#(#xs),*` is **repetition** — it expands the pattern once per element of `xs`, comma-separated (like a loop inside the generated code). We use it to splice one `let p = FromMonty::from_monty(&args[i])?;` per parameter (Rust Book *Macros*; the `quote` docs call these "interpolations").
  - ⚠️ Invariant #4: this macro IS the auto-bootstrap. The stub string and the dispatch are both derived from the one signature — author the function once, get registration + typing for free, no parallel hand-maintained surface.

- [ ] **Step 4: Run the macro tests to confirm they pass**

Run: `cargo test -p droplet-core tool::`
Expected: PASS — `macro_generates_stub_and_dispatch`, `dispatch_reports_bad_argument_type`, and the earlier `inventory_collects_submitted_tools`.

- [ ] **Step 5: Confirm clippy is clean on the generated code**

Run: `cargo clippy -p droplet-core --all-targets -- -D warnings`
Expected: clean. (If the generated `let _ = &mut *eng;` or the thunk trips a lint, adjust the macro — do not `#[allow]` blindly.)

- [ ] **Step 6: Commit**

```bash
git add crates/droplet-macros/src/lib.rs crates/droplet-core/src/tool.rs
git commit -m "V1a(4): #[droplet_tool] generates dispatch thunk + .pyi stub + inventory submit

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 5: The `query` tool

The first real tool: `query(path, sql) -> list[dict]`, self-contained (registers the local Parquet as `data`, runs the SQL, returns capped rows). Reuses the existing engine primitives unchanged.

**Files:**
- Create: `crates/droplet-core/src/tools.rs`
- Modify: `crates/droplet-core/src/lib.rs` (add `pub mod tools;`)
- Test: unit test inside `crates/droplet-core/src/tools.rs`

**Interfaces:**
- Produces: `#[droplet_tool] pub fn query(eng: &mut DuckEngine, path: String, sql: String) -> Result<Rows, DropletError>` — registered as the tool `"query"` with stub `def query(path: str, sql: str) -> list[dict]: ...`. Consumed by the driver (Task 6) and the wheel demo (Task 7).
- Consumes: `DuckEngine::register_parquet`, `DuckEngine::local_sql`, `DuckEngine::to_rows_values` (all existing); `crate::convert::Rows`.

- [ ] **Step 1: Write the failing test**

Create `crates/droplet-core/src/tools.rs`:

```rust
//! Fixed analyze primitives exposed to sandboxed agent code via `#[droplet_tool]`.
//!
//! V1a ships exactly one: `query`. V1b adds the handle-based surface (filter_rows/group_agg/...).

use droplet_macros::droplet_tool;

use crate::DropletError;
use crate::convert::Rows;
use crate::engine_duckdb::DuckEngine;

#[cfg(test)]
mod tests {
    use super::*;
    use monty::MontyObject;

    /// Write a tiny `sales.parquet` (region:str, amt:DOUBLE) via a throwaway DuckDB connection.
    /// `amt` is cast to DOUBLE on purpose: a decimal literal like `100.0` is DECIMAL in DuckDB, and
    /// `SUM` over DECIMAL/INTEGER widens to DECIMAL/HUGEINT (Arrow Decimal128) which the capped
    /// read-out does not yet decode; DOUBLE -> Float64 crosses cleanly. (HUGEINT/DECIMAL decoding is
    /// a later engine refinement.)
    fn write_sales_parquet(dir: &std::path::Path) -> String {
        let path = dir.join("sales.parquet");
        let p = path.to_str().unwrap().to_string();
        let conn = duckdb::Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "COPY (SELECT region, amt::DOUBLE AS amt \
             FROM (VALUES ('EU', 100.0), ('EU', 50.0), ('US', 200.0)) AS t(region, amt)) \
             TO '{p}' (FORMAT PARQUET)"
        ))
        .unwrap();
        p
    }

    #[test]
    fn query_tool_is_registered_with_stub() {
        let tool = inventory::iter::<crate::tool::Tool>()
            .find(|t| t.name == "query")
            .expect("query must be registered");
        assert_eq!(tool.stub, "def query(path: str, sql: str) -> list[dict]: ...");
    }

    #[test]
    fn query_returns_aggregates_via_dispatch() {
        let dir = std::env::temp_dir().join("droplet-v1a-query-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = write_sales_parquet(&dir);

        let tool = inventory::iter::<crate::tool::Tool>()
            .find(|t| t.name == "query")
            .unwrap();
        let mut eng = DuckEngine::new_in_memory().unwrap();
        let out = (tool.dispatch)(
            &mut eng,
            &[
                MontyObject::String(path),
                MontyObject::String(
                    "SELECT region, SUM(amt) AS t FROM data GROUP BY region".into(),
                ),
            ],
            &[],
        )
        .unwrap();

        // list[dict] back: {region -> t}.
        let MontyObject::List(items) = out else {
            panic!("expected a list");
        };
        let mut got = std::collections::BTreeMap::new();
        for it in items {
            let MontyObject::Dict(pairs) = it else { panic!() };
            let (mut region, mut t) = (None, None);
            for (k, v) in pairs.clone() {
                if let MontyObject::String(k) = k {
                    match (k.as_str(), v) {
                        ("region", MontyObject::String(s)) => region = Some(s),
                        ("t", MontyObject::Float(f)) => t = Some(f),
                        _ => {}
                    }
                }
            }
            got.insert(region.unwrap(), t.unwrap());
        }
        assert_eq!(got.get("EU"), Some(&150.0));
        assert_eq!(got.get("US"), Some(&200.0));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
```

- [ ] **Step 2: Add the module declaration**

In `crates/droplet-core/src/lib.rs`, add:

```rust
pub mod tools;
```

- [ ] **Step 3: Run the test to confirm it fails**

Run: `cargo test -p droplet-core tools::`
Expected: FAIL — `query` not found in inventory (function not written yet).

- [ ] **Step 4: Write the `query` tool**

In `crates/droplet-core/src/tools.rs`, add above `#[cfg(test)]`:

```rust
/// Run read-only SQL over a single local Parquet file, returning the (capped) result rows.
///
/// The agent writes `FROM data` in `sql`; `data` is bound to the file at `path`. The engine's cap
/// (invariant #6) bounds how many rows cross back. Local file only — the engine has the network
/// filesystems disabled (invariant #3), so a remote path fails instantly with no egress.
#[droplet_tool]
pub fn query(eng: &mut DuckEngine, path: String, sql: String) -> Result<Rows, DropletError> {
    let ds = eng.register_parquet(&path)?;
    let result = eng.local_sql(&sql, &[("data", &ds)])?;
    Ok(Rows(eng.to_rows_values(&result)?))
}
```

  - 🔗 Maps to: this is the smallest thing that is *actually Droplet* — code mode producing an answer over local data. V1b widens this into the full handle-based analyze surface; the macro + driver built here are the permanent machinery, not a scaffold.
  - `verify:` `COPY (...) TO '...' (FORMAT PARQUET)` works with the `duckdb` `parquet` feature (bundled, statically linked) without an `INSTALL/LOAD`. If it errors, write the fixture with `pyarrow` instead and commit it as `crates/droplet-core/tests/data/sales.parquet`.

- [ ] **Step 5: Run the tests to confirm they pass**

Run: `cargo test -p droplet-core tools::`
Expected: PASS (`query_tool_is_registered_with_stub`, `query_returns_aggregates_via_dispatch`).

- [ ] **Step 6: Commit**

```bash
git add crates/droplet-core/src/tools.rs crates/droplet-core/src/lib.rs
git commit -m "V1a(5): query(path, sql) tool — first macro-generated analyze primitive

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 6: The `run_code` driver on `Session` — First Working Droplet (pure Rust)

Promote the proven suspend/resume loop from `sandbox.rs` into a real `Session::run_code`. The session owns a persistent `MontyRepl`; `run_code` feeds agent code, dispatches each external call by name to the collected tools (against the session's engine), and resumes to completion. This task ends in the milestone's pure-Rust demo.

**Files:**
- Modify: `crates/droplet-core/src/session.rs` (add `repl` field; add `run_code` + helpers)
- Test: integration test inside `crates/droplet-core/src/session.rs`'s test module

**Interfaces:**
- Produces: `Session::run_code(&mut self, code: &str) -> Result<monty::MontyObject, DropletError>` — runs one agent program to completion, returning the value of its last expression. Consumed by the `droplet-py` wheel (Task 7).
- Consumes: `Session.duck` (the engine), the `Tool` inventory, and monty's `MontyRepl<NoLimitTracker>` API: `feed_start(self, code, Vec<(String, MontyObject)>, PrintWriter) -> Result<ReplProgress<T>, Box<ReplStartError<T>>>`; `ReplProgress::{Complete{repl,value}, FunctionCall(ReplFunctionCall), OsCall, NameLookup, ResolveFutures}`; `ReplFunctionCall{ function_name: String, args: Vec<MontyObject>, kwargs: Vec<(MontyObject, MontyObject)>, .. }.resume(impl Into<ExtFunctionResult>, PrintWriter)`; `ExtFunctionResult::{Return, NotFound}`.

- [ ] **Step 1: Add the imports and the `repl` field**

In `crates/droplet-core/src/session.rs`, add to the top `use` block:

```rust
use monty::{
    ExtFunctionResult, MontyObject, MontyRepl, NameLookupResult, NoLimitTracker, PrintWriter,
    ReplProgress, ReplStartError,
};

use crate::tool::Tool;
```

Add the field to `struct Session` (after `duck`):

```rust
    // The session's persistent Monty REPL — agent code across run_code steps shares this namespace
    // (invariant #8: monty is fine in core; only pyo3 is barred). Held in an Option so run_code can
    // take it out for the duration of a step and put it back on Complete.
    repl: Option<MontyRepl<NoLimitTracker>>,
```

  - 🆕 Concept: `Option<T>` with `.take()` lets us *move* the REPL out of `&mut self`, drive it (monty's `feed_start` consumes the REPL by value), then store the returned REPL back — the standard Rust pattern for "temporarily own a field" (Rust Book: *Enums and Pattern Matching* → `Option`; `Option::take`).

- [ ] **Step 2: Initialize `repl` in `Session::new`**

In `Session::new`, after `let duck = ...;`, add:

```rust
        // One persistent REPL per session. NoLimitTracker for now; a real resource limiter is a
        // later milestone (// SWAP: LimitedTracker for prod).
        let repl = Some(MontyRepl::new("session.py", NoLimitTracker));
```

And add `repl,` to the `Ok(Self { ... })` literal.

- [ ] **Step 3: Write the failing demo test**

In `crates/droplet-core/src/session.rs`'s `mod tests`, add:

```rust
    /// Reuse the engine test's fixture writer shape (DOUBLE amt; see tools.rs for why).
    fn write_sales_parquet(dir: &std::path::Path) -> String {
        let path = dir.join("sales.parquet");
        let p = path.to_str().unwrap().to_string();
        let conn = duckdb::Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "COPY (SELECT region, amt::DOUBLE AS amt \
             FROM (VALUES ('EU', 100.0), ('EU', 50.0), ('US', 200.0)) AS t(region, amt)) \
             TO '{p}' (FORMAT PARQUET)"
        ))
        .unwrap();
        p
    }

    /// FIRST WORKING DROPLET (pure Rust): agent code in the Monty sandbox calls the macro-generated
    /// `query` tool and gets the real aggregates back into its own code. This is V1a's "Done when".
    #[test]
    fn run_code_runs_agent_program_against_local_parquet() -> Result<(), DropletError> {
        let dir = std::env::temp_dir().join("droplet-v1a-runcode-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = write_sales_parquet(&dir);

        let mut session = Session::new("run-v1a")?;
        // The agent's program: query -> print -> leave `rows` as the final expression so its value
        // crosses back as the run_code result (proving the aggregates reached the agent's code).
        let code = format!(
            "rows = query({path:?}, 'SELECT region, SUM(amt) AS t FROM data GROUP BY region')\n\
             print(rows)\n\
             rows"
        );
        let value = session.run_code(&code)?;

        let MontyObject::List(items) = value else {
            panic!("expected list[dict], got {value:?}");
        };
        let mut got = std::collections::BTreeMap::new();
        for it in items {
            let MontyObject::Dict(pairs) = it else { panic!() };
            let (mut region, mut t) = (None, None);
            for (k, v) in pairs.clone() {
                if let MontyObject::String(k) = k {
                    match (k.as_str(), v) {
                        ("region", MontyObject::String(s)) => region = Some(s),
                        ("t", MontyObject::Float(f)) => t = Some(f),
                        _ => {}
                    }
                }
            }
            got.insert(region.unwrap(), t.unwrap());
        }
        assert_eq!(got.get("EU"), Some(&150.0));
        assert_eq!(got.get("US"), Some(&200.0));

        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    /// A call to a name that is not a registered tool must surface an error, not panic.
    #[test]
    fn run_code_unknown_tool_errors() {
        let mut session = Session::new("run-v1a-unknown").unwrap();
        let err = session.run_code("not_a_real_tool(1)");
        assert!(err.is_err(), "unknown tool must produce an error");
    }
```

  - `verify:` monty supports `print(...)` (it routes to `PrintWriter`; with `Disabled` it is silently discarded) and returns the last expression's value in `ReplProgress::Complete{value}` (the existing `sandbox.rs` tests confirm last-expression-value behavior for `feed_run`).

- [ ] **Step 4: Run the test to confirm it fails**

Run: `cargo test -p droplet-core session::tests::run_code_runs_agent_program_against_local_parquet`
Expected: FAIL — `run_code` does not exist yet.

- [ ] **Step 5: Implement `run_code` and the error helper**

In `crates/droplet-core/src/session.rs`, add to `impl Session` (after `duck_mut`):

```rust
    /// Run one agent program in the session's Monty sandbox to completion, returning the value of
    /// its last expression. External-function calls suspend the sandbox; the host dispatches them by
    /// name to the `#[droplet_tool]`-registered tools (run against this session's local engine) and
    /// resumes (PRODUCT.md §8 execution model; invariant #6 keeps results capped & data host-side).
    ///
    /// On a tool error the run aborts (the error folds into `DropletError`) and the session's REPL
    /// is consumed; create a new `Session` to continue. (Graceful in-sandbox error resume is later.)
    pub fn run_code(&mut self, code: &str) -> Result<MontyObject, DropletError> {
        let repl = self.repl.take().expect("session REPL present");
        let mut progress = repl
            .feed_start(code, vec![], PrintWriter::Disabled)
            .map_err(start_err)?;
        loop {
            match progress {
                ReplProgress::Complete { repl, value } => {
                    self.repl = Some(repl); // put it back for the next run_code step
                    return Ok(value);
                }
                ReplProgress::FunctionCall(call) => {
                    let reply: ExtFunctionResult = match inventory::iter::<Tool>()
                        .find(|t| t.name == call.function_name)
                    {
                        Some(tool) => (tool.dispatch)(&mut self.duck, &call.args, &call.kwargs)?.into(),
                        None => ExtFunctionResult::NotFound(call.function_name.clone()),
                    };
                    progress = call.resume(reply, PrintWriter::Disabled).map_err(start_err)?;
                }
                // Safe defaults for suspension kinds V1a doesn't use (carried from the M0 seam).
                ReplProgress::OsCall(c) => {
                    progress = c
                        .resume(MontyObject::None, PrintWriter::Disabled)
                        .map_err(start_err)?;
                }
                ReplProgress::NameLookup(l) => {
                    progress = l
                        .resume(NameLookupResult::Undefined, PrintWriter::Disabled)
                        .map_err(start_err)?;
                }
                ReplProgress::ResolveFutures(f) => {
                    let results: Vec<(u32, ExtFunctionResult)> = f
                        .pending_call_ids()
                        .iter()
                        .map(|&id| (id, ExtFunctionResult::Return(MontyObject::None)))
                        .collect();
                    progress = f.resume(results, PrintWriter::Disabled).map_err(start_err)?;
                }
            }
        }
    }
```

Add this free function at the bottom of `session.rs` (module scope, not inside `impl`):

```rust
/// Fold monty's boxed start/resume error (which carries the surviving REPL + the exception) into
/// the one boundary error type (invariant #10). The surviving REPL is dropped — see run_code's note.
fn start_err(e: Box<ReplStartError<NoLimitTracker>>) -> DropletError {
    DropletError::Monty(e.error)
}
```

  - ⚠️ Invariant #6: only the tool's return value (`query`'s capped `list[dict]`) crosses back into the sandbox. The engine and its rows stay host-side; the sandbox sees plain values.
  - ⚠️ Invariant #4: dispatch reads `inventory::iter::<Tool>()` — the tools register themselves via `#[droplet_tool]`. There is no `match` on tool names here and no hand-written stub list anywhere.

- [ ] **Step 6: Run the demo + the unknown-tool test**

Run: `cargo test -p droplet-core session::`
Expected: PASS — including `run_code_runs_agent_program_against_local_parquet` and `run_code_unknown_tool_errors`, plus the pre-existing session tests.

- [ ] **Step 7: Full crate gate**

Run: `cargo test -p droplet-core && cargo clippy -p droplet-core --all-targets -- -D warnings`
Expected: all green.

- [ ] **Step 8: Commit**

```bash
git add crates/droplet-core/src/session.rs
git commit -m "V1a(6): Session::run_code driver — first working Droplet (pure Rust demo)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 7: Expose `Session.run_code` through the `droplet-py` wheel

Bind the driver to Python: a `Session` pyclass with `run_code(code) -> object`, releasing the GIL around the (synchronous) engine work and converting the returned `MontyObject` to native Python. The existing direct `Engine` binding stays untouched (it is the labeled SDK/test convenience).

**Files:**
- Modify: `crates/droplet-py/Cargo.toml` (add `monty` dep)
- Modify: `crates/droplet-py/src/lib.rs` (add `Session` pyclass + `monty_to_py`)
- Modify: `crates/droplet-py/python/droplet/__init__.py` (export `Session`)
- Create: `crates/droplet-py/python/tests/test_session.py`

**Interfaces:**
- Produces: Python `droplet.Session(run_id).run_code(code) -> object` (list/dict/scalars/None mirroring the agent program's result).
- Consumes: `droplet_core::session::Session::{new, run_code}`; `monty::MontyObject` (so `droplet-py` adds `monty` as a direct dep — fine; invariant #8 bars only `pyo3` from core, not `monty` from `droplet-py`).

- [ ] **Step 1: Add the `monty` dependency**

In `crates/droplet-py/Cargo.toml`, under `[dependencies]`, append:

```toml
# droplet-py converts run_code's MontyObject result into Python, so it names monty directly.
# (Invariant #8 bars pyo3 from droplet-core, not monty from droplet-py.) Same pinned monty.
monty.workspace = true
```

- [ ] **Step 2: Add the `Session` pyclass and converter**

In `crates/droplet-py/src/lib.rs`, add imports at the top (after the existing `use` lines):

```rust
use monty::MontyObject;
```

Add this converter (after `set_cell`):

```rust
/// Convert a `MontyObject` (a run_code result) into a native Python object. V1a covers the shapes a
/// tool can return: scalars, None, and `list[dict]` (built recursively). Anything else is an error
/// rather than a silent guess.
fn monty_to_py(py: Python<'_>, obj: &MontyObject) -> PyResult<PyObject> {
    let out = match obj {
        MontyObject::None => py.None(),
        MontyObject::Bool(b) => b.into_pyobject(py)?.to_owned().into_any().unbind(),
        MontyObject::Int(i) => i.into_pyobject(py)?.into_any().unbind(),
        MontyObject::Float(f) => f.into_pyobject(py)?.into_any().unbind(),
        MontyObject::String(s) => s.into_pyobject(py)?.into_any().unbind(),
        MontyObject::List(items) => {
            let list = PyList::empty(py);
            for it in items {
                list.append(monty_to_py(py, it)?)?;
            }
            list.into_any().unbind()
        }
        MontyObject::Dict(pairs) => {
            let dict = PyDict::new(py);
            for (k, v) in pairs.clone() {
                dict.set_item(monty_to_py(py, &k)?, monty_to_py(py, &v)?)?;
            }
            dict.into_any().unbind()
        }
        other => {
            return Err(PyRuntimeError::new_err(format!(
                "run_code returned an unsupported value: {other:?}"
            )));
        }
    };
    Ok(out)
}
```

  - `verify:` the pyo3 0.28 conversion calls. `bool`/`i64`/`f64`/`&str` implement `IntoPyObject`; `into_pyobject(py)?` returns a `Bound`/`Borrowed`. The exact `.to_owned()/.into_any().unbind()` chain may need small tweaks per type (the `bool` case returns a borrowed singleton). Adjust until `cargo build -p droplet-py` is clean; the shape (recurse, build `PyList`/`PyDict`) is correct.

Add the pyclass (after the `Engine` impl block, before `#[pymodule]`):

```rust
/// A Droplet run: drives agent code in the Monty sandbox via `run_code`. `unsendable` for the same
/// `!Sync` reason as `Engine` (it owns the ephemeral DuckDB connection) — pinned to its thread.
#[pyclass(name = "Session", unsendable)]
pub struct Session {
    inner: droplet_core::session::Session,
}

#[pymethods]
impl Session {
    #[new]
    fn new(run_id: &str) -> PyResult<Self> {
        let inner = droplet_core::session::Session::new(run_id).map_err(to_pyerr)?;
        Ok(Self { inner })
    }

    /// Run one agent program in the sandbox; return its result as native Python. The GIL is released
    /// for the duration (invariant #9): Monty + DuckDB are pure Rust and don't need it, so other
    /// Python threads run meanwhile.
    fn run_code(&mut self, py: Python<'_>, code: &str) -> PyResult<PyObject> {
        let result = py.detach(|| self.inner.run_code(code)).map_err(to_pyerr)?;
        monty_to_py(py, &result)
    }
}
```

Register it in the module:

```rust
#[pymodule]
fn _droplet(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Engine>()?;
    m.add_class::<Dataset>()?;
    m.add_class::<Session>()?;
    Ok(())
}
```

  - `verify:` `py.detach(|| self.inner.run_code(code))` — the closure returns `Result<MontyObject, DropletError>`, which must be `Send` to cross `detach`. `MontyObject` and `DropletError` should be `Send`; if not, run without `detach` (correctness first) and leave a `// TODO` to restore GIL release.

- [ ] **Step 3: Export `Session` from the package**

Replace the import/`__all__` lines in `crates/droplet-py/python/droplet/__init__.py`:

```python
from ._droplet import Dataset, Engine, Session

__all__ = ["Dataset", "Engine", "Session"]
```

- [ ] **Step 4: Write the wheel demo test**

Create `crates/droplet-py/python/tests/test_session.py`:

```python
"""run_code through the PyO3 firewall — V1a's "Done when", driven from Python.

Agent code runs in the Monty sandbox, calls the macro-generated `query` tool over a local Parquet,
and the real aggregates come back into Python as a plain list[dict] (invariant #6: capped, no Arrow).
"""

import pyarrow as pa
import pyarrow.parquet as pq

import droplet


def _write_sales(tmp_path):
    # amt is float64: an un-cast SUM over an int column would be a DuckDB HUGEINT the read-out
    # doesn't decode yet (see droplet-core/src/tools.rs); float64 crosses cleanly.
    table = pa.table(
        {
            "region": ["EU", "EU", "US"],
            "amt": pa.array([100.0, 50.0, 200.0], pa.float64()),
        }
    )
    path = tmp_path / "sales.parquet"
    pq.write_table(table, path)
    return str(path)


def test_run_code_returns_aggregates(tmp_path):
    path = _write_sales(tmp_path)
    session = droplet.Session("run-py-v1a")
    code = (
        f"rows = query({path!r}, 'SELECT region, SUM(amt) AS t FROM data GROUP BY region')\n"
        "print(rows)\n"
        "rows"
    )
    rows = session.run_code(code)
    assert {r["region"]: r["t"] for r in rows} == {"EU": 150.0, "US": 200.0}


def test_run_code_unknown_tool_raises():
    session = droplet.Session("run-py-v1a-unknown")
    import pytest

    with pytest.raises(RuntimeError):
        session.run_code("not_a_real_tool(1)")
```

- [ ] **Step 5: Build the wheel and run the Python tests**

Run:
```bash
maturin develop -m crates/droplet-py/Cargo.toml
python -m pytest crates/droplet-py/python/tests/test_session.py -v
```
Expected: PASS.

  - `verify:` **inventory survives into the cdylib.** `inventory::iter::<Tool>()` must see `query` inside the wheel. Because `droplet-py` references `droplet_core::session::Session`, `droplet-core` is linked and its inventory submissions are normally retained. If `test_run_code_unknown_tool_raises` passes but `test_run_code_returns_aggregates` reports an *unknown tool* for `query`, the linker dropped the registration: add `pub fn link() {}` to `crates/droplet-core/src/tools.rs` and call `droplet_core::tools::link()` once in `Session::new` (forces the object to be retained). Re-run.

- [ ] **Step 6: Commit**

```bash
git add crates/droplet-py/Cargo.toml crates/droplet-py/src/lib.rs \
        crates/droplet-py/python/droplet/__init__.py \
        crates/droplet-py/python/tests/test_session.py Cargo.lock
git commit -m "V1a(7): droplet-py Session.run_code — code-mode demo from Python

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 8: Milestone verification gate

No new code — run the full quality bar end to end and confirm the V1a "Done when" holds from both surfaces.

**Files:** none (verification only).

- [ ] **Step 1: Format + lint the whole workspace**

Run:
```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```
Expected: both clean. (If `fmt --check` reports diffs, run `cargo fmt --all` and re-commit.)

- [ ] **Step 2: Full Rust test suite**

Run: `cargo test --workspace`
Expected: all green — including the pre-existing M0/M1 tests, the new `convert`/`tool`/`tools`/`session` tests, and the first-working-Droplet demo.

- [ ] **Step 3: Confirm the arrow graph is still single-major**

Run: `cargo tree -i arrow`
Expected: exactly one arrow major (the one `duckdb` pins). No top-level `arrow` was added.

- [ ] **Step 4: Rebuild the wheel and run the Python suite**

Run:
```bash
maturin develop -m crates/droplet-py/Cargo.toml
python -m pytest crates/droplet-py/python -v
```
Expected: the existing `test_engine.py` plus the new `test_session.py` all pass.

- [ ] **Step 5: Eyeball the "Done when" literally**

Run:
```bash
python - <<'PY'
import pyarrow as pa, pyarrow.parquet as pq, tempfile, os, droplet
d = tempfile.mkdtemp(); p = os.path.join(d, "sales.parquet")
pq.write_table(pa.table({"region": ["EU","EU","US"], "amt": pa.array([100.0,50.0,200.0], pa.float64())}), p)
s = droplet.Session("demo")
print(s.run_code(f"rows = query({p!r}, 'SELECT region, SUM(amt) AS t FROM data GROUP BY region'); print(rows); rows"))
PY
```
Expected: prints the aggregates twice (the agent's `print(rows)` and the returned value), e.g. `[{'region': 'EU', 't': 150.0}, {'region': 'US', 't': 200.0}]` — **V1a's "Done when" satisfied.**

- [ ] **Step 6: Final milestone commit (if any fmt/lock changes remain)**

```bash
git add -A
git commit -m "V1a: walking skeleton complete — code-mode query over a local file

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review

**Spec coverage (against `docs/superpowers/specs/2026-06-17-roadmap-replan-design.md` §4 "V1" and the V1a split):**

- "agent Python runs in the Monty sandbox" → Task 6 (`run_code` loop over `MontyRepl`). ✓
- "calls a typed, **macro-generated** tool" → Tasks 1+4+5 (`#[droplet_tool]` → `query`). ✓
- "analyzes a local Parquet, gets a real answer back" → Tasks 5+6 (DuckDB over `read_parquet`, capped rows return to the sandbox). ✓
- "Single process, local file, no cloud/cache/snapshot" → scope explicitly excludes all of these. ✓
- "the `#[droplet_tool]` macro is built **for real** … no hand-wired interim surface" (invariant #4) → Tasks 3+4 (inventory auto-collection; no hand `match`, no hand `.pyi`). ✓
- "Done when: `run_code(\"rows = query('sales.parquet','SELECT region, SUM(amt) AS t FROM data GROUP BY region'); print(rows)\")` returns the real aggregates" → Task 6 (pure Rust) + Task 7 (Python). ✓
- Invariants delivered: #3 (engine hardening retained), #4 (macro real), #6 (capped `Rows`, no handles leak), #8 (no pyo3 in core), #9 (`Python::detach`), #10 (`DropletError`). ✓
- "V1a … macro for one tool" then "V1b (the full local analyze surface)" → this plan is V1a only; V1b is the next plan. ✓ (Noted in scope.)

**Deferred-but-noted (correctly NOT in this plan):** type-check-before-run (V2), `Dataset` handles + full analyze surface (V1b), catalog/`load`/connectors/cache/snapshot (V3+).

**Placeholder scan:** every code step contains real code; commands have expected output; no "TBD"/"add error handling"/"similar to Task N". `verify:` markers flag genuine pre-1.0 API facts to confirm against the locked versions, each with a concrete fallback — these are checks, not placeholders.

**Type consistency:** `DispatchFn` signature (`&mut DuckEngine, &[MontyObject], &[(MontyObject, MontyObject)]) -> Result<MontyObject, DropletError>`) is identical in Task 3 (definition), Task 4 (generated thunk), and Task 6 (call site). `Rows`, `IntoMonty`, `FromMonty`, `Tool{name,stub,dispatch}`, `query(eng, path, sql)`, and `Session::run_code(&mut self, &str) -> Result<MontyObject, DropletError>` are named identically across the tasks that define and consume them.

---

## Execution Handoff

(Filled in after you choose an execution mode — see the next message.)
