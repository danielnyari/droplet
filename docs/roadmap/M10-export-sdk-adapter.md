# M10 — Governed `export` + Python SDK polish + pydantic-ai adapter + distributed test (SKETCH)

**Milestone goal:** add the last tool — a **governed `export`** that writes a result to Parquet on S3
under a destination allow-list, scoped credentials, schema validation, and an audit record — then
round out the Python SDK (`Catalog`, `Session`, `run_code` / `run_async`, and a single **backend
config** that picks S3 / Redis / DynamoDB in prod or the in-memory dev stores), ship the **thin**
pydantic-ai adapter as a *separate* package, and prove the whole v1 thesis with a **two-pod
distributed integration test**: a second pod reuses the cached `load` unload instead of re-hitting the
source, and a different pod resumes a snapshot by rebuilding DuckDB from the manifest.

**Done when (the v1 Success Criteria, [`PRODUCT.md`](../../PRODUCT.md) §20):** from the Python SDK, no
agent framework required, in a multi-pod deployment — register a catalog with an **Athena-backed**
dataset, then have an agent `load` a **scoped slice** (one Athena `UNLOAD`, cached fleet-wide) and
write a **multi-step local analysis** program over the local copy — group, derive, branch, score,
rank — **touching Athena zero further times**, with a **wrong field caught at type-check before
execution**, the session **snapshotted** to the shared store and **resumable on a different pod** that
**rebuilds DuckDB from the manifest**, and a **second run on another pod reusing the cached unload**
instead of re-hitting Athena.

**Prerequisite:** finish [`M8-snapshot-resume.md`](./M8-snapshot-resume.md). You need the
`SnapshotStore`, the write-behind snapshot + cross-pod resume, and lease-guarded resume already
working — and behind it, the cached `load` boundary ([`M5`](./M5-artifact-cache.md)/[`M6`](./M6-connectors-athena.md)),
the coordination store ([`M7`](./M7-coordination.md)), and read-only field search
([`M9`](./M9-field-search.md)). This milestone *wraps and proves* what M0–M9 built; the only genuinely
new engine code is the `export` write path (Chunk 1) and the `run_async` bridge (Chunk 5).

**Estimate:** ~13 chunks (one sitting each).

> This is a **SKETCH** file: chunk-level checkboxes with concept notes and invariant callouts, *not* the
> tiny per-line steps of M0/M1. Get the shape right first, then expand into tiny steps when you arrive.

> Legend (same as the other files): 🆕 = a new concept the first time it appears in this file, ✅ = an
> observable check, ⚠️ = a [`PRODUCT.md`](../../PRODUCT.md) §15 invariant (numbered 1–10, listed in the
> [README](./README.md) Golden Rules) you must not break, 🔗 = the Droplet concept the exercise unlocks,
> **verify:** = a fact the research couldn't fully pin to the locked version — check the crate
> source/docs first.

---

> 🧭 **What M10 actually is (read first, 5 min).** By the end of M9 almost every *piece* works: the
> cached `load` boundary (Athena `UNLOAD` → Parquet on S3, content-addressed, reused fleet-wide), the
> local DuckDB analyze surface (`filter_rows`, `group_agg`, `join`, `local_sql`, `to_rows`, …), the
> Monty `run_code` loop with type-check-before-run, the `#[droplet_tool]`-generated + catalog-derived
> typed tool surface, read-only Surreal field search, the three shared stores, and snapshot/resume.
> M10 does **four** jobs and almost no new engine work:
> 1. **Add the governed `export` tool** — the *one* write-side tool: take a local result handle and write
>    it as Parquet to S3, but only to an **allow-listed destination**, with **scoped credentials**, after
>    **schema validation**, leaving an **audit record**. This is the last item in the §10 tool surface and
>    the second governed boundary (load is the first).
> 2. **Polish the Python SDK** so a human (or an adapter) drives Droplet with `Catalog` → `Session` →
>    `run_code` / `run_async`, and a single backend-config object decides whether the three stores are
>    real cloud (S3 / Redis / DynamoDB) or the in-memory dev impls from M0.
> 3. **Write the pydantic-ai adapter** — a *separate package* under `adapters/droplet-pydantic-ai/` that
>    turns Droplet's typed tool surface into pydantic-ai tools. It must be thin and optional.
> 4. **Write the distributed integration test** — two pods (two processes) sharing the same cache +
>    coordination store, proving cached-unload reuse and cross-pod resume. This is the v1 acceptance gate
>    (§20).
>
> ⚠️ **Invariant 8:** *keep Python — and frameworks — out of the core; framework integrations are
> SEPARATE adapter packages.* pydantic-ai is **never** a dependency of `droplet-core`, `droplet-py`, or
> the base `droplet` Python package — it lives only in `adapters/droplet-pydantic-ai/`, which depends on
> `droplet`, not the other way round. This is the single most important rule of this milestone.

> 🧭 **The two governed boundaries.** Droplet has exactly two doors to the outside world, and they are
> the *only* two: **`load`** reads a bounded, typed, cached slice *in* from a source (M2/M5/M6), and
> **`export`** writes a validated result *out* to an allow-listed S3 destination (this milestone).
> Everything between them — the entire analyze surface — is local, throwaway, and unrestricted *because*
> it cannot reach either door on its own. When you build `export`, you are building the second (and last)
> guarded boundary, and it must be governed exactly the way `load` is.

