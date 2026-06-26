# Droplet v1 — Build Roadmap

> **This roadmap was rewritten _again_ (second pivot, 2026-06-15).** The core idea
> changed. Any earlier plan you may remember — where **the agent calls `run_sql`
> and DuckDB reads the company's data straight from S3** — is **obsolete. Forget
> it.** (And the even-older Polars / SurrealDB-as-storage / "Arrow seam" plan
> before that is doubly dead.)
>
> **The new core idea is a hard split — LOAD vs ANALYZE:**
> - **LOAD** is the *only* thing that ever touches the company's real database. It
>   pulls a small, bounded slice of data **once**, through a **connector**, into a
>   local file — and caches it so it never has to pull it again.
> - **ANALYZE** then runs **locally**, over that downloaded copy, as much as the
>   agent wants — with **no further contact** with the real database.
>
> So **DuckDB's job flipped**: it is no longer the thing that reaches out to S3.
> It is now the **local engine** that crunches the slice we already pulled down.
> If any file in this folder still says the agent runs `run_sql` against the
> source, or that DuckDB reads S3 directly, that line is stale — the table below
> (`00`, `M0`…`M10`) is the real plan.

---

## What you're building (in plain words)

You're building **Droplet**: a small **distributed** runtime that lets an AI agent
do real data analysis over a company's data — **without hammering the company's
production database**.

Here's the whole idea in one breath. The agent writes a little bit of **Python**.
That Python doesn't run loose on your machine — it runs **sandboxed** inside an
embedded interpreter called **Monty**. The sandboxed code can only call a small,
**typed** set of tools, and those tools come in two flavors:

- **`load(...)`** is the *one* tool that touches the company's real data. It pulls
  a **bounded slice** — only the columns and rows you asked for — out of the source
  (it might be Athena, Snowflake, BigQuery, Iceberg, plain S3 — **the agent never
  knows which**, that's the connector's job) and lands it as a local **Parquet**
  file. This download happens **once** and is **cached** by a hash of exactly what
  you asked for, so every server in the fleet reuses the same download instead of
  re-pulling it. We call one server a **pod**.
- **Everything else** — `filter_rows`, `group_agg`, `join`, `local_sql`,
  `to_rows`, and friends — runs **locally**, against that downloaded copy, inside an
  ephemeral **DuckDB**. This side is **wide open** on purpose: there's nothing to
  protect, because it's a local, throwaway copy. The agent can loop, branch, join,
  re-aggregate, and write its own Python over the small results it pulls back —
  **never touching the source again.**

The inversion is the whole point. A "text-to-SQL" tool sends *every* question to
production. Droplet sends **one bounded download** to production, then runs
**unlimited code locally**. *Lock down the boundary, set the analysis free.*

On top of that: a wrong column name is caught by a **type checker before the code
even runs**, and a run can be **snapshotted** (the interpreter's bytes + a tiny
manifest) and **resumed on any pod**, because all the durable state lives in a
**shared plane** (S3 + Redis/DynamoDB), not on one machine.

You already program (Python), so you know loops, functions, and types. What's new
is **Rust** — a compiled language with strict rules about who owns what memory.
That strictness is exactly what makes the sandbox safe, and this roadmap teaches
it one small step at a time.

The full spec is **`PRODUCT.md` at the repo root** (it is `PRODUCT.md`, *not*
`docs/PRODUCT.md`). It is the source of truth; this roadmap just teaches you how to
build it, one tiny step at a time.

---

## How to use this roadmap

- [ ] **Go top to bottom.** Do the warm-up first, then `M0`, then `M1`, and so on.
  Each milestone builds on the last; skipping ahead will leave you stuck.
- [ ] **One chunk per sitting.** Every file is split into "### Chunk N" sections. A
  chunk is ~30–90 min of small steps and ends at a natural "it compiles and a test
  passes" checkpoint. Do a chunk, then stop if you want — it's a save point.
- [ ] **The checkboxes are your save-game.** Every `- [ ]` is one tiny task
  (~10–30 min, one new idea). Tick it (`- [ ]` → `- [x]`) the moment its
  "✅ Done when" check passes. When you come back, the first empty box is exactly
  where you resume. Don't tick a box you haven't verified.
- [ ] **Don't infer middle steps in the DEEP files** (`00`, `M0`, `M1`, `M2`,
  `M3`): they spell out every step, including creating a file or adding a single
  dependency. If you ever feel you're guessing, re-read — the step is probably
  there.
