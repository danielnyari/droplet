# M1 — DuckDB engine

**Milestone goal:** make `droplet-core` run a SQL query through an embedded **DuckDB** engine — first
against a local Parquet file, then straight from **S3 over httpfs** — returning a **capped** Apache
Arrow result. DuckDB is synchronous, so every query runs inside `spawn_blocking`; errors fold into
`DropletError`.

**Done when (from the spec, build-order step 2):** *an agent-style call crunches an S3 Parquet and gets
a small Arrow result back.*

**Prerequisite:** finish [`M0-skeleton.md`](./M0-skeleton.md). You need the virtual workspace,
`droplet-core` building, `DropletError` (thiserror) with a couple of `#[from]` variants, the handle
registry, the `Session` type, the four store traits (`Source`, `ArtifactStore`, `SnapshotStore`,
`CoordinationStore`) with in-memory/local dev impls, and a Tokio runtime available to `droplet-core`.
M1 *adds an engine onto* that skeleton.

**Estimate:** ~7 chunks (A–G), each a focused sitting. Do them in order — later chunks build on the
connection and `run_sql` you write earlier.

The spec lives at `PRODUCT.md` (repo root). Reference it that way, never `docs/PRODUCT.md`.

---

## How to read this file

- Every `- [ ]` is a tiny task (~10–30 min for a Rust newbie). Check it off only when it's truly done.
- `🆕 Concept:` explains a new Rust/DuckDB idea the **first** time it shows up, with a Rust Book chapter
  name (run `rustup doc --book` to open the book offline).
- `✅ Done when:` is an observable check — usually a command's output or a passing test. Don't move on
  until you see it.
- `⚠️ Invariant:` quotes a load-bearing rule from `PRODUCT.md` (repo root) in plain words, by its number
  1–10. Never break these.
- `🔗 Maps to:` ties a tiny exercise to the real Droplet concept it unlocks.
- `verify:` flags a fact the research couldn't fully pin on the locked version — check it against the
  real crate source/docs (`docs.rs/duckdb`, the DuckDB SQL docs) before relying on it, don't guess.
- Code snippets are **anchors** (a few lines to orient you). You write the real implementation.

**The build/learn loop for M1:** add one capability → write a tiny `#[test]` that exercises it → watch
it fail → make it pass → move on. DuckDB with the `bundled` feature compiles its C++ engine from source,
so the **first** `cargo build` after adding it takes several minutes and a lot of CPU/RAM. That is
normal, not a hang — go make coffee.

> **One trap to internalize before you start:** the `duckdb` crate pins a **specific `arrow` major**
> (the research found `^58`). The latest `arrow` on crates.io is newer (`59`). If you add `arrow`
> yourself at the wrong major you get **two** incompatible Arrow crates and baffling
> `expected RecordBatch, found RecordBatch` errors. The fix is simple: **don't add `arrow` yourself —
> use the re-export `duckdb::arrow`.** Details in Chunk B.

---

### Chunk A — Add `duckdb` behind a Cargo feature

The point of M1 is to plug in DuckDB without making `cargo build -p droplet-core` slow *by default*.
DuckDB's `bundled` feature compiles a big C++ engine, so we make the whole engine **opt-in**: it only
compiles when someone enables a `duckdb` feature you add to `droplet-core`. A Cargo feature is just a
named on/off switch.

#### Pick your build path first

DuckDB needs the actual database engine compiled or supplied somehow. There are two paths; **you will
pick `bundled` to start.**

- **`bundled`** — Cargo compiles DuckDB's C++ from source (via the `cc` crate). **Zero external setup**,
  but the **first build is slow** (minutes, heavy CPU/RAM). This is the beginner-safe choice and what M1
  uses.
- **Prebuilt / `frozen-duckdb`** — ships a precompiled DuckDB binary so you skip the slow compile
  (PRODUCT.md §10 voices a *preference* for prebuilt). But the `frozen-duckdb` crate is **experimental
  and stale** (latest `0.1.0`, and it targets DuckDB `1.4.0` — *older* than the `duckdb` crate's engine),
  so adopting it blindly on a version-sensitive project is risky. **Defer it.**

- [ ] Decide: start with **`bundled`**. Write a one-line note to yourself (a `// BUILD PATH` comment near
  the dep, or a line in your dev notes) that says: *"Using `bundled` for zero-setup; if first-build time
  hurts, revisit a prebuilt lib via `DUCKDB_DOWNLOAD_LIB=1` against the **matching** upstream version
  before reaching for the stale `frozen-duckdb`."*
  - 🆕 Concept: a Cargo **feature** is a named on/off switch for optional code and dependencies; a
    crate's *own* features (like DuckDB's `bundled`) are sub-switches you turn on when you depend on it.
    (Rust Book: *More About Cargo and Crates.io*, ch. 14; full detail in the Cargo reference's "Features"
    section.)
  - 🔗 Maps to: PRODUCT.md §10 tech stack — *"prefer prebuilt lib / `frozen-duckdb` over `bundled`."*
    You're starting with `bundled` deliberately and leaving a paved path to switch later, not ignoring
    the preference.
  - verify: before you ever reach for it, re-check crates.io for a **newer `frozen-duckdb`** whose DuckDB
    version actually matches the `duckdb` crate you pin below — the `0.1.0`/DuckDB-`1.4.0` mismatch is the
    whole reason to defer it.