> 🧭 **The async split, one more time.** The genuinely new Rust code in M10 is `export`'s write path
> (Chunk 1) and the `run_async` bridge (Chunk 5). Everything else is Python glue in `python/droplet/`
> over the compiled `_droplet` module, plus a separate Python adapter package and a test harness. DuckDB
> stays sync behind `spawn_blocking`; the S3 write in `export` and Surreal/Redis/DynamoDB all stay
> `.await`-ed on the host Tokio runtime.

---

### Chunk 1 — The governed `export(source, dest, schema) -> ExportResult` tool

- [ ] Add `export(source, dest, schema) -> ExportResult` to the tool surface: it takes a **local result
  handle** (`source` — a `Dataset` produced by the analyze surface, *not* a source-engine reference),
  writes it as **Parquet to S3** at the requested destination, and returns an `ExportResult` (the
  written object key/URI, row count, byte size, schema fingerprint). The write lands in
  `droplet-core` (pyo3-free, behind the `Source`/`ArtifactStore`-adjacent write path) and is exposed
  through the `#[droplet_tool]` macro like every other primitive. This is the **only** tool that writes
  outside the pod.
  - 🆕 Concept: `export` is the **mirror image of `load`**. `load` is the only *read* door to a source;
    `export` is the only *write* door to a destination. Both are **governed**; everything between them is
    local and unrestricted. (No Rust Book chapter — the §10/§14 boundary design.)
  - ⚠️ Invariant 2 & 3: the `source` argument is a **local `Dataset` handle**, never a source-engine
    reference. `export` writes the *local result*; it cannot and must not pull fresh data from a
    production engine. Big data stays inside DuckDB behind the handle until the write streams it out.
  - ⚠️ Invariant 6 (boundary discipline): `export` moves the full result to S3 **straight from DuckDB**
    (DuckDB's `COPY (...) TO 's3://…' (FORMAT PARQUET)` over the handle), so the big frame **never enters
    the Monty sandbox**. The sandbox only ever sees the small `ExportResult` (a key + counts), not the
    rows. This is the same handle discipline as `to_rows`/`scalar`, applied to the write side.
  - ⚠️ Invariant 8 (Python out of the core): the write path is `droplet-core` Rust; the S3 client and the
    audit write live in core (pyo3-free). Only the thin SDK wrapper is added on the Python side.
  - 🔗 Maps to: `export(source, dest, schema) -> ExportResult` — the governed write in §10 / §14.
  - **verify:** whether you write via DuckDB's own `COPY … TO 's3://…'` (needs the `httpfs` extension
    `INSTALL`/`LOAD` at runtime + S3 creds set on the DuckDB connection) **or** materialize to a local
    Parquet file first and `put_object` it with `aws-sdk-s3`. Both are valid; the DuckDB-native `COPY`
    keeps the big frame out of host memory entirely (preferred per Invariant 6). Confirm the `httpfs`
    `COPY … TO` S3 syntax + credential-passing on `duckdb = "1.10503"` before committing to it. Recall
    `httpfs` is a **runtime** `INSTALL/LOAD`, *not* a Cargo feature.
  - ✅ Done when: a `run_code` that builds a small `group_agg` result then calls
    `export(result, dest="s3://…/out", schema=MySchema)` writes a readable Parquet object to S3 (MinIO in
    dev) and returns an `ExportResult` whose row count matches the result — with no rows having crossed
    into the sandbox.

### Chunk 2 — Govern the destination: allow-list, scoped creds, schema validation, audit

- [ ] Make `export` **governed** the way §14 demands, with four guards enforced **in `droplet-core`
  before the write**, not in the SDK:
  1. **Destination allow-list** — the `dest` URI must match a configured allow-list (bucket/prefix
     patterns the catalog/policy permits). A destination outside the list is rejected before any bytes
     move.
  2. **Scoped credentials** — the write uses **session-scoped** S3 credentials (write access only to the
     allow-listed prefixes), never broad ambient creds. Same per-session scoping `load` uses on the read
     side.
  3. **Schema validation** — the result's actual column names/types are validated against the declared
     `schema` (the Pydantic-derived export schema) **before** writing; a mismatch is an error, not a
     silently-wrong file.
  4. **Audit record** — every successful (and every *rejected*) export writes an audit entry (who/which
     run, dest, schema fingerprint, row/byte counts, timestamp) to the coordination store / an audit
     sink.
  - 🆕 Concept: a **governed boundary** is a door with a guard that runs *before* the action — allow-list
    + scoped creds + validation + audit. `load` already works this way (scoped, typed, cached); `export`
    is the symmetric write-side guard. The guard lives in **core**, so no SDK caller (and no adapter) can
    bypass it. (No Rust Book chapter — the §14 governance model.)
  - ⚠️ Invariant 2: "Only `load` touches the source — bounded/typed/governed; no arbitrary writes/SQL
    against production." `export` extends that discipline to the write side: it is the *only* write tool
    and it is governed; there is no escape hatch for an unvalidated or off-allow-list write.
  - ⚠️ Invariant 10 (one error type): a disallowed destination, a creds-scope violation, or a schema
    mismatch all fold into `DropletError` variants — surfaced to the agent as a typed, retryable tool
    error, never a panic and never a partial write.
  - ⚠️ Invariant 7 (distributed by default): the audit record is **mutable coordination state**, so it
    goes in the consistent store (Redis/DynamoDB) or a dedicated audit sink — not on pod-local disk that
    a stateless pod would lose.
  - 🔗 Maps to: §14 "**Export is governed** — destination allow-list, scoped creds, schema-validated,
    audited."
  - **verify:** how you derive the **scoped write credentials** — STS `AssumeRole` with a session policy
    limiting `s3:PutObject` to the allow-listed prefixes is the clean answer (`aws-sdk-sts`, `verify:`
    version), but a narrowed static key in dev is acceptable. Confirm the session-policy shape and that
    `aws-config`'s credential provider picks up the assumed role on the locked `aws-config`/`aws-sdk-s3`
    versions (`verify:`).
  - ✅ Done when: an `export` to an **allow-listed** dest succeeds and writes an audit row; an `export` to
    an **off-list** dest is rejected with a `DropletError` **before** any S3 write, and that rejection is
    also audited; an `export` whose result columns don't match `schema` fails validation with no object
    written.

