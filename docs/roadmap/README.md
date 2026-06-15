# Droplet v1 — Build Roadmap

> **This roadmap was rewritten.** The project **pivoted**. Any earlier plan you
> may remember — a **Polars** query engine, **SurrealDB used as the write/storage
> store**, or a Polars↔Surreal **"Arrow seam"**, or **single-process / local-only
> snapshots** — is **obsolete**. Forget it. The new v1 query engine is **DuckDB**,
> the object store is **S3**, coordination lives in **Redis/DynamoDB**, snapshots
> are **content-addressed in a shared store and resumable on any machine**, and
> **SurrealDB is read-only** (only for semantic field search). If a file in this
> folder still mentions Polars, SurrealDB-as-storage, an "Arrow seam", or
> local-only snapshotting (e.g. the old `M1-polars.md`, `M2-surrealdb.md`,
> `M3-arrow-seam.md`, `M4-snapshot-resume.md`, `M5-stubs-dx.md`, `M6-python-polish.md`),
> it is a **stale leftover** — the files in the table below (`00`, `M0`…`M8`) are
> the real plan.

---

## What you're building (in plain words)

You're building **Droplet**: a small **distributed** runtime that lets an AI agent
do data analysis over a company's data on **S3**, safely, across a fleet of
servers.

Here's the whole idea in one breath. The agent writes a little bit of **Python**.
That Python doesn't run loose on your machine — it runs **sandboxed** inside an
embedded interpreter called **Monty**. The sandboxed code can only call a tiny,
**typed** set of tools — `run_sql`, `search_fields`, `describe_schema`,
`list_tables`, `export`. When the code calls `run_sql`, the Python *pauses*, your
Rust host runs the SQL with **DuckDB** (reading Parquet/CSV straight from S3),
caps the number of rows, and hands a small result back. Expensive query results
are **materialized** once into a **content-addressed cache** on S3 — "content
addressed" just means *the file's name is a hash of its bytes*, so the same query
result is stored once and reused by every server (we call one server a **pod**)
instead of re-scanning S3. Finally, a run can be **snapshotted** (the
interpreter's bytes + a small manifest) and **resumed on any pod**, because all
the durable state lives in a **shared plane** (S3 + Redis/DynamoDB), not on one
machine.

The payoff: an agent can search for the right column, run an aggregation, get a
**wrong column name caught by the type checker *before* the SQL even runs**, have
the result cached fleet-wide, and have the whole session survive a move to a
different pod. That's v1.

You already program (Python), so you know loops, functions, and types. What's new
is **Rust** — a compiled language with strict rules about who owns what memory.
That strictness is exactly what makes the sandbox safe, and this roadmap teaches
it one small step at a time.

The full spec is **`PRODUCT.md` at the repo root** (it is `PRODUCT.md`, *not*
`docs/PRODUCT.md`). It is the source of truth; this roadmap just teaches you how
to build it, one tiny step at a time.

---

## How to use this roadmap

- [ ] **Go top to bottom.** Do the warm-up first, then `M0`, then `M1`, and so
  on. Each milestone builds on the last; skipping ahead will leave you stuck.
- [ ] **One chunk per sitting.** Every file is split into "### Chunk N" sections.
  A chunk is ~30–90 min of small steps and ends at a natural "it compiles and a
  test passes" checkpoint. Do a chunk, then stop if you want — it's a save point.
- [ ] **The checkboxes are your save-game.** Every `- [ ]` is one tiny task
  (~10–30 min, one new idea). Tick it (`- [ ]` → `- [x]`) the moment its
  "✅ Done when" check passes. When you come back, the first empty box is exactly
  where you resume. Don't tick a box you haven't verified.
- [ ] **Don't infer middle steps in the DEEP files** (warm-up, `M0`, `M1`): they
  spell out every step, including creating a file or adding a single dependency.
  If you ever feel you're guessing, re-read — the step is probably there.
- [ ] **The SKETCH files** (`M2`–`M8`) are coarser on purpose. When you *reach*
  one, your first job is to expand its chunks into tiny steps the way `M0`/`M1`
  are written.

---

## The build / learn loop

You are a programmer who knows some Python but is **new to Rust**. That's fine —
you'll learn Rust by building, not by reading a whole book first. The loop for
every single step is the same rhythm:

```
edit  →  cargo build  →  cargo test  →  tick the box  →  git commit
```

1. **edit** one small thing (a struct field, a function, a `#[test]`). One idea
   at a time.
2. **`cargo build`** — does it compile? Rust's compiler is your strictest, most
   helpful teacher; the errors are long but they tell you the fix. A green build
   already means a *lot* is correct.
3. **`cargo test`** — does the behavior match what you expected?
4. **tick the box** once "✅ Done when" passes (build + test green).
5. **`git commit`** at the end of a chunk so you can always roll back. Commit
   `Cargo.lock` too — Droplet produces binaries *and* pins fast-moving git deps
   (Monty), so the lockfile belongs in git.

Prefer **test-first** where it's natural: write a tiny `#[test]`, run it, watch it
**fail** (red), then write just enough code to make it **pass** (green). Seeing
the failure first proves the test actually tests something.

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
- **SurrealDB** jumped from 2.x to 3.x; most online snippets are 2.x and will
  mislead you (e.g. the **MTREE** vector index was *removed* in 3.0 — use **HNSW**;
  the in-memory engine type is `surrealdb::engine::local::Mem`, **not**
  `engine::mem::Mem`). The roadmap pins `3.1.4`.
- **PyO3** renamed `Python::allow_threads` → `Python::detach` in 0.26 with **no
  deprecated alias**, so any tutorial using `allow_threads` won't compile against
  the pinned `0.29`. (Likewise `with_gil` → `attach`.)
- **DuckDB ↔ Arrow** versions are coupled: the `duckdb` crate pins a specific
  `arrow` major (currently `58`), and the latest `arrow` on crates.io is newer
  (`59`). Don't add `arrow` yourself blindly — `M1` walks you through this.

---

## The roadmap files (read in this order)

**DEEP** = exhaustively granular, every tiny step spelled out (good for when
you're new). **SKETCH** = chunk-level tasks you'll expand into tiny steps once you
reach them.

| # | File | What it covers | Depth |
|---|------|----------------|-------|
| 0 | [`00-rust-warmup.md`](./00-rust-warmup.md) | Rust fundamentals: ownership, `Result`/`?`, **enums**, **traits + generics** (practice for the four store traits), **async + Tokio**, `Arc`/`Mutex`, and a light intro to **hashing / content-addressing**. Throwaway scratch crate. | **DEEP** |
| 1 | [`M0-skeleton.md`](./M0-skeleton.md) | Build step 1. Workspace `[workspace.dependencies]` + `DropletError` (`thiserror`) + handle registry + `Session` + the **four store traits** (`Source`, `ArtifactStore`, `SnapshotStore`, `CoordinationStore`) with in-memory/local dev impls + the `droplet-py` wheel (maturin) + a Monty smoke test + CI. | **DEEP** |
| 2 | [`M1-duckdb.md`](./M1-duckdb.md) | Build step 2. **DuckDB** engine: `run_sql`, reading S3 via `httpfs`, capped **Arrow** results, `spawn_blocking`, releasing the GIL. | **DEEP** |
| 3 | [`M2-artifact-cache.md`](./M2-artifact-cache.md) | Build step 3. `ArtifactStore` (S3) + **materialization** + **content-addressed cache** + freshness policy (Versioned/TTL/Passthrough) + cache index. | SKETCH |
| 4 | [`M3-coordination.md`](./M3-coordination.md) | Build step 4. `CoordinationStore`: **Redis** then **DynamoDB**; run registry + **leases**. | SKETCH |
| 5 | [`M4-monty-driver.md`](./M4-monty-driver.md) | Build step 5. The **`run_code` loop** + external-function tool surface (suspend/resume) + **type-check-before-run**. | SKETCH |
| 6 | [`M5-pydantic-schema.md`](./M5-pydantic-schema.md) | Build step 6. **Pydantic models → DuckDB types** + typed tool signatures + type stubs. | SKETCH |
| 7 | [`M6-field-search.md`](./M6-field-search.md) | Build step 7. **Read-only SurrealDB** vector field search + `search_fields`. | SKETCH |
| 8 | [`M7-snapshot-store.md`](./M7-snapshot-store.md) | Build step 8. `SnapshotStore` (S3): REPL bytes + manifest, **zstd**, content-addressed, write-behind; **cross-pod resume** rebuilding DuckDB from the manifest; lease-guarded. | SKETCH |
| 9 | [`M8-sdk-adapter.md`](./M8-sdk-adapter.md) | Build step 9. Python SDK polish + **pydantic-ai** adapter + a **two-pod** distributed integration test. | SKETCH |

---

## 🏆 Golden rules (the 10 invariants, in beginner words)

These are the load-bearing promises from `PRODUCT.md` §8. Don't break them — each
file will remind you with a **⚠️ Invariant** note when it's relevant.

1. **Keep Python out of the core.** `droplet-core` (pure Rust) must **never**
   depend on `pyo3`. All the Python-bridge code lives only in `droplet-py` (a
   `cdylib` wheel). `droplet-core` must build and test as plain Rust.
2. **No agent framework in the core.** The core depends on the `monty` crate +
   engines, never on an agent framework. `pydantic` is an SDK-layer schema thing;
   framework glue (e.g. pydantic-ai) lives in separate adapter packages.
3. **Snapshots are tiny: REPL bytes + a manifest, nothing else.** Never serialize
   an engine's memory (its "heap"). On resume you **rebuild** DuckDB from the
   manifest (re-attach source views + materialized artifacts). Snapshots are
   immutable, content-addressed, **versioned** (snapshot-format + schema), and
   zstd-compressed.
4. **Boundary discipline.** Big data stays inside DuckDB behind an opaque
   **handle** in a host-owned registry; only *result-returning* tools move rows
   into the sandbox, and always **capped**. The sandbox sees handles, not data.
5. **SurrealDB is read-only and schema-derived.** You build the field-search
   index once at startup and only ever read from it. Never snapshot it — rebuild
   it on resume.
6. **DuckDB is synchronous → wrap it in `spawn_blocking` and release the GIL**
   (`py.detach(...)`) while a query runs, so you don't freeze the async runtime or
   block other Python threads.
7. **Respect Monty's limits.** It's a *subset* of Python: no third-party imports,
   no classes, limited stdlib. So the tool surface is **flat typed functions** (no
   modules/classes), and you **type-check before you run**.
8. **Distributed by default.** Durable state lives in the shared plane: immutable
   data is content-addressed in the object store (S3); mutable coordination (run
   registry, leases, cache index) is in the consistent store (Redis/DynamoDB).
   Resume is **lease-guarded**; no pod affinity.
9. **Per-run isolation.** One run = one `Session` = its own ephemeral DuckDB + a
   working dir wiped on close; S3 credentials scoped per session; tool paths
   confined to the session dir / allow-listed sources.
10. **One error type at the boundary.** Libraries use **`thiserror`** (a tidy
    `DropletError` enum); binaries use **`anyhow`**. Every engine error folds into
    `DropletError`.

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
  version — go check the crate's source/docs *first*, before relying on it. Treat
  it as "trust but confirm". You'll see these most around **Monty** (`v0.0.18`
  API + postcard snapshot format), the **DuckDB↔arrow** major pairing, the
  **`ty`** type checker (Monty bundles it; you don't add it separately), and
  **SurrealDB** vector DDL params.

---

## Recommended setup

You don't need everything at once. Set up the Rust toolchain now; add the local
dev backends only when you reach the milestone that needs them.

### Now (for the warm-up, `M0`, `M1`)

- [ ] **Install Rust via `rustup`** (gets you `rustc`, `cargo`, and lets the repo
  pin a toolchain): `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`,
  then restart your terminal.
  - ✅ Done when: `rustc --version` and `cargo --version` both print a version.
- [ ] **Know the toolchain floor.** The repo targets **edition 2024**, which needs
  **Rust ≥ 1.85**. Later milestones pull crates with higher floors — **maturin
  1.14** (the wheel builder, `M0`) needs **Rust ≥ 1.89**, and **redis 1.2** (`M3`)
  needs **≥ 1.88** — so install a **recent stable ≥ 1.89** to be safe (the repo
  machine's `1.96.0` is fine). `M0` adds a `rust-toolchain.toml` pinning one exact
  version for everyone.
  - 🆕 Concept: a `rust-toolchain.toml` pins one Rust version *per repository*, so
    everyone (and future-you) builds with the same compiler. *Edition* (the
    language dialect, `2024`) and *compiler version* (`1.96`) are different knobs;
    edition 2024 just needs the compiler to be ≥ 1.85.
- [ ] **Enable `rust-analyzer`** in your editor (VS Code: the "rust-analyzer"
  extension by *The Rust Programming Language*; other editors via LSP). This is
  your live type-checker and the #1 way to catch mistakes before building.
  - ✅ Done when: opening a `.rs` file shows inline type hints, and a deliberate
    typo gets a red squiggle.
- [ ] **Create a Python virtual environment** for the SDK side later:
  `python -m venv .venv && source .venv/bin/activate`. You'll `pip install maturin`
  and `pip install "pydantic>=2.13,<3"` into it when you build the wheel in
  `M0`/`M5`. Python **≥ 3.10** is a safe floor (it matches the `abi3-py310` wheel
  target `M0` uses).
- [ ] (Optional but recommended) Install **Rustlings** for side practice:
  `cargo install rustlings`. Use it whenever a concept won't stick.

### Later — local dev backends (Docker; **not needed until M2+**)

These let you run the distributed pieces on your laptop without touching real AWS.
**Skip them until the milestone calls for them.** You can build and pass every
test in the warm-up, `M0`, and `M1` with **zero** external services — the four
store traits ship **in-memory/local dev impls** first, exactly so you can develop
offline. The Docker backends only swap in behind those same traits later.

- **MinIO** (an S3-compatible object store, for `M2`/`M7`): run the `minio/minio`
  image (ports `9000` API / `9001` console). Your S3 client points at
  `http://localhost:9000` with `force_path_style(true)` (required for MinIO).
  - verify: MinIO is non-versioned by default — if you rely on S3 `version_id` for
    the Versioned freshness token, enable bucket versioning or hash the `ETag`
    instead. Confirm when you reach `M2`.
- **Redis** (for `M3`, the coordination store — Redis path):
  `docker run --name droplet-redis -p 6379:6379 -d redis:8`
  (verify the PONG: `docker exec -it droplet-redis redis-cli ping`). Note the
  client crate is `redis` **1.x** (it left the long-running `0.2x` series), so
  ignore any tutorial pinning `redis = "0.25"`.
- **DynamoDB Local** (the other coordination backend, for `M3`):
  `docker run -p 8000:8000 -d amazon/dynamodb-local`; your DynamoDB client points
  at `http://localhost:8000`. Local DynamoDB still needs *some* (fake)
  credentials set in the environment before the SDK will talk to it.

---

Ready? Start with **[`00-rust-warmup.md`](./00-rust-warmup.md)**. Take it one
checkbox at a time — and `git commit` at the end of every chunk. You've got this.
