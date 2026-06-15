# M8 — Python SDK polish + pydantic-ai adapter + distributed test (SKETCH)

**Milestone goal:** round out the Python SDK (`Catalog`, `Session`, `run_code` / `run_async`, and a
single **backend config** that picks S3 / Redis / DynamoDB in prod or the in-memory dev stores), ship
the **thin** pydantic-ai adapter as a *separate* package, and prove the whole v1 thesis with a
**two-pod distributed integration test**: a second pod reuses the materialized cache instead of
re-scanning, and a different pod resumes a snapshot by rebuilding DuckDB from the manifest.

**Done when (the v1 Success Criteria, §11 of [`PRODUCT.md`](../../PRODUCT.md)):** from the Python SDK,
no agent framework required, in a multi-pod deployment — register a Pydantic model and an S3 Parquet
source, then an agent `run_code` step does `search_fields` to find the right column and `run_sql` an
aggregation DuckDB reads straight from S3, **with** a wrong column name caught by the type checker
before execution, **the** materialized result written to the shared cache so a second run on a different
pod reuses it instead of re-scanning, **and** the session snapshotted to the shared store and resumable
on a different pod that rebuilds DuckDB from the manifest.

**Prerequisite:** finish [`M7-snapshot-store.md`](./M7-snapshot-store.md). You need the `SnapshotStore`,
the write-behind snapshot + cross-pod resume, and lease-guarded resume already working. This milestone
*wraps and proves* what M0–M7 built; it adds almost no new engine code.

**Estimate:** ~12 chunks (one sitting each).

> This is a **SKETCH** file: chunk-level checkboxes with concept notes and invariant callouts, *not* the
> tiny per-line steps of M0/M1. Get the shape right first, then expand into tiny steps when you arrive.

> Legend (same as the other files): 🆕 = a new concept the first time it appears in this file, ✅ = an
> observable check, ⚠️ = a [`PRODUCT.md`](../../PRODUCT.md) invariant you must not break, 🔗 = the
> Droplet concept the exercise unlocks, **verify:** = a fact the research couldn't fully pin to the
> locked version — check the crate source/docs first.

---

> 🧭 **What M8 actually is (read first, 5 min).** By the end of M7 every *piece* works: DuckDB over S3,
> the content-addressed cache, the coordination store, the Monty `run_code` loop, the Pydantic-typed
> tool surface, read-only Surreal field search, and snapshot/resume. M8 does three jobs and **no new
> engine work**:
> 1. **Polish the Python SDK** so a human (or an adapter) drives Droplet with `Catalog` → `Session` →
>    `run_code` / `run_async`, and a single backend-config object decides whether the three stores are
>    real cloud (S3 / Redis / DynamoDB) or the in-memory dev impls from M0.
> 2. **Write the pydantic-ai adapter** — a *separate package* under `adapters/droplet-pydantic-ai/` that
>    turns Droplet's typed tool surface into pydantic-ai tools. It must be thin and optional.
> 3. **Write the distributed integration test** — two pods (two processes) sharing the same cache +
>    coordination store, proving cache reuse and cross-pod resume. This is the v1 acceptance gate.
>
> ⚠️ **Invariant #2:** *framework-agnostic core; framework integrations are SEPARATE adapter packages.*
> pydantic-ai is **never** a dependency of `droplet-core`, `droplet-py`, or the base `droplet` Python
> package — it lives only in `adapters/droplet-pydantic-ai/`, which depends on `droplet`, not the other
> way round. This is the single most important rule of this milestone.

> 🧭 **The async split, one more time.** The only genuinely new Rust code in M8 is the `run_async`
> bridge (Chunk 4). Everything else is Python glue in `python/droplet/` over the compiled `_droplet`
> module, plus a separate Python adapter package and a test harness. DuckDB stays sync behind
> `spawn_blocking`; Surreal/S3/Redis/DynamoDB stay `.await`-ed on the host Tokio runtime.

---

### Chunk 1 — `Catalog.register`: register Pydantic models + S3 sources

