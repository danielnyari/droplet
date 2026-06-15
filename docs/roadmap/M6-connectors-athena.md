# M6 — Real connectors: Athena + S3/Iceberg (SKETCH)

**Milestone goal:** build the **`droplet-connectors`** crate and the first **real** connectors behind the
`Source` trait, so `load` can finally hit an actual warehouse and land Parquet in the M5 ArtifactStore,
**cached fleet-wide**. Three connectors, all behind one trait:

- **S3 direct read** — the data is *already* Parquet on object storage; the connector just points at it.
  No unload.
- **Iceberg direct read** — also already Parquet; the connector resolves the table's current (or `as_of`)
  snapshot to a set of Parquet files and reads those. No unload.
- **Athena** — the first connector that performs a **source-native bulk unload**:
  `UNLOAD (SELECT …) TO 's3://…' WITH (format='PARQUET')`, then **picks up** the Parquet it wrote.

The trivial **local-Parquet dev connector** from M0/M2 stays the **default**, so you only need an AWS
account when you *opt in* to the Athena/Iceberg path. This is the milestone that satisfies PRODUCT.md §16
v1 scope: *"Connectors: S3 + Iceberg (direct parquet) and **Athena** (`UNLOAD` to S3) — proves the
unified abstraction with one real unload engine."*

**Done when (from the spec):** an agent's `load("usage_daily", columns=…, where=…, as_of="latest")`
triggers **one** Athena `UNLOAD`, the resulting Parquet is materialized into the M5 ArtifactStore under
its content-addressed key, and a **second** `load` of the same scope **reuses the cached unload** instead
of re-hitting Athena — *and the agent cannot tell* which engine produced the data (Invariant 1). The
local-Parquet dev connector still runs with **zero** AWS setup.

**Prerequisite:** finish [`M5-artifact-cache.md`](./M5-artifact-cache.md). You need the real
`ArtifactStore` (S3/MinIO) with content-addressed `put`/`get`, the **content-addressed cache** keyed
`hash(scoped query + source + freshness token)` with the cache index in the `CoordinationStore`, and the
**freshness policy** (Versioned / TTL / Passthrough). You also need, from
[`M2-load-boundary.md`](./M2-load-boundary.md), the **Catalog** (logical dataset → connector + schema),
the `load(name, columns, where, as_of) -> Dataset` call, and the typed **filter helpers**
(`eq`, `gt`, `between`, …). From M0 you need the **`Source` trait** itself (defined there with the
trivial local-Parquet dev connector as its first impl) and `DropletError` (`thiserror`). M6 *adds real
`Source` impls* behind that trait and the AWS clients they need — it does **not** redefine the trait, the
cache, or `load`.

**Estimate:** ~9 chunks (a chunk ≈ one focused sitting).

The spec lives at `PRODUCT.md` (repo root). Reference it that way, never `docs/PRODUCT.md`.

> This is a **SKETCH** file: chunk-level checkboxes with concept notes and invariant callouts, *not* the
> tiny per-line steps of M0–M3. Get the shape right first; when you reach this milestone, expand each
> chunk into tiny steps the way M0–M3 are written.

---

## How to read this file

- Every `- [ ]` is a task. SKETCH tasks are coarser than M0–M3 — each is roughly one focused sitting, not
  10 minutes.
- `🆕 Concept:` explains a Rust/Droplet idea the **first** time it shows up, with a Rust Book chapter name
  (run `rustup doc --book` to open the book offline).
- `✅ Done when:` is an observable check — a command's output or a passing test. Don't move on until you
  see it.