#### Pin the version

- [ ] In the **root** `Cargo.toml`, under `[workspace.dependencies]`, pin DuckDB to a full minor so a
  future engine bump can't sneak in:
  ```toml
  duckdb = { version = "1.10503", features = ["bundled", "parquet", "json"] }
  ```
  - 🆕 Concept: `[workspace.dependencies]` pins a crate once for the whole workspace; members opt in later
    with `dep.workspace = true`. No version drift across crates. (Rust Book: *More About Cargo and
    Crates.io*, ch. 14.)
  - ✅ Done when: the line is present in `[workspace.dependencies]` with exactly
    `["bundled", "parquet", "json"]` and nothing more.

- [ ] Understand the version number you just pinned (read-only — nothing to type).
  - 🆕 Concept (version scheme): the `duckdb` crate version is **not** plain semver. `1.10503` decodes as
    DuckDB upstream **v1.5.3** (`1.<major><minor 2-digits><patch 2-digits>`). A bump from `1.10502` →
    `1.10503` changes the **actual engine** (1.5.2 → 1.5.3). Pinning the full `1.10503` keeps the engine
    stable; treat engine upgrades as deliberate. (This is PRODUCT.md §10's `1.MAJOR_MINOR_PATCH.x`
    phrasing.)
  - Why these three sub-features: **`bundled`** compiles the engine (zero setup). **`parquet`** and
    **`json`** enable Parquet/JSON reading — and each one *pulls in `bundled`* anyway. You do **not** need
    `vtab-arrow`/`appender-arrow`: those are only for *writing* Arrow data *into* DuckDB; reading results
    *out* as Arrow needs no feature.
  - ⚠️ Do NOT add a top-level `arrow` dependency here. (Chunk B explains why; for now just don't.) And do
    NOT enable kitchen-sink meta-features like `modern-full` / `extensions-full` — keep the build as small
    as M1 needs.

- [ ] In `crates/droplet-core/Cargo.toml`, add `duckdb` as an **optional** dependency:
  ```toml
  [dependencies]
  duckdb = { workspace = true, optional = true }
  ```
  - 🆕 Concept: `optional = true` means the crate is NOT compiled unless something turns it on. (Cargo
    reference: "Features".)
  - ✅ Done when: `cargo build -p droplet-core` still succeeds and still does **not** compile DuckDB
    (nothing enables the optional dep yet — so the build stays fast).

- [ ] Add a `[features]` table to `crates/droplet-core/Cargo.toml` wiring a `duckdb` *feature* to the
  optional `duckdb` *dependency*:
  ```toml
  [features]
  duckdb = ["dep:duckdb"]
  ```
  - 🆕 Concept: `["dep:duckdb"]` says "enabling my `duckdb` feature enables the optional `duckdb`
    dependency." Feature and dependency can share a name; the `dep:` prefix disambiguates them. (Cargo
    reference: "Features".)
  - ✅ Done when: `cargo build -p droplet-core` (no flags) finishes fast and does NOT compile DuckDB, but
    `cargo build -p droplet-core --features duckdb` downloads and compiles it (**slow the first time —
    minutes is normal**).

