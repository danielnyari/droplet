# M5 — ArtifactStore + content-addressed load cache (SKETCH)

**Milestone goal:** put a **content-addressed cache** in front of the `load` boundary you built in
[`M2-load-boundary.md`](./M2-load-boundary.md), so the **same scoped load over the same source** runs the
connector **once** and is reused across runs and pods. The download the connector produces — the
**materialized Parquet** — *is* the cache artifact, stored under a content-addressed key in an
`ArtifactStore`.

**Done when (from the spec):** running the same `load(...)` twice pulls from the source **once** — the
second call resolves the `cache_key` to an existing `artifact_key` in the cache index, **skips the
connector entirely**, and re-attaches the same Parquet locally. (This is the M5 slice of Success
Criterion §20: "a second run on another pod reusing the cached unload instead of re-hitting Athena" —
here, against the local dev connector + in-memory stores; the real S3 + Athena versions land in
[`M6-connectors-athena.md`](./M6-connectors-athena.md).)

**Prerequisite:** finish [`M2-load-boundary.md`](./M2-load-boundary.md). You need a working
`load(name, columns, where, as_of) -> Dataset` that calls the **dev connector** (the trivial
local-Parquet `Source` from `M0`) and materializes the slice into local DuckDB, the four store traits
from `M0` (`Source`, `ArtifactStore`, `SnapshotStore`, `CoordinationStore`) with their in-memory/local
dev impls, `DropletError` (thiserror), and the Session / handle registry. M5 *fills in* a real
`ArtifactStore` dev impl and adds the **cache layer in front of the connector** — it does not invent the
traits, and it does not touch the analyze surface ([`M1-analyze-engine.md`](./M1-analyze-engine.md)) at
all.

**Estimate:** ~10 chunks (a chunk ≈ one focused sitting).

The spec lives at `PRODUCT.md` (repo root). Reference it that way, never `docs/PRODUCT.md`.

> This is a **SKETCH** file: chunk-level checkboxes with concept notes and invariant callouts, *not* the
> tiny per-line steps of `M0`–`M3`. Get the shape right first; expand each chunk into tiny steps when you
> reach this milestone.

---

### How to read this file

- Every `- [ ]` is a task. SKETCH tasks are coarser than `M0`–`M3` — each is roughly one sitting, not 10
  minutes.