- `⚠️ Invariant:` quotes a load-bearing rule from `PRODUCT.md` §15 in plain words, by its number **1–10**
  (the same numbering as the README's Golden Rules). Never break these.
- `🔗 Maps to:` ties a task to the real Droplet concept it unlocks.
- `verify:` flags a fact the research couldn't fully pin on the locked version — check the crate
  source/docs (here: the AWS SDK for Rust + the DuckDB httpfs docs) **before** relying on it. The AWS SDK
  crates ship **~weekly**, so every `aws-*` version in this file is a `verify:` to re-confirm on the day
  you pin.
- Code snippets are **anchors** (a few lines to orient you). You write the real implementation.

---

## What M6 adds (read first, 5 min)

Up to now, every `load` has gone through the **trivial local-Parquet dev connector** — the `Source` impl
M0 wrote so the whole pipeline could work with **zero** external services. `load("usage_daily", …)`
looked up the dataset in the catalog, the dev connector pointed at a local `.parquet` file, M5 cached it,
and DuckDB analyzed it. That proved the *shape* end to end. But it never touched a real warehouse.

M6 makes `load` real. It builds the **`droplet-connectors`** crate (PRODUCT.md §17) and three production
`Source` impls behind the **same** trait the dev connector already satisfies:

1. **`S3Source`** — for a dataset whose data is *already* Parquet on S3. The connector's whole job is to
   resolve the scope to a set of `s3://…/*.parquet` objects and hand them off. **No unload** — the source
   *is* Parquet, so there's nothing to convert.
2. **`IcebergSource`** — for an Apache Iceberg table. Iceberg's data files are *also* already Parquet; the
   connector resolves the table's current (or `as_of`) **snapshot** to its list of data files and reads
   those. The snapshot id doubles as the freshness token *and* the `as_of` time-travel token (PRODUCT.md
   §13). Still **no unload**.
3. **`AthenaSource`** — the real prize. Athena is a query engine, not a pile of Parquet, so the connector
   runs a **source-native bulk unload**:
   `UNLOAD (SELECT <columns> FROM <table> WHERE <scope>) TO 's3://<staging>/…' WITH (format='PARQUET')`,
   waits for it to finish, then **picks up** the Parquet files Athena wrote. That one unload is the
   *only* time Athena is touched for that scope — afterward the result is cached and every further analyze
   step runs locally (Invariant 2 + 3).

The payoff: the **same** `load(...)` call works against any of them, and the agent **cannot tell which**
(Invariant 1). Adding an engine later (Snowflake, BigQuery — deferred) is "one new `Source` impl, nothing
else changes" (PRODUCT.md §9).

> ⚠️ **Invariant 1** (the agent never sees the real engine): "All sources are reached through connectors
> that normalize to Parquet; the agent works against logical, local datasets only." This is the whole
> milestone. The `Source` trait is the seam that *enforces* it — the agent calls `load("usage_daily")`
> and gets a local `Dataset`; whether `usage_daily` is backed by a local file, an S3 prefix, an Iceberg
> table, or an Athena `UNLOAD` is **catalog configuration it never sees**.

> ⚠️ **Invariant 2** (only `load` touches the source, as a bounded/typed/cached unload): Athena is hit
> **once** per scope, by `UNLOAD`, through the cache. There is **no arbitrary SQL against production** —
> the `UNLOAD`'s `SELECT` is assembled *by the connector* from the typed `columns` + `where` the catalog
> validated, never from agent text.

> 📌 **Local dev = the dev connector + MinIO; real Athena needs AWS.** Everything in this file degrades
> gracefully: the **default** catalog binding stays the local-Parquet dev connector (zero setup); the
> S3/Iceberg connectors run against **MinIO** (the M5 local S3); only **Athena** needs a real AWS account
> (Athena workgroup + an S3 staging bucket for `UNLOAD` output) — there is **no faithful local Athena
> emulator**. Gate the Athena tests behind an env var so plain `cargo test` stays green with no AWS.

---

### The `Source` trait (recap — defined in M0, used here)

You are *implementing* this trait three more times, not defining it. Re-read M0's definition; the shape
is roughly:

```rust
#[async_trait]                       // async-fn-in-traits isn't dyn-compatible → async-trait (M0 dep)
pub trait Source: Send + Sync {
    /// Produce Parquet for a scoped load, and return where it landed + a freshness token.
    /// `ScopedLoad` carries the validated columns + where-filters + as_of (from the catalog, M2).
    async fn load(&self, scope: &ScopedLoad) -> Result<LoadOutput, DropletError>;
}
```

- `ScopedLoad` is the *already-validated* request the M2 `load` boundary built from the typed call
  (`columns`, `where`, `as_of`). The connector trusts it — typing happened at the boundary (Invariant 2).
- `LoadOutput` tells the caller **where the Parquet is** (local path, or `s3://…` object keys) **and** the
  **freshness token** for this slice, so M5's cache key can be computed. (`verify:` the exact field names
  of `ScopedLoad` / `LoadOutput` against what M0/M2 actually defined — match them, don't reinvent.)

> 🆕 **Concept: one trait, many impls = polymorphism via trait objects.** The catalog stores each dataset's
> connector as a `Box<dyn Source>`. `load` calls `source.load(&scope).await` without knowing the concrete
> type — Rust dispatches to the right impl at runtime. This is exactly the "trait object" pattern from the
> warm-up. (Rust Book: *Generic Types, Traits, and Lifetimes*, ch. 10 — trait objects; *Using Trait
> Objects That Allow for Values of Different Types*, ch. 18.)

---

### Chunk 1 — Create the `droplet-connectors` crate

- [ ] Add a new workspace member `crates/droplet-connectors/` (PRODUCT.md §17) and wire it into the root
  `Cargo.toml` `members`. It depends on `droplet-core` (for the `Source` trait, `ScopedLoad`/`LoadOutput`,
  and `DropletError`) and on the AWS SDK crates (next chunk).
  - 🆕 Concept: a Cargo **workspace member** is a sub-crate that shares the workspace's `Cargo.lock` and
    `[workspace.dependencies]`. Splitting connectors into their own crate keeps `droplet-core` free of the
    AWS SDKs unless a binary actually links a connector. (Rust Book: *More About Cargo and Crates.io*,
    ch. 14 — workspaces.)
  - 🆕 Concept: **why a separate crate at all?** The `Source` trait lives in `droplet-core`; the *impls*
    live here. That way `droplet-core` (and the snapshot/coordination logic) never needs the heavy cloud
    SDKs in its build graph — only the binary that wires a real catalog pulls `droplet-connectors` in.
    (PRODUCT.md §17 repo structure: `droplet-connectors/  # Source impls: s3, iceberg, athena`.)
  - ⚠️ **Invariant 8** (keep Python out of the core, and keep the core lean): `droplet-connectors` depends
    on `droplet-core`, **never** the reverse, and **never** on `pyo3`. Connectors are pure-Rust + AWS SDKs;
    they must be drivable from a Rust test with no Python in the loop.
  - ✅ Done when: `cargo build -p droplet-connectors` compiles an empty crate that `use droplet_core::Source;`
    resolves.

- [ ] Lay out modules: `s3.rs`, `iceberg.rs`, `athena.rs`, and a `lib.rs` that `pub mod`s them (you'll
  gate the cloud ones behind features in Chunk 2 so a default build stays light).
  - 🔗 Maps to: PRODUCT.md §9 — "Adding an engine = one connector; nothing else changes." This module
    layout is that promise made physical: one file per engine, each ending in `impl Source for …`.
  - ✅ Done when: `cargo build -p droplet-connectors` is green with three empty modules declared.

### Chunk 2 — Add the AWS SDK dependencies behind features

- [ ] Add the cloud client crates to `[workspace.dependencies]` in the **root** `Cargo.toml`, then opt
  `droplet-connectors` in. You already have `aws-config` + `aws-sdk-s3` + `blake3` from M5; M6 adds
  **`aws-sdk-athena`** (and, when you build the Iceberg connector against AWS Glue, possibly
  `aws-sdk-glue` — defer until Chunk 6):
  ```toml
  # verify EVERY version on crates.io the day you pin — aws-* ships ~weekly
  aws-config      = { version = "1", features = ["behavior-version-latest"] }  # verify exact, e.g. 1.8.x
  aws-sdk-s3      = "1"          # verify exact, e.g. 1.13x.x
  aws-sdk-athena  = "1"          # verify exact
  ```
  - 🆕 Concept: `[workspace.dependencies]` pins a crate's version **once** for the whole workspace; members
    opt in with `dep.workspace = true`. (Rust Book: *More About Cargo and Crates.io*, ch. 14.)
  - ⚠️ The `behavior-version-latest` feature on `aws-config` is **mandatory** — without it
    `BehaviorVersion::latest()` won't compile and `aws_config::defaults(...)` is unusable. (Same trap M5
    flagged; it bites again here.)
  - ⚠️ Never mix a `0.x` `aws-config` with a `1.x` SDK — the whole `aws-*` family shares `aws-smithy-*` 1.x
    internals only across the 1.x line. Keep every `aws-*` crate on the 1.x line.
  - verify: re-confirm the **exact** current versions of `aws-config`, `aws-sdk-s3`, and `aws-sdk-athena`
    on crates.io on the day you pin — these crates move almost weekly, so any number from memory will be
    stale. Lock them in `Cargo.lock` and commit it.
  - ✅ Done when: `cargo build -p droplet-connectors` is green with the AWS deps present.