- [ ] **The SKETCH files** (`M4`–`M10`) are coarser on purpose. When you *reach*
  one, your first job is to expand its chunks into tiny steps the way `M0`–`M3` are
  written.

### The "simple but working" target comes early

You don't have to build the whole distributed system before anything runs. The
roadmap is staged so that **by the end of `M3` you have a real, working Droplet** —
small, single-machine, but real: **an agent's Python runs in Monty, calls `load`
to pull a local slice, analyzes it locally, and gets an answer back.** That's your
first finish line. Everything from `M4` on *layers* the bigger pieces (the real
cloud engines, the cache, the distributed plane, snapshot/resume) onto that
working core, one milestone at a time.

---

## The build / learn loop

You are a programmer who knows some Python but is **new to Rust**. That's fine —
you'll learn Rust by building, not by reading a whole book first. The loop for
every single step is the same rhythm:

```
edit  →  cargo build  →  cargo test  →  tick the box  →  git commit
```

1. **edit** one small thing (a struct field, a function, a `#[test]`). One idea at
   a time.
2. **`cargo build`** — does it compile? Rust's compiler is your strictest, most
   helpful teacher; the errors are long but they tell you the fix. A green build
   already means a *lot* is correct.
3. **`cargo test`** — does the behavior match what you expected?
4. **tick the box** once "✅ Done when" passes (build + test green).
5. **`git commit`** at the end of a chunk so you can always roll back. Commit
   `Cargo.lock` too — Droplet produces binaries *and* pins fast-moving git deps
   (Monty), so the lockfile belongs in git.

Prefer **test-first** where it's natural: write a tiny `#[test]`, run it, watch it
**fail** (red), then write just enough code to make it **pass** (green). Seeing the
failure first proves the test actually tests something.

### Where to look when you're stuck (in this order)

- **`rust-analyzer`** in your editor — red squiggles + hover types catch most
  mistakes *before* you even build. Fastest feedback you have.
- **The `cargo` error message** — read it slowly, top to bottom; it usually names
  the file, line, the rule you broke, and often a suggested fix (`help:`). Run
  `cargo build` again after each fix.
