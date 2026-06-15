# M7 — SnapshotStore + cross-pod resume (SKETCH)

**Milestone goal:** snapshot a running session as **Monty REPL bytes + a session manifest** (never
engine heaps), store it as a zstd-compressed, content-addressed blob in the S3 `SnapshotStore`, and
**resume the run on a *different* pod** — rebuilding DuckDB from the manifest and continuing exactly
where it left off, lease-guarded.

**Done when (from the spec):** the session is snapshotted to the shared store and **resumable on a
different pod that rebuilds DuckDB from the manifest** (and continues the run to the same result).

**Prerequisite:** finish [`M6-field-search.md`](./M6-field-search.md). By now you have: the Monty
driver + `run_code` loop ([`M4-monty-driver.md`](./M4-monty-driver.md)), the four store traits with
S3/Redis/DynamoDB impls ([`M0-skeleton.md`](./M0-skeleton.md) / [`M2-artifact-cache.md`](./M2-artifact-cache.md)
/ [`M3-coordination.md`](./M3-coordination.md)), the artifact cache + cache index
([`M2-artifact-cache.md`](./M2-artifact-cache.md)), DuckDB reading S3 ([`M1-duckdb.md`](./M1-duckdb.md)),
and the read-only Surreal field index ([`M6-field-search.md`](./M6-field-search.md)). M7 ties them
together into a portable run.

**Estimate:** ~10 chunks.

> This is a **SKETCH** file: chunk-level checkboxes with concept notes and invariant callouts, *not*
> the tiny per-line steps of M0/M1. Get the shape right first; expand into tiny steps when you reach
> this milestone.

---

## Why this milestone is its own thing (read first, 5 min)

A Droplet run lives on **one pod at a time**, but its *state lives in the shared plane* so any pod can
pick it up. M7 is the mechanism that makes that true. The trick — and the whole reason snapshots stay
tiny — is that you **never** serialize the live engines. You serialize two small things:

- **Monty REPL bytes** — `repl.dump()` gives you the whole interpreter state as a `Vec<u8>` (postcard).
  The agent's Python variables, call stack, everything — but by boundary discipline (invariant #4,
  established back in M1/M4) the REPL only holds *handles + capped result rows*, not data, so this
  stays small.
- **A session MANIFEST** — a small struct of *references*: the schema ref, the source refs (which S3
  Parquet/CSV the session attached), and the materialized artifact keys (the content-addressed parquet
  the run produced). This is the *recipe* to rebuild DuckDB, not DuckDB itself.

On resume — possibly on a brand-new pod — you take the lease, load the snapshot, and **rebuild DuckDB
from the manifest** by re-attaching the source views and re-registering the materialized artifacts.
That's cheap: it registers views over S3, it does not re-scan data. The read-only Surreal field index
is **schema-derived**, so it is rebuilt from the schema too — never snapshotted.

> ⚠️ Invariant #3 (the spine of this whole file): *"Snapshot = REPL bytes + manifest only; never
> serialize engine heaps; reconstruct DuckDB on resume. Snapshots immutable, content-addressed,
> versioned, compressed."* If you ever find yourself trying to `serde` a DuckDB `Connection` or a
> Surreal handle, stop — you've broken the milestone.

> ⚠️ Invariant #8: *"Distributed by default: state lives in the shared plane; immutable data is
> content-addressed in the object store; mutable coordination (registry, leases, cache index) is in
> the consistent store. Resume is lease-guarded; no affinity."* The snapshot blob is the immutable
> object-store half; the run registry + lease ([`M3-coordination.md`](./M3-coordination.md)) are the
> consistent-store half; M7 joins them.

> ⚠️ Invariant #1: all of this lives in `droplet-core` — **no `pyo3`**. Snapshot, store, and cross-pod
> resume must be exercisable from a pure-Rust integration test (two `Session`s pointed at the same
> shared backends), never through Python.

The two sharp edges, both flagged below: **postcard is not self-describing and not versioned**, so the
manifest must carry explicit version fields and refuse a mismatch; and the **Monty REPL bytes are tied
to a Monty version**, so a fleet-wide Monty tag is a hard requirement for resume to work.

---

### Chunk 1 — Define the session MANIFEST struct (the rebuild recipe)