- [ ] Create a new module file `crates/droplet-core/src/engine_duckdb.rs` (empty for now), and gate it in
  `lib.rs`:
  ```rust
  #[cfg(feature = "duckdb")]
  pub mod engine_duckdb;
  ```
  - 🆕 Concept: `#[cfg(feature = "...")]` is **conditional compilation** — the item below it is included
    only when that feature is active. This is how Rust keeps optional code out of the default build.
    (Rust Book: *More About Cargo and Crates.io*, ch. 14; full detail in the Rust reference's "Conditional
    compilation".)
  - ✅ Done when: `cargo build -p droplet-core` compiles without the module;
    `cargo build -p droplet-core --features duckdb` compiles *with* it.

- [ ] In `engine_duckdb.rs` write the smallest possible smoke test to prove the engine links and runs a
  trivial query:
  ```rust
  #[cfg(test)]
  mod tests {
      use duckdb::Connection;

      #[test]
      fn duckdb_links_and_answers() -> duckdb::Result<()> {
          let conn = Connection::open_in_memory()?;
          let answer: i64 = conn.query_row("SELECT 42", [], |r| r.get(0))?;
          assert_eq!(answer, 42);
          Ok(())
      }
  }
  ```
  - 🆕 Concept: DuckDB is an **in-process OLAP SQL engine** — like SQLite but column-oriented for
    analytics. It runs *inside* your Rust process: no server, no port, no network. `Connection::open_in_memory()`
    gives you an ephemeral database that dies with the process. (No Book chapter — this is
    DuckDB-specific.)
  - 🆕 Concept: `conn.query_row(sql, params, closure)` runs a query expected to return one row and hands
    each column to your closure; `r.get::<_, i64>(0)` reads column 0 as an `i64`. The `[]` is "no bind
    parameters." (No Book chapter — duckdb-rs API.)
  - ✅ Done when: `cargo test -p droplet-core --features duckdb` is green and the test passes. This proves
    the dependency + feature wiring works before you write any real logic.

---

### Chunk B — Pin a compatible Arrow (the gotcha)

DuckDB hands query results back as **Apache Arrow** `RecordBatch`es, and Arrow is itself a separate
crate that DuckDB depends on at a **specific major**. Getting the major wrong is the single most
confusing failure in this milestone, so you'll confirm it deliberately with a command and use DuckDB's
own re-export.

- [ ] **Do not** add `arrow` to your own `Cargo.toml`. Instead, plan to use DuckDB's re-export:
  `duckdb::arrow`. DuckDB does `pub use arrow;` internally, so `duckdb::arrow::...` is *the exact same
  Arrow* DuckDB returns.
  - 🆕 Concept: to Cargo, **`arrow` major 58 and major 59 are different, incompatible crates.** If DuckDB
    wants 58 and you write `arrow = "59"`, Cargo compiles *both*, and an `arrow::RecordBatch` from DuckDB
    is a *different type* than your `arrow::RecordBatch` — producing the infamous
    `expected RecordBatch, found RecordBatch` error. (Rust Book: *More About Cargo and Crates.io*, ch. 14
    — SemVer; this is the practical sharp edge.)
  - 🆕 Concept: Apache **Arrow** is a columnar in-memory format. A `RecordBatch` is a chunk of rows across
    many columns at once (a `Schema` + one array per column, all the same length). This is the zero-copy
    **internal interchange** Droplet uses between DuckDB and the rest of the host. (No Book chapter —
    Arrow-specific.)
  - ⚠️ Invariant #10 (one error type) and Droplet's Arrow interchange (PRODUCT.md §4) both depend on a
    *single* Arrow in the tree. Two Arrows breaks both.

- [ ] Confirm exactly one Arrow major is in your dependency tree:
  ```bash
  cargo tree -i arrow --features duckdb
  ```
  - 🆕 Concept: `cargo tree -i <crate>` ("invert") shows what pulls in a crate and at which version. One
    entry = one Arrow = good. (No Book chapter — Cargo tooling.)
  - ✅ Done when: the output shows a **single** `arrow vX.y` line (the research expects a `58.x`), pulled
    in via `duckdb`. If you ever see two different majors, you added `arrow` yourself somewhere — remove
    it.
  - verify: re-check DuckDB's Arrow requirement (`arrow ^N`) **every time you bump `duckdb`** — the pinned
    major changes over DuckDB releases. If the major changes and you later add any other Arrow-touching
    crate (parquet/arrow-flight), they must *all* match. Confirm against
    `crates.io/crates/duckdb/<version>/dependencies` or just re-run `cargo tree -i arrow`.

- [ ] In `engine_duckdb.rs`, prove you can name an Arrow type **through DuckDB's re-export** (no top-level
  `arrow` dep):
  ```rust
  use duckdb::arrow::record_batch::RecordBatch;
  ```
  - Note: there is **no** `arrow` Cargo feature to enable here — the `arrow` crate is a *normal
    (non-optional) dependency* of `duckdb`, so `duckdb::arrow` is always available once `duckdb` itself
    is. There's nothing extra to switch on for *reading* Arrow results.
  - ✅ Done when: `cargo build -p droplet-core --features duckdb` is green with that `use` line present
    (even if `RecordBatch` is unused for a moment — a warning is fine).

---

### Chunk C — Open a DuckDB connection owned by the Session

Each Droplet `Session` is one run's analysis context. It owns its **own ephemeral in-memory DuckDB
connection** — created at session start, thrown away at session close. Start in-memory (`":memory:"`);
you'll never serialize the engine itself (that's invariant #3, enforced later in the snapshot
milestone). The connection lives behind a handle on the host; the sandbox never sees it.