- [ ] Gate the cloud connectors behind Cargo **features** so a default build (and the dev connector path)
  stays light:
  ```toml
  # crates/droplet-connectors/Cargo.toml
  [features]
  default = []
  athena  = ["dep:aws-sdk-athena", "dep:aws-config", "dep:aws-sdk-s3"]
  s3      = ["dep:aws-config", "dep:aws-sdk-s3"]
  iceberg = ["s3"]                      # iceberg reads Parquet via the same S3 path
  ```
  and mark the AWS deps `optional = true` in `[dependencies]`.
  - 🆕 Concept: a Cargo **feature** is a named on/off switch for optional code + deps; `dep:foo` enables an
    optional dependency. This is the same `#[cfg(feature = "duckdb")]` gating M1 used for DuckDB — here it
    keeps the AWS SDKs out of a build that only uses the dev connector. (Rust Book: *More About Cargo and
    Crates.io*, ch. 14; Cargo reference: "Features".)
  - 🔗 Maps to: the README's promise that "everything through M4 runs with zero external services" — the
    dev connector remains the default, and AWS only compiles when you turn a feature on.
  - ✅ Done when: `cargo build -p droplet-connectors` (no features) compiles without the AWS SDKs;
    `cargo build -p droplet-connectors --features athena` pulls them in.