- [ ] Define a `Manifest` struct in `droplet-core` with `#[derive(Serialize, Deserialize)]` carrying
  only *references*. `serde` is already a workspace dep with `features = ["derive"]` from M0:
  ```rust
  #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
  struct Manifest {
      snapshot_format_version: u32,   // bumped whenever this struct's shape changes
      monty_version: String,          // the Monty tag the REPL bytes were dumped on (e.g. "v0.0.18")
      schema_version: u32,            // the registered-schema version (M5/M6)
      schema_ref: String,             // key/ref to rebuild the Surreal field index + DuckDB types
      source_refs: Vec<String>,       // which S3 Parquet/CSV sources to re-attach as views
      artifact_keys: Vec<String>,     // content-addressed materialized parquet to re-register
  }
  ```
  - 🆕 Concept: a **manifest** is plain data describing how to rebuild something — here, the *recipe*
    for DuckDB (which sources to attach, which materialized artifacts to register), not the engine
    itself. Serializing references is cheap and engine-version-independent. (Rust Book: *Using Structs
    to Structure Related Data*, ch. 5)
  - 🆕 Concept: `#[derive(Serialize, Deserialize)]` is a **derive macro** that writes the
    to/from-bytes glue for you at compile time — the same derive works for JSON, postcard, etc.; you
    swap the *format* crate, not the struct. (Rust Book: *Generic Types, Traits, and Lifetimes*, ch.
    10 — derive macros implement traits for you.)
  - ⚠️ Invariant #3: the manifest is "manifest only" — it holds **keys and refs**, never rows, never an
    Arrow `RecordBatch`, never an engine object. If a field is bigger than a handful of strings, it
    doesn't belong here.
  - ✅ Done when: `cargo build -p droplet-core` is green with `Manifest` defined and the serde derives
    resolving.

### Chunk 2 — Add the snapshot-format version constant and the version-mismatch rule

- [ ] Add a `const SNAPSHOT_FORMAT_VERSION: u32 = 1;` next to `Manifest`, write it into
  `manifest.snapshot_format_version` at snapshot time, and **bump it whenever the `Manifest` struct
  changes shape** (add/remove/reorder a field).
  - 🆕 Concept: **postcard is not self-describing** — it stores no field names, so if you add, remove,
    or reorder a field, old bytes mis-decode *silently* (no error, wrong values). An explicit version
    field you check **first** turns that silent mis-decode into a loud, refusable failure. (No Rust
    Book chapter — serde-format property; see the postcard docs.)
  - ⚠️ Invariant #3: "versioned." The version is part of *why* "immutable + content-addressed" is safe
    to trust on resume — Chunk 8 gates on it before decoding anything else.
  - ✅ Done when: the constant exists and a code comment ties "change `Manifest` ⇒ bump this" together.

### Chunk 3 — Capture the Monty REPL bytes (`dump`) and record the Monty tag