- `🆕 Concept:` explains a Rust idea the **first time** it shows up, with a Rust Book chapter name.
- `✅ Done when:` is an observable check (a command's output or a passing test).
- `⚠️ Invariant:` flags a `PRODUCT.md` §15 rule you must not break (by its number 1–10 — the Golden Rules
  in the roadmap [`README.md`](./README.md)).
- `🔗 Maps to:` ties a step to the real Droplet concept it unlocks.
- `verify:` flags a fact the research couldn't fully pin on the locked version — check the crate
  source/docs **before** relying on it, don't guess.
- Code snippets are *anchors* (a few lines to orient you). You write the real implementation.

---

## What M5 adds (read first, 5 min)

`M2` gave you the **load boundary**: `load(...)` takes a scoped slice (`columns` + `where` + `as_of`),
hands it to a connector, the connector produces a **local Parquet** file, and DuckDB re-attaches it as a
`Dataset`. Today it does that **every single time** — even when the agent (or another pod, or a previous
run) already asked for the exact same slice.

The spec calls this out: **a load is the only thing that touches the source, and it is the expensive
thing** (`PRODUCT.md` §4, §6). In production a load is a native bulk `UNLOAD` to object storage — it costs
money and warehouse time. Re-pulling the same slice on every run, on every pod, defeats the entire point
of the load/analyze split. M5 fixes that with three pieces, in order:

1. **ArtifactStore:** a real implementation of the `M0` `ArtifactStore` trait. It stores immutable blobs
   (here, the materialized Parquet) under a **content-addressed key** = the hash of the bytes. Same bytes
   → same key, so identical downloads dedupe automatically. The dev impl is **in-memory**; the S3/MinIO
   impl arrives in [`M6-connectors-athena.md`](./M6-connectors-athena.md) behind this same trait.
2. **The load artifact:** the connector's output **is** the cache artifact. Take the Parquet the connector
   produced, `put` its bytes into the `ArtifactStore`, get back a content-addressed `artifact_key`. The
   load no longer has to re-run the connector to get that Parquet again — it reads the artifact and
   re-attaches it locally.
3. **Content-addressed cache:** before calling the connector, compute a
   `cache_key = hash(scoped_load_query + source + freshness_token)`. Look it up in the **cache index**
   (`cache_key → artifact_key`) held in the `CoordinationStore`. **Hit** → fetch that artifact, re-attach
   it, skip the connector entirely. **Miss** → run the connector + store the Parquet + record the mapping.

> 🆕 **Concept: content-addressing.** You name a blob by the **hash of its contents**, not by a path you
> choose. Feed the same bytes in, get the same key out — so the store dedupes for free and a key doubles
> as a checksum. (No dedicated Rust Book chapter; you met hashing in the warm-up's content-addressing
> intro. The hashing crate here is `blake3` — pinned `blake3 = "1"`, the same pin first introduced in
> [`00-rust-warmup.md`](./00-rust-warmup.md) and reused in [`M8-snapshot-resume.md`](./M8-snapshot-resume.md).
> verify: `blake3 = "1"` is the established project pin (00 / M8); confirm it matches those files.)

> ⚠️ **Invariant 7** (distributed by default): "immutable data is content-addressed in the object store;
> mutable coordination (run registry, leases, **cache index**) is in the consistent store; resume is
> lease-guarded; no affinity." M5 is the first milestone that builds this split for real — the
> materialized Parquet is the *immutable, content-addressed* half; the `cache_key → artifact_key` mapping
> is the *mutable coordination* half.

> ⚠️ **Invariant 2** (only `load` touches the source): the cache lives **in front of the connector** —
> the one guarded door. A cache *hit* means the source was **not** touched at all. The cache is the
> mechanism that makes "one bounded download, reused fleet-wide" true.

**Two stores, two storage models — don't mix them up:** the `ArtifactStore` holds **immutable**
content-addressed blobs (the load's Parquet); the `CoordinationStore` holds the **mutable** cache index (a
small `cache_key → artifact_key` map). The artifact never changes once written; the index entry can be
added or overwritten as freshness changes.

> 📌 **No cloud yet.** Everything in M5 runs against **in-memory dev impls** — no MinIO, no S3, no AWS
> SDK, no Athena. The `ArtifactStore` is an `Arc<tokio::sync::Mutex<HashMap<String, Vec<u8>>>>`; the cache
> index is an `Arc<tokio::sync::Mutex<HashMap<String, String>>>` (the async-aware lock `M0` calls for in
> these in-memory stores); the connector is the local-Parquet dev `Source` from `M0`. The
> real S3/MinIO `ArtifactStore` and the real Athena connector both land in
> [`M6-connectors-athena.md`](./M6-connectors-athena.md), dropping in **behind these same traits** with
> no change to the cache logic you write here. (That's the whole reason the traits exist.)

---

### Chunk 1 — Add the content-addressing dependency + sanity-check the traits

- [ ] Add the hashing crate to `[workspace.dependencies]` in the **root** `Cargo.toml`, then opt
  `droplet-core` in with `dep.workspace = true`:
  ```toml
  blake3 = "1"
  ```
  (`async-trait`, `tokio`, `serde`, `thiserror` are already workspace deps from earlier milestones.)
  - 🆕 Concept: `[workspace.dependencies]` pins a crate's version **once** for the whole workspace;
    members opt in with `dep.workspace = true`. (Rust Book: *More About Cargo and Crates.io*.)
  - 🆕 Concept: `blake3` is the content-addressing hash — a 32-byte digest. `blake3::hash(&bytes)` returns
    a `Hash`; `.to_hex()` gives a 64-char lowercase-hex value, and `.to_string()` makes it an owned
    `String` you can use as a key. `blake3` is much faster than `sha2` and is the project default; `sha2`
    is only a fallback if you ever have a reason. (No Rust Book chapter — hashing is project-side; you
    practiced it in the warm-up.)
  - ✅ Done when: `cargo build -p droplet-core` is green with `blake3` present.
- [ ] Define the `ArtifactStore` trait in `droplet-core`. `M0` deliberately **deferred** `ArtifactStore`
  to M5 (it shipped only the `Source` seam), so this milestone is where you write it. Give it an
  `#[async_trait]` `put` / `get` pair — the **caller** supplies the content-addressed key, mirroring how a
  real object store works (`put(key, bytes)` writes, `get(key)` reads):
  ```rust
  #[async_trait]
  pub trait ArtifactStore: Send + Sync {
      async fn put(&self, key: &str, bytes: Vec<u8>) -> Result<(), DropletError>;
      async fn get(&self, key: &str) -> Result<Vec<u8>, DropletError>;
  }
  ```
  - 🆕 Concept: the **caller computes the content-addressed key** (`format!("artifacts/{}.parquet",
    blake3::hash(&bytes).to_hex())`) and hands it to `put`, so `put` just writes bytes at a key. This is the
    shape the real `S3ArtifactStore` (`M6`) wants — an object store puts bytes at a key you choose. (No Rust
    Book chapter — Droplet design.)
  - 🆕 Concept: native `async fn` in traits is stable but **not dyn-compatible**, so anything stored as
    `Box<dyn Trait>` / `Arc<dyn Trait>` (a pluggable backend) still needs `#[async_trait]` (the
    `async-trait` crate, already a workspace dep from `M0`). (Rust Book: *Generic Types, Traits, and
    Lifetimes* — trait objects.)
  - ⚠️ **Invariant 8** (core never imports `pyo3`): all of M5 lives in `droplet-core`. No `pyo3`, no
    Python in the loop — the cache must be drivable from a pure-Rust test.
  - ✅ Done when: `cargo build -p droplet-core` is green and you can name the exact method signatures you'll
    implement.

### Chunk 2 — Implement the in-memory `ArtifactStore` dev impl

- [ ] Implement the `ArtifactStore` trait for a `MemArtifactStore` struct backed by
  `Arc<tokio::sync::Mutex<HashMap<String, Vec<u8>>>>` — the same naming/lock convention `M0` set for the
  in-memory `Mem*` dev stores. `put` writes the caller's bytes at the caller's key; `get` looks the key up.
  ```rust
  async fn put(&self, key: &str, bytes: Vec<u8>) -> Result<(), DropletError> {
      self.map.lock().await.insert(key.to_string(), bytes);
      Ok(())
  }
  async fn get(&self, key: &str) -> Result<Vec<u8>, DropletError> {
      self.map.lock().await.get(key).cloned()
          .ok_or_else(|| DropletError::NotFound(key.to_string()))
  }
  ```
  - 🆕 Concept: `Arc<tokio::sync::Mutex<…>>` is shared mutable state across async tasks (and, later, across
    "pods" in a test): `Arc` lets many owners share it, the `tokio::sync::Mutex` (the async-aware lock from
    `M0`) makes mutation safe **in async code** — `.lock().await` yields the lock as a future, and it
    releases when the guard drops. (Note: this is **not** `std::sync::Mutex`, whose `.lock()` returns a
    `Result` you'd `.unwrap()`; in async trait methods use tokio's, exactly as `M0` did.) (Rust Book:
    *Shared-State Concurrency* — `Arc<Mutex<…>>`.)
  - ⚠️ **Invariant 7** (immutable data content-addressed in the object store): the **caller** keys the blob
    by the **hash of the contents** (Chunk 4) — same bytes → same key — so the store dedupes immutable
    artifacts for free. Two identical downloads collapse to one stored blob.
  - 🔗 Maps to: the `S3ArtifactStore` in [`M6-connectors-athena.md`](./M6-connectors-athena.md) implements
    this **same trait** against MinIO/S3 — your cache logic won't change a line when it swaps in.
  - ✅ Done when: a Rust test `put`s some bytes at a content-addressed key, `get`s the same key, and the
    bytes round-trip equal. Computing the same key from identical bytes and `put`ting twice stores one
    entry.
- [ ] Fold any store error into `DropletError` at this boundary with `thiserror`. `M0` already defined
  `NotFound(String)` (the tuple variant carrying the missing key) — **reuse it** for a missing artifact
  key. Add only genuinely **new** variants here, e.g. an IO/transport variant for reading the Parquet bytes
  off disk (Chunk 3).
  - ⚠️ **Invariant 10** (one error type at the boundary): the store returns `DropletError`, never a raw
    backend error type. The in-memory impl barely needs this, but reusing the variant **now** means the
    S3 impl in `M6` folds `aws_sdk_s3::SdkError` into the same `NotFound` / transport variants with no
    signature change. (Rust Book: *Error Handling* — `?` and `#[from]`.)
  - ✅ Done when: `get`ting a missing key surfaces as a `DropletError::NotFound(key)`, not a panic or an
    `Option`.

### Chunk 3 — Capture the connector's Parquet as artifact bytes

- [ ] In the `load` path from `M2`, after the connector produces its **local Parquet** file (the dev
  connector's output), read that file's bytes so they can be `put` into the `ArtifactStore`. This is the
  "the download **is** the artifact" step.
  - 🆕 Concept: **the load artifact = the materialized Parquet.** Unlike the old design, you are **not**
    re-running a DuckDB query and materializing *its* result — the cache artifact is exactly the bytes the
    connector pulled from the source. `load` already produces this file in `M2`; M5 just captures and
    stores it. (No Rust Book chapter — Droplet design, `PRODUCT.md` §6.)
  - 📎 Note (per-run isolation, `PRODUCT.md` §14 — not one of the numbered Golden Rules): the connector
    writes its Parquet into the **session's working dir** (wiped on close), as in `M2`. M5 reads those bytes
    and hands them to the (shared) `ArtifactStore` — the working-dir copy stays per-run; the stored artifact
    is the fleet-wide-shareable copy.
  - ⚠️ **Invariant 9** (DuckDB sync → `spawn_blocking`; release the GIL): the connector download and the
    file read are I/O; the **re-attach** into DuckDB is the sync part that runs under `spawn_blocking` (as
    in `M2`). Keep the async `ArtifactStore::put` (next chunk) **off** the DuckDB thread — don't call the
    store from inside a `spawn_blocking` closure.
  - ✅ Done when: a test runs `load(...)` against the dev connector and can read back a non-empty `.parquet`
    bytes buffer from the session dir.

### Chunk 4 — Store the load artifact (the full "materialize the download" step)

- [ ] Compute the content-addressed `artifact_key` on the **caller side**
  (`let artifact_key = format!("artifacts/{}.parquet", blake3::hash(&bytes).to_hex());`) and `put` the
  captured Parquet bytes (Chunk 3) under it through the `ArtifactStore`. This completes the load-artifact
  primitive: `connector download → Parquet bytes → blake3 key → ArtifactStore::put(key, bytes)`.
  - ⚠️ **Invariant 9** (async boundary): the re-attach into DuckDB is sync under `spawn_blocking`; the
    `put` here is **async on the runtime** — `.await` it after the blocking task returns (or before the
    re-attach). Don't call the store from inside the blocking closure.
  - ⚠️ **Invariant 5** (snapshot = REPL bytes + manifest only): the artifact holds the **data**; the
    snapshot ([`M8-snapshot-resume.md`](./M8-snapshot-resume.md)) will record only the **artifact key /
    cache key**, never the rows. Storing the download here is exactly what keeps snapshots small and lets a
    resuming pod rebuild DuckDB by re-fetching the artifact.
  - 🔗 Maps to: the `artifact_key` the cache layer (Chunk 9) returns on a miss, and the keys the `M8`
    manifest records to make cross-pod resume work without re-loading.
  - ✅ Done when: a test runs `load(...)`, asserts a non-empty `artifact_key` was computed and `put`, and
    `get`ting that key from the store returns Parquet bytes that re-read with DuckDB's `read_parquet('...')`.

### Chunk 5 — Compute the first two-thirds of the cache key (scoped query + source)

- [ ] Build the first two inputs of the `cache_key`: a **canonical string for the scoped load** and the
  **source identifier**, then hash them with `blake3`. (The `freshness_token` third part comes in Chunk 6.)
  - 🆕 Concept: the **scoped load query** is the structured load request, not free-form SQL — the dataset
    name, the sorted `columns`, the normalized `where` filters, and `as_of`. Two loads that ask for the
    same slice must produce the **same** canonical string, so serialize it **deterministically**: sort the
    column list, sort/canonicalize the filter list, and use a stable representation (e.g. `serde` to a
    canonical JSON, or a hand-built string). Because the load request is *structured* (typed filters from
    `M2`, not raw SQL), this is far safer than normalizing arbitrary SQL — you control the shape. (No Rust
    Book chapter — project-side; ties to the typed filter helpers from `M2`.)
  - 🆕 Concept: the **source identifier** distinguishes the *same logical query against different sources*
    (e.g. a `usage_daily` backed by Athena vs by a local file). Include a stable id for the catalog binding
    so two different sources never collide on one cache key. (No Rust Book chapter — Droplet catalog,
    `PRODUCT.md` §6, §9.)
  - ⚠️ Be **conservative**: when unsure whether two loads are equivalent, treat them as **different**. A
    cache *miss* is safe (you just re-load); a wrong *hit* serves the wrong slice. Sort deterministically;
    never try to prove two filter sets "mean the same thing" semantically — that's a correctness trap.
  - ⚠️ **Invariant 2**: the cache key is computed from the **scoped, typed** load request — there is no
    arbitrary SQL here, so there is nothing to canonicalize unsafely. The boundary stays bounded.
  - ✅ Done when: a unit test shows two loads with the **same** slice (columns in a different order, filters
    in a different order) produce the **same** partial `cache_key`, while two genuinely different slices
    (different column, different filter, different source) produce different keys.

### Chunk 6 — Freshness policy: the `freshness_token` (PRODUCT §13)

The third input to the cache key. The spec (`PRODUCT.md` §13) defines **three** per-dataset policies. This
is the chunk that makes the cache *auto-invalidate when the source data changes* — without re-loading.

- [ ] Define a `FreshnessPolicy` enum with three variants and a method
  `token(&self, source) -> Result<String, DropletError>`:
  - 🆕 Concept: an **enum with data** lets each policy carry its own parameters (e.g. `Ttl(Duration)`); you
    `match` on it to compute the token. (Rust Book: *Enums and Pattern Matching*.)
  - **`Versioned` (default)** — token from the source's **version signal**: an Iceberg snapshot id, S3
    ETags / version-ids, or a watermark (`PRODUCT.md` §13). For the **dev connector** here, that signal is
    cheap and local — e.g. the source Parquet file's mtime + size, or a hash of its bytes. When the source
    changes, the signal changes → the token changes → the cache key changes → automatic invalidation,
    **without re-loading through the connector to find out**. The real S3 ETag / `version_id` and Iceberg
    snapshot-id versions arrive in [`M6-connectors-athena.md`](./M6-connectors-athena.md).
    - verify: where the version signal comes from is **per-`Source`** — put a `freshness_token(...)` method
      (or similar) on the `Source` trait so each connector supplies its own cheap signal. Confirm the exact
      shape against your `Source` trait from `M0`/`M2` before wiring it; the dev connector's signal is
      local-file metadata, the Athena/S3 connector's is the object ETag (`M6`).
  - **`Ttl(duration)`** — token = `floor(now / ttl)` as a string. Reuse the cached artifact for the whole
    window; skip the version check entirely. Cheaper (no source contact at all) at the cost of bounded
    staleness — good when the upstream already lags (`PRODUCT.md` §13).
  - **`Passthrough`** — never cache. The cache layer short-circuits: always run the connector fresh, never
    read or write the index.
  - ⚠️ **Invariant 2 / 7**: the `Versioned` check is a cheap **version signal, not a re-load** — it reads a
    small change-token (file metadata locally; an S3 `head_object` ETag in `M6`), never the slice's data.
    Checking freshness must **not** count as touching the source for real.
  - ✅ Done when: a test shows (a) the `Versioned` token **changes** when you modify the dev source file;
    (b) the `Ttl` token is stable within a window and rolls over after it; (c) `Passthrough` always forces
    a miss.

### Chunk 7 — Assemble the full cache key

- [ ] Combine the canonical scoped-load string (Chunk 5) + the source id (Chunk 5) + the `freshness_token`
  (Chunk 6) into the final `cache_key = blake3(scoped_load_query + source + freshness_token)`
  (`PRODUCT.md` §6, §13).
  - 🆕 Concept: concatenate the three inputs with an unambiguous separator (so `"a" + "bc"` can't collide
    with `"ab" + "c"`) before hashing — e.g. length-prefix each part, or join with a byte that can't appear
    in the parts. (No Rust Book chapter — hashing hygiene.)
  - ⚠️ **Invariant 7**: this single key is the fleet-wide handle on a load. Any pod that computes the same
    `cache_key` will resolve to the same `artifact_key` — that's what makes the download reusable across
    the fleet.
  - ✅ Done when: a unit test shows the same slice + same source + same freshness token → same `cache_key`;
    changing **any one** of the three inputs changes the key.

### Chunk 8 — The cache index in the CoordinationStore (in-memory dev impl)

- [ ] Add the cache-index methods `put_cache(cache_key, artifact_key) -> Result<(), DropletError>` /
  `get_cache(cache_key) -> Result<Option<String>, DropletError>` to the `CoordinationStore` trait. `M0`
  **deferred** `CoordinationStore` to `M7`, so define the trait here with just these two methods (the full
  run registry / leases land in [`M7-coordination.md`](./M7-coordination.md)), and implement them on an
  in-memory dev impl (an `Arc<tokio::sync::Mutex<HashMap<String, String>>>` is fine, matching the `Mem*`
  lock convention from `M0`).
  - 🆕 Concept: the cache index is **mutable shared state** — that's why it lives in the
    `CoordinationStore`, not the immutable `ArtifactStore`. (Rust Book: *Shared-State Concurrency*.)
  - 🆕 Concept: `get_cache` returns `Result<Option<String>, _>` — `Ok(None)` is a **miss** (a normal, safe
    outcome), distinct from an `Err` (the store itself failed). Don't model a miss as an error. (Rust Book:
    *Enums and Pattern Matching* — `Option`.)
  - ⚠️ **Invariant 7**: "mutable coordination (run registry, leases, **cache index**) is in the consistent
    store." In-memory is the **dev** impl only; the real consistent backends (Redis `HSET`/`HGET`,
    DynamoDB) arrive in [`M7-coordination.md`](./M7-coordination.md). Keep these methods on the trait now so
    the real impl drops in unchanged.
  - ✅ Done when: a test `put_cache`s a `cache_key → artifact_key`, reads it back with `get_cache` (→
    `Ok(Some(artifact_key))`), and a missing key returns `Ok(None)` (a miss).

### Chunk 9 — Wire the cache around the load path (the payoff)

- [ ] Assemble the **cache-aware load**: given the scoped load request `(dataset, columns, where, as_of)`
  plus its `source` and `freshness_policy`:
  1. If `Passthrough` → skip the cache, run the connector fresh, re-attach, return.
  2. Else compute `freshness_token` (Chunk 6), then `cache_key` (Chunk 7).
  3. Look up `cache_key` in the cache index with `get_cache(cache_key)` (Chunk 8).
     - **Hit** (`Ok(Some(artifact_key))`) → `get` the existing `artifact_key` from the `ArtifactStore`,
       re-attach the Parquet into the session's DuckDB, return the `Dataset`. **Do not call the connector.
       The source is not touched.**
     - **Miss** (`Ok(None)`) → run the connector (`M2`) → capture + `put` the Parquet (Chunks 3–4) →
       `put_cache(cache_key, artifact_key)` → re-attach → return.
  - ⚠️ **Invariant 2**: a hit must **never** call the connector — the source is the guarded door, and a
    cache hit is the whole point of not re-opening it.
  - ⚠️ **Invariant 7**: a hit means **state lived in the shared plane** — the Parquet is content-addressed
    in the `ArtifactStore`, the mapping is in the `CoordinationStore`, so any pod that computes the same
    `cache_key` reuses the same artifact. This is the "one bounded download, reused fleet-wide" guarantee.
  - 🔗 Maps to: this is the cache `load` will keep using once the **real** Athena connector + S3
    `ArtifactStore` land in [`M6-connectors-athena.md`](./M6-connectors-athena.md) — same control flow,
    real backends.
  - ✅ Done when: an integration test runs the **same** `load(...)` twice and asserts the **connector ran
    once** (e.g. a call-counter on the dev `Source` incremented only on the first call) while both calls
    return an equivalent `Dataset` (same rows).

### Chunk 10 — Prove cross-"pod" reuse + write the milestone test

- [ ] Prove cross-"pod" reuse with **two separate Session / cache instances** sharing the **same**
  in-memory `ArtifactStore` + cache index (clone the `Arc`s into both): run `load(...)` on instance A
  (miss → connector runs → artifact stored), then run the same `load(...)` on instance B (hit → connector
  does **not** run → artifact re-fetched and re-attached).
  - 🆕 Concept: two `Session`s sharing cloned `Arc<…>` stores **is** the test-bench model of two pods
    sharing one state plane. Real pods share Redis + S3 instead of cloned `Arc`s, but the cache logic is
    identical — which is exactly why the trait split pays off. (Rust Book: *Shared-State Concurrency*.)
  - 🔗 Maps to: the M5 slice of Success Criterion §20 — "a second run on another pod reusing the cached
    unload instead of re-hitting Athena." (Cross-pod *snapshot resume* is `M8`; cross-pod *cache reuse* is
    here.)
  - ✅ Done when: instance B returns the `Dataset` without the dev connector's call-counter incrementing.