- [ ] In `engine_duckdb.rs`, add a small wrapper that owns a `Connection`. Keep it a plain struct for now:
  ```rust
  use duckdb::Connection;

  pub struct DuckEngine {
      conn: Connection,
  }

  impl DuckEngine {
      /// One ephemeral in-memory DuckDB per Session.
      pub fn new_in_memory() -> duckdb::Result<Self> {
          let conn = Connection::open_in_memory()?;
          Ok(Self { conn })
      }
  }
  ```
  - 🆕 Concept: a **`Connection`** is your handle to a DuckDB database. `open_in_memory()` (== path
    `":memory:"`) is ephemeral and dies with the process; `Connection::open("file.db")` persists to disk.
    For Droplet, each Session gets its own *ephemeral* in-memory connection. (No Book chapter — duckdb-rs
    API.)
  - 🆕 Concept: putting `conn` inside a `struct` and exposing methods on it (`impl DuckEngine`) is Rust's
    way of bundling state with behavior. The field is private (no `pub`), so only `DuckEngine`'s methods
    can touch the raw `Connection`. (Rust Book: *Using Structs to Structure Related Data*, ch. 5; *Method
    Syntax*.)
  - ⚠️ Invariant #4 (boundary discipline): engine objects live behind a **handle** on the host; the
    sandbox only ever sees an opaque handle, never a `Connection`. `DuckEngine` is a host-side type — keep
    it out of any sandbox-facing API.

- [ ] Store the `DuckEngine` (or its handle) on the `Session` you built in M0. If your `Session` holds
  engines in the handle registry, register the `DuckEngine` there and keep only its handle id on
  `Session`. If `Session` owns engines directly for now, add a field like `duck: DuckEngine`.
  - 🆕 Concept: a `Connection` is **`!Sync`** — it must not be shared across threads. Owning exactly one
    per `Session` (not sharing one across sessions/threads) matches both DuckDB's threading rule and
    Droplet's per-session isolation. (Rust Book: *Fearless Concurrency*, ch. 16 — `Send`/`Sync`.)
  - ⚠️ Invariant #9 (per-run isolation): one run = one `Session` = one ephemeral DuckDB. Don't pool or
    share connections between sessions.
  - ✅ Done when: a `#[test]` can construct a `Session` and reach a live `DuckEngine` whose
    `Connection::open_in_memory()` succeeded.

---

### Chunk D — `run_sql` v1: local Parquet → capped Arrow

Now the first real query path. `run_sql` takes a SQL string, runs it against a **local** Parquet file
(no S3 yet), and returns Arrow `RecordBatch`es — **capped** so a huge result can't flood the sandbox.
You'll write the test first.

#### Get a fixture

- [ ] Create `crates/droplet-core/tests/data/` and generate a tiny `sample.parquet` with a known answer.
  Easiest with Python:
  ```python
  import duckdb
  duckdb.sql("""
      COPY (SELECT * FROM (VALUES
          ('a', 50), ('a', 150), ('b', 200), ('b', 90), ('c', 300)
      ) t(category, amount))
      TO 'crates/droplet-core/tests/data/sample.parquet' (FORMAT parquet)
  """)
  ```
  - 🆕 Concept: files under a crate's `tests/` directory are for **integration tests** — separate test
    binaries that use your crate as an outside user would. A non-`.rs` subfolder like `tests/data/` is
    just storage; Cargo won't try to compile it. (Rust Book: *Writing Automated Tests*, ch. 11 —
    "Integration Tests".)
  - Pick values so a known query has a known answer. With the data above:
    `SELECT category, SUM(amount) FROM ... GROUP BY category` gives `a → 200`, `b → 290`, `c → 300`.
    Write that expected answer down — your test asserts it.
  - ✅ Done when: the file exists at `crates/droplet-core/tests/data/sample.parquet` and is committed to
    git (`git status` shows it tracked).

#### Add the row cap

- [ ] Add a row-cap constant in `droplet-core` (next to `DuckEngine`, or in a small `consts` module):
  ```rust
  /// Max rows any tool may move into the sandbox in one result.
  pub const MAX_RESULT_ROWS: usize = 1000;
  ```
  - ⚠️ Invariant #4 (boundary discipline): *only result-returning tools move capped rows into the sandbox;
    heavy data stays in DuckDB behind handles.* This cap is load-bearing — it keeps snapshots small later.
    (PRODUCT.md §7 BOUNDARY DISCIPLINE.)
  - 🔗 Maps to: every `run_sql` result the agent ever sees is bounded by this. The cap is *the* thing that
    keeps the REPL holding "capped results, not data."

#### Write `run_sql` (sync core first)