- [ ] After a `run_code` step completes (the `ReplProgress::Complete` arm from the M4 driver), call
  `repl.dump()` to get the REPL state as `Vec<u8>`, and set `manifest.monty_version` to the exact
  pinned Monty tag at the same time.
  - 🆕 Concept: `repl.dump()` serializes the **whole Monty interpreter state** to postcard bytes;
    `MontyRepl::load(&bytes)` rebuilds it. You serialize the *sandbox*, not the engines. (No Rust Book
    chapter — Monty-specific.)
  - ⚠️ Invariant #3: you dump the REPL **only**. Anything heavy the agent touched lives behind a handle
    in the host registry — the REPL bytes carry the opaque handles, not the engine objects they point
    at (invariant #4 is what keeps this small).
  - ⚠️ verify: the exact `dump` / `load` signatures at the pinned tag `v0.0.18` — the digest observed
    `repl.dump() -> Result<Vec<u8>, postcard::Error>` and `MontyRepl::load(&bytes)`. Confirm whether
    `dump` takes `&self` / `&mut self` / consumes `self` before wiring it into the `Session`, by
    reading `crates/monty/src/repl.rs` at the tag. (This is pre-1.0 and churns.)
  - ⚠️ verify: postcard snapshot stability is **not** guaranteed across Monty versions — a snapshot
    dumped on `v0.0.18` may not `load` on a different Monty tag. So resume must check
    `manifest.monty_version` and refuse a mismatch (Chunk 8). Pin **one** Monty tag fleet-wide; treat
    any Monty bump as a snapshot-format break.

### Chunk 4 — Serialize the manifest with postcard and choose the blob layout

- [ ] Add `postcard = { version = "1", features = ["use-std"] }` to `[workspace.dependencies]`, opt
  `droplet-core` in, and serialize the `Manifest` to compact bytes with
  `postcard::to_allocvec(&manifest)?`. Round-trip it in a `#[test]`
  (`to_allocvec` → `from_bytes` → `assert_eq!`).
  - 🆕 Concept: **serde is the framework; postcard is one compact binary format.** Monty already uses
    postcard, so it's the consistent choice. `to_allocvec` needs the `alloc` feature; the `use-std`
    feature turns it on — without it `to_allocvec` doesn't exist and you get a "function not found"
    error. (No Rust Book chapter — see the postcard docs.)
  - ⚠️ Do **not** reach for `bincode` here despite older tutorials: bincode is officially
    **unmaintained** as of late 2025 (its own docs.rs page says so and points to postcard/rkyv).
    postcard is both maintained *and* Monty-consistent.
  - ✅ Done when: a `#[test]` postcard-round-trips a `Manifest` and asserts equality.
- [ ] Decide the **blob layout**: how the REPL bytes and the manifest bytes travel together inside one
  snapshot blob. Use a tiny outer struct you postcard-then-zstd as a whole, so one blob = one
  content-addressed object:
  ```rust
  #[derive(serde::Serialize, serde::Deserialize)]
  struct Snapshot {
      manifest_bytes: Vec<u8>, // postcard-encoded Manifest
      repl_bytes: Vec<u8>,     // Monty repl.dump() output (also postcard)
  }
  ```
  - ⚠️ verify: nesting postcard-in-postcard is fine (both halves are just `Vec<u8>` payloads), but
    write the round-trip test so you *see* both halves survive — encode `Snapshot`, decode it back, and
    assert both `manifest_bytes` and `repl_bytes` are byte-identical.

### Chunk 5 — zstd-compress the snapshot blob

- [ ] Add `zstd = "0.13"` to `[workspace.dependencies]`, opt `droplet-core` in, and compress the
  outer `Snapshot` bytes with `zstd::encode_all(&bytes[..], 3)?`; decompress on the way back with
  `zstd::decode_all(&blob[..])?`. Test the full round-trip (`Snapshot` → postcard → zstd → decompress
  → postcard → equal `Snapshot`).
  - 🆕 Concept: `zstd::encode_all` takes anything implementing `Read`; a `&[u8]` already does, so
    `&bytes[..]` works directly — no `Cursor` needed. Level `3` is a sane default (valid range is
    roughly `1..=22`; higher = smaller + slower; `0` means "use default", currently 3). (No Rust Book
    chapter.)
  - ⚠️ Invariant #3: "compressed" — the snapshot blob is zstd-compressed **by construction**. The
    round-trip `#[test]` is what lets you trust it before anything else depends on it.
  - ✅ Done when: a `#[test]` compresses then decompresses a snapshot blob and the decoded `Snapshot`
    equals the original.

### Chunk 6 — Content-address + version the blob

- [ ] Add `blake3 = "1"` to `[workspace.dependencies]`, opt `droplet-core` in, and compute the
  snapshot key as `blake3::hash(&blob).to_hex().to_string()` over the final (compressed) bytes. This
  hex string is the object key in the `SnapshotStore`.
  - 🆕 Concept: **content-addressing** = the key *is* a hash of the bytes, so identical snapshots
    dedupe automatically and a key always points at exactly those bytes. `blake3::hash(...).to_hex()`
    returns a `blake3::Hex` (an `ArrayString`), not a `String` — call `.to_string()` for an owned
    `String` key. (No Rust Book chapter.)
  - ⚠️ Invariant #3: "immutable, content-addressed, versioned." The blob is immutable (a new snapshot
    = a new key, never an overwrite), and the version lives *inside* the manifest
    (`snapshot_format_version`, `monty_version`, `schema_version`) so you can check it before decoding
    the rest (Chunk 8).
  - 🔗 Maps to: this is the **same** `blake3` content-addressing the `ArtifactStore` uses for
    materialized parquet (M2) — one hashing helper, two callers (artifact keys *and* snapshot keys).
    Reuse the M2 helper rather than writing a second one.

