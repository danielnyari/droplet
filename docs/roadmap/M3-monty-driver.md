# M3 — Monty driver: the agent loop (DEEP)

**Milestone goal:** drive Monty (the embedded Python sandbox) directly from `droplet-core` so an
agent's Python runs **one `run_code` step at a time**, is **type-checked against the typed tool stubs
before it executes**, and reaches the host only through a **flat, typed tool surface** — `load` (the
M2 boundary) plus the local analyze prims and filter helpers from M1/M2 — that returns **capped**
results back into the sandbox. The climax is the **first working Droplet**: agent Python in Monty
calls `load`, runs a couple of analyze prims, and gets capped rows back — single process, local
Parquet source, no cloud.

**Done when (from the spec, build-order step 4):** an agent `run_code` step runs Python in Monty
against the session; a wrong column name / wrong arg is **caught by the type checker before
execution** (so the model self-corrects); and the tools dispatched on the host — `load` and the
analyze prims — return capped rows into the sandbox. End to end: `load(...)` → `group_agg(...)` →
`to_rows(...)` returns a small list of dicts to the agent, with **zero further contact with the
source** after the load.

**Prerequisite:** finish [`M2-load-boundary.md`](./M2-load-boundary.md) (build-order step 3 boundary).
You need, from earlier milestones:

- `Session`, the generic handle registry, `DropletError` (thiserror), and the four store traits with
  in-memory/local dev impls — all from [`M0-skeleton.md`](./M0-skeleton.md). **M0 already embedded
  Monty** (the `MontyRepl` smoke test, the cross-step-state test, the suspend/resume loop with a fake
  `host_get`, the host-function-over-`Session`-state seam, and the `Monty(#[from] monty::MontyException)`
  variant). M3 builds the *real driver* on top of that seam — it does **not** re-teach the embed.
- The local analyze engine from [`M1-analyze-engine.md`](./M1-analyze-engine.md): an ephemeral DuckDB
  over a local Parquet file behind a `Dataset` handle, the prims (`filter_rows`, `group_agg`,
  `to_rows`, `scalar`, `local_sql`), `spawn_blocking`, and the `MAX_RESULT_ROWS` cap.
- The `load` boundary from [`M2-load-boundary.md`](./M2-load-boundary.md): a minimal `Catalog`, the
  `load(name, columns, where, as_of) -> Dataset` call that drives the dev local-Parquet connector and
  materializes locally, and the typed filter helpers (`eq`, `gt`, `between`, …).

Without those, the tools the driver dispatches to have nothing real to do.

**Estimate:** ~9 chunks (A–I), each a focused sitting. Do them in order — later chunks build on the
`run_code` loop and dispatch table you write earlier.

The spec lives at `PRODUCT.md` (repo root). Reference it that way, never `docs/PRODUCT.md`.

---

## How to read this file

- Every `- [ ]` is a tiny task (~10–30 min for a Rust newbie, one new idea). Tick it the moment its
  `✅ Done when` check passes — that's your save-game.
- `🆕 Concept:` explains a new Rust/Monty idea the **first** time it shows up, with a Rust Book
  chapter *name* (run `rustup doc --book` to open the book offline) when one applies.
- `✅ Done when:` is an observable check — usually a command's output or a passing test.
- `⚠️ Invariant #N:` quotes a load-bearing rule from `PRODUCT.md` §15 (the README's Golden Rules,
  numbered 1–10) in plain words. Never break these.
- `🔗 Maps to:` ties a tiny exercise to the real Droplet concept it unlocks.
- `verify:` flags a fact the research couldn't fully pin on the locked Monty tag (`v0.0.18`) — read
  the Monty source before relying on it, don't guess. This whole area is **pre-1.0 and churns fast**,
  so expect many of these.
- Code snippets are **anchors** (a few lines to orient you). You write the real implementation.

**The build/learn loop for M3:** add one capability → write a tiny `#[test]` that exercises it →
watch it fail → make it pass → tick the box → `git commit` at the end of the chunk.

---

## Why this milestone is its own thing (read first, 10 min)

M1 gave you an analyze engine (DuckDB over local Parquet, prims, caps). M2 gave you the `load`
boundary (catalog → dev connector → local Parquet → `Dataset`). But **nothing runs the agent's
Python yet.** M3 is the **driver** that turns "the agent emitted some code" into "that code executed
safely, calling our tools." Three ideas carry the whole milestone:

1. **`run_code`, one step per call.** Monty's `MontyRepl` is a *persistent* session (you met it in
   M0): you feed it successive code chunks and it keeps variables alive between them. That is exactly
   Droplet's per-`run_code`-step model — **one `MontyRepl` per `Session`, one `feed_*` per step.**
   `Session::run_code(code)` is the method you build here.

2. **Suspend / resume is how tools work.** When the sandboxed Python calls one of your tool functions
   (`load(...)`, `group_agg(...)`, …), the interpreter **pauses**, hands you the function name + args,
   and waits. *You* (the Rust host) run the real `load` / DuckDB work, then `resume(...)` with the
   return value. The sandbox never touches an engine directly — it only calls a flat function and gets
   a small, capped result back. You wired the *skeleton* of this in M0 with a fake `host_get`; M3 wires
   the *real* tools into it. (PRODUCT.md §8 "Execution".)

3. **Type-check happens BEFORE the run.** You call Monty's bundled type checker (`ty`, exposed through
   the `monty-type-checking` crate) against the generated stubs **first**. On a type error you
   **return** that error so the agent retries — you do **not** execute. This is the "wrong column /
   wrong arg caught before execution" promise (PRODUCT.md §4, §14).