- **The Rust Book** — this roadmap cites it by chapter *name* (e.g. "Understanding
  Ownership", "Generic Types, Traits, and Lifetimes", "Error Handling"). Open it
  offline with `rustup doc --book`.
- **Rustlings** — tiny standalone exercises (`cargo install rustlings`); great for
  drilling a concept (ownership, traits, error handling) until it clicks.

When an error mentions a specific crate API, check that crate's docs **for the
exact version this roadmap pins** — APIs in this stack move fast, and an old blog
post will lead you astray. A few traps worth knowing up front:

- **Monty** has no useful published docs (the crates.io `monty` is an unrelated
  `0.0.0` placeholder; `docs.rs/monty` shows nothing). Its real "docs" are the
  GitHub README and the Rust source under `crates/monty/src/`. The roadmap pins it
  as a **git dependency on tag `v0.0.18`**, and its API churns every few weeks —
  expect `verify:` notes, and read the source for exact signatures.
- **DuckDB is now your _local_ engine** (it reads the Parquet file `load` already
  pulled down — *not* S3 directly). Its `arrow` pairing still matters: the `duckdb`
  crate pins a specific `arrow` major (currently `58`), and the latest `arrow` on
  crates.io is newer (`59`). **Don't add `arrow` yourself blindly** — use the
  `duckdb::arrow` re-export; `M1` walks you through this and `cargo tree -i arrow`.
- **PyO3** is pinned to **`0.28`** (NOT `0.29`): monty `v0.0.18` transitively pulls
  pyo3 `0.28.x` via its `jiter` dep, and `pyo3-ffi` declares `links = "python"`,
  which lets only **one** pyo3 version exist graph-wide — so the whole workspace
  must match monty's. (`0.29` in `droplet-py` fails with a `pyo3-ffi ... links
  python` conflict.) Also: the `extension-module` feature is **deprecated and
  dropped** (it disables libpython linking and breaks `cargo test`/workspace
  builds — maturin builds the extension module itself). The GIL teaching still
  holds on `0.28`: pyo3 renamed `Python::allow_threads` → `Python::detach` in 0.26
  with **no** deprecated alias, so any tutorial using `allow_threads` won't compile
  (likewise `with_gil` → `attach`).
- **Proc-macros are advanced Rust** and you don't meet them until **`M4`** — when
  you do, that file gives you a from-scratch intro. Don't worry about them before
  then; in `M3` you wire the tool surface up *by hand* first, precisely so you can
  see what the macro will later generate for you.
- **SurrealDB** (only in `M9`, read-only) jumped 2.x→3.x; most snippets are 2.x and
  mislead (the **MTREE** vector index was *removed* in 3.0 — use **HNSW**; the
  in-memory engine is `surrealdb::engine::local::Mem`). The roadmap pins `3.1.4`.

---

## The roadmap files (read in this order)

**DEEP** = exhaustively granular, every tiny step spelled out (good for when you're
new). **SKETCH** = chunk-level tasks you'll expand into tiny steps once you reach
them. Files `00`–`M3` (everything up to your first working agent) are DEEP.

| # | File | What it covers | Depth |
|---|------|----------------|-------|
| 0 | [`00-rust-warmup.md`](./00-rust-warmup.md) | Rust fundamentals: ownership, `Result`/`?`, **enums**, **traits + generics** (practice for the `Source` connector trait and the store traits), **async + Tokio**, `Arc`/`Mutex`, and a light intro to **hashing / content-addressing**. Throwaway scratch crate. | **DEEP** |
| 1 | [`M0-skeleton.md`](./M0-skeleton.md) | **Skeleton.** Workspace `[workspace.dependencies]` + `DropletError` (`thiserror`) + handle registry + `Session` + the **`Source` connector trait** with a **trivial local-Parquet dev connector** + the `droplet-py` wheel (maturin) + a Monty smoke test + CI. *(The S3/coordination/snapshot stores come later, in `M5`–`M8`.)* | **DEEP** |
| 2 | [`M1-analyze-engine.md`](./M1-analyze-engine.md) | **Local analyze engine.** Ephemeral **DuckDB** over a **local Parquet** file → a `Dataset` handle; first analyze primitives (`filter_rows`, `group_agg`, `to_rows`, `scalar`) + `local_sql`; **capped Arrow** results; `spawn_blocking`; releasing the GIL. | **DEEP** |
| 3 | [`M2-load-boundary.md`](./M2-load-boundary.md) | **The `load` boundary.** A minimal **Catalog** (logical dataset → connector + schema), `load(name, columns, where, as_of) -> Dataset` that calls the connector and materializes locally, and the typed **filter helpers** (`eq`, `gt`, `between`, …). Still single-machine; no cache yet. | **DEEP** |
| 4 | [`M3-monty-driver.md`](./M3-monty-driver.md) | **The agent loop.** The **`run_code`** loop + the external-function tool surface (Monty *suspends* at a tool call, the host runs it, then *resumes*) + **type-check-before-run** + a **hand-wired** stub bundle exposing `load` + the analyze prims. ✅ **First working Droplet.** | **DEEP** |
| 5 | [`M4-droplet-tool-macro.md`](./M4-droplet-tool-macro.md) | **Auto-bootstrap.** A `#[droplet_tool]` **proc-macro** that emits the Monty registration *and* the Python type-stub from each Rust signature, plus runtime **schema-derived types** (per-dataset field `Literal`s, row `TypedDict`s) — **replacing `M3`'s hand-wiring**. (First proc-macro intro.) | SKETCH |
| 6 | [`M5-artifact-cache.md`](./M5-artifact-cache.md) | **`ArtifactStore` + content-addressed cache.** Object store (in-memory → S3/MinIO) + **content-addressed cache** keyed `hash(scoped query + source + freshness token)` + freshness policy (Versioned/TTL/Passthrough). One download, reused fleet-wide. | SKETCH |
| 7 | [`M6-connectors-athena.md`](./M6-connectors-athena.md) | **Real engines.** The `droplet-connectors` crate; **Athena** (`UNLOAD` → Parquet on S3) + S3/Iceberg **direct read**, all behind the `Source` trait. Now `load` hits a real warehouse, cached. | SKETCH |
| 8 | [`M7-coordination.md`](./M7-coordination.md) | **`CoordinationStore`.** **Redis** then **DynamoDB**: run registry + **leases** (one worker per run, reassignable) + the cache index in the consistent store. | SKETCH |
| 9 | [`M8-snapshot-resume.md`](./M8-snapshot-resume.md) | **`SnapshotStore` + cross-pod resume.** REPL bytes + manifest, **zstd**, content-addressed, write-behind; **resume on any pod** by rebuilding DuckDB from the manifest (re-attach cached Parquet); lease-guarded. | SKETCH |
| 10 | [`M9-field-search.md`](./M9-field-search.md) | **Discovery.** Read-only **SurrealDB** vector field search + `search_fields`, plus `list_datasets` / `describe_dataset`; catalog-derived typing end to end. | SKETCH |
| 11 | [`M10-export-sdk-adapter.md`](./M10-export-sdk-adapter.md) | **Ship it.** `export` to S3 Parquet (governed) + Python SDK polish + a **pydantic-ai** adapter + a **two-pod** distributed integration test (= the `PRODUCT.md` §20 success criterion). | SKETCH |

---

## 🏆 Golden rules (the invariants, in beginner words)

These are the load-bearing promises from `PRODUCT.md` §15. Don't break them — each
file reminds you with a **⚠️ Invariant** note when one is relevant.

1. **The agent never sees the real engine.** Every source is reached through a
   **connector** that turns it into local Parquet. The agent only ever works with
   *logical, local* datasets — it cannot tell (or ask) whether the data came from
   Athena, Snowflake, or a plain S3 file.
2. **Only `load` touches the source — and only as a bounded, typed, cached
   download.** There is **no arbitrary SQL against production**, ever. `load` is the
   single guarded door.
3. **Analyze runs only on the local copy.** The whole analyze surface is
   unrestricted *because* it's local and throwaway — but it physically cannot reach
   back to a source.
4. **The tool surface is auto-generated.** Fixed tools carry a `#[droplet_tool]`
   macro; the data-shaped types come from the catalog. No hand-maintained registry
   or stubs. *(Exception, on purpose: `M3` wires a tiny surface **by hand** as a
   teaching scaffold, and `M4` replaces it with the macro to satisfy this rule.)*
5. **Snapshots are tiny: REPL bytes + a manifest, nothing else.** Never serialize an
   engine's memory. On resume you **rebuild** DuckDB from the manifest (re-attach the
   cached Parquet). Snapshots are immutable, content-addressed, **versioned**, and
   zstd-compressed.