### Chunk 7 — Store via the S3 SnapshotStore + write the registry pointer

- [ ] Implement the `SnapshotStore` `put`/`get` against S3 (the trait was defined in M0 with an
  in-memory dev impl; here you fill in the S3 impl, mirroring the `ArtifactStore` S3 impl from M2). Use
  `aws-config = { version = "1.8.18", features = ["behavior-version-latest"] }` and
  `aws-sdk-s3 = "1.136.0"` (already wired in M2 for `ArtifactStore`):
  `put_object().bucket(b).key(k).body(ByteStream::from(bytes)).send().await?`; `get_object(...)`
  returns a body you `.collect().await?.into_bytes().to_vec()`.
  - 🆕 Concept: every AWS SDK call ends in `.send().await` — it's fully async and needs a Tokio
    runtime. S3 GET bodies are streams (`ByteStream`), so you `.collect().await` then `.into_bytes()`.
    (No Rust Book chapter — AWS SDK.)
  - ⚠️ verify: pin **exact** AWS crate versions in `Cargo.toml` (these crates publish ~weekly); the
    digest verified `aws-config 1.8.18` / `aws-sdk-s3 1.136.0` on 2026-06-15. Re-check before pinning,
    and confirm M2 already locked them so M7 reuses the same versions.
  - ⚠️ Invariant #8: the snapshot blob is the **immutable, content-addressed object-store** half of the
    shared plane. Store it under the blake3 key from Chunk 6; never mutate an existing key.
  - ⚠️ Invariant #10: fold `SdkError` and the S3 not-found case into `DropletError` via `thiserror`
    `#[from]` at the store boundary, so callers see one error type, never a raw `SdkError`.
  - ✅ Done when: a pure-Rust test does `SnapshotStore::put(blob)` then `get(key)` against MinIO
    (local-dev S3) and the bytes round-trip.
- [ ] After a successful `put`, write the snapshot pointer into the **run registry**
  (CoordinationStore, M3): `run_id -> { snapshot_key, status }`.
  - ⚠️ Invariant #8: "mutable coordination (registry, leases, cache index) is in the consistent store."
    The blob is in S3 (immutable); the *pointer* to the latest blob is in Redis/DynamoDB (consistent)
    so another pod can find it. Don't put the pointer in S3.
  - 🔗 Maps to: the per-run hash (`droplet:run:{run_id}`) / item you built in M3 — M7 just adds a
    `snapshot` field to it.

### Chunk 8 — Resume part 1: acquire the lease, load + decompress, version-gate

- [ ] On `Session::resume(run_id)` — possibly on a **different pod** — first **acquire the lease** for
  `run_id` via the CoordinationStore (M3) before touching the snapshot.
  - ⚠️ Invariant #8: "Resume is **lease-guarded**; no affinity." Exactly one pod may resume a run at a
    time. If the lease is held by another pod, back off — do **not** load and continue.
  - 🔗 Maps to: this is the Redis `SET NX PX` / DynamoDB `attribute_not_exists` lease you built in M3;
    M7 is its first real consumer beyond a unit test.
- [ ] Read the snapshot pointer from the run registry, `get` the blob from the `SnapshotStore`,
  `zstd::decode_all`, then `postcard::from_bytes` the outer `Snapshot`, then `postcard::from_bytes` the
  `manifest_bytes` to get the `Manifest`.
  - ✅ Done when: a test stores a snapshot on "pod A" (one `Session`/store handle) and reads the
    manifest back through a *separate* "pod B" `Session` pointed at the same MinIO + Redis.
