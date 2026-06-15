# M7 — CoordinationStore: Redis then DynamoDB (SKETCH)

**Milestone goal:** implement the `CoordinationStore` trait against a real backend — **Redis first**, then a
second **DynamoDB** impl — so the run registry, the leases, and the cache index live in a strongly
consistent store that every pod shares.

**Done when (from the spec, build order step 6):** the `CoordinationStore` trait has a working Redis impl
*and* a working DynamoDB impl, each providing the run registry (`run_id` → snapshot pointer + status),
leases (one active worker per run, short TTL, reassignable on expiry), and the cache index
(`cache_key` → `artifact_key`) — and both pass the same trait-level tests the in-memory dev impl passes.

**Prerequisite:** finish [`M5-artifact-cache.md`](./M5-artifact-cache.md) (build-order step 5). You need the
`CoordinationStore` trait *shape*, the in-memory dev impl, the `cache_key` / `artifact_key` notions, and
`DropletError` already in place — M5 already writes the cache index *through* this trait, so M7 is "swap in
a real backend." (The trait itself and the four store-trait skeletons come from
[`M0-skeleton.md`](./M0-skeleton.md), build-order step 1.)

**Estimate:** ~10 chunks.

> This is a **SKETCH** file: chunk-level checkboxes with concept notes and invariant callouts, *not* the
> tiny per-line steps of M0/M1/M2. Get the shape right first; expand into tiny steps when you reach this
> milestone.

---