- [ ] Write a **synchronous** `run_sql_blocking` method on `DuckEngine` that prepares the statement and
  collects Arrow out (you add the cap in the next box):
  ```rust
  use duckdb::arrow::record_batch::RecordBatch;

  impl DuckEngine {
      /// SYNC: run a SELECT and return Arrow batches.
      /// Caller is responsible for wrapping this in spawn_blocking (Chunk E).
      fn run_sql_blocking(&self, sql: &str) -> duckdb::Result<Vec<RecordBatch>> {
          let mut stmt = self.conn.prepare(sql)?;
          let batches: Vec<RecordBatch> = stmt.query_arrow([])?.collect();
          Ok(batches) // capping comes next
      }
  }
  ```
  - 🆕 Concept: a **prepared statement** (`conn.prepare(sql)?`) compiles the SQL once and can be queried
    for results. `.query_arrow([])?` runs it and yields an iterator of `RecordBatch`; `.collect()` gathers
    them into a `Vec`. The `[]` means no bind parameters. (No Book chapter — duckdb-rs API; the real chain
    is `conn.prepare(sql)?.query_arrow([])?.collect()`, no semicolons mid-chain.)
  - In the SQL the agent runs, read a local file with `read_parquet('path/sample.parquet')` — e.g.
    `SELECT category, SUM(amount) AS total FROM read_parquet('.../sample.parquet') GROUP BY category
    ORDER BY category LIMIT 1000`.
  - verify: `query_arrow([])` takes an empty params slice; if you ever bind parameters use `params![...]`.
    Confirm the empty-slice form compiles on `1.10503` (it does in the docs example) before adding params.

- [ ] Write the `cap_batches` helper that trims to at most `max_rows` total, slicing the boundary batch
  (slicing is a cheap zero-copy view):
  ```rust
  fn cap_batches(batches: Vec<RecordBatch>, max_rows: usize) -> Vec<RecordBatch> {
      let mut out = Vec::new();
      let mut remaining = max_rows;
      for b in batches {
          if remaining == 0 { break; }
          let take = remaining.min(b.num_rows());
          out.push(b.slice(0, take)); // zero-copy view of first `take` rows
          remaining -= take;
      }
      out
  }
  ```
  - 🆕 Concept: `RecordBatch::slice(offset, len)` is a **zero-copy** view (shares the underlying buffers).
    It **panics** if `offset + len > num_rows`, which is why we clamp with `.min(b.num_rows())`. (No Book
    chapter — arrow-rs API.)

- [ ] Apply the cap inside `run_sql_blocking` and also enforce a SQL `LIMIT` in the query, so an agent
  that forgets `LIMIT` still can't flood the sandbox:
  ```rust
  Ok(cap_batches(batches, crate::MAX_RESULT_ROWS))
  ```
  - Why both `LIMIT` *and* a code-side cap: `LIMIT` is the right boundary (DuckDB stops early instead of
    materializing everything), but the code-side `cap_batches` is a belt-and-suspenders guard. DuckDB has
    no built-in "max rows returned" knob, so the `LIMIT` in the SQL is what makes DuckDB stop early.
  - ⚠️ Invariant #4 again: this cap is the seam that keeps rows crossing into a result small even when the
    source is huge.
  - ✅ Done when: `run_sql_blocking` returns at most `MAX_RESULT_ROWS` rows across all batches (a unit test
    over a >1000-row query proves the total is clamped).

- [ ] Write an integration test (`crates/droplet-core/tests/duckdb_local.rs`, feature-gated) that runs the
  aggregation over the local fixture and asserts the **small capped** result:
  ```rust
  #![cfg(feature = "duckdb")]
  // build the SQL pointing at tests/data/sample.parquet, run it through DuckEngine,
  // assert total row count == 3 (a/b/c) and the summed values match a→200, b→290, c→300.
  ```
  - ✅ Done when: `cargo test -p droplet-core --features duckdb` passes and the assertion on the small
    aggregate is green. **This is the local half of the M1 "Done when."**

#### Fold DuckDB errors into `DropletError`

- [ ] Add a `#[from]` variant for DuckDB to `DropletError`, gated on the feature so the default build
  doesn't reference `duckdb`:
  ```rust
  #[cfg(feature = "duckdb")]
  #[error("duckdb error: {0}")]
  Duckdb(#[from] duckdb::Error),
  ```
  - 🆕 Concept: `#[from]` on a `thiserror` variant auto-generates a `From<duckdb::Error>` impl, so a `?`
    on a duckdb call inside a function returning `Result<_, DropletError>` converts the error for you.
    (Rust Book: *Error Handling*, ch. 9 — the `?` operator; `thiserror` is the crate that wires `#[from]`.)
  - ⚠️ Invariant #10 (one error type): *all engine errors fold into `DropletError`; thiserror in
    libraries.* Both the DuckDB error here and the `JoinError` in Chunk E must fold in — no raw engine
    errors leak past the boundary.
  - ✅ Done when: a `droplet-core` function that returns `Result<_, DropletError>` can call a duckdb
    operation with `?` and it compiles; the variant is `#[cfg(feature = "duckdb")]`-gated so the
    no-feature build still compiles.

---

### Chunk E — `spawn_blocking`: keep the async runtime free

DuckDB **blocks the OS thread** while a query runs. Droplet is an async (Tokio) program — Surreal and the
stores are async — so a blocking DuckDB call on a runtime thread would freeze everything. The rule: run
every DuckDB query inside `tokio::task::spawn_blocking`.