### Chunk 3 — `Catalog.register`: register Pydantic models + Athena-backed datasets

- [ ] In `python/droplet/`, give `Catalog` a `register(*models, sources=...)` method that takes the
  Pydantic models (the schema DSL) and the **dataset → connector** descriptors (an **Athena** dataset for
  the v1 success criterion, alongside the S3/Iceberg direct-read ones) and hands them down to
  `droplet-core` via the compiled `_droplet` module. This is the one entry that turns the catalog into
  everything downstream: DuckDB column types for the local copy, the read-only field-search index, the
  typed tool signatures, and the type stubs (the M4–M9 machinery — you are just exposing it on the SDK
  surface here).
  - 🆕 Concept: the **SDK layer** is plain Python in `python/droplet/` that wraps the compiled `_droplet`
    extension module (the `droplet-py` cdylib). The Python you write here is ergonomic glue; the work
    happens in Rust behind the PyO3 boundary. (No Rust Book chapter — the Python side.)
  - ⚠️ Invariant 8: `pydantic` is the **SDK-layer** schema DSL — it lives **only** at the SDK layer, never
    in `droplet-core`. `Catalog.register` is exactly where Pydantic models cross into Rust as *derived
    schema* (DuckDB types + stub text + field-search rows), not as a `droplet-core` dependency.
  - ⚠️ Invariant 1: the agent registers **logical datasets**; the engine binding (Athena vs S3 vs
    Iceberg) is config it never sees. `register` records the binding host-side; the generated tool surface
    exposes only the logical dataset name.
  - ⚠️ Invariant 4: the tool surface generated from the catalog is **flat typed functions** (Monty has no
    classes/modules) — `load`, the analyze prims, `search_fields`, `list_datasets`, `describe_dataset`,
    `export`. Don't expose a class-namespaced API to the model.
  - 🔗 Maps to: `Catalog.register(*models, sources=...)` — the catalog entry from §9.
  - ✅ Done when: `Catalog().register(MyModel, sources=[athena(...), s3_parquet(...)])` returns without
    error and a follow-up `list_datasets()` shows the registered datasets (logical names only).

### Chunk 4 — `Session`: one durable, ephemeral run context

- [ ] Expose `Session` on the SDK so a `Catalog` produces a `Session` per run. The `Session` owns an
  ephemeral DuckDB, a read-only Surreal handle, its manifest, and its snapshot lifecycle (all already
  built in `droplet-core` — you are surfacing them). Make sure closing a `Session` wipes its working dir.
  - 🆕 Concept: a Python **context manager** (`with Session(...) as s:`) is Python's RAII — `__enter__`
    sets up, `__exit__` tears down (wipes the dir, closes handles) even on error. Use it so per-run cleanup
    is automatic. (No Rust Book chapter — Python side; the Rust `Drop` equivalent already lives in
    `droplet-core`.)
  - ⚠️ Invariant 3 & the §14 per-run isolation: "one run = one `Session` = ephemeral DuckDB on pod-local
    tmpfs + a unique working dir, wiped on close; no cross-session shared mutable state." The SDK `Session`
    must enforce this — a new working dir per session, wiped on close; no two sessions share a DuckDB.
  - 🔗 Maps to: `Session` — "one durable, ephemeral analysis context per run" (§3/§14).
  - ✅ Done when: two `Session`s opened from the same `Catalog` get distinct working dirs, closing one
    leaves the other's dir intact, and the closed session's dir is gone.

### Chunk 5a — `run_code` on the SDK (sync), type-check-before-run wired through

- [ ] Expose `Session.run_code(code) -> Result` synchronously: it runs the agent's Python in Monty
  against the session, **type-checked first** (retry on type error), runs the multi-step local analysis,
  returns capped results, and snapshots after the step. Almost all of this is the M3/M5/M8 path — this
  chunk is making it one clean SDK call.
  - ⚠️ Invariant 2 & 3: inside `run_code`, the agent's program may `load` **once** (cached) and then run
    unlimited local analysis (`group_agg`, derive with `with_column`, branch in Python over `to_rows`,
    score, rank) — **touching the source zero further times**. The SDK must not offer any per-step path
    back to a source engine.
  - ⚠️ Invariant 4 (type-check before execution): the typed tool surface is **auto-generated from the
    catalog**, so a wrong **field name** (e.g. a column that isn't in the dataset's catalog-derived,
    schema-derived `Literal`) fails the type check *before* any DuckDB work and triggers the model-retry
    path — this is the Success-Criteria "wrong field caught at type-check before execution."
  - ⚠️ Invariant 6 (boundary discipline): "only `to_rows`/`scalar`/load-samples move rows into the sandbox,
    capped." Whatever `run_code` returns to Python is already capped host-side — never the full frame.
  - 🔗 Maps to: `run_code(code) -> Result` (§3, §8).
  - ✅ Done when: a `run_code` that `load`s a slice then runs a multi-step local analysis returns a small
    capped ranked result; a `run_code` referencing a non-existent field raises a type error *before* any
    `load` or DuckDB query runs.