### Chunk 3 — The local-Parquet dev connector stays the default

- [ ] Confirm (and, if needed, move) the **trivial local-Parquet dev connector** so it lives where the
  catalog defaults to it with **no AWS**. It was first written in M0 as the first `Source` impl; M6 keeps
  it as the **default** binding for any dataset that doesn't specify a cloud engine. Its `load` just points
  at a local `.parquet` path (and reports a trivial freshness token — e.g. the file mtime, or a fixed
  token under `Passthrough`).
  - 🆕 Concept: a **default impl as an escape hatch.** By keeping the dev connector as the catalog's default
    connector, the *entire* roadmap up to here keeps running with zero cloud setup; you only opt into AWS
    per-dataset. (No Rust Book chapter — design hygiene.)
  - ⚠️ **Invariant 1**: the dev connector satisfies the *exact same* `Source` trait as Athena. From the
    agent's side, `load("usage_daily")` is identical whether `usage_daily` is local-Parquet or Athena-backed
    — proving the abstraction holds *before* you add a single AWS call.
  - 🔗 Maps to: PRODUCT.md §20 success criterion only requires the *Athena* path for the final test; the dev
    connector is what lets every earlier test (and your day-to-day local dev) stay AWS-free.
  - ✅ Done when: with **no** AWS env/creds set and **no** features enabled, the existing `load` tests over
    the dev connector still pass — the new crate didn't break the AWS-free path.

### Chunk 4 — `S3Source`: direct Parquet read (no unload)

