# M1 — Local analyze engine

**Milestone goal:** plug **DuckDB** into `droplet-core` as the **local analyze engine**. You open one
**ephemeral, in-memory** DuckDB per `Session`, register a **local Parquet file** as a `Dataset`
**handle** that stays host-side, and implement the first **analyze primitives** over those handles —
`filter_rows`, `group_agg`, `to_rows` (capped), `scalar`, and `local_sql(sql, datasets=…)`. DuckDB is
synchronous, so every query runs inside `spawn_blocking`; errors fold into `DropletError`.

> **What changed since the old plan (read this once).** An earlier version of this milestone had DuckDB
> reach out over **httpfs** and read `s3://…` straight from the source, via a generic `run_sql`. **That
> is gone.** In the new design DuckDB never touches a source — it is the **local engine** that crunches a
> slice you've *already* pulled down. So this file gives you a `Dataset` handle over a **local** Parquet
> file plus a small, **unrestricted** analyze surface; the part that talks to S3/Athena moves to **M6**
> (the connector boundary). If you remember a `run_sql(s3://…)` step, forget it.

**Done when (from the spec, build-order step 3):** *a local analyze engine runs the dataframe primitives
and `local_sql` over a local Parquet `Dataset`, capped, inside `spawn_blocking`.*

**Prerequisite:** finish [`M0-skeleton.md`](./M0-skeleton.md). You need the virtual workspace,
`droplet-core` building, `DropletError` (thiserror) with a couple of `#[from]` variants, the generic
**handle `Registry<T>`** (`insert`/`get`/`remove`/`require`), the per-run **`Session`** (unique work dir
+ `Registry`), the four store traits (`Source`, `ArtifactStore`, `SnapshotStore`, `CoordinationStore`)
with in-memory/local dev impls, and a Tokio runtime available to `droplet-core`. M1 *adds the analyze
engine onto* that skeleton — it slots the real engine type into the `Registry` placeholder M0 left for it.

**Estimate:** ~7 chunks (A–G), each a focused sitting. Do them in order — later chunks build on the
connection, the `Dataset` handle, and the capped-result path you write earlier.

The spec lives at `PRODUCT.md` (repo root). Reference it that way, never `docs/PRODUCT.md`.

---

## How to read this file

- Every `- [ ]` is a tiny task (~10–30 min for a Rust newbie). Check it off only when it's truly done.
- `🆕 Concept:` explains a new Rust/DuckDB idea the **first** time it shows up, with a Rust Book chapter
  name (run `rustup doc --book` to open the book offline).
- `✅ Done when:` is an observable check — usually a command's output or a passing test. Don't move on
  until you see it.