### Chunk 5b — Confirm the GIL is released around the DuckDB work

- [ ] In `droplet-py`, confirm the `_droplet` entry that `run_code` calls **releases the GIL** around the
  blocking DuckDB work, with `py.detach(move || { ... })`. While `droplet-core` does the DuckDB analysis
  (sync, on `spawn_blocking`), `droplet-py` must hand the GIL back so other Python threads run.
  - 🆕 Concept: the **GIL** (Global Interpreter Lock) lets only one thread run Python at a time. Holding it
    during slow Rust work blocks every other Python thread; `py.detach(|| ...)` hands it back for the
    duration of the closure. (No Rust Book chapter — PyO3-specific.)
  - ⚠️ Invariant 9: "DuckDB is synchronous → `spawn_blocking` + release the GIL during query execution."
    `run_code` must not hold the GIL across a DuckDB analysis pass.
  - ⚠️ Invariant 8: this `py.detach` call lives in **`droplet-py`** only — `droplet-core` stays pyo3-free.
    The GIL is a wheel-layer concern, not a core concern.
  - ⚠️ The `detach` closure must be **Ungil**: move only owned Rust data in (a `String`, `Vec`, etc.),
    return owned data out — never touch the `py` token or a `Bound<'py, _>` inside it (compile error by
    design).
  - **verify:** on `pyo3 = "0.29"` the method is `Python::detach`, **not** the old `allow_threads`
    (renamed in 0.26 with no alias kept). Confirm against the pyo3 0.29 migration guide / CHANGELOG.
  - ✅ Done when: a small `#[test]`/manual check shows a second Python thread makes progress while a
    `run_code` DuckDB pass is in flight (GIL is not held across it).

### Chunk 5c — Add the `pyo3-async-runtimes` dependency (version-locked to pyo3)

- [ ] In `droplet-py`'s `Cargo.toml`, add the async bridge dep, locked to pyo3's minor:
  ```toml
  pyo3-async-runtimes = { version = "0.29", features = ["tokio-runtime"] }
  ```
  It **must** share pyo3's minor (`0.29` ↔ `0.29`) or you link two copies of pyo3 and get confusing
  trait-mismatch errors. It is the *renamed* successor to the abandoned `pyo3-asyncio`; never depend on
  the old name.
  - ⚠️ Invariant 8: this dep is in **`droplet-py`** only — `droplet-core` must stay pyo3-free, so it never
    sees `pyo3-async-runtimes` either.
  - **verify:** the exact `pyo3-async-runtimes` version that pairs with `pyo3 = "0.29"` (it tracks pyo3's
    minor, so `0.29.x`, but confirm the published patch on crates.io). Let `Cargo.lock` hold it.
  - ✅ Done when: `cargo build -p droplet-py` is green with the dep added, and `cargo tree` shows a single
    `pyo3 0.29.x` (not two copies).

### Chunk 5d — `run_async`: the awaitable entry point (PyO3 async bridge)

- [ ] Add `Session.run_async(code)` returning a Python awaitable, built in `droplet-py` with
  `pyo3_async_runtimes::tokio::future_into_py(py, async move { ... })`. The returned `T` must be
  `Send + 'static` and plain data (capped rows or a handle / an `ExportResult`), never a borrowed Python
  object.
  - 🆕 Concept: a Rust `Future` and a Python awaitable are different things; `future_into_py` converts the
    former into something Python can `await`. (Rust Book: *Fundamentals of Asynchronous Programming: Async,
    Await, Futures, and Streams*.)
  - ⚠️ Invariant 9: inside the async path the DuckDB call still goes through `spawn_blocking` in core; you
    `.await` Surreal/S3/Redis on the host runtime but never `.await` a blocking DuckDB pass directly.
  - **verify:** the exact signature/import of `pyo3_async_runtimes::tokio::future_into_py` and whether a
    Tokio runtime must be initialized once (e.g. via `#[pyo3_async_runtimes::tokio::main]`) before first
    use, on the 0.29 line specifically. Check `docs.rs/pyo3-async-runtimes/0.29` when you wire this.
  - ✅ Done when: `python -c "import asyncio, droplet; print(asyncio.run(session.run_async('1 + 2')))"`
    returns the expected value, proving a Rust Tokio future was awaited from Python.

### Chunk 6 — Backend config: select the concrete store impls vs in-memory dev stores

