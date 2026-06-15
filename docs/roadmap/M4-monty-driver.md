# M4 — Monty driver (SKETCH)

**Milestone goal:** drive Monty (the embedded Python sandbox) directly from `droplet-core` so an agent's
Python runs **one `run_code` step at a time**, is **type-checked against the typed tool stubs before it
executes**, and reaches the host only through a **flat, typed tool surface** (`run_sql`, `search_fields`,
`describe_schema` / `list_tables`, `export`) that returns **capped** results back into the sandbox.

**Done when (from the spec, build-order step 5):** an agent `run_code` step runs Python in Monty against
the session, with a wrong column name / wrong arg **caught by the type checker before execution** (so the
model self-corrects), and the tools dispatched on the host return capped rows into the sandbox.

**Prerequisite:** finish [`M3-coordination.md`](./M3-coordination.md) (build-order step 4). You need
`Session`, the handle registry, `DropletError`, the four store traits, the DuckDB `run_sql` path from
[`M1-duckdb.md`](./M1-duckdb.md), and the artifact/cache wiring from
[`M2-artifact-cache.md`](./M2-artifact-cache.md) before the tools have anything real to dispatch to.

**Estimate:** ~11 chunks.

> This is a **SKETCH** file: chunk-level checkboxes with concept notes and invariant callouts, *not* the
> tiny per-line steps of M0/M1. Get the shape right first; expand into tiny steps when you reach this
> milestone.

---

## How to read this file

- Every `- [ ]` is a chunk-level task (a sitting, not a 10-minute step). When you reach this milestone you
  expand each into tiny M0/M1-sized steps.
- `🆕 Concept:` explains a new Rust/Monty idea the **first** time it shows up, with a Rust Book chapter name
  (run `rustup doc --book` to open the book offline) when one applies.
- `✅ Done when:` is an observable check — usually a command's output or a passing test.
- `⚠️ Invariant #N:` quotes a load-bearing rule from `PRODUCT.md` (repo root) in plain words. Never break
  these.
- `🔗 Maps to:` ties an exercise to the real Droplet concept it unlocks.
- `verify:` flags a fact the research couldn't fully pin on the locked Monty tag — read the Monty source at
  `v0.0.18` before relying on it, don't guess. This whole area is **pre-1.0 and churns fast**, so expect
  many of these.

---

## Why this milestone is its own thing (read first, 5 min)

So far the engines (DuckDB from M1, the stores from M0–M3) exist, but nothing *runs the agent's Python*. M4
is the driver that turns "the agent emitted some code" into "that code executed safely." Three new ideas:

1. **REPL, one step per call.** Monty's `MontyRepl` is a *persistent* session: you feed it successive code
   chunks and it keeps variables alive between them. That is exactly Droplet's per-`run_code`-step model —
   one `MontyRepl` per `Session`, one `feed_*` per step.
2. **Suspend / resume is how tools work.** When the sandboxed Python calls one of your tool functions, the
   interpreter **pauses**, hands you the function name + args, and waits. *You* (the Rust host) run the real
   DuckDB / S3 work, then `resume(...)` with the return value. The sandbox never touches an engine directly
   — it only calls a flat function and gets a small, capped result back.
3. **Type-check happens BEFORE the run.** You call Monty's bundled type checker (`ty`, exposed through the
   `monty-type-checking` crate) against the generated stubs *first*. On a type error you **return** that
   error so the agent retries — you do **not** execute. This is the "wrong column caught before execution"
   promise.

> ⚠️ Invariant #7: "Respect Monty's limits: subset of Python, no third-party imports, no classes, limited
> stdlib; tool surface is **flat typed functions**; **type-check before execution**." Every chunk here serves
> that one rule.

> ⚠️ Invariant #1: all of this lives in `droplet-core` — **no `pyo3`**. The driver must be exercisable from a
> pure-Rust `#[test]`, with no Python interpreter (no CPython, no wheel) in the loop. Monty *is* the Python
> here; the GIL-releasing PyO3 layer is `droplet-py`'s job in a later milestone, never `droplet-core`'s.

> 🆕 **Concept: two different "monty"s.** The Rust crate `monty` (embedded in `droplet-core`) is what you use.
> `pydantic-monty` on PyPI (built from the `monty-python` crate) is a *separate* CPython wrapper that Droplet
> does **not** use. Don't confuse them. (Concept is project-side; no Rust Book chapter.)