- [ ] **Version-gate before doing anything else:** check
  `manifest.snapshot_format_version == SNAPSHOT_FORMAT_VERSION` **and**
  `manifest.monty_version == "<pinned tag>"`; on mismatch return a clear `DropletError` instead of
  attempting to `MontyRepl::load` or rebuild.
  - ⚠️ Invariant #3: "versioned." postcard mis-decodes silently on a shape change, and Monty REPL bytes
    don't `load` across Monty versions — so fail **loud**, never silently mis-decode. This is the guard
    that makes "immutable + versioned" actually safe.
  - 🔗 Maps to: the same "refuse a version mismatch" discipline M4 flagged for the snapshot format and
    Monty tag — enforced here at the resume boundary.

### Chunk 9 — Resume part 2: rebuild DuckDB from the manifest; rebuild Surreal from schema

- [ ] Rebuild a fresh ephemeral DuckDB `Connection` from `manifest`: re-attach the **source views**
  (re-create the `read_parquet('s3://...')` views from `source_refs`) and re-register the
  **materialized artifacts** (attach the content-addressed parquet from `artifact_keys`).
  - 🆕 Concept: rebuilding DuckDB is **cheap** — re-creating a view over S3 registers a *recipe*, it
    does not re-scan the data. The materialized artifacts are already in the `ArtifactStore`
    (content-addressed parquet from M2), so resume just re-attaches them. (No Rust Book chapter.)
  - ⚠️ Invariant #3: "reconstruct DuckDB on resume." You **never** loaded a DuckDB heap from the
    snapshot — you rebuilt it from the manifest's refs. This is the payoff of having serialized refs
    instead of engines in Chunk 1.
  - ⚠️ Invariant #6: DuckDB is synchronous — do the re-attach/registration inside `spawn_blocking` (and,
    at the `droplet-py` edge later, release the GIL), exactly as M1 established. Don't run DuckDB on the
    async executor.
  - ⚠️ Invariant #9: per-run isolation — the resumed `Session` gets its **own** ephemeral DuckDB
    connection and a fresh working dir, with S3 creds scoped to that session. Resume must not share a
    connection or dir with any other run on the pod.
- [ ] Restore the REPL with `MontyRepl::load(&snapshot.repl_bytes)` so the agent's variables and call
  state come back exactly as dumped.
  - ⚠️ verify: the `MontyRepl::load` signature and return type at the pinned tag (digest observed
    `MontyRepl::load(&bytes) -> Result<MontyRepl, postcard::Error>`); confirm before wiring it.
  - ⚠️ Invariant #4: the loaded REPL holds only **handles + capped rows** — the engine objects those
    handles point at are the freshly-rebuilt DuckDB/Surreal from this chunk, re-registered in the host
    handle registry, not anything carried in the snapshot.
- [ ] Rebuild the **read-only Surreal field index from the schema** (`manifest.schema_ref` +
  `schema_version`) — do **not** read it from the snapshot (it was never snapshotted).
  - ⚠️ Invariant #5: "SurrealDB is read-only and schema-derived." Because it's derived from the schema
    (M6's `build_field_index`), resume regenerates it the same way Session-open did. There is nothing
    Surreal-shaped in the snapshot to restore — and there must not be.
  - ⚠️ verify: that `manifest.schema_ref` + `schema_version` are sufficient to regenerate the
    *identical* field index on the resuming pod (same fields, same embedding model + `EMBED_DIM`) —
    otherwise `search_fields` could behave differently after resume.
- [ ] Continue the run: feed the next `run_code` step into the restored REPL and confirm it sees
  prior-step state (variables defined before the snapshot still resolve) and can call `run_sql` against
  the rebuilt DuckDB.
  - ✅ Done when: a step run *after* resume on pod B uses a variable set *before* the snapshot on pod A,
    and a `run_sql` over a re-attached source returns rows.

### Chunk 10 — Write-behind + the durability barrier, then the cross-pod test

- [ ] Make snapshotting **write-behind**: kick off the `put` + registry-update on the Tokio runtime
  *after* a `run_code` step finishes, without blocking the next step from starting — but enforce a
  **durability barrier** at the step boundary so step *N*'s snapshot is durably stored before step
  *N+1*'s results are externally observable.
  - 🆕 Concept: **write-behind** = let the slow store-write happen in the background instead of on the
    critical path; a **durability barrier** = a point you refuse to cross until the prior write is
    confirmed durable. Together: fast steps, but no "resumed from a snapshot that never landed." (No
    Rust Book chapter — distributed-systems pattern.)
  - ⚠️ Invariant #3 + spec §6: *"Write-behind during execution; durability barrier only at step
    boundary / suspend. Granularity: per `run_code` step."* So you snapshot **per `run_code` step**, at
    the suspend/step boundary — **not** per external-function call inside a step.
    (Incremental/per-call snapshots are explicitly OUT of v1 scope.)
  - 🔗 Maps to: the step boundary is the same clean seam M4 used for the suspend/resume loop — the REPL
    is stopped between steps, so there's no half-finished bytecode to capture.
  - ⚠️ verify: where exactly the barrier sits — likely "the step's result is not returned to the caller
    until that step's snapshot `put` is acked." Test both placements (barrier before vs after returning
    results) and pick the one that guarantees a resumable run with no lost step.