- [ ] Give the SDK one **backend config** object that decides which concrete impl backs each of the three
  store traits, and pass it through `Catalog` / `Session`. The three traits (`ArtifactStore`,
  `SnapshotStore`, `CoordinationStore`) already have multiple impls from M0–M8 — this chunk just lets the
  SDK *choose* between them by config, with sane env-driven defaults. The choices per trait:
  - `ArtifactStore` → **S3** (prod) or the local/in-memory dev impl (M0/M5).
  - `SnapshotStore` → **S3** (prod) or the local/in-memory dev impl (M0/M8).
  - `CoordinationStore` → **Redis** *or* **DynamoDB** (prod) or the in-memory dev impl (M0/M7).
  - 🆕 Concept: this is **dependency injection via a trait object** — in core each store is held as a
    `Box<dyn ArtifactStore>` (etc.), so the config swaps the concrete type without changing any call site.
    (Rust Book: *Object-Oriented Programming Features of Rust* — trait objects.)
  - 🆕 Concept: those store traits have **`async fn` methods used through `Box<dyn _>`**, which is **not**
    dyn-compatible with bare `async fn`. So each trait is annotated `#[async_trait]` (the `async-trait`
    crate, `0.1.89`) — that is what makes `Box<dyn ArtifactStore>` compile for async methods. (Rust Book:
    *Traits: Defining Shared Behavior*.)
  - ⚠️ Invariant 7 (distributed by default): "immutable data is content-addressed in the object store;
    mutable coordination (registry, leases, cache index) is in the consistent store." The **prod default**
    wires S3 + (Redis | DynamoDB); the in-memory stores are a **dev-only** convenience and must NEVER be
    the default in a multi-pod run (two pods with in-memory stores share nothing — the integration test
    would silently pass for the wrong reason).
  - 🔗 Maps to: "backend config" (§3) and §16's note that backends ship as impls behind the traits (+
    local/in-memory for dev).
  - **verify:** the prod store deps and their exact pins when you wire them. From the digest:
    `aws-config = { version = "1.8.18", features = ["behavior-version-latest"] }`,
    `aws-sdk-s3 = "1.136.0"`, `aws-sdk-dynamodb = "1.116.0"`,
    `redis = { version = "1.2", features = ["tokio-comp", "connection-manager"] }`. The `aws-sdk-*` crates
    bump ~weekly — re-check crates.io and let `Cargo.lock` hold the exact versions. (`redis` 1.x needs Rust
    ≥ 1.88, which the edition-2024 workspace already satisfies.)
  - ✅ Done when: the same `run_code` script runs unchanged under (a) the all-in-memory dev config and (b)
    the S3 + Redis (or DynamoDB) config, selected purely by the backend-config object / env.

### Chunk 7 — The pydantic-ai adapter: a THIN, SEPARATE package

- [ ] Create `adapters/droplet-pydantic-ai/` as its own installable package (its own `pyproject.toml`)
  that depends on `droplet` and `pydantic-ai`, and exposes Droplet's typed tool surface as pydantic-ai
  tools. The adapter's whole job is **translation**: take a Droplet `Session`, surface `load` / the
  analyze prims / `search_fields` / `list_datasets` / `describe_dataset` / `export` as pydantic-ai tool
  functions with the **typed signatures Droplet already generated** from the catalog, and route calls
  back into `session.run_code` / the session's tool table.
  - 🆕 Concept: an **adapter package** is a thin shim with its own `pyproject.toml`; installing it is
    opt-in (`pip install droplet-pydantic-ai`). The dependency arrow points adapter → `droplet`, never the
    reverse. (No Rust Book chapter — packaging.)
  - ⚠️ Invariant 8 — the load-bearing rule of this milestone: "the core never depends on an agent
    framework; framework integrations are **separate adapter packages**." Keep `pydantic-ai` out of
    `droplet-core`, `droplet-py`, and base `droplet`; it appears **only** in this adapter's dependencies.
    If you ever feel tempted to `import pydantic_ai` from the base package, stop — that breaks the
    invariant.
  - ⚠️ Invariant 4: the tools you expose to the agent are **flat typed functions** matching the stub
    surface — don't invent a richer namespaced API in the adapter than Monty / the model can use.
  - 🔗 Maps to: "plugs into any agent framework via a thin adapter … one example adapter (pydantic-ai)"
    (§1, §3, §16).
  - **verify:** the current pydantic-ai tool-registration API before writing the shim. As of the pinned
    pydantic-ai, a Python function becomes a tool via the **`@agent.tool`** decorator (gets a `RunContext`)
    or **`@agent.tool_plain`** (no context), via the **`Tool(fn, takes_ctx=...)`** class, or via the
    **`tools=[...]`** kwarg on `Agent`; a whole surface can be grouped in a **`FunctionToolset(tools=[…])`**
    and passed as `toolsets=[…]`. Typed-arg/structured validation is derived from the **function
    signature** (Pydantic), so the generated typed signatures carry straight through. A `FunctionToolset`
    built from the session's tool table is the natural fit for surfacing the whole Droplet surface at once.
    Confirm the exact import paths and that `FunctionToolset`/`takes_ctx` still match at the version you
    pin — the surface has moved between releases.
  - ✅ Done when: a pydantic-ai `Agent` built with a `FunctionToolset` of Droplet's typed tools can call
    `load` / `group_agg` / `export` and complete a turn, and the adapter installs from its own
    `pyproject.toml` with `droplet` + `pydantic-ai` as its only deps.

### Chunk 8 — Prove the adapter is genuinely optional (decoupling test)