> ⚠️ **Invariant #4 — and its one deliberate exception, which is the heart of M3.** The Golden Rule
> says: *"The tool surface is auto-generated. Fixed tools carry a `#[droplet_tool]` macro; the
> data-shaped types come from the catalog. No hand-maintained registry or stubs."* **M3 does the
> opposite on purpose:** it **hand-wires** the external-function table *and* hand-writes the Python
> `.pyi` type-stub bundle for `load` + the analyze prims. The README states this exception explicitly:
> *"`M3` wires a tiny surface **by hand** as a teaching scaffold, and `M4` replaces it with the macro
> to satisfy this rule."* So everything you build here that is "by hand" is a **temporary teaching
> scaffold** — you build it so you can *see* exactly what the `#[droplet_tool]` macro will later
> generate for you. [`M4-droplet-tool-macro.md`](./M4-droplet-tool-macro.md) deletes the hand-wiring.

> ⚠️ **Invariant #6 (boundary discipline).** *"Only `to_rows` / `scalar` / load-samples move actual
> rows into the sandbox, and always capped. The sandbox sees handles, not data."* Every tool you wire
> here either returns a small **handle** (a `Dataset` id) or **capped** rows — never a bulk recordset.
> This is what keeps snapshots small later (M8).

> ⚠️ **Invariant #8 (keep Python out of the core).** All of M3 lives in `droplet-core` — **no
> `pyo3`**. The driver must be exercisable from a pure-Rust `#[test]`, with **no CPython, no wheel** in
> the loop. Monty *is* the Python here; the GIL-releasing PyO3 layer is `droplet-py`'s job in a later
> milestone, never `droplet-core`'s.

> 🆕 **Concept: two different "monty"s.** The Rust crate `monty` (embedded in `droplet-core`) is what
> you use. `pydantic-monty` on PyPI (built from the `monty-python` crate) is a *separate* CPython
> wrapper that Droplet does **not** use. Don't confuse them; adding `monty-python` would pull `pyo3`
> into the core and break invariant #8. (Concept is project-side; no Rust Book chapter.)

The whole area is **pre-1.0 and churns fast** (this roadmap pins git tag `v0.0.18`, added back in M0).
Read the source at the pinned tag (`crates/monty/src/repl.rs`, `run_progress.rs`, `resource.rs`)
rather than trusting any signature from memory.

> **The shape of "first working Droplet" (so you know where you're heading).** By the end of Chunk I
> you'll have a single pure-Rust `#[tokio::test]` that does this, end to end:
>
> ```python
> # agent code, type-checked then run in Monty:
> usage = load("usage_daily", columns=["account_id", "active_minutes"], where=[], as_of="latest")
> agg   = group_agg(usage, by=["account_id"], metrics={"avg": ("active_minutes", "mean")})
> rows  = to_rows(agg)          # ← capped list[dict] crosses back into the sandbox
> ```
>
> `load` reads a local Parquet fixture through M2's dev connector; `group_agg` runs in M1's DuckDB;
> `to_rows` returns a small capped list. One process. No S3. No CPython. **That is Droplet.**

---

### Chunk A — One `MontyRepl` per `Session` (the per-`run_code`-step home)

In M0 you proved a free-standing `MontyRepl` keeps state across `feed_*` calls. Now you give the
`Session` its **own** REPL so each `run_code` step feeds the *same* interpreter — variables defined in
step 1 still resolve in step 2.

- [ ] In `crates/droplet-core/src/session.rs`, add a field on `Session` that owns the REPL. Because
  `feed_start` (Chunk C) **consumes** the REPL and hands it back inside `ReplProgress::Complete`, store
  it as an `Option` you can `take()` out and put back:
  ```rust
  use monty::{MontyRepl, NoLimitTracker};

  pub struct Session {
      // ... run_id, work_dir, registry from M0 ...
      repl: Option<MontyRepl<NoLimitTracker>>,
  }
  ```
  - 🆕 Concept: `Option<T>` is Rust's "maybe a value" enum (`Some(T)` / `None`) — there is no `null`.
    Storing the REPL as `Option<MontyRepl<_>>` lets you `take()` it (leaving `None` behind), use it in
    a loop that consumes it, and put the returned REPL back. (Rust Book: *Enums and Pattern Matching* —
    the `Option` enum.)
  - 🆕 Concept: `MontyRepl<T>` is **generic** over a resource tracker `T`. `NoLimitTracker` is the
    no-limits dev tracker (you pick the production one in Chunk H). The `<NoLimitTracker>` part of the
    type is how Rust knows which tracker this REPL uses. (Rust Book: *Generic Types, Traits, and
    Lifetimes*, ch. 10.)
  - ⚠️ Invariant #8 (keep Python out of the core): `monty` is fine in `droplet-core` — it *is* the
    sandbox. What's forbidden is `pyo3` / `monty-python`. Adding the `MontyRepl` field does **not**
    cross that line.

- [ ] Construct the REPL in `Session::new` so a fresh session has a live, empty interpreter:
  ```rust
  let repl = MontyRepl::new(&format!("{run_id}.py"), NoLimitTracker);
  // ... store Some(repl) in the struct ...
  ```
  - ⚠️ verify: the `MontyRepl::new(script_name, tracker)` argument order (the tracker is the **second**
    arg) at `v0.0.18` — read `crates/monty/src/repl.rs`. M0 already used this shape, but it's pre-1.0;
    re-confirm if M0 noted a mismatch.
  - ⚠️ Invariant #3 (analyze is per-session-local): **one run = one `Session` = one `MontyRepl`.** State
    is per-session, never shared across runs. Two concurrent runs hold two separate REPLs, never one
    shared interpreter. (This is PRODUCT.md §14 per-run isolation.)
  - ✅ Done when: a `#[test]` constructs a `Session` and the `repl` field is `Some(_)` (a live REPL).

- [ ] Add a tiny helper that proves cross-step state on the **session's** REPL (not a free-standing
  one): feed `x = 10` then `y = 20` then assert `x + y` is `30`, all through the session.
  - 🔗 Maps to: this is `Session.run_code(code)` in miniature — each call is one `feed_*` on the
    session's REPL, and the session *is* the living interpreter state between steps. You'll formalize
    `run_code` in Chunk C; this is just the plumbing proof.
  - ✅ Done when: the three-step test passes through the `Session`, confirming cross-step persistence
    within the session's own REPL. **Commit the chunk.**