The whole area is **pre-1.0 and churns fast** (this roadmap pins git tag `v0.0.18`). Read the source at the
pinned tag rather than trusting any signature from memory.

---

### Chunk 1 — Add Monty as a *git* dependency (not crates.io)

- [ ] Add both crates to `[workspace.dependencies]` in the root `Cargo.toml`, then opt `droplet-core` in
  with `monty.workspace = true` / `monty-type-checking.workspace = true`:
  ```toml
  monty               = { git = "https://github.com/pydantic/monty", tag = "v0.0.18" }
  monty-type-checking = { git = "https://github.com/pydantic/monty", tag = "v0.0.18" }
  ```
  - ⚠️ verify: crates.io has only a `monty` **0.0.0 placeholder** ("Coming soon", published 2025-12-31) —
    it is unrelated to this crate. `cargo add monty` grabs the trap. The real code lives **only** on GitHub —
    depend on it by **git + tag**, never the crates.io version.
  - 🆕 Concept: a **git dependency** points Cargo at a repo + a pinned `tag` / `rev` instead of a crates.io
    version. (Rust Book: *More About Cargo and Crates.io*, ch. 14.)
  - ⚠️ `monty-type-checking` drags in Astral's `ty` / `ruff` crates (pinned to a ruff git commit
    `6aaa91ac2b269df1414954ccd5134f0e6f5c6d30`) plus `salsa 0.26.1` — heavy, unpublished-API deps. **Expect a
    long first build.** `cargo update` could break the tree; keep `Cargo.lock` committed and avoid bumping
    these casually.
  - ✅ Done when: `cargo build -p droplet-core` is green (slow the first time) and `grep monty Cargo.lock`
    shows the `v0.0.18` git source, not a crates.io version.

### Chunk 2 — Smoke-test the embed (prove the API compiles)

- [ ] Write one `#[test]` that constructs a REPL and runs a trivial expression:
  ```rust
  use monty::{MontyRepl, NoLimitTracker, MontyObject, PrintWriter};
  let mut r = MontyRepl::new("t.py", NoLimitTracker);
  let v = r.feed_run("1 + 2", vec![], PrintWriter::Stdout)?;
  assert_eq!(v, MontyObject::Int(3));
  ```
  - 🆕 Concept: `MontyObject` is Monty's runtime value enum (`Int`, `Str`, `None`, …) — the Rust-side
    representation of a Python value crossing the boundary. (Concept is Monty-specific; no Rust Book chapter.)
  - ⚠️ verify: the exact `MontyRepl::new` signature at the tag — source shows
    `pub fn new(script_name: &str, resource_tracker: T) -> Self`. The **tracker-as-second-arg** ordering and
    the `feed_run(&mut self, &str, Vec<(String, MontyObject)>, PrintWriter)` shape both churn pre-1.0; read
    `crates/monty/src/repl.rs` at `v0.0.18` before relying on them.
  - ✅ Done when: `cargo test -p droplet-core` runs this test green — proving the git dep + API compile.

### Chunk 3 — Prove the REPL keeps state across steps (the per-`run_code`-step model)

- [ ] In one test, feed three chunks on the **same** `MontyRepl` and assert state carries over:
  `r.feed_run("x = 10", …)`, then `r.feed_run("y = 20", …)`, then assert `r.feed_run("x + y", …)` is `30`.
  - 🆕 Concept: `MontyRepl` (persistent session) vs `MontyRun` (one compiled program). Droplet needs the
    **REPL**: variables defined in an earlier `run_code` step must still resolve in the next step. (Concept is
    Monty-specific; no Rust Book chapter.)
  - 🔗 Maps to: this is `Session.run_code(code)` — each call is one `feed_*` on the session's REPL, and the
    session *is* the living interpreter state between steps.
  - ⚠️ Invariant #9 (per-run isolation): one run = one `Session` = **one `MontyRepl`**. State is per-session,
    never shared across runs. Two concurrent runs must hold two separate REPLs, not one shared interpreter.
  - ✅ Done when: the three-step test passes, confirming cross-step persistence within one REPL.

### Chunk 4 — Build the suspend / resume loop with a *fake* tool