6. **Boundary discipline.** Big data stays inside DuckDB behind an opaque **handle**;
   only `to_rows` / `scalar` / load-samples move actual rows into the sandbox, and
   always **capped**. The sandbox sees handles, not data — which is also what keeps
   snapshots small.
7. **Distributed by default.** Durable state lives in the shared plane: immutable
   data is content-addressed in the object store (S3); mutable coordination (run
   registry, leases, cache index) is in the consistent store (Redis/DynamoDB).
   Resume is **lease-guarded**; no pod affinity.
8. **Keep Python out of the core.** `droplet-core` (pure Rust) must **never** write
   `pyo3` code; the Python bridge lives only in `droplet-py`. The core depends on
   `monty` + engines, **never** on an agent framework (pydantic-ai glue lives in a
   separate adapter package). *(Caveat at monty `v0.0.18`: pyo3 appears
   **transitively** in `droplet-core`'s tree via monty's `jiter` dep — so this rule
   means "`droplet-core` writes no pyo3 code / never `use`s pyo3", not "pyo3 is
   absent from the lockfile". It becomes literally absent only when monty drops the
   `jiter`→pyo3 dep.)*
9. **DuckDB is synchronous → wrap it in `spawn_blocking` and release the GIL**
   (`py.detach(...)`) while a query runs, so you don't freeze the async runtime or
   block other Python threads.
10. **One error type at the boundary.** Libraries use **`thiserror`** (a tidy
    `DropletError` enum); binaries use **`anyhow`**. Every engine error folds into
    `DropletError`.

**Practical note (not an invariant, but it shapes the design):** Monty is a
*subset* of Python — no third-party imports, no classes, limited stdlib. So the tool
surface is **flat typed functions** (no modules/classes), and you **type-check
before you run**.

---

## Legend

Symbols you'll see throughout the files:

- 🆕 **Concept:** a 1–2 sentence plain explanation of a *new* Rust/Droplet idea,
  with the relevant Rust Book chapter *name* (e.g. *Rust Book: Understanding
  Ownership*). Appears the **first** time a concept shows up in a file. Skip it if
  you already get the idea.