---

### Chunk B — Name the flat tool surface (the scaffold's contract)

Before writing dispatch, pin down *exactly* which flat function names the sandbox may call and what
each returns. This is the surface the hand-wired table (Chunk D) and the hand-written stubs (Chunk F)
must agree on, name for name.

- [ ] Write down the flat tool surface as a table in a `// SCAFFOLD` comment near where the dispatch
  will live (`crates/droplet-core/src/driver.rs`, new file — declare `pub mod driver;` in `lib.rs`).
  This is the M3 subset of PRODUCT.md §10:

  | Monty name (flat) | Host work it triggers | Returns to sandbox |
  |-------------------|-----------------------|--------------------|
  | `load`            | M2: catalog → dev connector → local Parquet → `Dataset` | a `Dataset` **handle** (small) |
  | `filter_rows`     | M1: DuckDB filter over a handle → new handle | a `Dataset` **handle** |
  | `group_agg`       | M1: DuckDB group/aggregate over a handle → new handle | a `Dataset` **handle** |
  | `local_sql`       | M1: arbitrary DuckDB SQL over local handles → new handle | a `Dataset` **handle** |
  | `to_rows`         | M1: read a handle → capped list of row dicts | **capped** rows |
  | `scalar`          | M1: read a single value from a handle | one small value |
  | `eq` / `gt` / `between` / … | M2: build a typed filter value | a small filter object |

  - 🆕 Concept: a **dispatch table** here is just a `match call.function_name { "load" => …, … }` —
    Monty has no class/module namespacing, so you branch on the bare string name. (Concept is the M3
    suspend/resume pattern; no Rust Book chapter.)
  - ⚠️ Invariant #4 (scaffold exception): this table is **hand-maintained** — exactly the thing the
    Golden Rule forbids in steady state. You are doing it on purpose, once, so M4's `#[droplet_tool]`
    macro has a concrete target to generate. Put the word **SCAFFOLD** in the comment so future-you
    deletes it in M4.
  - ⚠️ Invariant #6 (boundary discipline): note in the table which tools return **handles** (`load`,
    `filter_rows`, `group_agg`, `local_sql`) versus **capped rows / one value** (`to_rows`, `scalar`).
    Only the last two move actual data into the sandbox, and capped. Everything else hands back a small
    `Dataset` id. This split is load-bearing — get it right here and the rest follows.
  - 🔗 Maps to: PRODUCT.md §7 "ANALYZE — local, unrestricted code mode" + §6 "LOAD". `load` is the
    *one* guarded door; the prims are the wide-open local surface.
  - ✅ Done when: the `// SCAFFOLD` table exists in `driver.rs` and you can recite, for each name,
    whether it returns a handle or capped data. **Commit.**

---

### Chunk C — The `run_code` loop (suspend / resume over the real seam)

This is the spine of the milestone. You met `feed_start` + the `ReplProgress` `match` loop in M0 with
a fake `host_get`. Here you wrap it into `Session::run_code`, threading the session so each suspension
can do **real** host work.

- [ ] In `driver.rs`, write `run_code(&mut self, code: &str) -> Result<MontyObject, DropletError>` on
  `Session`. `take()` the REPL out of the `Option`, `feed_start` the code, then loop over
  `ReplProgress`. In the `FunctionCall` arm, call `dispatch_tool` (Chunk D) and `resume` with its
  result; restore the REPL on `Complete`:
  ```rust
  use monty::{ReplProgress, MontyObject, PrintWriter, ExtFunctionResult, NameLookupResult};

  pub fn run_code(&mut self, code: &str) -> Result<MontyObject, DropletError> {
      let repl = self.repl.take().expect("session REPL present");
      let mut progress = repl.feed_start(code, vec![], PrintWriter::Stdout)?;
      let value = loop {
          match progress {
              ReplProgress::Complete { repl, value } => {
                  self.repl = Some(repl);         // put the REPL back for the next step
                  break value;
              }
              ReplProgress::FunctionCall(call) => {
                  let ret: ExtFunctionResult = self.dispatch_tool(&call)?; // host runs the real tool
                  progress = call.resume(ret, PrintWriter::Stdout)?;
              }
              ReplProgress::OsCall(c) =>
                  { progress = c.resume(MontyObject::None.into(), PrintWriter::Stdout)?; }
              ReplProgress::NameLookup(l) =>
                  { progress = l.resume(NameLookupResult::Undefined, PrintWriter::Stdout)?; }
              ReplProgress::ResolveFutures(f) => { /* Chunk G */ unimplemented!() }
          }
      };
      Ok(value)
  }
  ```
  - 🆕 Concept: **suspend / resume** = the interpreter stops when sandboxed Python calls an external
    function, hands you the name + args, and you `resume(...)` with the result. This is the *only* way
    the sandbox reaches a tool. (Concept is Monty-specific; no Rust Book chapter — but see PRODUCT.md §8
    "Execution".)
  - 🆕 Concept: a `match` over an enum like `ReplProgress` must cover **every** variant — the Rust
    compiler refuses to build a non-exhaustive `match`. That's what *forces* you to handle each
    suspension kind, so none is silently dropped. (Rust Book: *Enums and Pattern Matching*, ch. 6.)
  - 🆕 Concept: `self.repl.take()` moves the REPL out, leaving `None`. Because `feed_start` **consumes**
    the REPL (takes `self` by value) and returns it inside `Complete { repl, value }`, you must put it
    back — otherwise the next `run_code` finds `None`. (Rust Book: *Enums and Pattern Matching* —
    `Option::take`.)
  - ⚠️ Invariant #4 (scaffold exception): external functions in Rust are **not registered up front and
    are not classes/modules** — the REPL simply **suspends** at each call and you dispatch on
    `call.function_name`. The Python `external_functions=` dict is a `monty-python` convenience you do
    **not** use here. M3's "table" is your `match`; M4's macro will *generate* the registration metadata
    that replaces this hand-written `match`.
  - ⚠️ verify: `feed_start` appears to **consume `self`** (returns the repl back inside
    `ReplProgress::Complete { repl, value }`), while `feed_run` takes `&mut self`. Confirm both the
    `take()`/put-back ownership model and the full variant set against `repl.rs` / `run_progress.rs` at
    `v0.0.18` before relying on the loop above.