- [ ] In `python/droplet/`, give `Catalog` a `register(*models, sources=...)` method that takes the
  Pydantic models (the schema DSL) and the S3 source descriptors and hands them down to `droplet-core`
  via the compiled `_droplet` module. This is the one entry that turns Pydantic into everything
  downstream: DuckDB column types, the read-only field-search index, the typed tool signatures, and the
  type stubs (the M5/M6/M7 machinery — you are just exposing it on the SDK surface here).
  - 🆕 Concept: the **SDK layer** is plain Python in `python/droplet/` that wraps the compiled
    `_droplet` extension module (the `droplet-py` cdylib). The Python you write here is ergonomic glue;
    the work happens in Rust behind the PyO3 boundary. (No Rust Book chapter — this is the Python side.)
  - ⚠️ Invariant #2: "`pydantic` is the SDK-layer schema DSL." Pydantic lives **only** at the SDK layer,
    never in `droplet-core`. `Catalog.register` is exactly where Pydantic models cross into Rust as
    *derived schema* (DuckDB types + stub text), not as a `droplet-core` dependency.
  - ⚠️ Invariant #7: the tool surface generated from the models is **flat typed functions** (Monty has
    no classes/modules) — `run_sql`, `search_fields`, `describe_schema`, `list_tables`, `export`. Don't
    expose a class-namespaced API to the model.
  - 🔗 Maps to: `Catalog.register(*models, sources=...)` — the central abstraction from §4.
  - ✅ Done when: `Catalog().register(MyModel, sources=[s3_parquet(...)])` returns without error and a
    follow-up `list_tables()` shows the registered table.

### Chunk 2 — `Session`: one durable, ephemeral run context

- [ ] Expose `Session` on the SDK so a `Catalog` produces a `Session` per run. The `Session` owns an
  ephemeral DuckDB, a read-only Surreal handle, its manifest, and its snapshot lifecycle (all already
  built in `droplet-core` — you are surfacing them). Make sure closing a `Session` wipes its working dir.
  - 🆕 Concept: a Python **context manager** (`with Session(...) as s:`) is Python's RAII — `__enter__`
    sets up, `__exit__` tears down (wipes the dir, closes handles) even on error. Use it so per-run
    cleanup is automatic. (No Rust Book chapter — Python side; the Rust `Drop` equivalent already lives
    in `droplet-core`.)
  - ⚠️ Invariant #9 (per-run isolation): "one run = one `Session` = ephemeral DuckDB + a unique working
    dir wiped on close; S3 credentials scoped per session; tool paths confined to the session dir /
    allow-listed sources." The SDK `Session` must enforce this — a new working dir per session, wiped on
    close; no two sessions share a DuckDB.
  - 🔗 Maps to: `Session` — "one durable, ephemeral analysis context per run" (§4).
  - ✅ Done when: two `Session`s opened from the same `Catalog` get distinct working dirs, closing one
    leaves the other's dir intact, and the closed session's dir is gone.

### Chunk 3 — `run_code` on the SDK (sync), type-check-before-run wired through

- [ ] Expose `Session.run_code(code) -> Result` synchronously: it runs the agent's Python in Monty
  against the session, **type-checked first** (retry on type error), returns capped results, and
  snapshots after the step. Almost all of this is the M4/M5/M7 path — this chunk is making it one clean
  SDK call.
  - ⚠️ Invariant #7: "type-check before execution." A wrong column / wrong arg fails *before* the SQL
    runs and triggers the model-retry path — this is the Success-Criteria "wrong column name caught by
    the type checker before execution."
  - ⚠️ Invariant #4 (boundary discipline): "only result-returning tools move capped rows into the
    sandbox." Whatever `run_code` returns to Python is already capped host-side — never the full frame.
  - 🔗 Maps to: `run_code(code) -> Result` (§4).
  - ✅ Done when: a `run_code` that calls `search_fields` then `run_sql` returns a small capped result; a
    `run_code` referencing a non-existent column raises a type error *before* any DuckDB query runs.

### Chunk 4a — Confirm the GIL is released around the DuckDB work