- [ ] Write a test that imports and uses base `droplet` **with pydantic-ai NOT installed at all**, plus a
  separate test that exercises the adapter **with** it installed. This guards Invariant 8 mechanically:
  the framework-agnostic core/SDK must work standalone.
  - 🆕 Concept: a **decoupling test** asserts a *negative* — that removing an optional dependency does not
    break the base package. The cleanest form runs base `droplet` in an environment where
    `import pydantic_ai` would fail, and confirms `Catalog` → `Session` → `run_code` (load + local
    analysis + `export`) still works. (No Rust Book chapter — Python packaging.)
  - ⚠️ Invariant 8: "Keep Python frameworks out of the core … never an agent framework." This test is the
    proof.
  - ✅ Done when: the base-package test passes in a venv that has `droplet` but **not** `pydantic-ai`
    installed; the adapter test passes only in a venv that has both.

### Chunk 9 — Stand up the local shared backends (MinIO + Redis / DynamoDB Local)

- [ ] Before the two-pod test, stand up the **real shared stores** locally so both pods can point at the
  same plane: MinIO (S3-compatible, port 9000) for `ArtifactStore` + `SnapshotStore` (and the `export`
  destination bucket), and Redis (port 6379) **or** DynamoDB Local (port 8000) for `CoordinationStore`
  (registry + leases + cache index + the export audit). Create the buckets and the registry/leases table
  by hand so the happy path has something to hit.
  - 🆕 Concept: "pods" share state **only through this plane** — no orchestrator, no session affinity, no
    shared memory. The *only* thing connecting pod A and pod B is bytes in S3 + rows in the coordination
    store. (No Rust Book chapter — the §3/§11 distributed model.)
  - ⚠️ Invariant 7: use the **real** shared stores here, NOT the in-memory dev impls — with in-memory
    stores two pods share nothing and the test proves nothing. Wire the backend config (Chunk 6) to these
    local shared backends.
  - **verify:** local backend wiring from the digest —
    - MinIO: build the **S3 service** config with
      `aws_sdk_s3::config::Builder::from(&shared).endpoint_url("http://localhost:9000").force_path_style(true).build()`
      (`force_path_style` is on the S3 `Builder`, *not* on `aws-config`), then `Client::from_conf(...)`.
    - Redis at `redis://127.0.0.1:6379/`; DynamoDB Local with `.endpoint_url("http://localhost:8000")` on
      the **ddb** service builder.
    - All three still need *some* credentials even locally (env `AWS_ACCESS_KEY_ID` /
      `AWS_SECRET_ACCESS_KEY` for MinIO/DynamoDB Local).
    - MinIO is **non-versioned by default** — if the Versioned freshness token relies on S3 `version_id`,
      either enable bucket versioning or hash the **ETag** instead (always present; treat as an opaque
      change token — multipart/SSE-KMS ETags are *not* a plain MD5).
  - ✅ Done when: a tiny round-trip script `put`s then `get`s a key in MinIO and writes/reads one row in
    Redis (or DynamoDB Local), proving both shared backends are reachable.

> 🧭 **Athena in the two-pod test.** The success criterion (§20) names an **Athena**-backed dataset, but
> Athena has no faithful local emulator and needs a real AWS account (Athena + an S3 `UNLOAD` bucket).
> Two ways to run Chunks 10–13: **(a)** point the Athena connector at a real AWS account (the honest §20
> run — one real `UNLOAD`, then fleet-wide reuse), or **(b)** run the *mechanics* of the test against the
> local-Parquet/S3 dev connector and add the Athena run as a final manual gate. Either way the property
> under test is identical: **one** unload at the source, then **zero** further source contact while the
> cache is reused across pods. State which mode each test run used.

### Chunk 10 — Stand up the two-pod test rig (two processes, shared plane)