- [ ] Cover **every** arm (`OsCall` / `NameLookup` / `ResolveFutures`) with a safe default so nothing
  panics yet, even though only `FunctionCall` does real work in this chunk. Leave `ResolveFutures` as
  `unimplemented!()` for now (Chunk G fills it) — but every *other* arm must be panic-free.
  - ✅ Done when: a Monty script that calls one **undefined** function (return a hardcoded
    `MontyObject` from a stub `dispatch_tool` for now) round-trips `feed_start` → `FunctionCall` →
    `resume` → `Complete`, returning the hardcoded value, with no arm except `ResolveFutures` panicking.
    **Commit.**

---

### Chunk D — `dispatch_tool`: the hand-wired external-function table

Now fill in the `dispatch_tool` you stubbed in Chunk C. This is the **hand-wired table** invariant #4
calls out as a scaffold. Each arm reads the args off the `call`, runs the real M1/M2 host work, and
builds an `ExtFunctionResult` to hand back.

- [ ] Write `dispatch_tool(&mut self, call: &FunctionCall) -> Result<ExtFunctionResult, DropletError>`
  on `Session`, branching on `call.function_name`. Start with the two **handle-returning** prims that
  need no real engine call to *shape* (you wire their bodies to M1/M2 next):
  ```rust
  fn dispatch_tool(&mut self, call: &FunctionCall) -> Result<ExtFunctionResult, DropletError> {
      match call.function_name.as_str() {
          "load"        => self.tool_load(&call.args),       // → Dataset handle
          "filter_rows" => self.tool_filter_rows(&call.args),// → Dataset handle
          "group_agg"   => self.tool_group_agg(&call.args),  // → Dataset handle
          "local_sql"   => self.tool_local_sql(&call.args),  // → Dataset handle
          "to_rows"     => self.tool_to_rows(&call.args),    // → capped rows
          "scalar"      => self.tool_scalar(&call.args),     // → one value
          "eq" | "gt" | "between" => self.tool_filter(&call.function_name, &call.args),
          other => Err(DropletError::unknown_tool(other.to_string())),
      }
  }
  ```
  - 🆕 Concept: `call.function_name.as_str()` lets you `match` a `String` against string **literals**
    (you can't `match` a `String` directly against `"load"` without `.as_str()`). (Rust Book: *The
    `match` Control Flow Construct* + *Storing UTF-8 Encoded Text with Strings*.)
  - 🆕 Concept: `call.args` is a `Vec<MontyObject>` (positional Python args, each already a Monty
    value). You destructure them per tool — e.g. `load`'s first arg is the dataset name `Str`. (verify
    the `args` field name and whether kwargs arrive separately at the tag — see below.)
  - ⚠️ Invariant #4 (scaffold exception): this `match` *is* the hand-maintained registry the Golden
    Rule forbids in steady state. It's the scaffold. Keep it small and obvious so M4's macro has a
    one-to-one target.
  - ⚠️ verify: `ExtFunctionResult` construction — M0's examples used `MontyObject::Int(123).into()`.
    Confirm the `From<MontyObject>` impl **and** whether a tool can return an **error** that surfaces as
    a *catchable Python exception* in the sandbox (Droplet wants e.g. a load failure to be catchable so
    the agent can branch on it). Read `crates/monty/src/` at the tag.

- [ ] Add a `DropletError::unknown_tool(String)` variant (via `thiserror`) for the `other =>` arm, so a
  name the sandbox shouldn't be able to call is a clean error rather than a panic.
  - ⚠️ Invariant #10 (one error type at the boundary): every dispatch failure is a `DropletError`, never
    a raw `MontyException` or a `panic!`. (Rust Book: *Error Handling*, ch. 9.)
  - ✅ Done when: a script calling a bogus name (e.g. `definitely_not_a_tool()`) returns
    `Err(DropletError::UnknownTool(...))` from `run_code` — caught, not panicking. **Commit.**

---

### Chunk E — Wire the real tool bodies (`load` + the analyze prims)

The arms exist; now make them *do the real thing* against M1/M2. This is where "first working Droplet"
starts to breathe — the sandbox's `load(...)`/`group_agg(...)` calls reach actual engines.

- [ ] Implement `tool_load`: read the args (dataset name `Str`, `columns` list, `where` filter list,
  `as_of` `Str`), call M2's `load(...)`, store the returned `Dataset` in the **session's handle
  registry**, and return the **handle id** (a small `u64`) as a `MontyObject::Int`.
  ```rust
  fn tool_load(&mut self, args: &[MontyObject]) -> Result<ExtFunctionResult, DropletError> {
      // 1. parse name/columns/where/as_of out of `args` (see verify on kwargs below)
      // 2. let dataset = self.load(name, columns, where_, as_of)?;   // M2 boundary
      // 3. let handle: u64 = self.registry.insert(dataset);          // M0 registry
      Ok(MontyObject::Int(handle as i64).into())
  }
  ```
  - 🆕 Concept: the sandbox receives a **handle** (an opaque `u64`), never the `Dataset` itself. The
    real `Dataset` (which owns DuckDB state) lives host-side in the registry from M0. The agent's
    Python variable `usage` is *just that integer*. (This is the registry seam you built in M0.)
  - ⚠️ Invariant #1 (the agent never sees the real engine): `load` goes through M2's catalog +
    connector — the agent passes a *logical* dataset name and never learns it's a local Parquet file (in
    M6 the same call hits Athena; the agent's code is unchanged). The driver must not leak the
    connector/source identity into the return value.
  - ⚠️ Invariant #2 (only `load` touches the source): `load` is the single arm that calls a connector.
    No other arm in `dispatch_tool` may reach a source — they all operate on already-local handles.
  - ⚠️ verify: how Monty delivers keyword args. `load(name, columns=[...], where=[...], as_of="latest")`
    uses kwargs. Confirm whether `call` exposes a separate `kwargs` map or flattens everything into
    `args` at `v0.0.18` (read `run_progress.rs`). If kwargs aren't separately available, the hand-wired
    scaffold can require **positional** args for now and the M4 macro can formalize kwargs — note which
    you chose.
  - ✅ Done when: a script `usage = load("usage_daily", ["account_id","active_minutes"], [], "latest")`
    returns an integer handle and the registry holds a live `Dataset` for it.