- [ ] Switch from `feed_run` to `feed_start`, feed code that calls an undefined external function (e.g.
  `run_sql('select 1')`), and write the `loop { match progress { … } }` over `ReplProgress`. In the
  `ReplProgress::FunctionCall` arm, match `call.function_name`, return a hardcoded `MontyObject` via
  `.into()`, and `call.resume(ret, PrintWriter::Stdout)?`. Loop until `ReplProgress::Complete`.
  ```rust
  let mut progress = repl.feed_start(code, inputs, PrintWriter::Stdout)?;
  let value = loop {
      match progress {
          ReplProgress::Complete { repl: r, value } => { repl = r; break value; }
          ReplProgress::FunctionCall(call) => {
              let ret: ExtFunctionResult = dispatch_tool(&call)?; // host runs the real work
              progress = call.resume(ret, PrintWriter::Stdout)?;
          }
          ReplProgress::OsCall(c)         => { progress = c.resume(MontyObject::None.into(), PrintWriter::Stdout)?; }
          ReplProgress::NameLookup(l)     => { progress = l.resume(monty::NameLookupResult::Undefined, PrintWriter::Stdout)?; }
          ReplProgress::ResolveFutures(f) => { /* see Chunk 9 */ unimplemented!() }
      }
  };
  ```
  - 🆕 Concept: **suspend / resume** = the interpreter stops when sandboxed Python calls an external function,
    hands you the name + args, and you `resume(...)` with the result. This is the *only* way the sandbox
    reaches a tool. (Concept is Monty-specific; no Rust Book chapter.)
  - 🆕 Concept: a `match` over an enum like `ReplProgress` must cover **every** variant — the Rust compiler
    refuses to build a non-exhaustive `match`. That is what forces you to handle each suspension kind.
    (Rust Book: *Enums and Pattern Matching*, ch. 6.)
  - ⚠️ Invariant #7: external functions in Rust are **not registered up front and are not classes/modules** —
    the REPL simply **suspends** at each call and you dispatch on `call.function_name`. The Python
    `external_functions=` dict is a `monty-python` convenience you do not use here.
- [ ] Cover **every** arm (`OsCall` / `NameLookup` / `ResolveFutures`) with a safe default so nothing panics,
  even though Droplet's tools only need `FunctionCall`.
  - ⚠️ verify: `feed_start` appears to **consume `self`** (takes `self`, returns the repl back inside
    `ReplProgress::Complete { repl, value }`), while `feed_run` takes `&mut self`. This ownership model shapes
    how `Session` holds the REPL — confirm at the tag before writing the loop, and decide whether `Session`
    stores the REPL in an `Option<MontyRepl<T>>` you `take()` and put back.
  - ✅ Done when: a Monty script that calls `run_sql('select 1')` round-trips through `feed_start` → a
    `FunctionCall` arm → `resume` → `Complete`, returning the hardcoded value, with no arm panicking.

### Chunk 5 — Define the FLAT tool surface and the dispatch table

- [ ] Write a single `dispatch_tool(call) -> Result<ExtFunctionResult, DropletError>` that maps each **flat**
  function name to its host tool. Branch on `call.function_name`:
  | Monty name (flat)   | Host tool                                   | Returns to sandbox            |
  |---------------------|---------------------------------------------|-------------------------------|
  | `run_sql`           | DuckDB query (M1) — capped Arrow → rows      | small, capped result rows     |
  | `search_fields`     | read-only Surreal field search (M6)         | typed `FieldRef` list (small) |
  | `describe_schema`   | catalog/schema discovery                    | schema description            |
  | `list_tables`       | catalog/table discovery                     | table name list               |
  | `export`            | write Parquet/CSV to S3 (artifact store)    | a destination ref (handle)    |
  - 🆕 Concept: a **dispatch table** here is just a `match call.function_name { "run_sql" => …, … }` — Monty
    has fixed the API surface by *name*, so you branch on the string. (Concept is the M4 suspend/resume
    pattern; no Rust Book chapter.)
  - ⚠️ Invariant #7: the tool surface is **flat typed function names** — Monty has no class/module
    namespacing. So it is `run_sql(...)`, a bare name; never `db.run_sql(...)` or `tools.run_sql(...)`.
  - ⚠️ verify: `ExtFunctionResult` construction — examples use `MontyObject::Str(..).into()` /
    `MontyObject::None.into()`. Confirm the `From` impls **and** whether a tool can return an **error** (raise
    a sandbox exception) — Droplet needs e.g. a SQL error to surface as a catchable exception the model can
    handle. Read `crates/monty/src/` at the tag.