- [ ] In `droplet-py`, confirm the `_droplet` entry that `run_code` calls **releases the GIL** around the
  blocking DuckDB work, with `py.detach(move || { ... })`. While `droplet-core` does the DuckDB scan
  (sync, on `spawn_blocking`), `droplet-py` must hand the GIL back so other Python threads run.
  - 🆕 Concept: the **GIL** (Global Interpreter Lock) lets only one thread run Python at a time. Holding
    it during slow Rust work blocks every other Python thread; `py.detach(|| ...)` hands it back for the
    duration of the closure. (No Rust Book chapter — PyO3-specific.)
  - ⚠️ Invariant #6: "DuckDB is synchronous → `spawn_blocking` + release the GIL during query
    execution." `run_code` must not hold the GIL across a DuckDB scan.
  - ⚠️ Invariant #1: this `py.detach` call lives in **`droplet-py`** only — `droplet-core` stays
    pyo3-free. The GIL is a wheel-layer concern, not a core concern.
  - ⚠️ The `detach` closure must be **Ungil**: move only owned Rust data in (a `String`, `Vec`, etc.),
    return owned data out — never touch the `py` token or a `Bound<'py, _>` inside it (compile error by
    design).
  - **verify:** on `pyo3 = "0.29"` the method is `Python::detach`, **not** the old `allow_threads`
    (renamed in 0.26.0 with no alias kept). Confirm against the pyo3 0.29 migration guide / CHANGELOG.
  - ✅ Done when: a small `#[test]`/manual check shows a second Python thread makes progress while a
    `run_code` DuckDB scan is in flight (GIL is not held across the scan).

### Chunk 4b — Add the `pyo3-async-runtimes` dependency (version-locked to pyo3)

- [ ] In `droplet-py`'s `Cargo.toml`, add the async bridge dep, locked to pyo3's minor:
  ```toml
  pyo3-async-runtimes = { version = "0.29", features = ["tokio-runtime"] }
  ```
  It **must** share pyo3's minor (`0.29` ↔ `0.29`) or you link two copies of pyo3 and get confusing
  trait-mismatch errors. It is the *renamed* successor to the abandoned `pyo3-asyncio`; never depend on
  the old name.
  - ⚠️ Invariant #1: this dep is in **`droplet-py`** only — `droplet-core` must stay pyo3-free, so it
    never sees `pyo3-async-runtimes` either.
  - ✅ Done when: `cargo build -p droplet-py` is green with the dep added, and `cargo tree` shows a single
    `pyo3 0.29.x` (not two copies).

### Chunk 4c — `run_async`: the awaitable entry point (PyO3 async bridge)

- [ ] Add `Session.run_async(code)` returning a Python awaitable, built in `droplet-py` with
  `pyo3_async_runtimes::tokio::future_into_py(py, async move { ... })`. The returned `T` must be
  `Send + 'static` and plain data (capped rows or a handle), never a borrowed Python object.
  - 🆕 Concept: a Rust `Future` and a Python awaitable are different things; `future_into_py` converts
    the former into something Python can `await`. (Rust Book: *Fundamentals of Asynchronous Programming:
    Async, Await, Futures, and Streams*, ch. 17)
  - ⚠️ Invariant #6: inside the async path the DuckDB call still goes through `spawn_blocking` in core;
    you `.await` Surreal/S3 on the host runtime but never `.await` a blocking DuckDB scan directly.
  - **verify:** the exact signature/import of `pyo3_async_runtimes::tokio::future_into_py` and whether a
    Tokio runtime must be initialized once (e.g. via `#[pyo3_async_runtimes::tokio::main]`) before first
    use, on the 0.29 line specifically. Check `docs.rs/pyo3-async-runtimes/0.29` when you wire this.
  - ✅ Done when: `python -c "import asyncio, droplet; print(asyncio.run(session.run_async('1 + 2')))"`
    returns the expected value, proving a Rust Tokio future was awaited from Python.

### Chunk 5 — Backend config: select the concrete store impls vs in-memory dev stores