- [ ] Implement `tool_group_agg` (and `tool_filter_rows`, `tool_local_sql` the same way): read the
  **input handle** off `args`, `registry.require(handle)?` to get the `Dataset`, run the M1 prim, insert
  the **new** `Dataset` into the registry, and return its new handle.
  - 🆕 Concept: `registry.require(handle)?` (from M0) turns a missing handle into
    `DropletError::BadHandle` — so an agent passing a stale/garbage handle fails cleanly at the boundary
    rather than corrupting anything. (Rust Book: *Recoverable Errors with `Result`* — `?`.)
  - ⚠️ Invariant #6 (boundary discipline): these prims return a **handle**, not rows. Big data stays
    inside DuckDB behind the handle; nothing bulky crosses into the sandbox here. The agent chains
    `group_agg(filter_rows(usage, ...), ...)` purely by passing handles around.
  - ⚠️ Invariant #9 (DuckDB sync → spawn_blocking): the prim body runs M1's DuckDB path, which is sync
    and lives inside `spawn_blocking`. `run_code` itself is sync today (no `pyo3`, no async caller), so
    you'll either (a) block on the `spawn_blocking` join handle via a small runtime handle the `Session`
    owns, or (b) keep the M1 prim's sync core reachable for the in-process driver. Pick one and note it
    — don't run DuckDB on a Tokio worker thread directly. (verify the exact bridging you chose against
    M1's `query_arrow_blocking` / `analyze_local_parquet` — the sync DuckDB core plus its
    `spawn_blocking` async entrypoint.)
  - ✅ Done when: a script chaining `load` → `group_agg` returns a second handle, and the registry holds
    two distinct `Dataset`s. **Commit.**