- [ ] Wire the `run_sql` arm to the **real** M1 DuckDB path (not the hardcoded stub from Chunk 4) and assert
  it returns real capped rows.
  - ⚠️ Invariant #4 (boundary discipline): only the **result-returning** tools (`run_sql`, `search_fields`)
    move rows into the sandbox, and they move **capped** rows. `describe_schema` / `list_tables` / `export`
    move metadata or **handles** — heavy data stays in DuckDB / the artifact store behind handles. The sandbox
    sees only small, capped values + opaque handles, never a bulk recordset.
  - ⚠️ Invariant #6 (DuckDB sync → spawn_blocking): the `run_sql` arm dispatches into M1's DuckDB path, which
    runs the query inside `spawn_blocking` (and, later in `droplet-py`, releases the GIL). The dispatch loop
    must `.await` that blocking call's join handle, not run DuckDB on the loop thread directly.
  - ✅ Done when: each flat name routes to its host tool in `dispatch_tool`, and a script calling `run_sql`
    end-to-end returns real capped rows (not a hardcoded stub) from the M1 DuckDB path.

### Chunk 6 — Wire type-check-BEFORE-run (the retry seam)

- [ ] Before `feed_start`, build `SourceFile`s for (a) the agent's code and (b) the generated stub source,
  call `monty_type_checking::type_check(&src, Some(&stubs))`, and branch on its three outcomes:
  ```rust
  use monty_type_checking::{type_check, SourceFile};
  let src   = SourceFile::new("session.py", agent_code);
  let stubs = SourceFile::new("stubs.pyi", generated_stub_source); // run_sql/search_fields/… sigs
  match type_check(&src, Some(&stubs)) {
      Ok(None)        => { /* clean — proceed to feed_start */ }
      Ok(Some(diags)) => return Err(DropletError::type_check(diags)), // -> model retries; DO NOT run
      Err(internal)   => return Err(DropletError::type_check_internal(internal)),
  }
  ```
  - ⚠️ Invariant #7: **type-check before execution.** On `Ok(Some(diags))` you **return** the diagnostics so
    the caller maps them to a model retry — you must **not** call `feed_start`. The whole point is that a wrong
    column / wrong arg fails *before* any DuckDB work happens.
  - 🆕 Concept: type-checking is a **separate, explicit call** (`monty_type_checking::type_check`) you make
    *before* feeding code to the REPL — it is **not** automatic in the Rust API. Monty bundles Astral's `ty`
    checker internally; you do **not** add a separate `ty` dependency or shell out to `ty check`. The stubs are
    passed as a second `SourceFile` the checker treats as the contract. (Concept is Monty/`ty`-specific; no
    Rust Book chapter.)
  - ⚠️ verify: the entry point — observed
    `pub fn type_check(python_source: &SourceFile, stubs_file: Option<&SourceFile>) -> Result<Option<TypeCheckingDiagnostics>, String>`.
    Note the two distinct failure modes: `Ok(Some(diags))` = real **type errors** (→ retry); `Err(String)` =
    an **internal** checker error (→ surface, don't loop). Confirm the `SourceFile` constructor and how to turn
    `TypeCheckingDiagnostics` into per-error messages to feed back to the model. `ty` itself is pre-1.0 BETA
    (0.0.x), so treat its diagnostic shape as unstable and re-confirm at the pinned tag.
  - ✅ Done when: with stub `def run_sql(sql: str) -> Table: ...`, feeding `run_sql(123)` returns
    `Ok(Some(diags))` (and you never reach `feed_start`); feeding `run_sql('select 1')` returns `Ok(None)`
    and proceeds. This is the spec's "wrong column caught before execution" in miniature.

> 📝 The **stub generation** itself (Pydantic models → the `.pyi` tool-surface text) is `M5` work
> ([`M5-pydantic-schema.md`](./M5-pydantic-schema.md)). In M4, hand-write a stub string to prove the
> type-check loop; M5 wires the real generated stubs into this same seam.

### Chunk 7 — Decide what gets type-checked each step (cumulative source)

- [ ] Decide whether each step type-checks **only the new chunk** or the **cumulative** session source, and
  encode the choice in the driver. Names defined in an earlier `run_code` step must still resolve when the
  type checker reads a later step.
  - ⚠️ verify: whether `type_check` should receive the **concatenated/cumulative** source (or whether stub
    continuity alone suffices) so prior-step names resolve. This is **unspecified** in the docs — build the
    driver to test both forms and keep whichever the checker accepts. Do **not** assume single-chunk checking
    is enough.
  - 🔗 Maps to: this is what makes multi-step agent sessions type-safe end-to-end, not just per-isolated-chunk.
  - ✅ Done when: a two-step session where step 1 defines a name and step 2 uses it **passes** the type check
    (proving prior-step names resolve), and a deliberately-undefined name in step 2 is **caught**.

### Chunk 8 — Cap the rows that cross back into the sandbox

- [ ] Make every result-returning tool (`run_sql`, `search_fields`) convert its host result into a **small,
  capped** `MontyObject` before `resume`. Reuse M1's row-cap constant for SQL results so the sandbox never
  receives a bulk recordset; keep `search_fields` to a small `K`.
  - ⚠️ Invariant #4 (boundary discipline): "only result-returning tools move capped rows into the sandbox;
    everything else moves handles." The cap is what keeps the REPL (and therefore the snapshot) small —
    handles + capped results, not data.
  - 🔗 Maps to: this cap is the same boundary discipline that keeps M7 snapshots small by construction.
  - ✅ Done when: a test runs `run_sql` over a source with more rows than the cap and asserts the sandbox
    receives exactly the capped count, never the full set.

### Chunk 9 — Handle the remaining `ReplProgress` arms safely

- [ ] Fill in `OsCall`, `NameLookup`, and `ResolveFutures` with correct (not just panic-free) behavior.
  Droplet's tools are sync-from-the-sandbox's view, but Monty supports `asyncio`, so agent code *can* suspend
  with `ResolveFutures`.
  ```rust
  ReplProgress::ResolveFutures(f) => {
      let r: Vec<(u32, ExtFunctionResult)> = f.pending_call_ids().iter()
          .map(|&id| (id, /* resolved tool result for this pending call */ ))
          .collect();
      progress = f.resume(r, PrintWriter::Stdout)?;
  }
  ```
  - 🆕 Concept: `ResolveFutures` is Monty's async/await suspension — it hands you a batch of *pending* call
    ids to resolve at once, rather than one `FunctionCall` at a time. (Concept is Monty-specific; no Rust Book
    chapter.)
  - ⚠️ verify: the semantics of `pending_call_ids()` and the exact **shape of the resume vector**
    (`Vec<(u32, ExtFunctionResult)>`) at the tag — async handling is the easiest arm to get subtly wrong.
  - ⚠️ Invariant #7: even via `asyncio`, the **only** things the sandbox can reach are the flat tool names —
    `ResolveFutures` resolves the *same* tool calls, just batched. No new surface leaks in.
  - ✅ Done when: an agent script using `async def` + `await run_sql(...)` completes through the
    `ResolveFutures` arm without panicking, returning capped rows.

### Chunk 10 — Fold Monty + type-check errors into `DropletError`

- [ ] Add `DropletError` variants (via `thiserror`) for: a Monty run/exception error, a `feed_start` start
  error (`Box<ReplStartError<T>>`), type-checking **diagnostics** (the retry signal), and a type-check
  **internal** error. Map each at the boundary so every `?` converts cleanly.
  - ⚠️ Invariant #10: "One error type at the boundary — `thiserror` in libraries, `anyhow` at binaries; all
    engine errors fold into `DropletError`." So a sandbox exception, a checker diagnostic, and a Monty start
    failure all surface as one `DropletError`, never raw `MontyException` / `TypeCheckingDiagnostics`.
  - 🆕 Concept: `thiserror`'s `#[from]` generates the `From<…>` impl that lets `?` auto-convert an engine
    error into your `DropletError` at the boundary. (Rust Book: *Error Handling*, ch. 9.)
  - ⚠️ Keep the **type-check-error** variant distinct from a generic run error: the caller needs to tell "the
    model should **retry** with the diagnostics" apart from "something genuinely broke."
  - ✅ Done when: a deliberately bad SQL string surfaces as a `DropletError`, a type error surfaces as the
    distinct **retry** variant, and neither leaks a raw Monty type past the host.

### Chunk 11 — Pick the resource tracker and write the milestone test

- [ ] Start with `NoLimitTracker` for dev; leave a clear seam to swap to
  `LimitedTracker::new(ResourceLimits { … })`.
  - 🆕 Concept: the resource tracker is a **generic type parameter** on `MontyRepl<T>` chosen at construction —
    `NoLimitTracker` (no limits, dev) vs `LimitedTracker` (enforces `ResourceLimits`). (Rust Book: *Generic
    Types, Traits, and Lifetimes*, ch. 10.)
  - ⚠️ verify: the exact **`ResourceLimits` field names/types** (e.g. `Duration` vs milliseconds, plus the
    `DEFAULT_MAX_RECURSION_DEPTH` const). Read `crates/monty/src/resource.rs` at `v0.0.18` before constructing
    it — do **not** guess.
- [ ] Write the milestone `#[test]`: type-check a step that calls a **wrong** column/arg and assert it is
  **rejected before any DuckDB call**; then type-check + run a *correct* step end-to-end through the suspend/
  resume loop (`search_fields` to find a column, `run_sql` to aggregate) and assert the capped rows come back.
  - ⚠️ Invariant #4: assert the result is **capped** — the rows crossing back into the sandbox are bounded,
    never the full recordset.
  - ⚠️ Invariant #1: this whole test runs in pure-Rust `droplet-core` — **no `pyo3`**, no CPython.
  - ✅ Done when: the wrong-column step is caught pre-execution (DuckDB is never touched) **and** the correct
    step runs through Monty returning capped rows — the spec's "wrong column caught before execution + tools
    dispatched returning capped rows."

---

## Notes carried forward (don't act yet)

- **Stubs are M5.** This file hand-writes a stub string to exercise the type-check seam. The real
  Pydantic-models → `.pyi` tool-surface generation lands in [`M5-pydantic-schema.md`](./M5-pydantic-schema.md)
  and feeds the *same* `type_check(&src, Some(&stubs))` call — don't build stub generation here. There is no
  official "pydantic → .pyi" generator; M5 writes it.
- **`search_fields` is M6.** In M4 the `search_fields` arm can return a stub `FieldRef`; the read-only
  SurrealDB vector search behind it is [`M6-field-search.md`](./M6-field-search.md). ⚠️ Invariant #5:
  SurrealDB is **read-only and schema-derived** — the field index is rebuilt from the schema, never written to
  from a tool.
- **Snapshot is M7, but design for it now.** `MontyRepl::dump()` → `Vec<u8>` (postcard) captures the whole
  interpreter state and is the snapshot's REPL bytes ([`M7-snapshot-store.md`](./M7-snapshot-store.md)). **Do
  not** serialize DuckDB or Surreal — that matches Invariant #3 (REPL bytes + manifest only; rebuild engines
  on resume) and Invariant #8 (immutable state is content-addressed in the **shared** object store, not kept
  local to one pod). Three design constraints to honor *now* so M7 is clean: (1) snapshot only at a clean seam
  (the `FunctionCall` boundary, where the VM is paused waiting on you); (2) the postcard format is **tied to
  the Monty tag** — pin one Monty tag fleet-wide and record it in the manifest so cross-version loads fail
  loudly rather than mis-decode; (3) the snapshot blob is content-addressed and `zstd`-compressed into the
  shared `SnapshotStore` (S3), so any pod can resume it.
- **Document the Python subset you verified** directly in the tool-surface design, so generated stubs/tools
  stay flat. At `v0.0.18` Monty **cannot**: define classes, use `match`, import third-party libs, or use most
  stdlib; supported stdlib includes `sys`, `os`, `typing`, `asyncio`, `re`, `datetime`, `json` (+ `open()` /
  file I/O added in `v0.0.18`). ⚠️ verify: re-read the README "limitations" at the **exact tag** — the
  supported-module list changes release to release.
- **Pin the dep tree.** Keep `Cargo.lock` committed; avoid `cargo update` for `monty` / `ruff` / `ty` /
  `salsa`. If you must bump `monty`, treat it as a **snapshot-format change** (bump Droplet's snapshot-format
  version, refuse cross-version loads). This is the single biggest time-sink in this area.

---

> 📌 When you reach this milestone, expand each chunk into tiny steps the way M0/M1 are written.