- [ ] Make sure `tokio` is available to `droplet-core` with the runtime + macros features (you likely
  added this in M0). If not, in `[workspace.dependencies]`:
  ```toml
  tokio = { version = "1", features = ["rt-multi-thread", "macros", "sync"] }
  ```
  and opt in from `crates/droplet-core/Cargo.toml` with `tokio.workspace = true`.
  - ✅ Done when: `cargo build -p droplet-core --features duckdb` is green with `tokio` in scope.

- [ ] Add an **async** `run_sql` that wraps the blocking core in `spawn_blocking`. The connection must be
  *owned inside* the closure work, so model this the way Droplet will: the `DuckEngine` (and its
  `Connection`) lives on the blocking side. A minimal shape:
  ```rust
  use duckdb::arrow::record_batch::RecordBatch;

  // Note: the closure must be `move`, `'static`, and `Send`. A `Connection`
  // is NOT `Sync`, so create/own it inside the task (never share across threads).
  pub async fn run_sql(sql: String) -> Result<Vec<RecordBatch>, crate::DropletError> {
      let batches = tokio::task::spawn_blocking(move || -> Result<_, crate::DropletError> {
          let engine = DuckEngine::new_in_memory()?;     // per-call/per-session, owned here
          let out = engine.run_sql_blocking(&sql)?;
          Ok(out)
      })
      .await??; // outer ? = JoinError (did the task panic?); inner ? = DropletError
      Ok(batches)
  }
  ```
  - 🆕 Concept: **synchronous vs async.** DuckDB blocks its thread. `tokio::task::spawn_blocking` moves
    that work onto a dedicated blocking-thread pool so the async runtime's worker threads stay free.
    (Rust Book: *Fundamentals of Asynchronous Programming*, ch. 17; the rule is "don't do blocking work on
    the async threads.")
  - 🆕 Concept: the closure is `move` so it **takes ownership** of `sql`; it must be `Send + 'static` so
    Tokio can run it on another thread. Pass owned data in (a `String`, not a `&str`) and return owned data
    out. (Rust Book: *Understanding Ownership*, ch. 4; *Fearless Concurrency*, ch. 16 — `move` closures.)
  - ⚠️ Invariant #6 (verbatim): *"DuckDB is synchronous → spawn_blocking + release the GIL during query
    execution."* This task satisfies the `spawn_blocking` half (the GIL half is droplet-py's, below).
  - ✅ Done when: an async `#[tokio::test]` calls `run_sql(...).await?` over the local fixture and gets the
    same capped result Chunk D's sync test produced.

- [ ] Understand the **two `?`** after `.await` (read-only — the shape above already has them).
  - 🆕 Concept: `spawn_blocking(...).await` returns `Result<Result<T, DropletError>, JoinError>`. The
    **first** `?` unwraps Tokio's `JoinError` (did the blocking task panic?); the **second** unwraps your
    inner `Result` (did the query fail?). That's why the call ends in `.await??`. (Rust Book: *Error
    Handling*, ch. 9.)

- [ ] Fold `JoinError` into `DropletError` so the double-`?` works cleanly:
  ```rust
  #[error("blocking task failed: {0}")]
  Join(#[from] tokio::task::JoinError),
  ```
  - ⚠️ Invariant #10 (one error type): the `JoinError` folds into `DropletError` just like the DuckDB error
    — no raw engine/runtime errors leak past the boundary.
  - ✅ Done when: the async `run_sql` compiles with `.await??` and no manual error mapping.

- [ ] Leave a forward-reference comment near `run_sql` for the Python layer (do **not** implement it
  here):
  ```rust
  // GIL (droplet-py): when this is called via PyO3, the thin wrapper in
  // droplet-py must release the GIL around the call (py.detach(...)) so other
  // Python threads run while DuckDB works. droplet-core itself has NO pyo3
  // (invariant #1) — the GIL release lives only in droplet-py.
  ```
  - ⚠️ Invariant #1 (verbatim): *"droplet-core (Rust) must not depend on pyo3; PyO3 lives only in
    droplet-py."* The GIL release is the *other half* of invariant #6, but it belongs in `droplet-py`, not
    here. This comment is the seam, not an implementation.
  - verify: the exact GIL-release call. The research found the method was **renamed** from
    `Python::allow_threads` to `Python::detach` in pyo3 0.26 (no deprecated alias). Confirm `py.detach(...)`
    against the pyo3 version droplet-py pins when you actually build that layer — but write nothing pyo3 in
    `droplet-core`.
  - ✅ Done when: the note is in code near `run_sql` and you can state in one sentence why the GIL release
    is *not* in `droplet-core`.

---

### Chunk F — S3 reads via httpfs