- [ ] Implement `tool_filter` for `eq` / `gt` / `between` (the typed filter helpers from M2): read the
  field name + value(s) off `args`, build M2's filter value, and return it as a small `MontyObject`
  (e.g. a struct-ish value the agent passes into `load`'s `where=`).
  - 🔗 Maps to: this lets the agent write `where=[eq("region","EU"), between("day","2025-10-01","2026-03-31")]`
    exactly as in PRODUCT.md §6. The filter object is small; it carries no data.
  - ✅ Done when: a script building `eq("account_id", 1)` returns a small filter value with no panic.

---

### Chunk F — Hand-write the `.pyi` stub bundle (the type-check contract)

The dispatch side now works at runtime. But invariant #4's *other* half is **types**: the agent's code
must be checkable *before* it runs. In steady state M4 generates these stubs from the Rust signatures;
**in M3 you hand-write a `.pyi` string** that exactly describes the flat surface from Chunk B. This is
the second piece of the scaffold.

- [ ] Add a `fn tool_stubs() -> String` in `driver.rs` that returns a Python **stub** (`.pyi`) source
  describing every flat tool, marked `# SCAFFOLD`:
  ```python
  # SCAFFOLD — hand-written tool stubs; M4 generates these from #[droplet_tool] signatures.
  from typing import Any, Literal

  Dataset = int          # a host-side handle; the agent only ever holds the id

  def load(name: str, columns: list[str], where: list[Any], as_of: str) -> Dataset: ...
  def filter_rows(ds: Dataset, where: list[Any]) -> Dataset: ...
  def group_agg(ds: Dataset, by: list[str], metrics: dict[str, Any]) -> Dataset: ...
  def local_sql(sql: str, datasets: dict[str, Dataset]) -> Dataset: ...
  def to_rows(ds: Dataset) -> list[dict[str, Any]]: ...
  def scalar(ds: Dataset) -> Any: ...
  def eq(field: str, value: Any) -> Any: ...
  def gt(field: str, value: Any) -> Any: ...
  def between(field: str, lo: Any, hi: Any) -> Any: ...
  ```
  - 🆕 Concept: a `.pyi` **stub** is a type-only Python file — signatures with `...` bodies, no
    implementation. The type checker reads it as the *contract* for functions that exist at runtime but
    aren't defined in the agent's source. (This is Python's standard stub mechanism; the type checker is
    Monty's bundled `ty`.)
  - 🆕 Concept: `Dataset = int` is a **type alias** — to the checker a `Dataset` *is* an `int`, which
    matches reality (the agent holds an integer handle). It documents intent and lets a future `Dataset`
    `NewType` tighten things in M4 without changing the agent's code. (Concept is Python typing-side.)
  - ⚠️ Invariant #4 (scaffold exception): this string is **hand-maintained** stubs — precisely what the
    Golden Rule forbids in steady state. It's the scaffold's typing half. M4's macro emits this *from*
    the Rust signatures so the two can never drift; here you keep the names byte-for-byte in sync with
    Chunk B/D **by hand**.
  - ⚠️ The stub surface must match the dispatch surface **name for name**. If `dispatch_tool` handles
    `group_agg` but the stub spells it `groupagg`, the type checker passes code the host then rejects at
    runtime — the worst kind of drift. Keep one list; M4 ends this risk.
  - 🔗 Maps to: the real schema-derived stubs (per-dataset field `Literal`s, row `TypedDict`s, the
    specialized `load(...)` signature) are [`M4-droplet-tool-macro.md`](./M4-droplet-tool-macro.md). M3
    proves the *seam* with a flat hand-written bundle; M4 wires real generated stubs into the **same**
    type-check call.
  - ✅ Done when: `tool_stubs()` returns a non-empty `.pyi` string whose function names exactly match
    the `dispatch_tool` arms. **Commit.**

---

### Chunk G — Type-check BEFORE run (the retry seam)

Now connect the stubs to the bundled checker and put it *in front of* `feed_start`. This is the
"wrong column caught before execution" promise: on a type error you **return**, you do **not** run.

- [ ] Add the `monty-type-checking` crate to the workspace (same repo + tag as `monty`) and opt
  `droplet-core` in:
  ```toml
  # [workspace.dependencies] in the root Cargo.toml
  monty-type-checking = { git = "https://github.com/pydantic/monty", tag = "v0.0.18" }
  ```
  ```toml
  # crates/droplet-core/Cargo.toml
  monty-type-checking.workspace = true
  ```
  - ⚠️ `monty-type-checking` drags in Astral's `ty` / `ruff` crates (pinned to a ruff git commit) plus
    `salsa` — heavy, unpublished-API deps. **Expect a long first build** (you already paid this for
    `monty` in M0; the checker adds more). Keep `Cargo.lock` committed; avoid `cargo update` on this
    tree — a bump could break the whole `ty`/`ruff`/`salsa` graph.
  - 🆕 Concept: Monty **bundles** Astral's `ty` type checker — you do **not** add a separate `ty`
    dependency or shell out to `ty check`. You call it as a library through `monty-type-checking`.
    (Concept is Monty/`ty`-specific; no Rust Book chapter.)
  - ✅ Done when: `cargo build -p droplet-core` is green (slow first time) and `grep monty Cargo.lock`
    shows the `v0.0.18` git source for `monty-type-checking`.

- [ ] In `run_code`, **before** `feed_start`, build `SourceFile`s for (a) the agent's code and (b) the
  stub bundle, call `type_check`, and branch on its three outcomes:
  ```rust
  use monty_type_checking::{type_check, SourceFile};

  let src   = SourceFile::new("session.py", code);
  let stubs = SourceFile::new("stubs.pyi", &Self::tool_stubs());
  match type_check(&src, Some(&stubs)) {
      Ok(None)        => { /* clean — proceed to feed_start below */ }
      Ok(Some(diags)) => return Err(DropletError::type_check(diags)),       // → model retries; DO NOT run
      Err(internal)   => return Err(DropletError::type_check_internal(internal)),
  }
  ```
  - ⚠️ Invariant #4 (scaffold exception) + the spec's typing promise: on `Ok(Some(diags))` you
    **return** the diagnostics so the caller maps them to a model retry — you must **not** call
    `feed_start`. The whole point is a wrong column / wrong arg fails *before* any `load` or DuckDB work
    happens.
  - 🆕 Concept: type-checking is a **separate, explicit call** you make *before* feeding code to the
    REPL — it is **not** automatic in the Rust API. The stubs are passed as a second `SourceFile` the
    checker treats as the contract. (Concept is Monty/`ty`-specific; no Rust Book chapter.)
  - 🆕 Concept: two distinct failure modes. `Ok(Some(diags))` = real **type errors** in the agent's
    code (→ feed back to the model so it retries). `Err(String/internal)` = the **checker itself** broke
    (→ surface loudly, don't loop). Don't collapse them — the caller treats them completely differently.
    (Rust Book: *Recoverable Errors with `Result`* — distinguishing error kinds.)
  - ⚠️ verify: the entry point shape — research observed
    `pub fn type_check(python_source: &SourceFile, stubs_file: Option<&SourceFile>) -> Result<Option<TypeCheckingDiagnostics>, String>`.
    Confirm the `SourceFile::new` constructor, the return type, and how to turn `TypeCheckingDiagnostics`
    into per-error messages to feed the model. `ty` is pre-1.0 BETA (0.0.x), so treat its diagnostic
    shape as unstable and re-confirm at the pinned tag.

- [ ] Add the two `DropletError` variants the match needs: a type-check **diagnostics** variant (the
  *retry* signal) and a type-check **internal** variant (the checker broke). Keep them **distinct** from
  a generic run error — the caller needs to tell "the model should retry" apart from "something genuinely
  broke."
  - ⚠️ Invariant #10 (one error type): both fold into `DropletError` via `thiserror` — no raw
    `TypeCheckingDiagnostics` leaks past the host. But the *retry* variant must be a recognizable,
    separate variant so the agent loop can act on it.
  - ✅ Done when: with the stub `def load(name: str, ...) -> Dataset: ...`, feeding `load(123, ...)`
    (wrong type for `name`) returns `Ok(Some(diags))` → the **retry** `DropletError` variant, and
    `feed_start` is **never reached**; feeding a well-typed `load("usage_daily", ...)` returns
    `Ok(None)` and proceeds. This is the spec's "wrong column caught before execution" in miniature.
    **Commit.**

- [ ] Decide what gets type-checked each step: **only the new chunk** or the **cumulative** session
  source. Names defined in an earlier `run_code` step must still resolve when the checker reads a later
  step.
  - ⚠️ verify: whether `type_check` should receive the **concatenated/cumulative** source so prior-step
    names resolve, or whether the stub bundle alone suffices. This is **unspecified** — build the driver
    to test both forms and keep whichever the checker accepts. Do **not** assume single-chunk checking is
    enough; a multi-step agent session must stay type-safe end to end, not just per isolated chunk.
  - ✅ Done when: a two-step session where step 1 defines a name and step 2 uses it **passes** the type
    check, and a deliberately-undefined name in step 2 is **caught** before execution.

---

### Chunk H — Cap what crosses back, and pick the resource tracker

Two safety knobs before the finale: the rows `to_rows`/`scalar` return must be **capped**, and you pick
the tracker that bounds the sandbox.

- [ ] Make `tool_to_rows` and `tool_scalar` convert their host result into a **small, capped**
  `MontyObject` before `resume`. Reuse M1's `MAX_RESULT_ROWS` for `to_rows` so the sandbox never
  receives a bulk recordset; `scalar` returns exactly one value.
  - 🆕 Concept: `to_rows` turns a `Dataset` handle into a Python `list[dict]` — the *only* prim (besides
    `scalar`) that moves real rows into the sandbox. M1 already caps at `MAX_RESULT_ROWS`; the driver
    just hands that capped result across as a `MontyObject` list. (Concept ties M1's cap to the
    boundary.)
  - ⚠️ Invariant #6 (boundary discipline): only result-returning tools (`to_rows`, `scalar`) move rows
    into the sandbox, and `to_rows` moves **capped** rows. The cap is what keeps the REPL (and therefore
    the M8 snapshot) small — handles + capped results, not data.
  - 🔗 Maps to: this same cap is what makes M8 snapshots small by construction (REPL bytes hold capped
    results, never bulk data).
  - ✅ Done when: a test loads a fixture with **more rows than the cap**, runs `to_rows`, and asserts the
    sandbox receives exactly `MAX_RESULT_ROWS` (never the full set).

- [ ] Confirm the resource tracker choice. `Session` uses `NoLimitTracker` for dev (Chunk A). Leave a
  clear seam to swap to `LimitedTracker::new(ResourceLimits { … })` later.
  - 🆕 Concept: the resource tracker is the **generic type parameter** on `MontyRepl<T>` chosen at
    construction — `NoLimitTracker` (no limits, dev) vs `LimitedTracker` (enforces `ResourceLimits`:
    step count, recursion depth, time). It's how Droplet will bound a runaway agent loop. (Rust Book:
    *Generic Types, Traits, and Lifetimes*, ch. 10.)
  - ⚠️ verify: the exact `ResourceLimits` field names/types (e.g. `Duration` vs milliseconds, plus any
    `DEFAULT_MAX_RECURSION_DEPTH` const). Read `crates/monty/src/resource.rs` at `v0.0.18` before
    constructing it — do **not** guess.
  - ✅ Done when: `to_rows` is capped (test green) and there's a one-line `// SWAP: LimitedTracker for
    prod` note where the tracker is chosen. **Commit.**

---

### Chunk I — Handle the async (`ResolveFutures`) arm, then the FIRST WORKING DROPLET test

Two steps: finish the last `ReplProgress` arm so async agent code can't panic, then write the
milestone test that proves Droplet actually works end to end.

- [ ] Fill in the `ResolveFutures` arm with correct (not just panic-free) behavior. Droplet's tools are
  sync from the sandbox's view, but Monty supports `asyncio`, so agent code *can* suspend with
  `ResolveFutures`. Resolve each pending call id through the **same** `dispatch_tool`:
  ```rust
  ReplProgress::ResolveFutures(f) => {
      let resolved: Vec<(u32, ExtFunctionResult)> = f.pending_call_ids().iter()
          .map(|&id| {
              // look up the pending call for `id`, run dispatch_tool, pair the result with the id
              todo!("resolve pending call `id` via dispatch_tool")
          })
          .collect::<Result<_, _>>()?;
      progress = f.resume(resolved, PrintWriter::Stdout)?;
  }
  ```
  - 🆕 Concept: `ResolveFutures` is Monty's async/await suspension — it hands you a **batch** of pending
    call ids to resolve at once, rather than one `FunctionCall` at a time. (Concept is Monty-specific; no
    Rust Book chapter.)
  - ⚠️ Invariant #4 (scaffold exception): even via `asyncio`, the **only** things the sandbox can reach
    are the flat tool names — `ResolveFutures` resolves the *same* tools, just batched. No new surface
    leaks in.
  - ⚠️ verify: the semantics of `pending_call_ids()` and the exact resume-vector shape
    (`Vec<(u32, ExtFunctionResult)>`) at the tag — async handling is the easiest arm to get subtly wrong.
    Read `run_progress.rs`.
  - ✅ Done when: an agent script using `async def` + `await load(...)` completes through the
    `ResolveFutures` arm without panicking, returning the same handle a sync call would.

- [ ] **Write the milestone test — the FIRST WORKING DROPLET.** One pure-Rust `#[tokio::test]` (or sync
  `#[test]`, per your `run_code` bridging choice in Chunk E), single process, local Parquet fixture, no
  S3, no CPython. Two legs:

  1. **Wrong column is caught before execution.** `run_code` a step whose `where=`/`columns=` names a
     field the stub doesn't allow (or passes a wrong-typed arg). Assert it returns the **type-check
     retry** `DropletError` variant and that **no `load`/DuckDB call ran** (e.g. a host counter the
     connector bumps is still `0`).

  2. **A correct program runs end to end and returns capped rows.** `run_code`:
     ```python
     usage = load("usage_daily", columns=["account_id", "active_minutes"], where=[], as_of="latest")
     agg   = group_agg(usage, by=["account_id"], metrics={"avg": ("active_minutes", "mean")})
     rows  = to_rows(agg)
     ```
     Assert `rows` is a small `list[dict]`, the values match the fixture's known averages, and the row
     count is `≤ MAX_RESULT_ROWS`.

  - ⚠️ Invariant #2 (only `load` touches the source): assert the connector was hit **exactly once** (the
    `load`), and `group_agg` / `to_rows` touched it **zero** times — they ran purely on the local handle.
  - ⚠️ Invariant #6 (boundary discipline): assert the rows that crossed back are **capped** — bounded,
    never the full recordset.
  - ⚠️ Invariant #8 (keep Python out of the core): this whole test runs in pure-Rust `droplet-core` —
    **no `pyo3`**, no CPython, no wheel.
  - ✅ Done when: leg 1 is caught pre-execution (the connector is never touched) **and** leg 2 runs
    through Monty returning capped, correct rows — *agent Python in Monty called `load`, ran analyze
    prims, and got capped rows back, single process, local Parquet.* **This is the M3 "Done when" — the
    first working Droplet.** Commit, and breathe: you have a real (small) Droplet.