- [ ] Give the SDK one **backend config** object that decides which concrete impl backs each of the three
  store traits, and pass it through `Catalog` / `Session`. The three traits (`ArtifactStore`,
  `SnapshotStore`, `CoordinationStore`) already have multiple impls from M0–M7 — this chunk just lets the
  SDK *choose* between them by config, with sane env-driven defaults. The choices per trait:
  - `ArtifactStore` → **S3** (prod) or the local/in-memory dev impl (M0).
  - `SnapshotStore` → **S3** (prod) or the local/in-memory dev impl (M0).
  - `CoordinationStore` → **Redis** *or* **DynamoDB** (prod) or the in-memory dev impl (M0).
  - 🆕 Concept: this is **dependency injection via a trait object** — in core each store is held as a
    `Box<dyn ArtifactStore>` (etc.), so the config swaps the concrete type without changing any call
    site. (Rust Book: *Object-Oriented Programming Features of Rust*, ch. 18 — trait objects.)
  - 🆕 Concept: those store traits have **`async fn` methods used through `Box<dyn _>`**, which is **not**
    dyn-compatible with bare `async fn`. So each trait is annotated `#[async_trait]` (the `async-trait`
    crate, `0.1.89`) — that is what makes `Box<dyn ArtifactStore>` compile for async methods. (Rust Book:
    *Traits: Defining Shared Behavior*, ch. 10.)
  - ⚠️ Invariant #8 (distributed by default): "immutable data is content-addressed in the object store;
    mutable coordination (registry, leases, cache index) is in the consistent store." The **prod default**
    wires S3 + (Redis | DynamoDB); the in-memory stores are a **dev-only** convenience and must NEVER be
    the default in a multi-pod run (two pods with in-memory stores share nothing — the integration test
    would silently pass for the wrong reason).
  - 🔗 Maps to: "backend config" (§4) and §9's note "Backends ship as impls behind the traits …
    (+ local/in-memory for dev)."
  - **verify:** the prod store deps and their exact pins when you wire them. From the digest:
    `aws-config = { version = "1.8.18", features = ["behavior-version-latest"] }`,
    `aws-sdk-s3 = "1.136.0"`, `aws-sdk-dynamodb = "1.116.0"`,
    `redis = { version = "1.2", features = ["tokio-comp", "connection-manager"] }`. The `aws-sdk-*`
    crates bump ~weekly — re-check crates.io and let `Cargo.lock` hold the exact versions. (`redis` 1.x
    needs Rust ≥ 1.88, which the edition-2024 workspace already satisfies.)
  - ✅ Done when: the same `run_code` script runs unchanged under (a) the all-in-memory dev config and
    (b) the S3 + Redis (or DynamoDB) config, selected purely by the backend-config object / env.

### Chunk 6 — The pydantic-ai adapter: a THIN, SEPARATE package

- [ ] Create `adapters/droplet-pydantic-ai/` as its own installable package (its own `pyproject.toml`)
  that depends on `droplet` and `pydantic-ai`, and exposes Droplet's typed tool surface as pydantic-ai
  tools. The adapter's whole job is **translation**: take a Droplet `Session`, surface `run_sql` /
  `search_fields` / `describe_schema` / `list_tables` / `export` as pydantic-ai tool functions with the
  **typed signatures Droplet already generated** from the Pydantic models, and route calls back into
  `session.run_code` / `session.tools()`.
  - 🆕 Concept: an **adapter package** is a thin shim with its own `pyproject.toml`; installing it is
    opt-in (`pip install droplet-pydantic-ai`). The dependency arrow points adapter → `droplet`, never
    the reverse. (No Rust Book chapter — packaging concept.)
  - ⚠️ Invariant #2 — the load-bearing rule of this milestone: "framework integrations are **separate
    adapter packages**." Keep `pydantic-ai` out of `droplet-core`, `droplet-py`, and base `droplet`; it
    appears **only** in this adapter's dependencies. If you ever feel tempted to `import pydantic_ai`
    from the base package, stop — that breaks the invariant.
  - ⚠️ Invariant #7: the tools you expose to the agent are **flat typed functions** matching the stub
    surface — don't invent a richer namespaced API in the adapter than Monty / the model can use.
  - 🔗 Maps to: "plugs into any agent framework via a thin adapter … one example adapter (pydantic-ai)"
    (§1, §3).
  - **verify:** the current pydantic-ai tool-registration API (how a Python function becomes an agent
    tool — `@agent.tool` decorator vs `Tool(...)` vs `tools=[...]`) and whether typed-arg / structured
    validation is declared on the function signature or separately. Check the pydantic-ai docs at the
    version you pin before writing the shim; the surface has moved between releases.

### Chunk 7 — Prove the adapter is genuinely optional (decoupling test)

- [ ] Write a test that imports and uses base `droplet` **with pydantic-ai NOT installed at all**, plus a
  separate test that exercises the adapter **with** it installed. This guards Invariant #2 mechanically:
  the framework-agnostic core/SDK must work standalone.
  - 🆕 Concept: a **decoupling test** asserts a *negative* — that removing an optional dependency does not
    break the base package. The cleanest form runs base `droplet` in an environment where
    `import pydantic_ai` would fail, and confirms `Catalog` → `Session` → `run_code` still works. (No
    Rust Book chapter — Python packaging.)
  - ⚠️ Invariant #2: "framework-agnostic core … never an agent framework." This test is the proof.
  - ✅ Done when: the base-package test passes in a venv that has `droplet` but **not** `pydantic-ai`
    installed; the adapter test passes only in a venv that has both.