- [ ] Implement `impl Source for S3Source` for a dataset whose data is **already Parquet on S3**. The
  connector resolves the scope to a set of `s3://bucket/prefix/*.parquet` object keys and returns them as
  the `LoadOutput` (no conversion — the source *is* Parquet). DuckDB will read them via `httpfs` (the
  M1/M5 S3-read path: `INSTALL httpfs; LOAD httpfs;` + `read_parquet('s3://…')`).
  - 🆕 Concept: **direct read vs unload.** "Unload" only exists because a *query engine* (Athena) has to
    materialize its result somewhere first. When the data is already files-on-object-storage (S3, Iceberg),
    there is **nothing to materialize** — the connector just points DuckDB at the existing Parquet. This is
    why S3/Iceberg are the *easy* connectors and Athena is the hard one. (PRODUCT.md §6: "Iceberg / S3 →
    already parquet, read directly (no unload).")
  - 🆕 Concept (carried from M1): DuckDB reads `s3://…` through the **httpfs** extension, loaded at
    **runtime** with `INSTALL httpfs; LOAD httpfs;` — there is **no** Cargo feature for it. Credentials go
    in via `CREATE SECRET (TYPE s3, …)` built from the **session's scoped** S3 config, never hard-coded.
    For MinIO, the secret needs `ENDPOINT` + `URL_STYLE 'path'`. (DuckDB httpfs docs.)
  - ⚠️ **Invariant 2** (bounded load): apply the scope **before** the data lands — push the `where` filters
    and `columns` projection into the `read_parquet('s3://…')` SELECT (DuckDB does predicate/projection
    pushdown on Parquet), so you don't pull the whole object just to filter it locally. The slice is
    bounded at the boundary, not after.
  - ⚠️ **Invariant 9** (DuckDB sync → `spawn_blocking`): the S3 *listing* (AWS SDK) is **async** on the
    runtime; the DuckDB `read_parquet` is **sync** under `spawn_blocking`. Keep them in separate phases —
    never call the AWS SDK from inside the DuckDB blocking closure, and never block the runtime on DuckDB.
  - verify: whether you let **DuckDB httpfs** read the S3 objects directly, or **download** them through the
    M5 `ArtifactStore` first and read locally. Both are valid; the M5 cache already content-addresses
    downloads, so the cleaner path is usually "download once via ArtifactStore → DuckDB reads the local
    copy." Confirm which one M5 wired and match it (don't add a second download path).
  - ✅ Done when: a feature-gated test points `S3Source` at a Parquet object in **MinIO**, calls
    `source.load(&scope).await`, and the returned `LoadOutput` reads back the **scoped** rows (projected +
    filtered), not the whole object.

- [ ] Compute the **freshness token** for `S3Source` from S3 object metadata (the M5 `Versioned` policy):
  hash the objects' **ETags** (always present) and optionally `version_id` (only on versioned buckets).
  Read it with a cheap `head_object` (HEAD = metadata only, **no** data transfer).
  - ⚠️ **Invariant 7** (distributed by default): the freshness check is a **cheap version check, not a
    re-scan** — it reads object metadata in the shared plane, never the data. When an object changes, its
    ETag changes → the token changes → the M5 cache key changes → automatic invalidation.
  - verify: `head_object().e_tag()` / `.version_id()` return `Option<&str>`, and `version_id()` is `None`
    on non-versioned MinIO buckets — confirm against the `aws-sdk-s3` version you pin (this is the same
    `verify:` M5 raised; re-check it here for the connector path).
  - ✅ Done when: overwriting the source object in MinIO changes the `S3Source` freshness token, and the
    next `load` of the same scope **misses** the M5 cache and re-reads.

### Chunk 5 — `IcebergSource`: snapshot-resolved direct read (no unload)

- [ ] Implement `impl Source for IcebergSource`. Iceberg's data files are **also already Parquet**, but
  you don't read a raw prefix — you resolve the table's **snapshot** (current, or the `as_of` one) to its
  list of data-file paths, then read *those* Parquet files (the S3-read path from Chunk 4).
  - 🆕 Concept: **Apache Iceberg** is a table format over Parquet files on object storage. A table has
    immutable **snapshots**; each snapshot has a unique id and a manifest listing the exact data files at
    that point in time. Reading "the table" means: pick a snapshot → expand its manifests to a file list →
    read those Parquet files. (No Rust Book chapter — Iceberg-specific.)
  - 🆕 Concept: the Iceberg **snapshot id is a perfect freshness token *and* an `as_of` token.** PRODUCT.md
    §13: "Iceberg snapshots double as the `as_of` token for reproducible, cacheable time-travel." Reading
    `as_of=<snapshot_id>` is exact, immutable, and content-addressable — so it caches forever (the data at
    that snapshot can never change). `as_of="latest"` resolves to the current snapshot id, which *does*
    change as the table is written, naturally invalidating the cache.
  - ⚠️ **Invariant 2**: same bounded-load rule as S3 — push `columns`/`where` into the read so the slice is
    scoped at the boundary, not after the whole snapshot lands.
  - ⚠️ **Invariant 7**: resolving the snapshot is a **metadata** operation (read the catalog/metadata
    files), not a scan — cheap version check, not a re-scan.
  - verify: **how** you resolve the snapshot. There are two realistic v1 paths, both `verify:` against
    current crates:
    - **DuckDB's `iceberg` extension** — `INSTALL iceberg; LOAD iceberg;` then
      `iceberg_scan('s3://…/table', allow_moved_paths => true)` / `iceberg_metadata(...)`. Lets DuckDB do
      the snapshot resolution *and* the read in one place (and supports time-travel options). Confirm the
      exact function names + `as_of`/`snapshot_id` option spelling against the **DuckDB 1.5 iceberg
      extension** docs — this extension's API has shifted across DuckDB releases.
    - **The `iceberg-rust` crate** (`iceberg` / `iceberg-catalog-*`) — resolve the snapshot in Rust, get the
      file list, then read those Parquet files via the Chunk-4 S3 path. More moving parts; confirm crate
      name + version on crates.io and that it supports your catalog (Glue / REST / filesystem). Pick one
      path; don't wire both.
  - ✅ Done when: a feature-gated test reads a small Iceberg table (the simplest is a **local** filesystem
    Iceberg table written by Python/PyIceberg or DuckDB into MinIO), and `as_of="latest"` returns its rows;
    reading a pinned older snapshot id returns the *older* rows.

### Chunk 6 — `AthenaSource`, part 1: run the `UNLOAD`

This is the first connector that **performs a source-native bulk unload**. Two sub-steps: kick off the
`UNLOAD` (this chunk), then pick up the Parquet it wrote (Chunk 7).

- [ ] Build the Athena client from the M5 shared AWS config, then assemble the `UNLOAD` SQL **from the
  validated scope** (never from agent text):
  ```rust
  let athena = aws_sdk_athena::Client::new(&shared);  // shared = aws_config::defaults(...).load().await
  // assembled by the connector from scope.columns + scope.where + the catalog's table binding:
  let sql = format!(
      "UNLOAD (SELECT {cols} FROM {table} WHERE {pred}) \
       TO '{staging}' \
       WITH (format = 'PARQUET', compression = 'SNAPPY')",
      cols = scope.projection(), table = binding.qualified_table(),
      pred = scope.predicate_sql(), staging = self.staging_prefix(&scope_hash),
  );
  ```
  - 🆕 Concept: an Athena **`UNLOAD`** runs a `SELECT` and writes its result to S3 as files (here Parquet),
    in **parallel, with no global ordering**. It is the engine's *cheapest* bulk operation (PRODUCT.md §4)
    — one unload instead of a thousand agent queries. (Amazon Athena User Guide → UNLOAD.)
  - ⚠️ **Invariant 2** (no arbitrary SQL against production): the `SELECT` inside `UNLOAD` is built by the
    **connector** from the *already-typed, catalog-validated* `columns` + `where` (M2). The agent never
    supplies SQL; a wrong field was rejected at type-check before any of this ran. This is the single
    guarded door.
  - ⚠️ **The `TO` destination must be EMPTY.** `UNLOAD` does **not** overwrite — it errors (or orphans
    data) if the prefix already has files. Use a **scope-unique** staging prefix per unload (e.g. derived
    from the cache key / scope hash) so every unload writes to a fresh location.
  - verify: the **exact** Athena API call shape on the `aws-sdk-athena` version you pin —
    `start_query_execution()` with `.query_string(sql)`, a `.work_group(...)` **or** a
    `ResultConfiguration { OutputLocation }`, and the `QueryExecutionContext { Database }`. Builder method
    names and whether `OutputLocation` is required when the workgroup already sets one differ across SDK
    versions — confirm against the SDK docs, don't guess.
  - ✅ Done when (real-AWS, env-gated): a test submits the `UNLOAD` and gets back a `QueryExecutionId`
    string without error.

- [ ] **Poll** the query to completion. Athena is async on its own side — `StartQueryExecution` returns
  immediately; you call `GetQueryExecution(query_execution_id)` in a loop until
  `Status.State == "SUCCEEDED"` (or fail on `FAILED` / `CANCELLED`).
  - 🆕 Concept: a **poll loop** for an external async job — submit, then repeatedly ask "done yet?" with a
    backoff sleep between checks (`tokio::time::sleep`), until a terminal state. This is the standard shape
    for any "kick off a cloud job, wait for it" connector. (Rust Book: *Fundamentals of Asynchronous
    Programming*, ch. 17 — `.await` + timers.)
  - ⚠️ **Invariant 9**: the poll loop is **async** (it `.await`s `sleep` and the SDK call) — it runs on the
    Tokio runtime, **not** inside any `spawn_blocking` DuckDB closure. Athena I/O and DuckDB work never
    share a thread.
  - ⚠️ **Invariant 10** (one error type at the boundary): fold Athena's `SdkError` and a
    `FAILED`/`CANCELLED` terminal state into `DropletError` via `thiserror` (`#[from]` for the SDK error +
    a dedicated `LoadFailed`-style variant carrying Athena's `StateChangeReason`). Don't leak
    `aws_sdk_athena` error types past the connector.
  - verify: the terminal-state strings and where the failure reason lives —
    `get_query_execution().query_execution().status()` → `.state()` (an enum like
    `QueryExecutionState::Succeeded`) and `.state_change_reason()`. Confirm the enum/accessor spelling on
    your pinned `aws-sdk-athena`.
  - ✅ Done when (env-gated): the poll loop returns `Succeeded` for a tiny `UNLOAD` and surfaces a clean
    `DropletError` (not a raw SDK error) when you point it at a non-existent table.

### Chunk 7 — `AthenaSource`, part 2: pick up the Parquet via the manifest

The `UNLOAD` wrote *several* Parquet files to your staging prefix with **no predictable names**. Don't
guess at them or list the prefix blindly — Athena writes a **manifest** listing exactly the files this
query produced.

- [ ] Read the **manifest** from the finished query and use it as the authoritative file list:
  `GetQueryExecution(...).statistics().data_manifest_location()` is an `s3://…/…-manifest.csv` whose lines
  are the S3 paths of the Parquet files this `UNLOAD` wrote. Fetch it (S3 `get_object`), parse the lines,
  and that's your exact file set.
  - 🆕 Concept: an Athena **manifest** is a small text file Athena writes alongside an `UNLOAD`/CTAS,
    listing the result data files for *that* query. Using it is more reliable than listing the staging
    prefix, because a prefix can accumulate files from retries or concurrent unloads. (Amazon Athena User
    Guide → "Identifying query output files" / `DataManifestLocation`.)
  - ⚠️ **Invariant 2**: the manifest gives you *exactly* the bounded slice this scoped load produced —
    nothing more. Read those files, not the whole prefix.
  - verify: that `DataManifestLocation` is populated for **`UNLOAD`** specifically (it is documented for
    INSERT/CTAS/UNLOAD), and its exact accessor path on your `aws-sdk-athena`
    (`statistics().and_then(|s| s.data_manifest_location())` returns `Option<&str>`). If for some reason the
    manifest is empty for an `UNLOAD`, fall back to listing the **scope-unique** staging prefix you wrote
    to (safe *because* it's unique per unload).
  - ✅ Done when (env-gated): from a finished `UNLOAD`, you parse the manifest into a non-empty list of
    `s3://…*.parquet` paths.

- [ ] **Materialize through the M5 ArtifactStore.** Hand the manifest's Parquet files to the load path the
  same way the other connectors do, so the unloaded Parquet **becomes the content-addressed cache
  artifact** (PRODUCT.md §6: "The unloaded parquet **is** the content-addressed cache artifact"). Either
  copy the files into the ArtifactStore under their content-addressed key, or register them so M5's cache
  index maps `cache_key → artifact_key`.
  - ⚠️ **Invariant 7** (distributed by default): the unloaded Parquet is the **immutable, content-addressed**
    half (object store); the `cache_key → artifact_key` mapping is the **mutable coordination** half
    (CoordinationStore). M5 already built both — M6 just feeds the Athena output into them.
  - 🔗 Maps to: this is what makes "one unload, reused fleet-wide" real — the *next* `load` of the same
    scope, on **any** pod, computes the same M5 `cache_key`, finds the `artifact_key`, and **never touches
    Athena**.
  - ✅ Done when (env-gated): after one `UNLOAD`, the ArtifactStore holds the Parquet under a content key,
    and the M5 cache index has the `cache_key → artifact_key` entry.

- [ ] Compute the Athena **freshness token** (the third input to M5's cache key). Athena has no ETag of its
  own, so the realistic v1 token is one of:
  - **TTL** (PRODUCT.md §13: `floor(now / ttl)`) — the simplest default for a query engine; reuse the
    unloaded Parquet for the window, then re-unload.
  - **A watermark** from the underlying table (e.g. a `MAX(updated_at)` or a partition value) — a cheap
    one-row query that changes only when the data does. (Confirm whether your dataset has a natural
    watermark column in the catalog.)
  - ⚠️ **Invariant 7**: the token must be a **cheap** signal — a TTL bucket (no query) or a tiny watermark
    query — **not** a re-unload. Re-unloading to check freshness would defeat the entire point.
  - verify: which freshness policy each Athena-backed dataset uses is **catalog config** (PRODUCT.md §13:
    "per-dataset, configurable"). Confirm the M5 `FreshnessPolicy` enum already has the variant you need
    (`Ttl(Duration)` exists; a `Watermark` variant may need adding) and match its shape.
  - ✅ Done when: two `load`s of the same Athena scope within the TTL window produce **one** `UNLOAD` (the
    second is a cache hit); after the TTL rolls over (or the watermark changes), the next `load`
    **re-unloads**.

### Chunk 8 — Register connectors in the catalog (the binding the agent never sees)

- [ ] Extend the M2 **Catalog** so a logical dataset can bind to **any** of the four connectors
  (dev-local / s3 / iceberg / athena) via **configuration**, and `load` dispatches through the stored
  `Box<dyn Source>`. The dataset's *schema* (for `load` typing + field search) is unchanged — only the
  *binding* differs.
  - 🆕 Concept: this is the catalog's **"engine binding is configuration it never sees"** rule (PRODUCT.md
    §6) made concrete: the same `usage_daily` schema can be backed by Athena in prod and the dev connector
    in a test, and *nothing the agent calls changes*. (No Rust Book chapter — Droplet design.)
  - ⚠️ **Invariant 1** (the load-bearing one for this whole milestone): the catalog stores the connector
    choice; the agent-facing `load("usage_daily", …)` signature is **identical** across all four. The agent
    **cannot** ask which engine backs a dataset — there is no tool for it, and `describe_dataset` exposes
    schema only, never the connector.
  - ⚠️ **Invariant 2**: whichever connector is bound, `load` is still the *only* call that touches it, and
    still bounded/typed/cached. Swapping the binding doesn't open a second door.
  - ✅ Done when: a test registers the **same** logical dataset twice — once dev-local, once Athena-bound —
    and the **same** `load(...)` call works against both, returning a local `Dataset` either way. The agent
    code is byte-for-byte identical between the two.

### Chunk 9 — The milestone test: one Athena unload, cached, engine-agnostic

- [ ] Write the **milestone integration test** (env-gated for AWS) that proves the M6 "Done when",
  exercising the §20 success-criterion slice for connectors:
  1. Register a catalog with an **Athena-backed** dataset (and the dev-local one alongside, to prove
     Invariant 1).
  2. `load("usage_daily", columns=[…], where=[eq(…), between(…)], as_of="latest")` → **one** `UNLOAD` →
     Parquet materialized to the ArtifactStore under its content key, cache index updated.
  3. Run a small **local** analysis over the returned `Dataset` (group/derive/`to_rows`) — assert Athena is
     touched **zero** further times (Invariant 2 + 3).
  4. `load` the **same** scope again (simulate a second pod with a second store instance sharing the cache
     index + S3) → **cache hit**, **no** new `UNLOAD`.
  5. Run the *same* agent code against the **dev-local** binding of the same dataset → identical results,
     **no AWS** (Invariant 1).
  - ⚠️ **Invariant 8**: this whole milestone lives in `droplet-connectors` + `droplet-core` — **no `pyo3`**.
    The test drives connectors from pure Rust, no Python in the loop.
  - ⚠️ **Invariant 10**: confirm a deliberately bad table / staging bucket surfaces as a `DropletError`
    (`LoadFailed` carrying Athena's reason, or `NotFound`), never a raw `aws_sdk_*` `SdkError`.
  - 🔗 Maps to: PRODUCT.md §20 — "have an agent `load` a scoped slice (one Athena `UNLOAD`, cached
    fleet-wide) … touching Athena zero further times … a second run on another pod reusing the cached
    unload instead of re-hitting Athena." M6 delivers the connector half of that; the cross-pod *snapshot*
    half is M8.
  - ✅ Done when: with AWS configured and the env gate set, all five legs pass; with the gate unset, the
    Athena legs **skip cleanly** and the dev-local leg still passes. **This is M6's "Done when."**

---

## Notes carried forward (don't act yet)

- **Snowflake + BigQuery are deferred** (PRODUCT.md §16). They are *more `Source` impls* —
  Snowflake `COPY INTO 's3://…' … FILE_FORMAT=(TYPE=PARQUET)`, BigQuery
  `EXPORT DATA OPTIONS(format='PARQUET', uri='gs://…')` (PRODUCT.md §6) — each following the **exact**
  Athena shape from Chunks 6–7: run the engine's native unload, pick up the Parquet, materialize through
  M5. Build the `Source` seam cleanly now so adding them later is "one file, nothing else changes."
- **`export` is the mirror image of `load`** (PRODUCT.md §10, built in M10). M6 is the *inbound* governed
  boundary (source → local Parquet); `export` is the *outbound* governed boundary (local result → S3
  Parquet / Iceberg write-back). They share the ArtifactStore plumbing — keep the connector code generic
  enough that M10's export path can reuse the S3 write code.
- **The Athena workgroup/staging bucket and credentials are per-session, scoped config** (Invariant 9 from
  M1's S3 path): build the Athena client and the `OutputLocation` from the **session's** scoped AWS config,
  never fleet-wide keys baked into the connector. Confirm the catalog carries this per-dataset.
- **Verify when you pin.** Every `aws-*` crate moves ~weekly — re-confirm `aws-config` / `aws-sdk-s3` /
  `aws-sdk-athena` exact versions and that `BehaviorVersion::latest()` still resolves to a current default.
  Re-confirm the Athena builder shapes (`start_query_execution` / `get_query_execution`,
  `QueryExecutionState` enum, `data_manifest_location()`), the DuckDB **iceberg** extension function names,
  and (if used) the `iceberg-rust` crate name/version — these are the facts most likely to have drifted.

---

> 📌 When you reach this milestone, expand each chunk into tiny steps the way M0–M3 are written. Then move
> on to [`M7-coordination.md`](./M7-coordination.md) — swap the in-memory cache index + run registry for
> the real **Redis**, then **DynamoDB**, `CoordinationStore`, with leases for one-worker-per-run.
