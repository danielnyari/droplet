# M2 — ArtifactStore + materialization + content-addressed cache (SKETCH)

**Milestone goal:** stand up the real S3 `ArtifactStore` behind the `Chunk-F` trait, **materialize** a
DuckDB result into immutable, content-addressed Parquet in S3, and put a **content-addressed cache** in
front of it so the same query over the same data is scanned **once** and reused across runs and pods.

**Done when (from the spec):** running the same normalized query twice produces the Parquet **once** —
the second call resolves the `cache_key` to an existing `artifact_key` in the cache index and **skips the
DuckDB scan**, returning the same artifact. (This is the M2 slice of Success Criterion #11: "the
materialized result written to the shared cache so a second run on a different pod reuses it instead of
re-scanning.")

**Prerequisite:** finish [`M1-duckdb.md`](./M1-duckdb.md). You need a working `run_sql` that returns
capped Arrow results inside `spawn_blocking`, the four store traits from M0 (`Source`, `ArtifactStore`,
`SnapshotStore`, `CoordinationStore`) with their in-memory/local dev impls, `DropletError` (thiserror),
and the Session/handle registry. M2 *fills in* the real `ArtifactStore` and adds the cache layer on top
of M1's query path — it does not invent the traits.

**Estimate:** ~10 chunks (a chunk ≈ one focused sitting).

The spec lives at `PRODUCT.md` (repo root). Reference it that way, never `docs/PRODUCT.md`.

> This is a **SKETCH** file: chunk-level checkboxes with concept notes and invariant callouts, *not* the
> tiny per-line steps of M0/M1. Get the shape right first; expand into tiny steps when you reach this
> milestone.

---

### How to read this file

- Every `- [ ]` is a task. SKETCH tasks are coarser than M0/M1 — each is roughly one sitting, not 10
  minutes.
- `🆕 Concept:` explains a Rust idea the **first time** it shows up, with a Rust Book chapter name.
- `✅ Done when:` is an observable check (a command's output or a passing test).
- `⚠️ Invariant:` flags a `PRODUCT.md` rule you must not break (by its number 1–10).
- `🔗 Maps to:` ties an exercise to the real Droplet concept it unlocks.
- `verify:` flags a fact the research couldn't fully pin on the locked version — check the crate
  source/docs **before** relying on it, don't guess.
- Code snippets are *anchors* (a few lines to orient you). You write the real implementation.

---

## What M2 adds (read first, 5 min)

M1 gave you a query engine: SQL in, capped Arrow rows out, scanned straight from S3 every time. The
problem the spec calls out: **re-scanning is not free** — a big aggregation over S3 Parquet costs money
and time on every run, on every pod. M2 fixes that with three pieces, in order:

1. **ArtifactStore (S3):** a real implementation of the M0 `ArtifactStore` trait, backed by the AWS S3
   SDK. It stores immutable blobs (here, Parquet) under a **content-addressed key** = the hash of the
   bytes. Same bytes → same key, so identical results dedupe automatically.
2. **Materialization:** instead of (only) handing rows to the sandbox, take a DuckDB result, write it to
   a **Parquet file**, and `put` that file into the ArtifactStore. The agent later reads from the
   artifact (cheap) instead of re-scanning the source.
3. **Content-addressed cache:** before running a query, compute a
   `cache_key = hash(normalized_query + source + freshness_token)`. Look it up in the **cache index**
   (`cache_key → artifact_key`) held in the `CoordinationStore`. **Hit** → return the existing artifact,
   skip DuckDB entirely. **Miss** → run + materialize + record the mapping.

> 🆕 **Concept: content-addressing.** You name a blob by the **hash of its contents**, not by a path you
> choose. Feed the same bytes in, get the same key out — so the store dedupes for free and a key is also
> a checksum. (There's no dedicated Rust Book chapter; you met hashing in the warm-up's
> content-addressing intro. The hashing crate here is `blake3`.)

> ⚠️ **Invariant #8** (distributed by default): "state lives in the shared plane; **immutable data is
> content-addressed in the object store**; mutable coordination (registry, leases, **cache index**) is in
> the consistent store." M2 is the first milestone that builds this split for real — the Parquet artifact
> is the *immutable, content-addressed* half; the `cache_key → artifact_key` mapping is the *mutable
> coordination* half.

**Two stores, two storage models, don't mix them up:** the ArtifactStore holds **immutable**
content-addressed blobs in S3; the CoordinationStore holds the **mutable** cache index (a small key→key
map). The artifact never changes once written; the index entry can be added/overwritten as freshness
changes.

> 📌 **Local dev = MinIO.** Everywhere this file says "S3", local dev runs against **MinIO** (an
> S3-compatible server on `localhost:9000`). The AWS SDK talks to both — you just point the S3 client at
> MinIO's endpoint with `endpoint_url(...)` + `force_path_style(true)`. The `CoordinationStore` cache
> index uses M0's **in-memory dev impl** for now; the real Redis/DynamoDB impls land in
> [`M3-coordination.md`](./M3-coordination.md).

---

### Chunk 1 — Add the AWS S3 + hashing dependencies

- [ ] Add the object-store + content-addressing crates to `[workspace.dependencies]` in the **root**
  `Cargo.toml`, then opt `droplet-core` in with `dep.workspace = true`:
  ```toml
  aws-config = { version = "1.8.18", features = ["behavior-version-latest"] }
  aws-sdk-s3 = "1.136.0"
  blake3     = "1"
  ```
  (`tokio` is already a workspace dep from earlier milestones; the AWS SDK needs it because every call is
  async.)
  - 🆕 Concept: `[workspace.dependencies]` pins a crate's version **once** for the whole workspace;
    members opt in with `dep.workspace = true`. (Rust Book: *More About Cargo and Crates.io*, ch. 14.)
  - ⚠️ The `behavior-version-latest` feature on `aws-config` is **mandatory** — without it
    `BehaviorVersion::latest()` won't compile and `aws_config::defaults(...)` is unusable. This is the #1
    first-hour trap in this area.
  - 🆕 Concept: `blake3` is the content-addressing hash — a 32-byte digest. `blake3::hash(&bytes)` returns
    a `Hash`; `.to_hex()` gives a 64-char lowercase-hex value, and `.to_string()` makes it an owned
    `String` you can use as a key. The spec says "blake3/sha2 per digest" — start with `blake3` (much
    faster; recommended in the research). `sha2` is the fallback only if you have a reason. (No Rust Book
    chapter — hashing is project-side.)
  - ✅ Done when: `cargo build -p droplet-core` is green with all three deps present.
- [ ] Lock and commit the exact AWS versions. `aws-config` / `aws-sdk-*` publish **~weekly**; pin the
  exact versions above and let `Cargo.lock` hold them. Re-check crates.io before bumping, and never mix a
  `0.x` `aws-config` with a `1.x` SDK (they share `aws-smithy-*` 1.x internals only across the whole 1.x
  line).
  - ✅ Done when: `Cargo.lock` records the exact `aws-*` versions and you've committed it.
  - verify: re-confirm `aws-config = "1.8.18"` and `aws-sdk-s3 = "1.136.0"` are still current on crates.io
    on the day you pin — these crates move almost weekly, so a from-memory number will be stale.

### Chunk 2 — Stand up MinIO locally

- [ ] Run MinIO with Docker (S3-compatible object store; API port `9000`, console `9001`) and create a
  `droplet` bucket by hand (via the console or `mc`). Set `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY`
  env vars before running any Rust — MinIO defaults are often `minioadmin` / `minioadmin`.
  - 🆕 Concept: MinIO speaks the S3 API, so the **same** Rust code works against MinIO locally and real
    AWS in prod — only the **endpoint** and the **path-style** flag differ. (No Rust Book chapter —
    local-dev infra.)
  - ⚠️ MinIO still requires *some* credentials. With no creds the SDK errors at *send* time, not at client
    build — so set the env vars first to avoid a confusing late failure.
  - ✅ Done when: the MinIO console shows an empty `droplet` bucket.

### Chunk 3 — Build the shared AWS config once

- [ ] In a throwaway `#[tokio::main]` example, build the **shared** config a single time:
  ```rust
  use aws_config::BehaviorVersion;
  let shared = aws_config::defaults(BehaviorVersion::latest())
      .region("us-east-1")
      .load()
      .await;
  ```
  - 🆕 Concept: every AWS call ends in `.send().await` — the SDK is fully async and needs a Tokio runtime.
    The `#[tokio::main]` macro starts that runtime for you in an example; in `droplet-core` you `.await`
    these on the runtime the engine already owns. (Rust Book: *Fundamentals of Asynchronous Programming*,
    ch. 17.)
  - 🆕 Concept: `BehaviorVersion` pins the SDK's default behaviors so a future SDK upgrade can't silently
    change them; `::latest()` is fine for a new project. (No Rust Book chapter — SDK-specific.)
  - ⚠️ **Invariant #6** (DuckDB sync → `spawn_blocking`; release the GIL during execution): the async S3
    code must stay **off** the DuckDB thread. DuckDB is sync behind `spawn_blocking`; S3 is async and
    `.await`ed on the runtime. Never run an S3 call inside a `spawn_blocking` DuckDB closure, and never
    block the runtime on a DuckDB call.
  - verify: what `BehaviorVersion::latest()` currently resolves to (a dated default that moves over
    releases). `::latest()` is fine unless you need reproducibility — then read which dated version it
    pins on your installed `aws-config`.
  - ✅ Done when: the example loads config without panicking.

### Chunk 4 — Point an S3 client at MinIO

- [ ] Layer S3-specific config on top of the shared config so it points at MinIO, then make a client:
  ```rust
  let s3_conf = aws_sdk_s3::config::Builder::from(&shared)
      .endpoint_url("http://localhost:9000") // MinIO; omit for real AWS
      .force_path_style(true)                 // REQUIRED for MinIO
      .build();
  let s3 = aws_sdk_s3::Client::from_conf(s3_conf);
  // real AWS: just `aws_sdk_s3::Client::new(&shared)`
  ```
  - ⚠️ `force_path_style(true)` is a method on `aws_sdk_s3::config::Builder`, **not** on `aws-config`.
    Without it MinIO requests use virtual-host style (`bucket.localhost`) and fail.
  - 🆕 Concept: take the endpoint and bucket from **configuration**, never hard-code them in
    `droplet-core` — local dev (MinIO) and prod (AWS) differ only in these values. (No Rust Book chapter —
    config hygiene.)
  - ✅ Done when: the same example does a `put_object` then `get_object` on a tiny key and the bytes
    round-trip equal against MinIO.

### Chunk 5 — Implement `S3ArtifactStore` behind the `Chunk-F` trait

- [ ] Implement the M0 `ArtifactStore` trait (the `Chunk-F` trait) for a new
  `S3ArtifactStore { client, bucket }` struct. Use `#[async_trait]` on the impl, since this trait is used
  as `Box<dyn ArtifactStore>` (a pluggable backend).
  - 🆕 Concept: native `async fn` in traits is stable but **not dyn-compatible**, so anything stored as
    `Box<dyn Trait>` still needs `#[async_trait]` (from the `async-trait` crate, already a workspace dep
    from M0). (Rust Book: *Generic Types, Traits, and Lifetimes*, ch. 10 — trait objects.)
  - The two core methods (PUT bytes in → content-addressed key out; GET key → bytes out):
    ```rust
    async fn put(&self, bytes: Vec<u8>) -> Result<String, DropletError> {
        let key = format!("artifacts/{}.parquet", blake3::hash(&bytes).to_hex());
        self.client.put_object().bucket(&self.bucket).key(&key)
            .body(ByteStream::from(bytes)).send().await?;
        Ok(key)
    }
    // GET: get_object, then out.body.collect().await?.into_bytes().to_vec()
    async fn get(&self, key: &str) -> Result<Vec<u8>, DropletError> { /* ... */ }
    ```
  - 🆕 Concept: S3 GET bodies are **streams** (`ByteStream`), not `Vec<u8>` — you must
    `out.body.collect().await?.into_bytes().to_vec()`. PUT bodies are also `ByteStream`
    (`ByteStream::from(vec)` for in-memory bytes). (No Rust Book chapter — SDK-specific.)
  - ⚠️ **Invariant #8** (immutable data content-addressed in the object store): the key is the **hash of
    the contents** — same bytes → same key, so the object store dedupes immutable artifacts for free.
  - ✅ Done when: a Rust test `put`s some bytes, gets back a hex key, `get`s the same key, and the bytes
    round-trip. Calling `put` twice with identical bytes returns the **same** key.
- [ ] Fold the S3 errors into `DropletError` at this boundary with `thiserror`.
  - ⚠️ **Invariant #10** (one error type at the boundary): map `aws_sdk_s3`'s `SdkError` into
    `DropletError` — at least a `NotFound` variant (reachable via `err.as_service_error()` →
    `.is_no_such_key()`) and a catch-all transport error. Don't leak `aws_sdk_s3` error types past the
    store. (Rust Book: *Error Handling*, ch. 9 — `?` and the `?`-operator.)
  - ✅ Done when: `get`ting a missing key surfaces as a `DropletError::NotFound` (or equivalent), not a raw
    `SdkError`.
- [ ] verify: confirm the exact `ByteStream::from` (in-memory, **sync**) vs `ByteStream::from_path(path).await`
  (file, **async**, returns `Result`) call shapes against `aws-sdk-s3 = "1.136.0"` before wiring
  file-backed Parquet in the next chunk — they're different and mixing them is a common compile error.

### Chunk 6 — Materialize a DuckDB result to a local Parquet file

- [ ] Inside the **same `spawn_blocking` closure** that owns the connection (M1's pattern), make DuckDB
  write the query result to a local Parquet file with
  `COPY (<query>) TO 'path.parquet' (FORMAT PARQUET)`.
  - 🆕 Concept: **materialize** = turn a query *result* into a durable artifact. DuckDB writes Parquet
    itself with `COPY ... TO ... (FORMAT PARQUET)` — no Arrow round-trip is needed for the artifact path
    (you still cap *rows into the sandbox* separately, per M1's `LIMIT`). (No Rust Book chapter — DuckDB
    SQL.)
  - ⚠️ **Invariant #9** (per-run isolation): write the Parquet into the **session's working dir** (wiped
    on close), not a shared temp path — one run = one ephemeral DuckDB + a unique working dir.
  - ⚠️ **Invariant #6**: the `COPY` runs in DuckDB (sync) under `spawn_blocking`; it must not be blurred
    with the async S3 `put` that follows (next chunk). Two phases, two threads.
  - ✅ Done when: a test runs a query, finds a non-empty `.parquet` in the session dir, and can re-read it
    with `read_parquet('...')`.

### Chunk 7 — Push the Parquet to S3 (the full materialize primitive)

- [ ] Read the local Parquet bytes and `put` them through `S3ArtifactStore`, getting back the
  content-addressed `artifact_key`. This completes the **materialize** primitive:
  `query → Parquet → S3 → artifact_key`.
  - ⚠️ **Invariant #6 / async boundary**: the `COPY` (Chunk 6) ran sync under `spawn_blocking`; the `put`
    here runs **async on the runtime** — `.await` it *after* the blocking task returns. Don't call S3 from
    inside the blocking closure.
  - ⚠️ **Invariant #3** (snapshot = REPL bytes + manifest only): the artifact holds the **data**; the
    snapshot (M7) will hold only the **artifact key**, never the rows. Materializing here is what keeps
    snapshots small later.
  - 🔗 Maps to: the `materialize(query, source) -> artifact_key` primitive the cache layer (Chunk 10)
    calls on a miss, and the keys the M7 manifest records.
  - ✅ Done when: a test materializes a query and asserts the returned key exists in MinIO (a
    `head_object` on it succeeds).

### Chunk 8 — Compute the cache key (normalized query + source)

- [ ] Build the first two-thirds of the `cache_key`: **normalize** the SQL (the spec's `normalized_query`)
  and combine it with the `source` identifier, then hash with `blake3`. (The `freshness_token` third part
  comes in Chunk 9.)
  - 🆕 Concept: **normalizing** a query means reducing trivially-different-but-equivalent SQL to one
    canonical string (e.g. trim and collapse internal whitespace) so `SELECT *` and `select  *` map to the
    **same** cache key. Be **conservative** — when unsure whether two queries are equivalent, treat them
    as different. A cache *miss* is safe; a wrong *hit* returns stale/wrong data. (No Rust Book chapter —
    project-side.)
  - ⚠️ verify: how aggressive to make normalization. A trivial v1 (trim + collapse internal whitespace) is
    safe; do **not** try to semantically canonicalize SQL (column reordering, alias rewriting) — that's a
    correctness trap. Keep it textual and conservative.
  - ✅ Done when: a unit test shows two whitespace-different-but-identical queries produce the **same**
    `cache_key`, and two genuinely different queries produce different keys.

### Chunk 9 — Freshness policy: the `freshness_token`

The third input to the cache key. The spec defines **three** per-dataset policies. This is the chunk that
makes the cache *auto-invalidate when data changes* without re-scanning.

- [ ] Define a `FreshnessPolicy` enum with three variants and a method
  `token(&self, source) -> Result<String, DropletError>`:
  - 🆕 Concept: an **enum with data** lets each policy carry its own parameters (e.g. `Ttl(Duration)`) and
    you `match` on it to compute the token. (Rust Book: *Enums and Pattern Matching*, ch. 6.)
  - **`Versioned` (default)** — token = hash of the source S3 objects' **ETags / version-ids**, read with
    a cheap `head_object` (HEAD = metadata only, **no** data transfer). When the data changes, the ETag
    changes → the token changes → the cache key changes → automatic invalidation, **without re-scanning**.
    ```rust
    let head = s3.head_object().bucket(b).key(k).send().await?;
    let etag: Option<&str> = head.e_tag();        // present on every object
    let ver:  Option<&str> = head.version_id();   // only if bucket versioning is ON
    ```
    - ⚠️ verify: `version_id()` is `None` on non-versioned buckets (MinIO's default). For the `Versioned`
      token, hash the **ETag** (always present) and *optionally* the version-id; if you rely on
      `version_id`, confirm bucket versioning is enabled. Also: ETag is an **opaque** change token, not
      always `md5(bytes)` (multipart / SSE-KMS objects differ) — that's fine, you only need "did it
      change?". Confirm `e_tag()` / `version_id()` return `Option<&str>` on `aws-sdk-s3 = "1.136.0"` before
      relying on them.
  - **`Ttl(duration)`** — token = `floor(now / ttl)` as a string. Reuse the cached artifact for the whole
    window; skip the version check entirely. Cheaper (no HEAD) at the cost of bounded staleness.
  - **`Passthrough`** — never cache. The cache layer short-circuits: always run + materialize fresh, never
    read or write the index.
  - ⚠️ **Invariant #8**: the freshness check is a cheap **version check, not a re-scan** — it reads object
    metadata in the shared plane, never the data.
  - ✅ Done when: a test shows (a) the `Versioned` token changes when you overwrite the source object in
    MinIO; (b) the `Ttl` token is stable within a window and rolls over after it; (c) `Passthrough` always
    forces a miss.

### Chunk 10 — The cache index in the CoordinationStore (in-memory dev impl)

- [ ] Add `cache_key → artifact_key` get/put methods to the `CoordinationStore` trait, and implement them
  on M0's **in-memory** dev impl (an `Arc<Mutex<HashMap<String, String>>>` is fine for now).
  - 🆕 Concept: the cache index is **mutable shared state** — that's why it lives in the CoordinationStore,
    not the immutable object store. (Rust Book: *Shared-State Concurrency*, ch. 16 — `Arc<Mutex<…>>`.)
  - ⚠️ **Invariant #8**: "mutable coordination (registry, leases, **cache index**) is in the consistent
    store." In-memory is the **dev** impl only; the real consistent backends (Redis `HSET`/`HGET`,
    DynamoDB) arrive in [`M3-coordination.md`](./M3-coordination.md). Keep this method on the trait so the
    real impl drops in unchanged.
  - ✅ Done when: a test `put`s a `cache_key → artifact_key`, reads it back, and a missing key returns
    `None` (a miss).

### Chunk 11 — Wire the cache around the query path (the payoff)

- [ ] Assemble the **cache-aware run**: given `(sql, source, freshness_policy)`:
  1. If `Passthrough` → skip the cache, materialize fresh, return.
  2. Else compute `freshness_token` (Chunk 9), then
     `cache_key = hash(normalized_query + source + token)` (Chunk 8).
  3. Look up `cache_key` in the cache index (Chunk 10).
     - **Hit** → return the existing `artifact_key`. **Do not run DuckDB.**
     - **Miss** → run + materialize (Chunks 6–7) → `put` the `cache_key → artifact_key` mapping → return.
  - ⚠️ **Invariant #8**: a hit means **state lived in the shared plane** — the artifact is content-addressed
    in S3, the mapping is in the consistent store, so any pod that computes the same `cache_key` reuses the
    same artifact. This is the "scan once, reuse across the fleet" guarantee.
  - ✅ Done when: an integration test runs the **same** `(sql, source)` twice and asserts the DuckDB scan
    ran **once** (e.g. a counter incremented only on the first call) while both calls return the **same**
    `artifact_key`.

### Chunk 12 — Prove cross-"pod" reuse + write the milestone test

- [ ] Prove cross-"pod" reuse with **two separate store instances** sharing the same in-memory index +
  MinIO: build the cache key on instance A (miss → materialize), then look it up on instance B (hit → no
  scan).
  - 🔗 Maps to: this is the M2 slice of the spec's Success Criterion — "the materialized result written to
    the shared cache so a second run on a different pod reuses it instead of re-scanning." (Cross-pod
    *snapshot resume* is M7; cross-pod *cache reuse* is here.)
  - ✅ Done when: instance B returns the artifact without ever calling DuckDB.
- [ ] Write the **milestone test** end-to-end against MinIO + the in-memory index: register a `Versioned`
  source; run an aggregation → it materializes Parquet to S3 and records the cache entry; run it
  **again** → it hits the cache and skips DuckDB; **overwrite the source object** in MinIO → the
  `Versioned` token changes → the next run **misses** and re-materializes.
  - ⚠️ **Invariant #1** (core never imports pyo3): this whole milestone lives in `droplet-core` — **no
    `pyo3`**. The cache must be drivable from a pure-Rust test with no Python in the loop.
  - ⚠️ **Invariant #10**: confirm a deliberately bad bucket name surfaces as a `DropletError`, not a raw
    `SdkError` — every new failure path (S3 `SdkError`, the `blake3`/IO around Parquet, a cache-index
    error) folds into `DropletError` via `thiserror` `#[from]`.
  - ✅ Done when: the test passes all three legs (cold miss → warm hit → invalidate-on-change miss). This
    is M2's "Done when."

---

## Notes carried forward (don't act yet)

- **The cache index is a stub until M3.** The in-memory `HashMap` impl proves the shape but is **not**
  distributed — two real pods don't share it. [`M3-coordination.md`](./M3-coordination.md) swaps in Redis
  (`HSET droplet:cache_index <cache_key> <artifact_key>` / `HGET`) and DynamoDB behind the same
  `CoordinationStore` trait. Design the trait method now so that swap is invisible to the cache logic.
- **Per-run intermediates reuse the same `put`.** The ArtifactStore stores both the **fleet-wide cache**
  artifacts (this file) *and* a run's private intermediate Parquet. Same content-addressed `put`;
  different lifecycle. Keep the store generic — don't bake "cache" assumptions into `ArtifactStore::put`.
- **Snapshots will reference these keys (M7).** When you build the snapshot manifest
  ([`M7-snapshot-store.md`](./M7-snapshot-store.md)), it records the materialized **artifact keys** —
  never the rows (Invariant #3). M2's materialization is what makes that possible; nothing to do now, just
  don't lose track of the keys.
- **Verify when you pin.** `aws-config` / `aws-sdk-s3` move almost weekly — re-confirm the exact versions
  on crates.io before pinning, and re-confirm `BehaviorVersion::latest()` still resolves to a current
  default. Confirm `head_object().e_tag()` / `version_id()` return `Option<&str>` on the version you
  install before relying on them.

---

> 📌 When you reach this milestone, expand each chunk into tiny steps the way M0/M1 are written.