### Chunk 8 — Stand up the local shared backends (MinIO + Redis / DynamoDB Local)

- [ ] Before the two-pod test, stand up the **real shared stores** locally so both pods can point at the
  same plane: MinIO (S3-compatible, port 9000) for `ArtifactStore` + `SnapshotStore`, and Redis (port
  6379) **or** DynamoDB Local (port 8000) for `CoordinationStore`. Create a bucket and the
  registry/leases table by hand so the happy path has something to hit.
  - 🆕 Concept: "pods" share state **only through this plane** — no orchestrator, no session affinity, no
    shared memory. The *only* thing connecting pod A and pod B is bytes in S3 + rows in the coordination
    store. (No Rust Book chapter — the distributed model from §4.)
  - ⚠️ Invariant #8: use the **real** shared stores here, NOT the in-memory dev impls — with in-memory
    stores two pods share nothing and the test proves nothing. Wire the backend config (Chunk 5) to these
    local shared backends.
  - **verify:** local backend wiring from the digest —
    - MinIO: build the **S3 service** config with `aws_sdk_s3::config::Builder::from(&shared)
      .endpoint_url("http://localhost:9000").force_path_style(true).build()` (`force_path_style` is on the
      S3 `Builder`, *not* on `aws-config`), then `Client::from_conf(...)`.
    - Redis at `redis://127.0.0.1:6379/`; DynamoDB Local with `.endpoint_url("http://localhost:8000")` on
      the **ddb** service builder.
    - All three still need *some* credentials even locally (env `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY`
      for MinIO/DynamoDB Local).
  - ✅ Done when: a tiny round-trip script `put`s then `get`s a key in MinIO and writes/reads one row in
    Redis (or DynamoDB Local), proving both shared backends are reachable.

### Chunk 9 — Stand up the two-pod test rig (two processes, shared plane)

- [ ] Build the harness: **two pods = two separate processes** (truer than two threads), each running the
  SDK, both pointed at the **same** MinIO `ArtifactStore` + `SnapshotStore` and the **same** Redis (or
  DynamoDB Local) `CoordinationStore` via the Chunk 5 config. One S3 (MinIO) Parquet source is registered
  against a Pydantic model on each pod.
  - ⚠️ Invariant #9 (per-run isolation): each pod still gets its own `Session` = ephemeral DuckDB + a
    unique working dir; the pods share the *plane*, never a DuckDB or a working dir.
  - 🔗 Maps to: "Stateless pods … its state lives in the shared plane, so any pod can resume it" (§4).
  - ✅ Done when: both pod processes start, each connects to MinIO + Redis (or DynamoDB Local), and each
    can independently register the catalog against the same S3 source.

### Chunk 10 — Prove cross-pod CACHE REUSE (second pod does not re-scan)

- [ ] On **pod A**, run the `run_code` step: `search_fields` to find the right column, then `run_sql` an
  aggregation that DuckDB reads straight from S3. The result is **materialized** into the `ArtifactStore`
  (content-addressed Parquet) and the `cache_key → artifact_key` mapping is written to the cache index in
  the `CoordinationStore`. Then on **pod B**, run the *same* query and assert pod B **reuses the cached
  artifact instead of re-scanning S3**.
  - 🆕 Concept: the **cache key** is `hash(normalized_query + source + freshness_token)`; only
    `freshness_token` varies by policy (Versioned default = hash of the S3 object ETags/version-ids;
    TTL = `floor(now/ttl)`; Passthrough = never cache). Because the key is deterministic across pods, pod
    B computes the **same** key, finds the index entry, and fetches the artifact — no S3 scan. (No Rust
    Book chapter — the M2/M3 cache design.)
  - 🆕 Concept: how to *observe* "did not re-scan" — instrument the materialization path with a counter
    (a coordination-store key, a log line, or a metric the test reads), or point the source at a MinIO
    bucket and assert pod B issued **zero** `read_parquet`/GET-object calls against it. The cache hit must
    be **detectable, not assumed**. (No Rust Book chapter.)
  - ⚠️ Invariant #8: "immutable data is content-addressed in the object store; mutable coordination
    (registry, leases, **cache index**) is in the consistent store." Cache reuse is the literal payoff of
    this split.
  - ⚠️ Invariant #4: the rows that crossed into pod A's sandbox were **capped**; the *materialized
    artifact* (the full result) lives in S3 behind a handle/key, which is what pod B reuses — the sandbox
    never held the big data on either pod.
  - 🔗 Maps to: Success-Criteria "the materialized result written to the shared cache so a second run on a
    different pod reuses it instead of re-scanning."
  - **verify:** S3 `version_id` is only populated when the bucket has versioning enabled (MinIO has it off
    by default), so for the Versioned freshness token hash the **ETag** (always present) and/or enable
    versioning explicitly. Treat ETag as an opaque change token (multipart/SSE-KMS ETags are *not* a plain
    MD5).
  - ✅ Done when: pod B returns the same aggregation result as pod A while the scan counter / source GET
    count shows pod B did **not** re-read the underlying S3 Parquet (cache hit, served from the artifact).