---

## M3 done checklist

Tick all of these to call M3 complete (the spec's build-order step-4 "Done when" expanded):

- [ ] Each `Session` owns its **own** `MontyRepl` (in an `Option` you `take()`/restore), so each
  `run_code` step feeds the same interpreter and cross-step state persists (per-run isolation: one run =
  one `Session` = one REPL).
- [ ] `Session::run_code(code)` runs the `feed_start` → `ReplProgress` `match` loop; **every** variant
  is handled (no non-exhaustive `match`, no panic).
- [ ] The flat tool surface (`load`, `filter_rows`, `group_agg`, `local_sql`, `to_rows`, `scalar`, the
  filter helpers) is dispatched by a **hand-wired** `match` on `call.function_name`, each arm running the
  real M1/M2 host work and returning a **handle** or **capped** result (invariant #6).
- [ ] The `.pyi` stub bundle is **hand-written** and matches the dispatch surface name-for-name; both
  are clearly marked **SCAFFOLD** (the deliberate invariant-#4 exception that M4 replaces with
  `#[droplet_tool]`).
- [ ] `type_check(&src, Some(&stubs))` runs **before** `feed_start`; a type error returns the distinct
  **retry** `DropletError` variant and execution never starts; a clean check proceeds. Cumulative vs
  per-step checking is decided and tested.
- [ ] `to_rows` / `scalar` results are **capped** (`MAX_RESULT_ROWS`); only those two move rows into the
  sandbox; everything else moves handles (invariant #6).
- [ ] Monty + type-check errors fold into `DropletError` via `thiserror`; the retry variant is distinct
  from generic run errors (invariant #10).
- [ ] The whole driver is exercisable from a pure-Rust `#[test]` — **no `pyo3`, no CPython** (invariant
  #8).
- [ ] The milestone test passes both legs: a wrong column/arg is caught **before** any source/DuckDB
  call, and a correct `load → group_agg → to_rows` program returns capped, correct rows from a local
  Parquet fixture — the **first working Droplet**.

**Spec "Done when": an agent `run_code` step runs Python in Monty, a wrong field is caught at
type-check before execution, and `load` + the analyze prims return capped rows into the sandbox.** ✅

---

## Notes carried forward (don't act yet)

- **The hand-wiring is temporary — M4 deletes it.** Both scaffolds (the `dispatch_tool` `match` and the
  hand-written `.pyi` stubs) exist *only* to make the seam concrete.
  [`M4-droplet-tool-macro.md`](./M4-droplet-tool-macro.md) introduces the `#[droplet_tool]` proc-macro
  that emits the Monty registration **and** the Python stub fragment from each Rust signature, plus
  runtime **schema-derived** types (per-dataset field `Literal`s, row `TypedDict`s, the specialized
  `load(...)` signature). When you get there, the macro output should be a drop-in replacement for the
  two scaffolds — feeding the *same* `type_check(&src, Some(&stubs))` call and the *same* dispatch loop.
  Design both scaffolds so that swap is mechanical. (This is what finally satisfies invariant #4.)
- **The cache, cloud connectors, and the state plane are later.** In M3 `load` reads a **local Parquet
  fixture** through M2's dev connector — no S3, no Athena, no cache. The content-addressed cache is
  M5; real engines (Athena `UNLOAD`) are M6; the distributed plane (artifact/coordination stores) is
  M5–M7. M3's `load` arm must stay behind M2's `Source` trait so those swap in unchanged (invariant #1).
- **Snapshot is M8, but design for it now.** `MontyRepl::dump()` → `Vec<u8>` (postcard) captures the
  whole interpreter state and is the snapshot's REPL bytes
  ([`M8-snapshot-resume.md`](./M8-snapshot-resume.md)). **Do not** serialize DuckDB — that matches
  invariant #5 (REPL bytes + manifest only; rebuild DuckDB from the manifest on resume) and invariant #7
  (immutable state is content-addressed in the **shared** object store, not local to one pod). Three
  constraints to honor *now* so M8 is clean: (1) snapshot only at a clean seam (the `FunctionCall`
  boundary, where the VM is paused waiting on you); (2) the postcard format is **tied to the Monty tag**
  — pin one tag fleet-wide and record it in the manifest so cross-version loads fail loudly rather than
  mis-decode; (3) the snapshot blob is content-addressed + `zstd`-compressed into the shared
  `SnapshotStore` (S3). The capping you did in Chunk H is what keeps these blobs small.
- **`search_fields` / `list_datasets` / `describe_dataset` are M9.** M3's surface is deliberately just
  `load` + the analyze prims + filters — the smallest set that makes a working Droplet. Discovery tools
  (and the read-only SurrealDB vector field index behind `search_fields`) are
  [`M9-field-search.md`](./M9-field-search.md). When you add them, they slot into the **same**
  `dispatch_tool` + stub bundle (then auto-generated by M4's machinery), not a new mechanism.
- **Document the Python subset you verified** directly next to the stub bundle, so the hand-written (and
  later generated) stubs stay flat. At `v0.0.18` Monty **cannot**: define classes, use `match`
  statements, import third-party libs, or use most stdlib; supported stdlib includes `sys`, `os`,
  `typing`, `asyncio`, `re`, `datetime`, `json` (+ `open()` / file I/O). ⚠️ verify: re-read the README
  "limitations" at the **exact tag** — the supported-module list changes release to release. The tool
  surface is **flat typed functions** (no modules/classes) precisely because of this.
- **Pin the dep tree.** Keep `Cargo.lock` committed; avoid `cargo update` for `monty` /
  `monty-type-checking` / `ruff` / `ty` / `salsa`. If you must bump `monty`, treat it as a
  **snapshot-format change** (bump Droplet's snapshot-format version, refuse cross-version loads). This
  is the single biggest time-sink in this area.

---

> 📌 You're at the first finish line. From [`M4-droplet-tool-macro.md`](./M4-droplet-tool-macro.md)
> onward, every milestone *layers* the bigger pieces (the macro, the cache, real connectors, the
> distributed plane, snapshot/resume) onto this working core — one milestone at a time.