- ✅ **Done when:** the observable check that proves a step worked — a command's
  output or a passing test. Tick the box only when this passes.
- ⚠️ **Invariant:** the spec rule (numbered 1–10 above) this step must respect.
- 🔗 **Maps to:** the real Droplet concept a warm-up exercise unlocks (so the
  practice doesn't feel random).
- **verify:** a fact the research couldn't fully pin down for the locked crate
  version — go check the crate's source/docs *first*, before relying on it. Treat it
  as "trust but confirm". You'll see these most around **Monty** (`v0.0.18` API +
  snapshot format), the **DuckDB↔arrow** major pairing, the **`ty`** type checker
  (Monty bundles it), the **proc-macro** crates (`syn`/`quote`), and the cloud SDKs.

---

## Recommended setup

You don't need everything at once. Set up the Rust toolchain now; add the local dev
backends only when you reach the milestone that needs them.

### Now (for the warm-up, `M0`–`M3`)

- [ ] **Install Rust via `rustup`**: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`,
  then restart your terminal.
  - ✅ Done when: `rustc --version` and `cargo --version` both print a version.
- [ ] **Know the toolchain floor.** The repo targets **edition 2024** (needs Rust
  ≥ 1.85). Later milestones pull crates with higher floors — **maturin 1.14** (the
  wheel builder, `M0`) needs **≥ 1.89**, and **redis 1.2** (`M7`) needs **≥ 1.88** —
  so install a **recent stable ≥ 1.89** to be safe. `M0` adds a `rust-toolchain.toml`
  pinning one exact version for everyone.
  - 🆕 Concept: a `rust-toolchain.toml` pins one Rust version *per repository*.
    *Edition* (the language dialect, `2024`) and *compiler version* (`1.96`) are
    different knobs; edition 2024 just needs the compiler ≥ 1.85.
- [ ] **Enable `rust-analyzer`** in your editor. This is your live type-checker and
  the #1 way to catch mistakes before building.
  - ✅ Done when: opening a `.rs` file shows inline type hints, and a deliberate typo
    gets a red squiggle.
- [ ] **Create a Python virtual environment** for the SDK side:
  `python -m venv .venv && source .venv/bin/activate`. You'll `pip install maturin`
  and `pip install "pydantic>=2.13,<3"` into it when you build the wheel in `M0`.
  Python **≥ 3.10** is a safe floor (matches the `abi3-py310` wheel target).
- [ ] (Optional) Install **Rustlings** for side practice: `cargo install rustlings`.

### Later — local dev backends (Docker; **not needed until `M5`+**)

Everything through `M4` runs with **zero external services** — your first working
agent (`M3`) reads a **local Parquet file** through a dev connector, and the store
traits ship **in-memory dev impls** first. The Docker backends below only swap in
behind those same traits when you reach the distributed milestones.

- **MinIO** (S3-compatible object store, for `M5`/`M8`): run the `minio/minio` image
  (ports `9000`/`9001`). Your S3 client points at `http://localhost:9000` with
  `force_path_style(true)` (required for MinIO).
  - verify: MinIO is non-versioned by default — if you rely on S3 `version_id` for
    the Versioned freshness token, enable bucket versioning or hash the `ETag`
    instead. Confirm when you reach `M5`.
- **Redis** (for `M7`): `docker run --name droplet-redis -p 6379:6379 -d redis:8`
  (check with `docker exec -it droplet-redis redis-cli ping` → `PONG`). The client
  crate is `redis` **1.x**, so ignore any tutorial pinning `redis = "0.25"`.
- **DynamoDB Local** (the other coordination backend, `M7`):
  `docker run -p 8000:8000 -d amazon/dynamodb-local`; point the DynamoDB client at
  `http://localhost:8000` (it still needs *some* fake credentials in the env).
- **Athena** (`M6`) needs a real AWS account (Athena + an S3 bucket for `UNLOAD`
  output) — there's no faithful local emulator. `M6` shows how to keep the dev
  local-Parquet connector as the default so you only need AWS when you choose to.

---

Ready? Start with **[`00-rust-warmup.md`](./00-rust-warmup.md)**. Take it one
checkbox at a time — and `git commit` at the end of every chunk. You've got this.