### Chunk 11 — Prove cross-pod SNAPSHOT RESUME (rebuild DuckDB from the manifest)

- [ ] On **pod A**, snapshot the session after the `run_code` step (REPL bytes + manifest → zstd →
  content-addressed blob in the `SnapshotStore`; `run_id → snapshot pointer` recorded in the registry).
  Stop pod A. On **pod B**, `Session.resume(run_id)`: acquire the **lease**, load the snapshot, **rebuild
  DuckDB from the manifest** (re-attach the source views + materialized artifact keys — *not* a
  deserialized engine heap), reload the Monty REPL, and continue the run to the same final result.
  - 🆕 Concept: **resume rebuilds, it does not deserialize.** The snapshot carries only the Monty REPL
    bytes (postcard) + the manifest (schema ref, source refs, materialized artifact keys). Resume
    re-attaches source views and re-registers the materialized artifacts into a *fresh* ephemeral DuckDB —
    cheap, because the heavy data was never in the snapshot. Read-only Surreal is schema-derived and
    rebuilt, never loaded. (No Rust Book chapter — the M7 snapshot subsystem.)
  - 🆕 Concept: a **lease** is "one active worker per run, short TTL, reassignable — not affinity." Pod B
    takes the lease before resuming so two pods can't drive the same run at once. (No Rust Book chapter —
    the M3 coordination design.) **verify** the exact acquire primitive on your backend: on Redis it is
    `SET key worker NX PX ttl_ms` (via `set_options(...).conditional_set(ExistenceCheck::NX)
    .with_expiration(SetExpiry::PX(ttl_ms))`, decoding the reply as `bool`); on DynamoDB a `put_item` with
    `condition_expression("attribute_not_exists(pk)")` (contention surfaces as
    `ConditionalCheckFailedException`).
  - ⚠️ Invariant #3: "Snapshot = REPL bytes + manifest only; never serialize engine heaps; reconstruct
    DuckDB on resume. Snapshots immutable, content-addressed, versioned, compressed." If resume ever tries
    to deserialize a DuckDB heap, the invariant is broken — it must *rebuild* from the manifest.
  - ⚠️ Invariant #8: "Resume is lease-guarded; no affinity." Pod B must hold the lease before it resumes;
    a third pod attempting the same `run_id` while the lease is held must be rejected / back off.
  - ⚠️ Invariant #3 (versioning): the manifest records `snapshot-format` + schema versions **and** the
    pinned Monty tag (the REPL bytes are postcard, not portable across Monty versions). Resume must
    **refuse a version mismatch loudly** rather than mis-decode. **verify:** confirm the Monty tag is in
    the manifest and the load path checks it — the whole fleet must run one Monty tag (digest pins
    `v0.0.18`).
  - 🔗 Maps to: `Session.resume(run_id)` and Success-Criteria "the session snapshotted to the shared store
    and resumable on a different pod that rebuilds DuckDB from the manifest."
  - ✅ Done when: pod B (a *different* process from pod A, with pod A stopped) resumes `run_id`, rebuilds
    DuckDB from the manifest, and finishes the run with the **same** final result a single uninterrupted
    run would produce — under a held lease.

### Chunk 12 — The v1 acceptance test: stitch it into one green run