- [ ] Write the **milestone test** end-to-end against the in-memory stores + dev connector:
  register a `Versioned` dataset; `load` a scoped slice → it runs the connector, stores the Parquet, and
  records the cache entry; `load` it **again** → it hits the cache and skips the connector; **modify the
  dev source file** → the `Versioned` token changes → the next `load` **misses** and re-runs the connector.
  Add a `Ttl` and a `Passthrough` dataset and assert their behavior (window reuse; always-miss).
  - ⚠️ **Invariant 8** (core never imports `pyo3`): this whole milestone lives in `droplet-core` — **no
    `pyo3`**, no Python in the loop. The cache must be drivable from a pure-Rust test.
  - ⚠️ **Invariant 10**: confirm a deliberately broken store/connector surfaces as a `DropletError`, not a
    raw backend error — every new failure path (a missing artifact key, the `blake3`/IO around Parquet, a
    cache-index miss treated as an error) folds into `DropletError` via `thiserror` `#[from]`.
  - ✅ Done when: the test passes all legs — cold miss → warm hit → invalidate-on-change miss, plus `Ttl`
    window reuse and `Passthrough` always-miss. This is M5's "Done when."

---

## Notes carried forward (don't act yet)

- **The stores are in-memory dev impls.** They prove the *shape*, not distribution — two real pods don't
  share an `Arc`. [`M6-connectors-athena.md`](./M6-connectors-athena.md) swaps in the **S3/MinIO**
  `ArtifactStore` (real bytes, content-addressed in object storage) behind the same trait, and the **real
  Athena connector** (`UNLOAD → Parquet on S3`) behind the same `Source` trait.
  [`M7-coordination.md`](./M7-coordination.md) swaps the cache index in-memory `HashMap` for **Redis**
  (`HSET droplet:cache_index <cache_key> <artifact_key>` / `HGET`) and **DynamoDB**. Design the trait
  methods now so those swaps are invisible to the cache logic you wrote in Chunk 9.
- **The cache fronts the *load*, not an analyze query.** Nothing in the analyze surface
  ([`M1-analyze-engine.md`](./M1-analyze-engine.md)) is cached or content-addressed here — analyze runs
  freely on the local copy and never re-reads the source. The only thing this cache protects is the
  connector boundary. Don't bleed cache assumptions into the analyze primitives.
- **Per-run intermediates can reuse the same `put` (later).** The `ArtifactStore` is generic — it can
  store both the **fleet-wide load cache** artifacts (this file) *and*, eventually, a run's private
  intermediate Parquet (analysis materializations). Same content-addressed `put`, different lifecycle.
  Keep the store generic — don't bake "load cache" assumptions into `ArtifactStore::put`.
- **Snapshots will reference these keys (`M8`).** When you build the snapshot manifest
  ([`M8-snapshot-resume.md`](./M8-snapshot-resume.md)), it records the **cache keys / artifact keys** of
  the loaded datasets — never the rows (Invariant 5). On resume, the pod rebuilds DuckDB by re-fetching
  those artifacts and re-attaching them. M5's load-artifact storage is what makes that possible; nothing
  to do now, just don't lose track of the keys.

---

> 📌 When you reach this milestone, expand each chunk into tiny steps the way `M0`–`M3` are written.