- `⚠️ Invariant:` quotes a load-bearing rule from `PRODUCT.md` §15 (repo root) in plain words, by its
  number 1–10 (the same list as the README's "Golden rules"). Never break these.
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
  (PRODUCT.md §18 voices a *preference* for prebuilt). But the `frozen-duckdb` crate is **experimental
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
  - 🔗 Maps to: PRODUCT.md §18 tech stack — *"prefer prebuilt lib / `frozen-duckdb` over `bundled`."*
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
    stable; treat engine upgrades as deliberate. (This is PRODUCT.md §18's tech-stack pin.)
  - Why these three sub-features: **`bundled`** compiles the engine (zero setup). **`parquet`** and
    **`json`** enable Parquet/JSON reading — and each one *pulls in `bundled`* anyway. You read local
    Parquet via the `parquet` feature; you do **not** need `vtab-arrow`/`appender-arrow` (those are only
    for *writing* Arrow data *into* DuckDB; reading results *out* as Arrow needs no feature).
  - ⚠️ Do NOT add a top-level `arrow` dependency here. (Chunk B explains why; for now just don't.) And do
    NOT enable kitchen-sink meta-features like `modern-full` / `extensions-full` — keep the build as small
    as M1 needs. In particular, **no `httpfs`/S3 setup in this milestone** — that lives in M6.

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
  - ⚠️ Invariant #10 (one error type) and Droplet's Arrow interchange (PRODUCT.md §18) both depend on a
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
you'll never serialize the engine itself (that's invariant #5, enforced later in the snapshot
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
  - ⚠️ Invariant #6 (boundary discipline): engine objects live behind a **handle** on the host; the
    sandbox only ever sees an opaque handle, never a `Connection`. `DuckEngine` is a host-side type — keep
    it out of any sandbox-facing API.

- [ ] Store the `DuckEngine` on the `Session` you built in M0. M0 left the per-session registry typed as a
  placeholder (`handles: Registry<()>`); the `DuckEngine` is the engine type that placeholder was waiting
  for. The simplest M1 shape is a dedicated field — add `duck: DuckEngine` to `Session` and build it in
  `Session::new` (you'll move engine objects into the `Registry` proper when the handle types multiply
  later). Construct it right after the work dir is created. (M0's forward-looking comment penciled this slot
  in as `duck: duckdb::Connection` — a placeholder; M1 refines it to a `DuckEngine` **wrapper** so it can
  also hold the monotonic `next_id` counter, so M1 is authoritative here. You can update that M0 comment to
  `duck: DuckEngine` when M1 lands.)
  - 🆕 Concept: a `Connection` is **`!Sync`** — it must not be *shared* across threads (it can still be
    *moved* to one thread, which is what `spawn_blocking` does in Chunk E). Owning exactly one per
    `Session` (not sharing one across sessions/threads) matches both DuckDB's threading rule and Droplet's
    per-session isolation. (Rust Book: *Fearless Concurrency*, ch. 16 — `Send`/`Sync`.)
  - ⚠️ Invariant #3 (analyze runs only on the local, throwaway copy): the analyze engine works solely on
    the local DuckDB and physically cannot reach back to a source. A consequence is per-session isolation —
    one run = one `Session` = one ephemeral local DuckDB; don't pool or share connections between sessions.
  - ✅ Done when: a `#[test]` (feature-gated) can construct a `Session` and reach a live `DuckEngine` whose
    `Connection::open_in_memory()` succeeded.

---

### Chunk D — Register a local Parquet file as a `Dataset` handle

This is the heart of the new design. The agent never holds *data*; it holds a **`Dataset` handle** — an
opaque token that names a table living **inside the host's DuckDB**. In M1 you create that handle from a
**local** Parquet file (in M2 the same handle will come from `load(...)`; the analyze surface doesn't
care where the Parquet came from). The trick: register the Parquet as a **DuckDB view** under a generated
table name, and hand back a small `Dataset` value the sandbox can pass around.

> **Why a view, not a copy.** `CREATE VIEW t AS SELECT * FROM read_parquet('…')` doesn't load the file
> into memory — it just teaches DuckDB "the name `t` means *that* Parquet." Every later primitive
> (`filter_rows`, `group_agg`, `local_sql`) then refers to the dataset *by name*, and DuckDB reads only
> what each query needs. The big data stays on disk / inside DuckDB; only the **handle** crosses to the
> sandbox.

- [ ] Define a small `Dataset` handle type in `engine_duckdb.rs` (or a `dataset.rs` module). It carries
  just enough to find the data again — the DuckDB table/view name it's registered under:
  ```rust
  /// An opaque handle to a table living inside the host's DuckDB.
  /// The sandbox holds these; it never holds rows. (Invariant #6.)
  #[derive(Clone, Debug)]
  pub struct Dataset {
      /// The DuckDB view/table name this handle resolves to, e.g. "ds_0".
      table: String,
  }

  impl Dataset {
      pub fn table(&self) -> &str { &self.table }
  }
  ```
  - 🆕 Concept: a **handle** is a tiny value that *stands in for* a big resource held elsewhere. Here
    `Dataset` is just a table name — cheap to clone, cheap to pass, and (crucially) cheap to put in a
    snapshot later. The actual columns never travel with it. (No Book chapter — the pattern; you built the
    generic version in M0's `Registry`.)
  - ⚠️ Invariant #6 (boundary discipline, verbatim core): *"only `to_rows`/`scalar`/load-samples move rows
    into the sandbox, capped; everything else is handles."* `Dataset` is the "everything else is handles"
    half — it makes the boundary discipline concrete.
  - 🔗 Maps to: PRODUCT.md §7 — the dataframe primitives all take and return `Dataset` handles; "data
    stays host-side."

- [ ] Generate **unique** table names so two registrations never collide. Add a per-engine counter to
  `DuckEngine` (a `u64` field, bumped on each register), mirroring the monotonic counter idea from M0's
  `Registry`:
  ```rust
  pub struct DuckEngine {
      conn: Connection,
      next_id: u64, // monotonic; names the next dataset "ds_{n}"
  }
  ```
  - 🆕 Concept: the same **monotonic counter** trick as M0's handle registry — only-ever-increases, so
    `ds_0`, `ds_1`, … are unique within a session and never reused. (Rust Book: nothing new; reuse the
    M0 idea.)
  - Remember to initialise `next_id: 0` in `new_in_memory`.
  - ✅ Done when: `cargo build -p droplet-core --features duckdb` is green with the new field.

- [ ] Write a **failing** test for `register_parquet` first (test-first), pointing at a fixture you'll
  create next:
  ```rust
  #[test]
  fn register_parquet_returns_a_handle() -> Result<(), crate::DropletError> {
      let mut eng = DuckEngine::new_in_memory()?;
      let ds = eng.register_parquet("crates/droplet-core/tests/data/sample.parquet")?;
      assert_eq!(ds.table(), "ds_0");
      Ok(())
  }
  ```
  - ✅ Done when: it fails to compile (no `register_parquet` yet) — the red you want before writing it.

- [ ] Create `crates/droplet-core/tests/data/` and generate a tiny `sample.parquet` with a known answer.
  Easiest with Python (using the same DuckDB engine):
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
  - Pick values so known queries have known answers. With the data above:
    `SELECT category, SUM(amount) FROM ... GROUP BY category` gives `a → 200`, `b → 290`, `c → 300`, and
    `SELECT SUM(amount) FROM ...` gives `790`. Write those down — your tests assert them.
  - ✅ Done when: the file exists at `crates/droplet-core/tests/data/sample.parquet` and is committed to
    git (`git status` shows it tracked).

- [ ] Implement `register_parquet`: it `CREATE VIEW`s the Parquet under the next `ds_{n}` name and returns
  a `Dataset` handle:
  ```rust
  impl DuckEngine {
      /// Register a LOCAL Parquet file as a Dataset handle (a DuckDB view).
      /// No data is copied — the view just names the file.
      pub fn register_parquet(&mut self, path: &str) -> Result<Dataset, crate::DropletError> {
          let table = format!("ds_{}", self.next_id);
          self.next_id += 1;
          // read_parquet is a DuckDB table function; the view is lazy.
          let sql = format!(
              "CREATE VIEW {table} AS SELECT * FROM read_parquet('{path}')"
          );
          self.conn.execute_batch(&sql)?; // ? folds duckdb::Error into DropletError (Chunk D below)
          Ok(Dataset { table })
      }
  }
  ```
  - 🆕 Concept: `read_parquet('path')` is a DuckDB **table function** — it reads a Parquet file *as if* it
    were a table. Wrapping it in `CREATE VIEW` gives the file a stable name you can reference later; the
    read stays **lazy** (nothing happens until a query touches the view). (No Book chapter — DuckDB SQL.)
  - 🆕 Concept: `execute_batch` runs one or more SQL statements for their **side effects** (no result rows
    come back) — the right call for DDL like `CREATE VIEW`. (duckdb-rs API.)
  - ⚠️ Invariant #3 (analyze is local): the only file path that ever reaches this function is a **local**
    one. There is no `s3://` here, and no httpfs is loaded — DuckDB cannot reach a source from M1. (The
    boundary that *does* touch a source is `load`, in M2/M6.)
  - ⚠️ Security note (carried to M2): in M1 the path comes from *your test*, so a `format!`-built SQL
    string is fine. When `register_parquet` is fed a path derived from agent input, switch the path to a
    **bind parameter** to avoid SQL-injection via the filename. Leave a `// TODO(M2): bind the path`
    comment now.
  - ✅ Done when: `register_parquet_returns_a_handle` passes; `cargo test -p droplet-core --features duckdb`
    is green.

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

### Chunk E — The capped read-out: `to_rows` and `scalar`

Now the **only** two primitives that move actual rows from DuckDB into the sandbox — and both are
**capped**. Everything else (Chunk F) stays handle-to-handle and never materializes data. You build the
capped Arrow read-out here once, then reuse it.

#### Add the row cap

- [ ] Add a row-cap constant in `droplet-core` (next to `DuckEngine`, or in a small `consts` module):
  ```rust
  /// Max rows any tool may move into the sandbox in one result.
  pub const MAX_RESULT_ROWS: usize = 1000;
  ```
  - ⚠️ Invariant #6 (boundary discipline): *only `to_rows`/`scalar`/load-samples move rows into the
    sandbox, capped; everything else is handles.* This cap is load-bearing — it keeps snapshots small
    later. (PRODUCT.md §15 #6.)
  - 🔗 Maps to: every `to_rows` result the agent ever sees is bounded by this. The cap is *the* thing that
    keeps the REPL holding "capped results, not data."

#### The internal Arrow query path (sync core)

- [ ] Write a **synchronous** private helper on `DuckEngine` that runs a `SELECT` and collects Arrow out.
  Both `to_rows` and `scalar` route through it:
  ```rust
  use duckdb::arrow::record_batch::RecordBatch;

  impl DuckEngine {
      /// SYNC: run a SELECT and return Arrow batches. Internal — callers cap.
      /// Caller wraps the public async entrypoint in spawn_blocking (Chunk F end).
      fn query_arrow_blocking(&self, sql: &str) -> Result<Vec<RecordBatch>, crate::DropletError> {
          let mut stmt = self.conn.prepare(sql)?;
          let batches: Vec<RecordBatch> = stmt.query_arrow([])?.collect();
          Ok(batches)
      }
  }
  ```
  - 🆕 Concept: a **prepared statement** (`conn.prepare(sql)?`) compiles the SQL once and can be queried
    for results. `.query_arrow([])?` runs it and yields an iterator of `RecordBatch`; `.collect()` gathers
    them into a `Vec`. The `[]` means no bind parameters. (No Book chapter — duckdb-rs API; the real chain
    is `conn.prepare(sql)?.query_arrow([])?.collect()`, no semicolons mid-chain.)
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

#### `to_rows` — capped rows of a dataset

- [ ] Implement `to_rows(dataset)`: it selects from the handle's table with a hard `LIMIT`, runs the
  Arrow path, then caps in code too (belt-and-suspenders):
  ```rust
  impl DuckEngine {
      /// Move up to MAX_RESULT_ROWS rows of a dataset into the caller as Arrow.
      pub fn to_rows(&self, ds: &Dataset) -> Result<Vec<RecordBatch>, crate::DropletError> {
          let sql = format!(
              "SELECT * FROM {} LIMIT {}",
              ds.table(), crate::MAX_RESULT_ROWS
          );
          let batches = self.query_arrow_blocking(&sql)?;
          Ok(cap_batches(batches, crate::MAX_RESULT_ROWS))
      }
  }
  ```
  - Why both `LIMIT` *and* a code-side cap: the `LIMIT` lets DuckDB **stop early** (it never materializes
    more than 1000 rows), and `cap_batches` is a second guard in case a primitive forgets the `LIMIT`.
    DuckDB has no built-in "max rows returned" knob, so the SQL `LIMIT` is what makes it stop early.
  - ⚠️ Invariant #6 again: this is one of exactly two functions allowed to move rows across the boundary,
    and it is capped. Every other primitive returns a handle.
  - 🔗 Maps to: PRODUCT.md §7 worked example — `for r in to_rows(agg):` loops over the *small* result the
    agent pulled back; the agent's own Python runs over those capped rows.
  - ✅ Done when: a feature-gated test registers the fixture, calls `to_rows`, and asserts the total row
    count is `5` (the fixture has 5 rows, under the cap). A second test over a synthetic >1000-row dataset
    asserts the total is clamped to `1000`.

#### `scalar` — exactly one value

- [ ] Implement `scalar(dataset_or_sql)`: the agent's way to pull a **single** value (a count, a sum, a
  max) into the sandbox. Simplest shape: take a `Dataset` plus a SQL expression to evaluate, return one
  Arrow value (or read it out as a typed Rust value). A minimal version that returns an `i64`:
  ```rust
  impl DuckEngine {
      /// Pull exactly one numeric value out (e.g. a COUNT or SUM).
      pub fn scalar_i64(&self, ds: &Dataset, expr: &str) -> Result<i64, crate::DropletError> {
          // CAST to BIGINT so the column type is unambiguously i64 — see the
          // HUGEINT verify note below. SUM over an INTEGER column is HUGEINT, not BIGINT.
          let sql = format!("SELECT CAST({expr} AS BIGINT) FROM {} LIMIT 1", ds.table());
          let v: i64 = self.conn.query_row(&sql, [], |r| r.get(0))?;
          Ok(v)
      }
  }
  ```
  - 🆕 Concept: `scalar` is the *narrowest* boundary crossing — exactly one value, so it's inherently
    capped. `query_row` (from the Chunk A smoke test) is perfect here: one row, one column. (duckdb-rs
    API.)
  - ⚠️ Invariant #6: `scalar` is the other allowed row-mover, and "one value" is the tightest possible
    cap. Keep its result tiny by construction.
  - Note: a fuller `scalar` would return a typed enum (int/float/string/null) instead of hard-coding
    `i64`. Hard-code `i64` for M1 to keep it small; widen it when the type-mapping work lands later.
  - verify: `SUM` over an INTEGER column returns a DuckDB **HUGEINT** (i128), *not* a BIGINT (i64) — so
    reading a bare `SUM(amount)` as `r.get::<_, i64>(0)` can raise a runtime `InvalidColumnType` (the
    error names a column-type mismatch, not the real cause). The `CAST(... AS BIGINT)` above sidesteps
    this. Confirm whether duckdb `1.10503`'s `i64` reader coerces a HUGEINT source — if it does, the CAST
    is belt-and-suspenders; if not, the CAST is what makes `scalar_i64` work. (Alternatives: read it as
    `i128`, or make the fixture's `amount` column BIGINT.)
  - ✅ Done when: a test asserts `scalar_i64(&ds, "SUM(amount)")` returns `790` over the fixture.

---

### Chunk F — Handle-to-handle primitives + `local_sql`

These primitives are the **unrestricted** analyze surface (invariant #3): they take `Dataset` handles and
return **new `Dataset` handles** — no rows cross into the sandbox. Each one runs SQL that *materializes a
result inside DuckDB* under a fresh `ds_{n}` name. Because it's all local and throwaway, `local_sql` can
run **arbitrary** DuckDB SQL — there is nothing to protect.

> **The pattern for every primitive below is the same:** build a SQL string from the input handle(s),
> `CREATE VIEW ds_{next} AS <that SQL>`, and return `Dataset { table: "ds_{next}" }`. You already wrote
> the "register a view, hand back a handle" move in Chunk D — factor it into a small private helper and
> reuse it.

- [ ] Factor the "create a view from SQL, return a handle" move into a private helper so every primitive
  reuses it:
  ```rust
  impl DuckEngine {
      /// Materialize `select_sql` as a new view and return its handle.
      fn new_view(&mut self, select_sql: &str) -> Result<Dataset, crate::DropletError> {
          let table = format!("ds_{}", self.next_id);
          self.next_id += 1;
          self.conn.execute_batch(&format!("CREATE VIEW {table} AS {select_sql}"))?;
          Ok(Dataset { table })
      }
  }
  ```
  - 🆕 Concept: pulling the shared move into one method is just **DRY** (don't repeat yourself); now
    `register_parquet` and every primitive call `new_view(...)`. Refactor `register_parquet` to call it.
    (Rust Book: nothing new — basic method extraction.)
  - ✅ Done when: `register_parquet` is rewritten in terms of `new_view` and its test still passes.

- [ ] Implement `filter_rows(dataset, where_sql) -> Dataset`: a `WHERE` over the handle's table, producing
  a new handle. For M1, accept a raw SQL predicate string (the *typed* filter helpers `eq`/`gt`/`between`
  that build this string land in **M2**):
  ```rust
  impl DuckEngine {
      pub fn filter_rows(&mut self, ds: &Dataset, where_sql: &str)
          -> Result<Dataset, crate::DropletError>
      {
          let sql = format!("SELECT * FROM {} WHERE {}", ds.table(), where_sql);
          self.new_view(&sql)
      }
  }
  ```
  - 🆕 Concept: a primitive that returns a `Dataset` instead of rows is a **handle-to-handle** op — the
    agent chains `filter_rows(…)` → `group_agg(…)` → `to_rows(…)` and only the *last* call moves data.
    Heavy work stays in DuckDB. (No Book chapter — the Droplet boundary pattern.)
  - ⚠️ Invariant #3 (analyze unrestricted *because* local): the predicate can be any local SQL — it's safe
    precisely because it can only touch the local copy, never a source.
  - ✅ Done when: a test does `filter_rows(&ds, "amount > 100")` then `to_rows(...)` on the result and
    asserts exactly the rows with `amount > 100` come back (150, 200, 300 → 3 rows).

- [ ] Implement `group_agg(dataset, by, metrics) -> Dataset`: a `GROUP BY` over the handle, producing a
  new handle. For M1, keep the signature small — `by` is a list of column names and `metrics` is a list of
  `(alias, sql_expr)` pairs you splice into the `SELECT`:
  ```rust
  impl DuckEngine {
      pub fn group_agg(
          &mut self,
          ds: &Dataset,
          by: &[&str],
          metrics: &[(&str, &str)], // (alias, "SUM(amount)") etc.
      ) -> Result<Dataset, crate::DropletError> {
          let select_cols = /* join by + "expr AS alias" for each metric */ todo!();
          let group_cols  = by.join(", ");
          let sql = format!(
              "SELECT {select_cols} FROM {} GROUP BY {group_cols}",
              ds.table()
          );
          self.new_view(&sql)
      }
  }
  ```
  - 🆕 Concept: `&[&str]` is a **slice of string slices** — a borrowed, read-only list of column names.
    `slice.join(", ")` builds the comma list. Building SQL by string-splicing is fine here because every
    piece is host-controlled in M1 (the agent-facing typed builder is M2). (Rust Book: *The Slice Type*,
    ch. 4.)
  - ⚠️ Invariant #6: the aggregate result stays a **handle** — even though `group_agg` shrinks the data, it
    does not move rows into the sandbox. The agent calls `to_rows` on the result when it actually wants the
    (small) numbers.
  - 🔗 Maps to: PRODUCT.md §7's worked example builds exactly such a `group_agg(usage, by=["account_id"],
    metrics={…})` and then loops `to_rows(agg)`. You're building the engine side of that line.
  - ✅ Done when: a test does `group_agg(&ds, &["category"], &[("total","SUM(amount)")])` then `to_rows`
    on the result, asserting `a→200, b→290, c→300` (3 rows).

- [ ] Implement `local_sql(sql, datasets) -> Dataset`: the **unrestricted** escape hatch — arbitrary
  DuckDB SQL over named local datasets. The `datasets` argument maps the names the agent used in the SQL to
  the real `ds_{n}` views (so the agent writes readable SQL like `SELECT … FROM usage`):
  ```rust
  impl DuckEngine {
      /// Arbitrary local SQL over named datasets. Unrestricted: local & ephemeral.
      pub fn local_sql(
          &mut self,
          sql: &str,
          datasets: &[(&str, &Dataset)], // ("usage", &ds) — alias the SQL refers to
      ) -> Result<Dataset, crate::DropletError> {
          // Register each alias as a view name the SQL can reference, then run it.
          // Simplest M1 approach: CREATE OR REPLACE TEMP VIEW <alias> AS SELECT * FROM <ds.table()>;
          // (OR REPLACE makes a repeated alias idempotent — a plain CREATE TEMP VIEW errors with
          // "view already exists" if local_sql is called twice with the same alias in one session.)
          // then materialize `sql` as a new handle.
          todo!()
      }
  }
  ```
  - 🆕 Concept: this is the difference the whole product is built on. `local_sql` is **arbitrary** SQL with
    no guardrails — and that's *safe* because it can only ever touch the local, ephemeral DuckDB. Compare
    `load` (M2), the single guarded door that touches a source: there, SQL is **never** arbitrary. (No Book
    chapter — this is Droplet's core thesis.)
  - ⚠️ Invariant #3 (verbatim core): *"Analyze runs solely against the local materialized copy; it is
    unrestricted but cannot reach a source."* `local_sql` is the purest expression of that rule — wide open,
    yet physically unable to reach S3 (no httpfs is loaded; there's no `s3://` path anywhere in M1).
  - ⚠️ Invariant #2 (the contrast — do not blur it): *"only `load` touches a source… no arbitrary SQL
    against production."* `local_sql` being unrestricted is fine *only* because it is local; never let this
    pattern leak to the load side.
  - ✅ Done when: a test runs `local_sql("SELECT category, SUM(amount) AS total FROM usage GROUP BY
    category", &[("usage", &ds)])`, then `to_rows` on the result, asserting the same `a→200, b→290, c→300`.

#### Wrap the engine entrypoint in `spawn_blocking`

DuckDB **blocks the OS thread** while a query runs. Droplet is an async (Tokio) program — the stores are
async — so a blocking DuckDB call on a runtime thread would freeze everything. The rule: run every DuckDB
query inside `tokio::task::spawn_blocking`. You expose **one** async boundary; the synchronous primitives
above all run *inside* it.

- [ ] Make sure `tokio` is available to `droplet-core` with the runtime + macros features (you added this
  in M0). If not, in `[workspace.dependencies]`:
  ```toml
  tokio = { version = "1", features = ["rt-multi-thread", "macros", "sync"] }
  ```
  and opt in from `crates/droplet-core/Cargo.toml` with `tokio.workspace = true`.
  - ✅ Done when: `cargo build -p droplet-core --features duckdb` is green with `tokio` in scope.

- [ ] Add an **async** entrypoint that wraps a blocking unit of analyze work in `spawn_blocking`. Because a
  `Connection` is `!Sync` and must live on the blocking thread, the cleanest M1 shape is to run a whole
  closure of analyze work against an engine *owned inside* the task. A minimal shape that runs one analyze
  step and returns capped rows:
  ```rust
  use duckdb::arrow::record_batch::RecordBatch;

  // The closure is `move`, `'static`, and `Send`; the Connection is NOT `Sync`,
  // so we create/own the engine inside the task (never share across threads).
  pub async fn analyze_local_parquet(path: String)
      -> Result<Vec<RecordBatch>, crate::DropletError>
  {
      let rows = tokio::task::spawn_blocking(move || -> Result<_, crate::DropletError> {
          let mut eng = DuckEngine::new_in_memory()?;      // owned here, on the blocking thread
          let ds  = eng.register_parquet(&path)?;
          let agg = eng.group_agg(&ds, &["category"], &[("total", "SUM(amount)")])?;
          eng.to_rows(&agg)                                 // capped Arrow back
      })
      .await??; // outer ? = JoinError (did the task panic?); inner ? = DropletError
      Ok(rows)
  }
  ```
  - 🆕 Concept: **synchronous vs async.** DuckDB blocks its thread. `tokio::task::spawn_blocking` moves
    that work onto a dedicated blocking-thread pool so the async runtime's worker threads stay free.
    (Rust Book: *Fundamentals of Asynchronous Programming*, ch. 17; the rule is "don't do blocking work on
    the async threads.")
  - 🆕 Concept: the closure is `move` so it **takes ownership** of `path`; it must be `Send + 'static` so
    Tokio can run it on another thread. Pass owned data in (a `String`, not a `&str`) and return owned data
    out. (Rust Book: *Understanding Ownership*, ch. 4; *Fearless Concurrency*, ch. 16 — `move` closures.)
  - 🆕 Concept: keeping the whole multi-step analyze inside **one** `spawn_blocking` (rather than one per
    primitive) keeps the `!Sync` `Connection` on a single thread for its whole life — the simplest correct
    shape for M1. (A later milestone may hold the engine across calls behind a dedicated worker; not now.)
  - ⚠️ Invariant #9 (verbatim): *"DuckDB is synchronous → `spawn_blocking`; release the GIL at the PyO3
    boundary during query work."* This task satisfies the `spawn_blocking` half (the GIL half is
    droplet-py's, below).
  - ✅ Done when: an async `#[tokio::test]` calls `analyze_local_parquet(path).await?` over the fixture and
    gets the same capped aggregate the Chunk F sync test produced (`a→200, b→290, c→300`).

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
  - ✅ Done when: the async entrypoint compiles with `.await??` and no manual error mapping.

- [ ] Leave a forward-reference comment near the async entrypoint for the Python layer (do **not**
  implement it here):
  ```rust
  // GIL (droplet-py): when an analyze primitive is called via PyO3, the thin
  // wrapper in droplet-py must release the GIL around the call (py.detach(...))
  // so other Python threads run while DuckDB works. droplet-core itself has NO
  // pyo3 (invariant #8) — the GIL release lives only in droplet-py.
  ```
  - ⚠️ Invariant #8 (verbatim): *"droplet-core (Rust) does not depend on pyo3; PyO3 lives only in
    droplet-py."* The GIL release is the *other half* of invariant #9, but it belongs in `droplet-py`, not
    here. This comment is the seam, not an implementation.
  - verify: the exact GIL-release call. The research found the method was **renamed** from
    `Python::allow_threads` to `Python::detach` in pyo3 0.26 (no deprecated alias). Confirm `py.detach(...)`
    against the pyo3 version droplet-py pins when you actually build that layer — but write nothing pyo3 in
    `droplet-core`.
  - ✅ Done when: the note is in code near the async entrypoint and you can state in one sentence why the
    GIL release is *not* in `droplet-core`.

---

### Chunk G — Integration test + the M1 "Done"

Lock the milestone with one integration test that exercises the whole local analyze chain over the
fixture — registered handle → handle-to-handle primitives → capped read-out → all inside `spawn_blocking`.

- [ ] Write an integration test (`crates/droplet-core/tests/analyze_local.rs`, feature-gated) that drives
  the full analyze surface over `tests/data/sample.parquet`:
  ```rust
  #![cfg(feature = "duckdb")]
  // 1. register_parquet(sample.parquet) -> Dataset handle
  // 2. filter_rows(ds, "amount > 100")  -> Dataset handle
  // 3. group_agg(..., ["category"], [("total","SUM(amount)")]) -> Dataset handle
  // 4. to_rows(result)  -> capped Arrow; assert the small expected aggregate
  // 5. scalar_i64(ds, "SUM(amount)") -> 790
  // 6. local_sql("SELECT category, SUM(amount) AS total FROM u GROUP BY category",
  //              [("u", &ds)]) -> Dataset; to_rows -> a→200, b→290, c→300
  ```
  - 🆕 Concept: an integration test in `tests/` uses your crate as an *outside user* would — it can only
    call `pub` items. If a test can't reach something, that something needs to be `pub` (or the test belongs
    inside the module as a unit test). (Rust Book: *Writing Automated Tests*, ch. 11.)
  - ✅ Done when: `cargo test -p droplet-core --features duckdb` passes the whole chain. **This is the local
    analyze half of the M1 "Done when."**

- [ ] Confirm the full async path end to end: an async call (`analyze_local_parquet(path).await?`) →
  `spawn_blocking` → DuckDB over the **local** Parquet → handle-to-handle primitives → capped Arrow
  `Vec<RecordBatch>` back. This *is* the spec's step-3 "done": **a local analyze engine runs the dataframe
  primitives and `local_sql` over a local Parquet `Dataset`, capped, inside `spawn_blocking`.**
  - ⚠️ Invariant #9: the query ran inside `spawn_blocking` (async runtime never stalled). Invariant #6: the
    result that crossed the boundary was capped. Invariant #3: every primitive touched only the local copy.
    Invariant #10: any failure surfaced as `DropletError`.
  - ✅ Done when: a single `#[tokio::test]` demonstrates that chain and asserts a small result. **This is
    the M1 "Done when."**

---

## M1 done checklist

Tick all of these to call M1 complete (the spec's step-3 "Done when" expanded):

- [x] `duckdb = "1.10503.1"` with features `["bundled", "parquet", "json"]` (and only those) is pinned in
  `[workspace.dependencies]` and gated behind an **opt-in** `duckdb` feature in `droplet-core`. Default
  build stays fast; a `// BUILD PATH` note records the prebuilt/`frozen-duckdb` escape hatch. **No
  `httpfs`/S3 setup anywhere — that's M6.**
- [x] Exactly **one** Arrow major in the tree (`cargo tree -i arrow` shows a single `58.3.0`), and the code
  names Arrow types via `duckdb::arrow` — no top-level `arrow` dependency.
- [x] Each `Session` owns its own ephemeral in-memory `Connection` behind a host-side `DuckEngine`
  (invariants #3, #6); the sandbox never sees a `Connection`. (`duck()` + `duck_mut()` accessors.)
- [x] A **local** Parquet file registers as a `Dataset` **handle** (a DuckDB view under a monotonic
  `ds_{n}` name); the handle is what crosses to the sandbox, never the rows (invariant #6).
- [x] The handle-to-handle primitives `filter_rows`, `group_agg`, and the unrestricted `local_sql(sql,
  datasets=…)` each return a **new `Dataset` handle**; only `to_rows` (capped via SQL `LIMIT` +
  `cap_batches`) and `scalar` move actual rows/values into the caller (invariants #3, #6).
- [x] `duckdb::Error` and `tokio::task::JoinError` both fold into `DropletError` via `#[from]` (the duckdb
  variant feature-gated) — invariant #10.
- [x] The analyze work runs inside `tokio::task::spawn_blocking` and is awaited with `.await??`; the async
  runtime is never stalled (invariant #9). A comment forward-references the GIL release that will live in
  `droplet-py` (invariant #8).
- [x] A local-Parquet integration test drives `register_parquet → filter_rows → group_agg → to_rows`,
  asserts the small capped aggregate (`a→200, b→290, c→300`), and checks `scalar` (`790`) and `local_sql`.

**Spec "Done when": a local analyze engine runs the dataframe primitives and `local_sql` over a local
Parquet `Dataset`, capped, inside `spawn_blocking`.** ✅ **DONE.**

---

## M1 build corrections (back-ported after implementation)

> Same convention as M0's back-ported corrections: anchors in the chunks above are kept as-is for the
> learning narrative; the load-bearing facts that differed in reality are pinned here.

- **DuckDB crate version is `1.10503.1`, not `1.10503`** — `1.10503.1` is the real latest on crates.io
  (engine still DuckDB v1.5.3). The lockfile pins it exactly; engine bumps stay deliberate.
- **Arrow major is `58.3.0`** — confirmed via `cargo tree -i arrow` (the chunk's `58.x` guess was right).
- **`Statement::query_arrow` takes `&mut self`** — so `let mut stmt = conn.prepare(sql)?;` is required
  (the "doesn't need mut" the chunk warned about was a different variable; the compiler is authoritative).
- **DuckDB error `#[from]` lands with Chunk C, not D** — `Session::new` needs `?` to fold it when it
  builds the engine. Functionally identical; just an earlier step.
- **Invariant #3 is enforced STRUCTURALLY, not by "no httpfs loaded".** Bundled DuckDB *auto-loads*
  `httpfs` on the first remote path and **does** reach S3/HTTP by default — "physically cannot reach a
  source" was false. `new_in_memory` now sets `autoinstall/autoload_known_extensions=false` +
  `disabled_filesystems='HTTPFileSystem,S3FileSystem'`. Do **not** use `enable_external_access=false` — it
  also blocks the LOCAL file reads `register_parquet` needs. `disabled_filesystems` is a one-way latch
  that holds even after an explicit `LOAD httpfs`. (Local-filesystem sandboxing remains an M6 concern.)
- **`local_sql` binds aliases as CTEs, not lingering `TEMP VIEW`s.** Temp-view aliases are resolved by
  name at query time, so reusing an alias for a different dataset silently changed what an *earlier*
  handle returned (and an alias named `ds_N` could shadow a real handle). CTEs make each result view
  self-contained.
- **`group_agg` with an empty `by`** omits `GROUP BY` entirely (a grand-total) instead of emitting a
  trailing `GROUP BY ` parser error.
- **CI** gained a cached `--features duckdb` clippy/test step so the opt-in engine is actually exercised.

When green, move on to [`M2-load-boundary.md`](./M2-load-boundary.md) — add the `load(name, columns,
where, as_of) -> Dataset` boundary (a Catalog + the connector that materializes a slice locally) and the
typed filter helpers (`eq`, `gt`, `between`, …) that build the predicates `filter_rows` consumes here.
The analyze surface you just built is what `load`'s `Dataset` flows into.