- [ ] Write the end-to-end **"snapshot on pod A → resume on pod B"** integration test (pure Rust):
  two pods share one MinIO (`ArtifactStore` + `SnapshotStore`) and one Redis/DynamoDB-local
  (`CoordinationStore`). **Pod A** opens a Session, runs a `run_code` step that materializes a result
  and snapshots; **pod B** (a *separate* Session/store-handle set, no shared in-process state) acquires
  the lease, resumes from the snapshot key, rebuilds DuckDB from the manifest, and runs a follow-up
  step to completion.
  - 🆕 Concept: the two "pods" are just two independent `Session` objects bridged **only** by the
    shared backends (S3 + coordination store) — no shared memory, no affinity. That's the whole
    distributed claim, proven in one test. (No Rust Book chapter.)
  - ⚠️ Invariant #1: both pods are pure-Rust `Session`s — **no `pyo3`** anywhere in this test. Cross-pod
    resume must work standalone before any Python wraps it.
  - ⚠️ Invariant #8: assert the lease actually gated the resume — a second concurrent resume attempt
    while pod B holds the lease must be rejected.
  - ✅ Done when: pod B's final result equals what a single uninterrupted run on pod A would produce —
    the spec's "Done when": *resumable on a different pod that rebuilds DuckDB from the manifest.*
- [ ] Add a **guard test**: corrupt/age a snapshot so its `monty_version` (or
  `snapshot_format_version`) no longer matches, and assert resume returns a clear `DropletError` rather
  than a panic or a silently mis-decoded REPL.
  - ⚠️ Invariant #3: this protects the postcard-not-versioned and Monty-version-tied hazards from
    Chunks 2, 3, 8 — fail loud, never mis-decode.

---

## Notes carried forward

- **One Monty tag fleet-wide is a hard requirement.** Resume `load`s REPL bytes produced by some Monty
  version; postcard gives no cross-version guarantee. Pin `v0.0.18` everywhere and treat any Monty bump
  as a snapshot-format break (bump `SNAPSHOT_FORMAT_VERSION` and refuse old blobs). Re-verify the
  `dump`/`load` signatures and the postcard layout at the pinned tag before shipping.
- **Snapshots are small *by construction*, not by luck.** They stay tiny only because boundary
  discipline (invariant #4) keeps the REPL holding handles + capped result rows, never data. If a
  snapshot ever balloons, the leak is upstream — a tool returned uncapped rows into the sandbox — not
  in the snapshot code.
- **The blob is immutable; the pointer is mutable.** The content-addressed snapshot blob lives in S3
  and is never overwritten; only the registry pointer (`run_id -> latest snapshot_key`) in the
  consistent store changes per step. Keep that split clean (invariant #8).
- **Surreal and DuckDB are rebuilt, never restored.** Nothing engine-shaped is in the snapshot. If a
  future change tempts you to "just cache the Surreal index in the snapshot to save a rebuild," that
  violates invariants #3 and #5 — rebuild from the manifest/schema instead.
- **Reuse, don't re-derive.** The `blake3` content-addressing (M2), the AWS S3 client + `ByteStream`
  pattern (M2), the lease (M3), and the registry hash (M3) all already exist — M7 is mostly *wiring*
  those proven pieces together, plus the `Manifest` + `Snapshot` serde structs and the version gate.

---

> 📌 When you reach this milestone, expand each chunk into tiny steps the way M0/M1 are written.