> 🧭 **What M7 is really about (read first, 5 min).** M0 gave you the four store *traits* and dev-only
> in-memory impls. M5 used the `ArtifactStore` for real (S3) and started writing the **cache index** through
> `CoordinationStore`. **M7 makes `CoordinationStore` real.** It is the one store that holds *mutable*,
> *coordinated* state — the registry, the leases, the cache index — so it must be **strongly consistent**
> (invariant #7). You build it **twice**, behind one trait, so a deployment can pick Redis *or* DynamoDB.
>
> 🆕 **Concept: the same trait, two impls.** A `trait` in Rust is a contract. You already have
> `CoordinationStore` (defined in M0) and an in-memory impl. M7 adds `RedisCoordinationStore` and
> `DynamoCoordinationStore` — both `impl CoordinationStore for …`. Code that holds a
> `Box<dyn CoordinationStore>` never knows or cares which one it got. (Rust Book: *Generic Types, Traits,
> and Lifetimes*, ch. 10; and *Object-Oriented Programming Features of Rust*, ch. 18 — trait objects.)
>
> 🆕 **Concept: a lease (distributed lock with a deadline).** A lease is "**I, worker X, am the one active
> worker for run R, until time T**." It must be *atomic* (only one winner even with many pods racing) and
> *self-expiring* (if X crashes, the lease auto-frees at T so another pod can resume the run). It is **not**
> affinity — any pod may win it next time. (No Rust Book chapter — this is a design idea, not a language
> feature.)
>
> 🆕 **Concept: the cache index is one of the three coordinated jobs.** It is *not* an afterthought bolted
> onto the registry — `PRODUCT.md` §11 names it as the third piece of `CoordinationStore` alongside the
> registry and the leases. M5 already computes the content-address `cache_key = hash(scoped query + source +
> freshness token)` and writes `cache_key → artifact_key` through this trait; the artifact bytes live in S3
> (`ArtifactStore`), but the *pointer* that lets the whole fleet agree "this exact scoped load is already in
> S3 at *that* key" must live in the **consistent** store, so a second pod's `load` sees the first pod's
> download instead of re-pulling the source. That fleet-wide reuse is the entire payoff of caching.
> (No Rust Book chapter — design idea.)
>
> 🆕 **Concept: `#[async_trait]` is still required here.** `CoordinationStore`'s methods are `async` *and*
> the store is used as `Box<dyn CoordinationStore>` (pluggable backend). Native `async fn` in traits is
> stable in modern Rust but is **not** dyn-compatible, so any trait you call behind a `dyn` pointer must be
> annotated with `#[async_trait]` (the `async-trait` crate, `0.1.89`). Both your `impl CoordinationStore for
> RedisCoordinationStore` and the Dynamo impl carry the same `#[async_trait]` attribute the trait does. M0
> already added `async-trait` to `[workspace.dependencies]`. (No Rust Book chapter — this is a crate-level
> mechanism; *Object-Oriented Programming Features of Rust*, ch. 18 covers the `dyn` side.)
>
> 🔗 **Maps to:** invariant #7 ("mutable coordination — registry, leases, cache index — is in the consistent
> store; resume is lease-guarded; no affinity"). The lease you build here is exactly what M8's cross-pod
> `Session.resume(run_id)` will hold before it touches a run; the cache index you build here is exactly what
> M5's `load` reads to skip a re-download fleet-wide.

---

### Chunk 1 — Re-read the `CoordinationStore` trait and pin the operations

- [ ] Open the `CoordinationStore` trait you defined in M0 and write down the exact method set M7 must
  satisfy, grouped by the three jobs from the spec:
  - **Run registry:** `put_run` / `get_run` (or set-status / read-snapshot-pointer) — `run_id` →
    `{ status, snapshot_pointer }`.
  - **Leases:** `acquire_lease`, `renew_lease`, `release_lease` — one active worker per run, short TTL,
    reassignable on expiry.
  - **Cache index:** `cache_index_get` / `cache_index_put` — `cache_key` → `artifact_key` (M5 already
    calls these).
  - ⚠️ Invariant #7: *"mutable coordination (run registry, leases, cache index) is in the **consistent
    store**."* These three jobs are the *whole* reason `CoordinationStore` exists — keep them together
    behind one trait. (Immutable parquet bytes live in S3 via `ArtifactStore`; the *index pointing at*
    those bytes is mutable coordination and belongs here.)
  - ⚠️ Invariant #10: the trait's methods return `Result<_, DropletError>`. Both backends will fold their
    native errors into `DropletError`; do that at the impl boundary, never leak `redis::RedisError` or an
    AWS `SdkError` past the trait.
  - ✅ Done when: you have a short written list of the trait methods and which of the three jobs each one
    serves — no code yet.
- [ ] Decide the lease ownership / TTL types now, in the trait, so both backends agree: a lease carries a
  `worker_id: String` (who holds it) and a TTL you express the **same way** to both impls. Pick a
  **`Duration`** (or a plain `ttl_ms: u64`) at the trait boundary; each impl converts at its edge — Redis
  to `PX` **milliseconds**, DynamoDB to an **epoch-seconds** TTL attribute.
  - 🆕 Concept: an **owner check (fencing).** Renew and release must verify *you still own* the lease
    before mutating it, so you never stomp a lease that already expired and was re-acquired by another pod.
    Carry `worker_id` for exactly this. (No Rust Book chapter — design rule, not syntax.)
  - ⚠️ Invariant #7: the lease is per-run — one run is one isolated `Session` resumed on one pod at a time.
    The `worker_id` + per-run key is what enforces "one active worker per run" across the fleet, with **no
    affinity** (any pod may hold it next).
  - ✅ Done when: the trait's lease methods take a `worker_id` and a TTL, and you've noted "release / renew
    are owner-checked."

### Chunk 2 — Stand up local Redis and prove a round-trip from `redis-cli`

- [ ] Run a local Redis with Docker so you have something to hit:
  `docker run --name droplet-redis -p 6379:6379 -d redis:8`. Confirm it answers.
  - 🆕 Concept: **local dev backend.** Just like M5 used MinIO to stand in for S3, you use a local Redis
    container to stand in for a managed Redis. No code change — only the connection URL differs. (No Rust
    Book chapter — local dev infra.)
  - ⚠️ verify: the image tag. `redis:8` was current as of mid-2026; if it 404s, use `redis:latest`. Keep
    `-p 6379:6379` (Docker Hub's bare example omits the port) so the host can reach
    `redis://127.0.0.1:6379/`. Protected mode is off and there's no password by default — fine for local
    dev only.
  - ✅ Done when: `docker exec -it droplet-redis redis-cli ping` prints `PONG`.

### Chunk 3 — Add the `redis` crate and a `ConnectionManager` helper

- [ ] Add the dependency to the crate that owns the Redis impl (`droplet-core`):
  `redis = { version = "1.2", features = ["tokio-comp", "connection-manager"] }`. (`tokio` is already a
  workspace dep from M1/M5.)
  - 🆕 Concept: **Cargo features are mandatory here.** With *no* features, `redis` is sync-only.
    `tokio-comp` enables async on the Tokio runtime; `connection-manager` enables `ConnectionManager`
    (auto-reconnect). The `aio` async-core module comes in transitively via `tokio-comp`. (Rust Book:
    *More About Cargo and Crates.io*, ch. 14 — feature flags.)
  - ⚠️ verify: `redis` recently jumped from the long `0.2x` line to **1.x** (latest `1.2.3`, edition 2024,
    MSRV Rust 1.88). Any tutorial pinning `redis = "0.25"` is stale — pin `redis = "1"` (or `"1.2"`).
    Edition 2024 / Rust 1.88 aligns with the workspace toolchain (M0 pins the exact toolchain).
  - ✅ Done when: `cargo build -p droplet-core` succeeds with `redis` added.
- [ ] Write a `connect()` helper: `redis::Client::open("redis://127.0.0.1:6379/")?` then
  `ConnectionManager::new(client).await?`. Store **one** `ConnectionManager` on your
  `RedisCoordinationStore` struct; `clone()` it per call.
  - 🆕 Concept: **no connection pool needed in async Redis.** A `MultiplexedConnection` pipelines many
    concurrent commands over one socket; `ConnectionManager` wraps that *and* adds auto-reconnect.
    `clone()` is cheap and shares the same underlying connection — that's the intended pattern. (Rust Book:
    *Smart Pointers*, ch. 15 / *Fearless Concurrency*, ch. 16 — cheap shared handles.)
  - ✅ Done when: a `#[tokio::test]` connects, does `let _: () = con.set("ping", "1").await?;` then
    `let v: Option<String> = con.get("ping").await?;`, and asserts `v == Some("1".into())`. (Remember
    `use redis::AsyncCommands;` or the methods won't exist — a classic beginner trap.) Under the generic
    `AsyncCommands` trait, `set` returns a generic `RV`, so a `set` whose result you ignore still needs an
    `RV` annotation such as `: ()` — otherwise the compiler says "type annotations needed." (Chunk 5 needs
    this same generic `AsyncCommands` so its `set_options(...) : bool` decode works.)

### Chunk 4 — Redis: run registry + cache index (the easy two)

- [ ] Implement the **cache index** with one Redis hash:
  `hset("droplet:cache_index", cache_key, artifact_key)` to write, `hget(...) -> Option<String>` to look
  up (`None` = cache miss). This backs M5's `cache_key → artifact_key` lookups, so any pod's `load` can
  discover that another pod already unloaded this exact scoped slice to S3.
  - 🆕 Concept: **HSET/HGET vs SET/GET.** `SET`/`GET` work on a whole key holding one string (good for a
    lease key). `HSET`/`HGET` store many `field → value` pairs under **one** key (a hash) — so the cache
    index is *one* Redis key, not thousands of top-level keys. (No Rust Book chapter — Redis data model.)
  - ⚠️ Invariant #7: the index entry is mutable coordination, so it lives here in the consistent store; the
    parquet bytes it points at are immutable and content-addressed in S3 (`ArtifactStore`, M5). Pointer here,
    bytes there — that split *is* the distributed-state-plane design.
  - ✅ Done when: a test `put`s a `cache_key → artifact_key`, then `get`s it back, and a missing key
    returns `None`.
- [ ] Implement the **run registry** as a per-run hash `droplet:run:{run_id}` with fields `status` and
  `snapshot` via `hset_multiple`; read back with `hget` / `hgetall`.
  - ⚠️ Invariant #7: the registry is the canonical "where is run R and what's its latest snapshot pointer"
    record — it must live in the consistent store, not on any single pod's disk. This is what lets *any*
    pod answer "resume run R."
  - ✅ Done when: a test sets `status = running`, `snapshot = snap:v1` for a `run_id`, reads both back, and
    an unknown `run_id` reads as empty.

### Chunk 5 — Redis: the lease via `SET … NX PX` (the load-bearing one)

- [ ] Implement `acquire_lease` as a single atomic command:
  `SET droplet:lease:{run_id} {worker_id} NX PX {ttl_ms}`. Build it with
  `SetOptions::default().conditional_set(ExistenceCheck::NX).with_expiration(SetExpiry::PX(ttl_ms))`, call
  `con.set_options(key, worker_id, opts)`, and **annotate the result `: bool`** — Redis decodes
  `OK → true` (we won) / `nil → false` (someone else holds it).
  - 🆕 Concept: **why NX + PX = a correct lease.** `NX` ("not exists") means only the *first* worker to ask
    wins the key → exactly one owner, no race even under a stampede of pods. `PX` adds a millisecond TTL so
    a crashed owner's lease auto-frees and another pod can take over. `NX` without expiry leaks the lock on
    crash; expiry without `NX` doesn't guarantee a single owner — you need **both**. (No Rust Book chapter
    — design idea.)
  - ⚠️ Invariant #7: *"leases (one active worker per run; short TTL; reassignable — **not affinity**)."*
    Reassignable-on-expiry is the whole point: there is **no** session affinity, so the next holder may be
    a different pod.
  - ⚠️ verify: the `bool` decoding on `redis` 1.2.x. `set_options` is generic over the return type; the
    `OK → true` / `nil → false` mapping is by deserialization — confirm against `docs.rs/redis/1.2.x`.
    `Option<()>` (`Some(())` = acquired, `None` = rejected) works too. Do **not** add `.get(true)` to the
    options: that changes the reply to the *old value* (`Option<String>`) and changes the semantics.
  - ⚠️ verify: `SetExpiry::PX` is **milliseconds**, `EX` is **seconds**. The spec says `PX`; using `EX`
    with a ms value sets a TTL 1000× too long — a silent lease-reassignment bug.
  - ✅ Done when: a test calls `acquire_lease(run, "worker-A", ttl)` then
    `acquire_lease(run, "worker-B", ttl)` for the *same* run → the second returns `false`. With a tiny
    `ttl_ms`, after the TTL passes a third acquire (any worker) returns `true` (auto-expiry reclaim).
- [ ] Implement `renew_lease` (extend the TTL while working) and `release_lease`. For **release**, use an
  **owner-checked compare-and-delete** (a small Lua `EVAL`: delete only if the stored value equals *our*
  `worker_id`) so you never delete a lease that already expired and was re-acquired by another pod.
  - 🆕 Concept: **compare-and-delete via `EVAL`.** Redis runs a tiny Lua script atomically; the script
    reads the key, compares it to your `worker_id`, and deletes only on a match. This is the standard
    safe-release pattern. (No Rust Book chapter — design idea.)
  - ⚠️ verify: the `redis` 1.2.x script / `EVAL` API (e.g.
    `redis::Script::new(src).key(k).arg(v).invoke_async`) against `docs.rs` before writing it — only
    `acquire` needs `SET NX PX`; release / renew need the script form (renew should also be owner-checked:
    only re-set the TTL if the value is still our `worker_id`). Note: `redis::Script` lives behind the
    default `script` feature — fine as long as you don't pass `default-features = false` (the Chunk 3
    dependency line doesn't); if you ever do, re-add `"script"` explicitly.
  - ✅ Done when: a test where worker-A holds the lease and worker-B calls `release_lease` (wrong owner)
    leaves the lease **intact**; worker-A's `release_lease` frees it.

### Chunk 6 — Redis: fold errors and slot the impl behind the trait

- [ ] Fold `redis::RedisError` into `DropletError` with a `thiserror` `#[from]` variant, then finish
  `#[async_trait] impl CoordinationStore for RedisCoordinationStore` so every `?` converts at the boundary.
  - 🆕 Concept: `thiserror`'s `#[from]` generates a `From<redis::RedisError>` impl, which is what lets `?`
    auto-convert at the boundary. (Rust Book: *Error Handling*, ch. 9.)
  - ⚠️ Invariant #10 (verbatim): *"One error type at the boundary: `thiserror` in libraries, `anyhow` at
    binaries; all engine errors fold into `DropletError`."*
  - ⚠️ Don't forget the `#[async_trait]` attribute on the `impl` block (see the concept note up top):
    without it, `Box<dyn CoordinationStore>` won't compile against an `async fn` trait.
  - ✅ Done when: the same trait-level test you wrote for the in-memory dev impl in M0/M5 (registry
    round-trip, cache-index round-trip, lease single-winner) passes against `RedisCoordinationStore`, and a
    deliberately broken command surfaces as a `DropletError`, not a raw `redis::RedisError`.
- [ ] Confirm the dev path still works *without* Redis: tests over the in-memory impl run with no
  container. Keep both impls behind `Box<dyn CoordinationStore>` so callers (M8's resume, M5's `load`)
  never branch on backend.
  - ⚠️ Invariant #7: production uses the consistent store; the in-memory impl is dev-only and is **not**
    safe across pods — say so in a doc comment so nobody ships it.
  - ✅ Done when: `cargo test` is green with the Redis container *down* (in-memory tests) and *up* (Redis
    tests).

### Chunk 7 — Stand up DynamoDB Local and create the tables

- [ ] Run **DynamoDB Local** with Docker on port 8000:
  `docker run --name droplet-ddb -p 8000:8000 -d amazon/dynamodb-local`. By hand (or via
  `aws --endpoint-url http://localhost:8000`), create the tables M7 needs — e.g. `droplet_leases`,
  `droplet_runs`, `droplet_cache_index`, each with a string partition key `pk`.
  - 🆕 Concept: **local dev backend, again.** DynamoDB Local is the AWS equivalent of MinIO — a container
    that speaks the real DynamoDB API so you don't need a cloud account to develop. You point the SDK at
    `http://localhost:8000`. (No Rust Book chapter — local dev infra.)
  - ⚠️ verify: DynamoDB Local still requires *some* credentials. Set `AWS_ACCESS_KEY_ID` /
    `AWS_SECRET_ACCESS_KEY` to anything non-empty (e.g. `fake` / `fake`) before the SDK loads, or the
    client errors at send time.
  - ✅ Done when: a `list-tables` against `http://localhost:8000` shows your tables.

### Chunk 8 — Add the AWS SDK and a shared config + DynamoDB client

- [ ] **verify:** these AWS crates publish ~weekly, so re-check before pinning — `aws-config = 1.8.18` /
  `aws-sdk-dynamodb = 1.116.0` were current 2026-06-15, not on the approved PINS list, and rest entirely on
  this note; don't copy the numbers as gospel. Add the deps:
  `aws-config = { version = "1.8.18", features = ["behavior-version-latest"] }`,
  `aws-sdk-dynamodb = "1.116.0"`, and ensure `tokio` has enough features (`["full"]` is simplest here).
  - 🆕 Concept: **`BehaviorVersion` is a required knob.** `aws_config::defaults(BehaviorVersion::latest())`
    pins the SDK's default behaviors so a future upgrade doesn't silently change them — and `::latest()`
    only exists when the `behavior-version-latest` feature is on. Forgetting that feature is the #1
    first-hour trap. (Rust Book: *More About Cargo and Crates.io*, ch. 14 — features.)
  - ⚠️ verify: `aws-config = 1.8.18` / `aws-sdk-dynamodb = 1.116.0` were current 2026-06-15. Pin exact and
    let `Cargo.lock` hold them; re-check before bumping. Do **not** mix a `0.x` `aws-config` with a `1.x`
    SDK — the whole `1.x` line shares `aws-smithy-*` internals. (M6 already added `aws-config` + S3/Athena
    SDKs for the connectors; reuse the same `aws-config` pin here for the DynamoDB SDK.)
  - ✅ Done when: `cargo build -p droplet-core` succeeds with the AWS deps added.
- [ ] Build the shared config once and a DynamoDB-Local client from it:
  `let shared = aws_config::defaults(BehaviorVersion::latest()).region("us-east-1").load().await;` then a
  ddb-specific config via `aws_sdk_dynamodb::config::Builder::from(&shared).endpoint_url("http://localhost:8000").build()`
  → `Client::from_conf(...)`. (Real AWS: just `aws_sdk_dynamodb::Client::new(&shared)`.)
  - 🆕 Concept: **every AWS call ends in `.send().await`.** The SDK is fully async over Tokio. Keep this
    code *off* the DuckDB thread — DuckDB stays sync behind `spawn_blocking` (invariant #9); coordination
    is async. (No Rust Book chapter — SDK design.)
  - ⚠️ verify: `endpoint_url` for DynamoDB Local goes on the **service** config builder (or env
    `AWS_ENDPOINT_URL_DYNAMODB`), not on `aws-config`'s loader — so S3 (MinIO, 9000) and DynamoDB (8000)
    can point at different local ports.
  - ✅ Done when: a `put_item` / `get_item` round-trip on `droplet_runs` (the registry table) succeeds
    against `http://localhost:8000`.

### Chunk 9 — DynamoDB: registry, cache index, and the lease via a conditional `put_item`

- [ ] Implement the **registry** and **cache index** as plain `put_item` / `get_item`. An item is a
  `HashMap<String, AttributeValue>`; use `AttributeValue::S(String)` for strings. Registry item:
  `pk = "run#{run_id}"`, attrs `status` + `snapshot`. Cache-index item: `pk = "cache#{cache_key}"`, attr
  `artifact_key`.
  - 🆕 Concept: **`AttributeValue` is an enum, and numbers are strings on the wire.** `::S(String)` for
    text, `::N(String)` for numbers (yes, the *number* is a `String`), `::B` binary, `::Bool`, `::M` map,
    `::L` list. (Rust Book: *Enums and Pattern Matching*, ch. 6.)
  - ⚠️ Invariant #7: the cache-index item behaves identically to the Redis hash — same `cache_key →
    artifact_key` semantics, same consistent-store guarantee, so M5's `load` reuse is backend-agnostic.
  - ✅ Done when: registry and cache-index round-trips pass against DynamoDB Local, matching the Redis
    impl's behavior.
- [ ] Implement `acquire_lease` as a conditional write: `put_item` with item `pk = "lease#{run_id}"`,
  `owner = worker_id`, a numeric `ttl` attribute
  (`AttributeValue::N((now_secs + lease_secs).to_string())`), and
  `.condition_expression("attribute_not_exists(pk)")`. On `Ok` you hold it; on the conditional-check
  failure another pod holds it.
  - 🆕 Concept: **DynamoDB's "insert if not exists."** DynamoDB has no native NX; you emulate it with a
    normal `put_item` plus `condition_expression("attribute_not_exists(pk)")`. If the row exists the whole
    call fails with `ConditionalCheckFailedException` — that failure **is** your "someone else holds the
    lease" signal. (No Rust Book chapter — design idea.)
  - 🆕 Concept: **detecting the conditional failure.** A failed `.send()` gives `SdkError<PutItemError>`;
    call `err.as_service_error()` then `.is_conditional_check_failed_exception()` to recognize contention
    vs a real transport error. Match the right op's error type (`PutItemError` for put, `UpdateItemError`
    for update). (No Rust Book chapter — SDK error model.)
  - ⚠️ Invariant #7: same lease semantics as Redis — one active worker, reassignable on expiry, **not**
    affinity. The two backends must behave identically behind the trait.
  - ⚠️ verify: DynamoDB **TTL is a background sweeper**, not precise expiry — deletes lag up to ~48h, and
    DynamoDB Local may not sweep at all. So the `ttl` attribute is only garbage-collection. **Lease
    correctness comes from `condition_expression` + an explicit `now > ttl ⇒ stale` check in your
    condition + the owner (`worker_id`) check** — never from TTL deletion timing. Build the
    "expired ⇒ reacquirable" logic into the conditional write (e.g. the condition succeeds when the row is
    absent *or* its stored `ttl` is in the past), not the sweeper.
  - ✅ Done when: a test acquires for worker-A, a second acquire (worker-B, same run, lease not expired) is
    rejected via `is_conditional_check_failed_exception()`, and once the stored `ttl` is in the past a new
    acquire succeeds.
- [ ] Implement `renew_lease` / `release_lease` with `update_item` / `delete_item`, guarded by a condition
  that `owner = worker_id` (the owner / fencing check). Use the `UpdateItemError` / `DeleteItemError`
  service-error helpers the same way as the put.
  - ⚠️ Invariant #7: the owner-guarded release is what keeps one run's lease from being torn down by a
    worker that no longer owns the run — per-run isolation across the fleet, with no affinity.
  - ✅ Done when: a wrong-owner `release_lease` is rejected; the true owner's release frees the lease.

### Chunk 10 — DynamoDB: fold errors, finish the trait, run both backends through the same tests

- [ ] Fold the AWS `SdkError` / typed service errors into `DropletError` with `thiserror` at the impl
  boundary, then finish `#[async_trait] impl CoordinationStore for DynamoCoordinationStore`. Cover at
  least: not-found (`ResourceNotFoundException`), conflict (`ConditionalCheckFailedException`), and a
  catch-all transport error.
  - ⚠️ Invariant #10: same rule as the Redis impl — no raw `SdkError` leaks past the trait; everything is
    `DropletError`.
  - ⚠️ Keep the **conflict** case mapped to a *distinct* `DropletError` variant the caller can recognize as
    "lease taken," apart from a generic transport failure — M8's resume needs to tell "another pod owns
    this run" from "Dynamo is unreachable."
  - ✅ Done when: a deliberately bad call surfaces as a `DropletError`, and the conflict case maps to a
    distinct `DropletError` variant the caller can recognize as "lease taken."
- [ ] Run the **one shared test suite** (registry round-trip, cache-index round-trip, and lease
  single-winner / reassign-on-expiry tests) against **all three** impls — in-memory, Redis, DynamoDB —
  parametrized over `Box<dyn CoordinationStore>`. They must all pass.
  - 🆕 Concept: **trait-level conformance tests.** Write the test against the *trait*, then run it once per
    backend. This is how you guarantee Redis and DynamoDB are truly interchangeable — and it's the safety
    net for swapping backends later. (Rust Book: *Writing Automated Tests*, ch. 11.)
  - ⚠️ Invariant #7: this conformance is what the spec's "Redis **or** DynamoDB" promise rests on — a
    deployment picks one, and the rest of Droplet (M8 resume, M5 cache reuse) is none the wiser.
  - ✅ Done when: the same lease / registry / cache-index suite is green against in-memory, Redis
    (container up), and DynamoDB Local (container up).

---

## Notes carried forward (don't act yet)

- **The lease is for M8's resume.** Nothing in M7 *uses* the lease yet — `Session.resume(run_id)` in
  [`M8-snapshot-resume.md`](./M8-snapshot-resume.md) is the first caller. There, a pod must
  `acquire_lease(run_id)` **before** rebuilding DuckDB from the manifest, `renew_lease` while running, and
  `release_lease` at the end. M7 just has to make the lease *correct and identical across backends*.
  (Invariant #7: "resume is lease-guarded.")
- **The cache index is for M5's `load`.** M5 already writes `cache_key → artifact_key` through this trait;
  once a real backend exists, a second pod's `load` reads the index, finds the hit, and re-attaches the
  cached parquet from S3 instead of re-unloading the source. Keep the `cache_key` / `artifact_key` field
  names stable so M5's reads match M7's writes. (Invariant #7 + PRODUCT.md §11.)
- **The registry holds the snapshot pointer M8 writes.** The `snapshot` field you set in the registry here
  is where M8's `SnapshotStore` content-addressed blob key lands, so any pod can find a run's latest
  snapshot. Keep the field name stable.
- **TTL is GC, not a lock timeout — in *both* backends.** Redis `PX` auto-frees a key precisely; DynamoDB
  TTL is a lazy sweeper (~48h, none locally). Lease correctness must come from atomic acquire
  (`SET NX PX` / conditional `put_item`) plus an explicit owner + `now > ttl` check — never from deletion
  timing. Re-state this when you implement, so the two backends stay equivalent.
- **Async-trait, not native `async fn`.** Both impls are reached through `Box<dyn CoordinationStore>`, so
  the trait and both `impl` blocks need `#[async_trait]`. If you ever switch the stores to generics-only
  (static dispatch everywhere), you could drop it — but the pluggable-backend design relies on the `dyn`
  pointer, so keep `#[async_trait]`.
- **Verify the fast-moving bits when you pin.** `redis` is freshly 1.x (old `0.2x` snippets mislead); the
  AWS crates publish weekly (re-check `aws-config` / `aws-sdk-dynamodb` versions and the
  `behavior-version-latest` feature); confirm the `set_options → bool` decoding and the `EVAL` script API
  on `redis` 1.2.x, and the `is_conditional_check_failed_exception()` path on the *right* DynamoDB op error
  before relying on them.

---

> 📌 When you reach this milestone, expand each chunk into tiny steps the way M0/M1/M2 are written.