Now teach DuckDB to read `s3://...` directly. S3 support is the **httpfs** extension, loaded at
**runtime** with SQL — there is *no* Cargo feature for it. You install/load it, hand DuckDB credentials
via a `CREATE SECRET`, then `read_parquet('s3://...')`. You'll also load ICU so date/time SQL works.

- [ ] In `DuckEngine::new_in_memory` (or a `setup` method it calls), install + load **httpfs** once per
  connection:
  ```rust
  self.conn.execute_batch("INSTALL httpfs; LOAD httpfs;")?;
  ```
  - 🆕 Concept: a DuckDB **extension** (`httpfs`, `icu`, `parquet`) is loaded at **runtime** via SQL
    `INSTALL x; LOAD x;`, **not** added in `Cargo.toml`. `httpfs` is what lets DuckDB read `s3://` and
    `https://` URLs. (No Book chapter — DuckDB-specific.)
  - 🆕 Concept: `execute_batch` runs one or more SQL statements for their side effects (no result rows) —
    the right call for DDL and extension setup. (duckdb-rs API.)
  - ⚠️ Gotcha: forgetting `LOAD httpfs` makes `read_parquet('s3://...')` fail with an unhelpful error.
    Also, DuckDB needs external access enabled — it is **on by default** unless you explicitly built a
    `Config` with `enable_external_access(false)`, so don't disable it.
  - ✅ Done when: a test connection runs the httpfs `execute_batch` call without error.

- [ ] In the same setup, install + load **ICU** once per connection:
  ```rust
  self.conn.execute_batch("INSTALL icu; LOAD icu;")?;
  ```
  - Why ICU: ICU is **not** bundled even with the `bundled` feature (the crate strips it to stay under the
    crates.io package-size limit). Without `LOAD icu`, timezone/collation date-time ops can fail. Load it
    at startup so date/time SQL works. (PRODUCT.md §10: *"load ICU at runtime."*)
  - ✅ Done when: a test connection runs the icu `execute_batch` call without error.

- [ ] Provide S3 credentials/region with a **`CREATE SECRET`** (the modern DuckDB way), set from values
  the Session supplies — not hard-coded:
  ```rust
  // values come from the per-session S3 config (env, IAM role, or explicit creds)
  self.conn.execute_batch(
      "CREATE SECRET s3_creds (
           TYPE s3,
           KEY_ID    '...',
           SECRET    '...',
           REGION    'us-east-1'
       );",
  )?;
  ```
  - 🆕 Concept: a DuckDB **secret** stores credentials for a service (here S3) so subsequent
    `read_parquet('s3://...')` calls authenticate automatically. It's the documented modern path,
    preferred over the legacy `SET s3_region=...; SET s3_access_key_id=...; SET s3_secret_access_key=...`
    (both work on 1.5.x). For IAM roles, `CREATE SECRET` supports a `PROVIDER credential_chain` form
    instead of literal keys. (No Book chapter — DuckDB httpfs docs.)
  - ⚠️ Invariant #9 (per-run isolation): *"S3 credentials scoped per session."* Build the secret from the
    Session's scoped credentials; never bake fleet-wide keys into the engine. Different sessions get
    different secrets.
  - verify: confirm the exact `CREATE SECRET` parameter names (`KEY_ID` / `SECRET` / `REGION`, and the
    `PROVIDER credential_chain` spelling) against the **DuckDB 1.5 httpfs docs** before shipping — secret
    syntax has shifted across DuckDB versions.

- [ ] Read straight from S3 with a capped query (same `run_sql` path, just an `s3://` URL):
  ```sql
  SELECT category, SUM(amount) AS total
  FROM read_parquet('s3://bucket/path/*.parquet')
  GROUP BY category
  ORDER BY category
  LIMIT 1000
  ```
  - 🆕 Concept: `read_parquet('s3://...')` is a table function — DuckDB streams the Parquet from S3 over
    httpfs and queries it as if it were a local table. With `LIMIT`, DuckDB stops early instead of scanning
    everything. (DuckDB-specific.)
  - ⚠️ Invariant #4 again: the `LIMIT` + your `cap_batches` keep the rows crossing into a result small even
    when the source object is huge.