- [ ] Wire Chunks 10 + 11 into a single end-to-end integration test that exercises the full Success
  Criteria from the **Python SDK**, no agent framework: register model + S3 source → pod A `run_code`
  (`search_fields` + `run_sql`) with a wrong column caught by the type checker → cache reuse on pod B →
  snapshot on pod A → resume on pod B rebuilding DuckDB from the manifest, lease-guarded. **This green
  test is the v1 acceptance gate.**
  - ⚠️ Invariant #2: this top-level test imports **only** `droplet` (no `pydantic-ai`) — proving the SDK
    drives the whole criterion framework-free.
  - ⚠️ Invariant #1: the Rust under it is `droplet-core` (pyo3-free) wrapped by `droplet-py`; pydantic-ai
    is nowhere in the stack.
  - 🔗 Maps to: §11 Success Criteria in full.
  - ✅ Done when: one test run reproduces the entire Success Criteria sentence end-to-end against the local
    shared plane and passes.

---

## Out of scope — do NOT build these in M8 (or v1 at all)

These come straight from §3 "Out" of [`PRODUCT.md`](../../PRODUCT.md). Listed so you don't accidentally
start building them while polishing the SDK — every one is a *later* concern:

- [ ] **More than one adapter.** v1 ships **exactly one** example adapter (pydantic-ai). Do not build
  LangChain / LlamaIndex / OpenAI-Agents adapters — the seam is proven by one.
- [ ] **Any source other than S3.** No Athena / ClickHouse / Snowflake / Postgres / BigQuery. The
  `Source` trait exists so they *can* land later; building one now is scope creep.
- [ ] **Orchestrator / scheduler / control plane / observability UI.** The load balancer distributing
  runs is assumed external; Droplet has **no** orchestrator and **no** UI in v1 (no session affinity, no
  scheduler — that's the whole "stateless pods" point).
- [ ] **Managed-tier features** (hosted snapshotting, usage analytics, data catalogue). Build only the
  pluggable backend/telemetry **seams** (which you already have via the trait objects + backend config),
  **not** the features.
- [ ] **Typed `find` / `aggregate` ORM helpers.** SQL via `run_sql` covers v1; no ORM surface.
- [ ] **Graph / vector beyond field search.** Surreal is read-only, schema-derived field search **only**.
- [ ] **Writable SurrealDB.** It is rebuilt per session and never written to after the one-time build step
  (Invariant #5).
- [ ] **Incremental / per-call snapshots.** Snapshot granularity is **per `run_code` step**, full REPL +
  manifest — no finer.
- [ ] **Non-Python SDKs.** v1 is Python-only.

> Treat anything above as a "no" for the whole of v1. If a task seems to need one of these, re-read §3 —
> the answer is almost certainly to use the existing seam, not to build the feature.

---

## Notes carried forward

- **The in-memory stores are a trap in the distributed test.** They are perfect for the M0–M7 unit tests
  and the decoupling test, but the moment you write the *two-pod* test you MUST switch the backend config
  to the real shared stores (MinIO + Redis/DynamoDB Local). With in-memory stores the two pods share
  nothing, so cache reuse and cross-pod resume would "pass" without proving anything. This is the easiest
  way to fool yourself in M8 — wire the shared backends first (Chunk 8).
- **Keep the adapter thin.** The temptation in Chunk 6 is to add convenience features (prompt templates,
  retry policies, result formatting). Resist it — the adapter is a *translation shim*, and anything
  load-bearing belongs in `droplet`, behind the typed tool surface, where every other (future) adapter
  would also get it for free.
- **Pin the whole fleet to one Monty tag.** Cross-pod resume only works if every pod runs the identical
  Monty version (the REPL bytes are postcard, not portable across tags). The manifest records the tag
  (digest pins `v0.0.18`) and resume refuses a mismatch — but operationally, deploy one tag fleet-wide.
  verify: this is enforced in the manifest version check from M7; re-confirm before the integration test.
- **`Cargo.lock` committed = reproducible pods.** Both pods (and CI) must build from the same lockfile, or
  a `cargo update` could drift the DuckDB / arrow / Monty / store versions between pods and break resume.
  The workspace produces binaries/wheels, so the lockfile belongs in git. (This matters most for the
  `arrow` pin: it must match DuckDB's pinned major — `arrow = "58"` for `duckdb = "1.10503"` — or the
  two-arrow-versions error bites; re-run `cargo tree -i arrow` after any DuckDB bump.)

---

> 📌 When you reach this milestone, expand each chunk into tiny steps the way M0/M1 are written.