- [ ] Build the harness: **two pods = two separate processes** (truer than two threads), each running the
  SDK, both pointed at the **same** MinIO `ArtifactStore` + `SnapshotStore` and the **same** Redis (or
  DynamoDB Local) `CoordinationStore` via the Chunk 6 config. The **same Athena-backed dataset** (or its
  dev stand-in, per Chunk 9's note) is registered against a Pydantic model on each pod.
  - ⚠️ Invariant 3 (per-run isolation): each pod still gets its own `Session` = ephemeral DuckDB + a
    unique working dir; the pods share the *plane*, never a DuckDB or a working dir.
  - 🔗 Maps to: "Stateless pods … its state lives in the shared plane, so any pod can resume it" (§3).
  - ✅ Done when: both pod processes start, each connects to MinIO + Redis (or DynamoDB Local), and each
    can independently register the catalog against the same dataset.

### Chunk 11 — Prove cross-pod CACHED-UNLOAD REUSE (second pod does not re-hit the source)

- [ ] On **pod A**, run the `run_code` step: the agent `load`s a **scoped slice** of the Athena dataset
  (one Athena `UNLOAD` → Parquet on S3), then runs the multi-step **local** analysis over the local copy
  (group / derive / branch / score / rank) — **touching Athena zero further times**. The unloaded Parquet
  **is** the content-addressed artifact in the `ArtifactStore`, and the `cache_key → artifact_key` mapping
  is written to the cache index in the `CoordinationStore`. Then on **pod B**, run the *same* `load` and
  assert pod B **reuses the cached unload instead of issuing a second Athena `UNLOAD`**.
  - 🆕 Concept: the **cache key** is `hash(scoped query + source + freshness_token)`; only `freshness_token`
    varies by policy (Versioned default = the source's version signal / S3 ETags; TTL = `floor(now/ttl)`;
    Passthrough = never cache). Because the key is deterministic across pods, pod B computes the **same**
    key, finds the index entry, and fetches the existing Parquet artifact — **no second unload**. (No Rust
    Book chapter — the M5/M6 cache design.)
  - 🆕 Concept: how to *observe* "did not re-hit the source" — instrument the connector's unload path with
    a counter (a coordination-store key, a log line, or a metric the test reads) and assert pod B issued
    **zero** Athena `UNLOAD`s. The cache hit must be **detectable, not assumed**. (No Rust Book chapter.)
  - ⚠️ Invariant 2: only `load` touches the source, and pod B's `load` resolves entirely from the cache —
    so the source is hit **exactly once**, by pod A. The local analysis on either pod never touches the
    source at all (Invariant 3).
  - ⚠️ Invariant 7: "immutable data is content-addressed in the object store; mutable coordination
    (registry, leases, **cache index**) is in the consistent store." Cached-unload reuse is the literal
    payoff of this split.
  - ⚠️ Invariant 6: the rows that crossed into either pod's sandbox were **capped** (`to_rows`/`scalar`);
    the *unloaded artifact* (the full slice) lives in S3 behind a handle/key, which is what pod B reuses —
    the sandbox never held the big data on either pod.
  - 🔗 Maps to: Success-Criteria "a second run on another pod reusing the cached unload instead of
    re-hitting Athena."
  - ✅ Done when: pod B returns the same ranked analysis result as pod A while the unload counter shows pod
    B issued **zero** Athena `UNLOAD`s (cache hit, served from the artifact).

### Chunk 12 — Prove cross-pod SNAPSHOT RESUME (rebuild DuckDB from the manifest)

- [ ] On **pod A**, snapshot the session after the `run_code` step (REPL bytes + manifest → zstd →
  content-addressed blob in the `SnapshotStore`; `run_id → snapshot pointer` recorded in the registry).
  Stop pod A. On **pod B**, `Session.resume(run_id)`: acquire the **lease**, load the snapshot, **rebuild
  DuckDB from the manifest** (re-attach the loaded-dataset cache keys + any materialized intermediate keys
  as views — *not* a deserialized engine heap), reload the Monty REPL, and continue the run to the same
  final ranked result.
  - 🆕 Concept: **resume rebuilds, it does not deserialize.** The snapshot carries only the Monty REPL
    bytes (postcard) + the manifest (catalog ref, loaded-dataset refs + their cache keys, materialized
    intermediate keys). Resume re-attaches the cached Parquet into a *fresh* ephemeral DuckDB — cheap,
    because the heavy data was never in the snapshot. Read-only Surreal is schema-derived and rebuilt,
    never loaded. (No Rust Book chapter — the M8 snapshot subsystem.)
  - 🆕 Concept: a **lease** is "one active worker per run, short TTL, reassignable — not affinity." Pod B
    takes the lease before resuming so two pods can't drive the same run at once. (No Rust Book chapter —
    the M7 coordination design.) **verify** the exact acquire primitive on your backend: on Redis it is
    `SET key worker NX PX ttl_ms` (via `set_options(...).conditional_set(ExistenceCheck::NX)
    .with_expiration(SetExpiry::PX(ttl_ms))`, decoding the reply as `bool`); on DynamoDB a `put_item` with
    `condition_expression("attribute_not_exists(pk)")` (contention surfaces as
    `ConditionalCheckFailedException`).
  - ⚠️ Invariant 5: "Snapshot = REPL bytes + manifest only; never serialize engine heaps; reconstruct
    DuckDB on resume. Snapshots immutable, content-addressed, versioned, compressed." If resume ever tries
    to deserialize a DuckDB heap, the invariant is broken — it must *rebuild* from the manifest.
  - ⚠️ Invariant 7: "Resume is lease-guarded; no affinity." Pod B must hold the lease before it resumes; a
    third pod attempting the same `run_id` while the lease is held must be rejected / back off.
  - ⚠️ Invariant 5 (versioning): the manifest records `snapshot-format` + catalog version **and** the
    pinned Monty tag (the REPL bytes are postcard, not portable across Monty versions). Resume must
    **refuse a version mismatch loudly** rather than mis-decode. **verify:** confirm the Monty tag is in
    the manifest and the load path checks it — the whole fleet must run one Monty tag (digest pins
    `v0.0.18`).
  - 🔗 Maps to: `Session.resume(run_id)` and Success-Criteria "the session snapshotted to the shared store
    and resumable on a different pod that rebuilds DuckDB from the manifest."
  - ✅ Done when: pod B (a *different* process from pod A, with pod A stopped) resumes `run_id`, rebuilds
    DuckDB from the manifest, and finishes the run with the **same** final ranked result a single
    uninterrupted run would produce — under a held lease.

### Chunk 13 — The v1 acceptance test: stitch it into one green run (§20)