- [ ] Stand up a target to read from: either a **public sample S3 bucket** (read-only, no creds) or a
  **local MinIO** (S3-compatible, runs in Docker). For MinIO, set the endpoint in the secret with the
  appropriate `ENDPOINT` / `URL_STYLE 'path'` options.
  - 🆕 Concept: **MinIO** is an S3-compatible object store you can run locally in Docker for dev — same
    `s3://` API, no AWS account. The roadmap's later milestones (M2 onward) run their S3 against MinIO too.
    (No Book chapter — infra tooling; see the README's "local dev backends" setup.)
  - verify: the exact `CREATE SECRET` options for a MinIO endpoint (`ENDPOINT`, `URL_STYLE 'path'`,
    `USE_SSL false`) against the DuckDB 1.5 httpfs docs — endpoint/path-style options are the usual MinIO
    friction point.
  - ✅ Done when: from a Rust call, DuckDB reads a Parquet object from your S3/MinIO target and returns
    rows (even just a `SELECT count(*) ... LIMIT 1`).

---

### Chunk G — Tests: local then S3/MinIO, and the M1 "Done"

Two integration tests lock the milestone: one over local Parquet (fast, always runs in CI), one over
S3/MinIO (gated so CI without a backend doesn't fail).

- [ ] **Local test** (you wrote the core of this in Chunk D — keep it): aggregation over
  `tests/data/sample.parquet`, asserting the **small capped** result (`a→200, b→290, c→300`, 3 rows).
  - ✅ Done when: `cargo test -p droplet-core --features duckdb` passes the local aggregation test.

- [ ] **S3/MinIO test**: the *same* aggregation but the source is `read_parquet('s3://...')` against your
  MinIO bucket (or a public sample bucket). Gate it so it only runs when a backend is configured — e.g.
  `#[ignore]` by default, or skip early when a `DROPLET_TEST_S3` env var is unset:
  ```rust
  #[tokio::test]
  async fn s3_aggregation_capped() {
      if std::env::var("DROPLET_TEST_S3").is_err() {
          eprintln!("skipping: set DROPLET_TEST_S3 to run the S3/MinIO test");
          return;
      }
      // run_sql over read_parquet('s3://...') LIMIT 1000; assert the small capped result
  }
  ```
  - 🆕 Concept: an environment-variable guard (or `#[ignore]`) lets a test exist in the repo but only run
    when its external dependency is present — so plain `cargo test` stays green on a machine with no MinIO.
    (Rust Book: *Writing Automated Tests*, ch. 11 — "Ignoring Some Tests Unless Specifically Requested".)
  - ✅ Done when: with MinIO up and `DROPLET_TEST_S3` set, the S3 test passes and returns the same capped
    aggregate as the local test; with the var unset, it skips cleanly.

- [ ] Confirm the full async path end to end: an async call (`run_sql(sql).await?`) → `spawn_blocking` →
  DuckDB reads S3 over httpfs → capped Arrow `Vec<RecordBatch>` back. This *is* the spec's step-2 "done":
  **an agent-style call crunches an S3 Parquet and gets a small Arrow result back.**
  - ⚠️ Invariant #6: the query ran inside `spawn_blocking` (async runtime never stalled). Invariant #4: the
    result was capped before crossing the boundary. Invariant #10: any failure surfaced as `DropletError`.
  - ✅ Done when: a single `#[tokio::test]` demonstrates that chain and asserts a small result. **This is
    the M1 "Done when."**

---

## M1 done checklist

Tick all of these to call M1 complete (the spec's step-2 "Done when" expanded):

- [ ] `duckdb = "1.10503"` with features `["bundled", "parquet", "json"]` (and only those) is pinned in
  `[workspace.dependencies]` and gated behind an **opt-in** `duckdb` feature in `droplet-core`. Default
  build stays fast; a `// BUILD PATH` note records the prebuilt/`frozen-duckdb` escape hatch.
- [ ] Exactly **one** Arrow major in the tree (`cargo tree -i arrow` shows a single `58.x`), and the code
  names Arrow types via `duckdb::arrow` — no top-level `arrow` dependency.
- [ ] Each `Session` owns its own ephemeral in-memory `Connection` behind a host-side `DuckEngine`
  (invariants #4, #9); the sandbox never sees a `Connection`.
- [ ] `run_sql` returns capped Arrow `RecordBatch`es: SQL `LIMIT` + a `cap_batches` slice guard against
  `MAX_RESULT_ROWS` (invariant #4).
- [ ] `duckdb::Error` and `tokio::task::JoinError` both fold into `DropletError` via `#[from]` (the duckdb
  variant feature-gated) — invariant #10.
- [ ] Every query runs inside `tokio::task::spawn_blocking` and is awaited with `.await??`; the async
  runtime is never stalled (invariant #6). A comment forward-references the GIL release that will live in
  `droplet-py` (invariant #1).
- [ ] `httpfs` and `icu` are installed/loaded at runtime per connection; S3 reads use `CREATE SECRET`
  built from per-session credentials (invariant #9) and `read_parquet('s3://...')`.
- [ ] A local-Parquet integration test asserts a small capped aggregate; the same test over S3/MinIO
  passes when a backend is configured and skips cleanly otherwise.

**Spec "Done when": an agent-style call crunches an S3 Parquet and gets a small Arrow result back.** ✅

When green, move on to [`M2-artifact-cache.md`](./M2-artifact-cache.md) — materialize those capped
results into the content-addressed S3 cache so a scan happens once and is reused across the fleet.