- [ ] Wire Chunks 11 + 12 into a single end-to-end integration test that exercises the full Success
  Criteria from the **Python SDK**, no agent framework: register a catalog with an Athena-backed dataset →
  pod A `run_code` that `load`s a scoped slice (**one Athena `UNLOAD`**) and writes a multi-step local
  analysis (**group / derive / branch / score / rank**, **Athena untouched after the load**) → a **wrong
  field caught at type-check before execution** → **cached-unload reuse** on pod B (zero further Athena
  hits) → **snapshot** on pod A → **resume** on pod B **rebuilding DuckDB from the manifest**,
  lease-guarded → (optionally) a governed `export` of the ranked result to an allow-listed S3 dest. **This
  green test is the v1 acceptance gate.**
  - ⚠️ Invariant 8: this top-level test imports **only** `droplet` (no `pydantic-ai`) — proving the SDK
    drives the whole criterion framework-free.
  - ⚠️ Invariant 1: the Rust under it is `droplet-core` (pyo3-free) wrapped by `droplet-py`; pydantic-ai is
    nowhere in the stack, and the agent never learns the dataset is Athena-backed.
  - 🔗 Maps to: §20 Success Criteria in full.
  - ✅ Done when: one test run reproduces the entire §20 Success-Criteria sentence end-to-end against the
    local shared plane (Athena per Chunk 9's mode note) and passes.

---

## Out of scope — do NOT build these in M10 (or v1 at all)

These come straight from §16 "Deferred" of [`PRODUCT.md`](../../PRODUCT.md). Listed so you don't
accidentally start building them while polishing the SDK — every one is a *later* concern:

- [ ] **More than one adapter.** v1 ships **exactly one** example adapter (pydantic-ai). Do not build
  LangChain / LlamaIndex / OpenAI-Agents adapters — the seam is proven by one.
- [ ] **Iceberg write-back on `export`.** v1 `export` writes **Parquet to S3 only**. The §10 phrase
  "parquet on object storage *or* Iceberg write-back" keeps the seam open, but the Iceberg write path is
  **deferred** — build the governed Parquet-to-S3 write, not the Iceberg writer.
- [ ] **Snowflake + BigQuery connectors.** v1 source connectors are S3 + Iceberg (direct) + **Athena**
  (`UNLOAD`). The `Source` trait exists so Snowflake/BigQuery *can* land later; building one now is scope
  creep.
- [ ] **Orchestrator / scheduler / control plane / observability UI.** The load balancer distributing runs
  is assumed external; Droplet has **no** orchestrator and **no** UI in v1 (no session affinity, no
  scheduler — that's the whole "stateless pods" point).
- [ ] **Managed-tier features** (hosted snapshotting, usage analytics, data catalogue). Build only the
  pluggable backend/telemetry **seams** (which you already have via the trait objects + backend config),
  **not** the features.
- [ ] **A metric / semantic modeling layer beyond field search.** Surreal is read-only, schema-derived
  field search **only** (§16 defers anything richer).
- [ ] **Writable SurrealDB.** It is rebuilt per session and never written to after the one-time build step
  (Invariant 5 / §9).
- [ ] **Incremental / per-call snapshots.** Snapshot granularity is **per `run_code` step**, full REPL +
  manifest — no finer (§12).
- [ ] **Non-Python SDKs.** v1 is Python-only.

> Treat anything above as a "no" for the whole of v1. If a task seems to need one of these, re-read §16 —
> the answer is almost certainly to use the existing seam, not to build the feature.

---

## Notes carried forward

- **`export` is governed, not convenient.** The temptation in Chunks 1–2 is to make `export` "just write
  the file." It is the **second guarded door** (load is the first): allow-list + scoped creds + schema
  validation + audit, enforced in **`droplet-core`** before any byte moves (§14). If a caller could write
  anywhere with ambient creds, you'd have re-opened the production boundary you spent M2–M6 closing.
- **The in-memory stores are a trap in the distributed test.** They are perfect for the M0–M9 unit tests
  and the decoupling test, but the moment you write the *two-pod* test you MUST switch the backend config
  to the real shared stores (MinIO + Redis/DynamoDB Local). With in-memory stores the two pods share
  nothing, so cached-unload reuse and cross-pod resume would "pass" without proving anything. This is the
  easiest way to fool yourself in M10 — wire the shared backends first (Chunk 9).
- **Keep the adapter thin.** The temptation in Chunk 7 is to add convenience features (prompt templates,
  retry policies, result formatting). Resist it — the adapter is a *translation shim*, and anything
  load-bearing belongs in `droplet`, behind the typed tool surface, where every other (future) adapter
  would also get it for free.
- **One unload, observed.** The whole §20 thesis is "one bounded unload to production, then unlimited
  local code." The test only proves it if the **unload counter** is real and asserted (Chunk 11) — an
  un-instrumented "it returned the right number" run can pass while silently re-hitting Athena. Make the
  zero-further-hits property *detectable*.
- **Pin the whole fleet to one Monty tag.** Cross-pod resume only works if every pod runs the identical
  Monty version (the REPL bytes are postcard, not portable across tags). The manifest records the tag
  (digest pins `v0.0.18`) and resume refuses a mismatch — but operationally, deploy one tag fleet-wide.
  verify: this is enforced in the manifest version check from M8; re-confirm before the integration test.
- **`Cargo.lock` committed = reproducible pods.** Both pods (and CI) must build from the same lockfile, or
  a `cargo update` could drift the DuckDB / arrow / Monty / store versions between pods and break resume.
  The workspace produces binaries/wheels, so the lockfile belongs in git. (This matters most for the
  `arrow` pin: it must match DuckDB's pinned major — `arrow ^58` for `duckdb = "1.10503"` — or the
  two-arrow-versions error bites. Never add `arrow` yourself; use the `duckdb::arrow` re-export and re-run
  `cargo tree -i arrow` after any DuckDB bump.)

---

> 📌 When you reach this milestone, expand each chunk into tiny steps the way M0/M1 are written.
