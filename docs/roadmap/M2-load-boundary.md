# M2 — The `load` boundary (the single guarded door)

**Milestone goal:** build **`load`** — the one and only tool that ever touches a source. Concretely:
a tiny **`Catalog`** (configuration mapping a *logical dataset name* → its **connector** + **schema**,
**hidden from the agent**), a **`load(name, columns, where, as_of) -> Dataset`** function that looks the
dataset up, asks the **`Source` connector** (the trivial local-Parquet dev one from M0) for a *scoped*
Parquet slice, **materializes** it into the **M1 analyze engine**, and hands back a `Dataset` handle — plus
the typed **filter helpers** (`eq`, `gt`, `lt`, `gte`, `lte`, `in_`, `between`, `contains`) the agent uses
to express `where`. After M2, the agent can pull a bounded local slice and then analyze it with the M1
primitives — **and there is still no path to run arbitrary SQL against a source.**

**Done when (from the spec, BUILD ORDER step 5, single-machine slice of it):** *a `load(...)` call resolves a
catalog dataset, runs its connector to produce a scoped local Parquet, materializes it into an ephemeral
DuckDB, and returns a `Dataset` handle the M1 analyze primitives can read — with the connector the only
thing that touched the "source".*

**Prerequisite:** finish [`M0-skeleton.md`](./M0-skeleton.md) **and** [`M1-analyze-engine.md`](./M1-analyze-engine.md).
From **M0** you need: the virtual workspace, `droplet-core`, `DropletError` (thiserror) with `#[from]`
variants, the generic **handle registry** (`Registry<T>` with `add` / `get` / `require`), the **`Session`**
type, the **four store traits** — and in particular the **`Source` trait** with its **`LocalParquetSource`** dev
impl. From **M1** you need: the ephemeral per-session **DuckDB engine** (`DuckEngine`), the **`Dataset`**
handle, `MAX_RESULT_ROWS` + `cap_batches`, the `spawn_blocking` pattern, and the analyze primitives
(`filter_rows`, `group_agg`, `to_rows`, `scalar`, `local_sql`). M2 *adds the load door in front of* that
analyze engine.

**Estimate:** ~7 chunks (A–G), each a focused sitting. Do them in order — later chunks build on the
`Catalog`, the scope types, and the filter helpers you write earlier.

The spec lives at `PRODUCT.md` (repo root). Reference it that way, never `docs/PRODUCT.md`. The parts M2
implements are **§6 (LOAD), §9 (Catalog & connectors), and §10 (tool surface / filters)**.

---

## How to read this file

- Every `- [ ]` is a tiny task (~10–30 min for a Rust newbie). Check it off only when it's truly done.
- `🆕 Concept:` explains a new Rust/Droplet idea the **first** time it shows up, with a Rust Book chapter
  *name* (run `rustup doc --book` to open the book offline).
- `✅ Done when:` is an observable check — usually a command's output or a passing test. Don't move on until
  you see it.
- `⚠️ Invariant:` quotes a load-bearing rule from `PRODUCT.md` §15 in plain words, by its number 1–10 (the
  same ten in the roadmap README's "Golden rules"). Never break these.
- `🔗 Maps to:` ties a tiny step to the real Droplet concept it unlocks.
- `verify:` flags a fact the research couldn't fully pin on the locked version — check it against the real
  crate source/docs before relying on it, don't guess.
- Code snippets are **anchors** (a few lines to orient you). You write the real implementation.

**The build/learn loop for M2:** add one small thing (a struct field, a function, a `#[test]`) → `cargo
build` → write/​run a tiny `#[test]` → watch it fail → make it pass → tick the box → `git commit` at the end
of each chunk. Same rhythm as M0/M1.

> **The one idea to hold onto before you start.** M1 gave you an engine that can run SQL over a *local*
> Parquet file you happened to have. M2 answers the question *"where did that local Parquet come from, and
> who is allowed to ask for it?"* The answer is **`load`, and only `load`.** Everything in this file exists
> to make `load` the *single guarded door*: the agent names a **logical** dataset, picks **columns** and a
> **`where`** filter from a typed menu, and gets back a **local** `Dataset` — and it has **no** way to send a
> raw query, a table name, or a connection string to a real source. That door is invariant #2.

---

### Chunk A — The two invariants this whole file defends

Before any code, fix the two rules M2 exists to enforce. They are the reason the design looks the way it
does; every later step points back here.

- [ ] Read these two invariants and keep them open in a split pane while you work. They are PRODUCT.md §15
  rules **1** and **2** (verbatim shortened):
  - **Invariant #1 — the agent never sees the source engine.** All sources are reached through *connectors*
    that normalize to Parquet; the agent works against **logical, local** datasets only. It cannot tell —
    or ask — whether `"usage_daily"` is backed by Athena, Snowflake, or a plain S3 file.
  - **Invariant #2 — only `load` touches a source, and only via a bounded, typed, cached unload. No
    arbitrary SQL against production, ever.** `load` is the single guarded door.
  - 🔗 Maps to: PRODUCT.md §6 (*"the engine binding is configuration it never sees"*) and §14 (*"Load is the
    only governed boundary … there is no arbitrary SQL against production, ever."*).

- [ ] Write yourself a one-line design rule, as a comment you'll drop at the top of the load module later:
  *"The agent passes a **dataset name** + a **typed scope**; it never passes a SQL string, a table, or a
  connector to `load`. The SQL that hits the (dev) source is built **here**, from the catalog, never from
  agent input."*
  - 🆕 Concept: an **invariant** is a property your code keeps true at all times, by construction — not a
    runtime check you hope fires. Here we keep invariant #2 true by *never giving the agent a way to express*
    a raw query: the only inputs to `load` are a name and typed scope values. (No Book chapter — design
    discipline; the Rust angle is "make illegal states unrepresentable" via the type system.)
  - ⚠️ Invariant #2: this sentence *is* the milestone. Re-read it whenever a later step tempts you to let a
    string flow from the sandbox into a query.
  - ✅ Done when: you can state, in one breath, what the agent is and is **not** allowed to hand `load`.

---

### Chunk B — Filter helpers: a typed scope the agent *can* express (`eq`, `gt`, … `contains`)

The agent's `where` is a **list of typed filter values**, not a SQL string. Each helper (`eq`, `gt`, …) is a
plain **value-constructor**: it builds a small data object describing *one* condition. The agent composes
them; **the host** turns the list into safe SQL. Because the agent can only ever build these objects, it can
only ever express in-scope, well-formed conditions — that's how invariant #2 holds. This chunk builds the
Rust side of those helpers (the Python-facing names land in droplet-py later; here you build the core
representation and a SQL renderer).

PRODUCT.md §10 names exactly eight: `eq`, `gt`, `lt`, `gte`, `lte`, `in_`, `between`, `contains` (the first
arg is always a field name — a `Literal` against the schema, once auto-bootstrap exists in M4).

- [ ] Create `crates/droplet-core/src/load.rs` and declare `pub mod load;` in `lib.rs`. Drop the design-rule
  comment from Chunk A at the top of the file.
  - 🆕 Concept: a Rust **module** is a namespace for related items; `pub mod load;` in `lib.rs` makes
    `crate::load::…` reachable. (Rust Book: *Managing Growing Projects with Packages, Crates, and Modules*.)
  - ✅ Done when: `cargo build -p droplet-core` is green with an empty `load.rs`.

- [ ] Define the **operator** as an enum, and a **`Filter`** struct carrying a field, an operator, and the
  value(s). Start with a string-typed value (`FilterValue`) you can widen later:
  ```rust
  /// One comparison operator the agent may express. Mirrors PRODUCT.md §10.
  #[derive(Debug, Clone, PartialEq)]
  pub enum FilterOp {
      Eq, Gt, Lt, Gte, Lte,
      In,        // value is a list
      Between,   // value is exactly two bounds (lo, hi)
      Contains,  // substring match on a text field
  }

  /// A literal value (kept simple for M2 — widen to typed variants later).
  #[derive(Debug, Clone, PartialEq)]
  pub enum FilterValue {
      One(String),
      Many(Vec<String>),
      Range(String, String), // (lo, hi) for Between
  }

  /// One scope condition: `field <op> value`. The agent builds these via the
  /// helpers below; it never writes the SQL.
  #[derive(Debug, Clone, PartialEq)]
  pub struct Filter {
      pub field: String,
      pub op: FilterOp,
      pub value: FilterValue,
  }
  ```
  - 🆕 Concept: an **enum** with data-carrying variants (`One(String)`, `Range(String, String)`) is Rust's
    sum type — a value is *exactly one* of these shapes, and the compiler forces you to handle each. This is
    how we make "a between needs two bounds, an `in` needs a list" *unrepresentable wrongly*. (Rust Book:
    *Enums and Pattern Matching*.)
  - 🆕 Concept: `#[derive(Debug, Clone, PartialEq)]` gives you printing (`{:?}`), copying-by-clone, and
    `==` for free — handy in tests. (Rust Book: *Appendix C — Derivable Traits*.)
  - ✅ Done when: `cargo build -p droplet-core` compiles with the three types present.

- [ ] Write the eight **value-constructor helpers** as plain functions returning a `Filter`. They are
  deliberately tiny — each just packages its arguments:
  ```rust
  pub fn eq(field: &str, v: &str)  -> Filter { Filter { field: field.into(), op: FilterOp::Eq,  value: FilterValue::One(v.into()) } }
  pub fn gt(field: &str, v: &str)  -> Filter { Filter { field: field.into(), op: FilterOp::Gt,  value: FilterValue::One(v.into()) } }
  pub fn lt(field: &str, v: &str)  -> Filter { Filter { field: field.into(), op: FilterOp::Lt,  value: FilterValue::One(v.into()) } }
  pub fn gte(field: &str, v: &str) -> Filter { Filter { field: field.into(), op: FilterOp::Gte, value: FilterValue::One(v.into()) } }
  pub fn lte(field: &str, v: &str) -> Filter { Filter { field: field.into(), op: FilterOp::Lte, value: FilterValue::One(v.into()) } }
  pub fn in_(field: &str, vs: &[&str]) -> Filter {
      Filter { field: field.into(), op: FilterOp::In,
               value: FilterValue::Many(vs.iter().map(|s| s.to_string()).collect()) }
  }
  pub fn between(field: &str, lo: &str, hi: &str) -> Filter {
      Filter { field: field.into(), op: FilterOp::Between, value: FilterValue::Range(lo.into(), hi.into()) }
  }
  pub fn contains(field: &str, sub: &str) -> Filter {
      Filter { field: field.into(), op: FilterOp::Contains, value: FilterValue::One(sub.into()) }
  }
  ```
  - 🆕 Concept: `&str` is a **borrowed** string slice; `.into()` / `.to_string()` makes an **owned** `String`
    the `Filter` can keep. The function borrows briefly and stores owned data — no lifetimes leak out. (Rust
    Book: *Understanding Ownership* → "Slices"; *Storing UTF-8 Encoded Text with Strings*.)
  - Note: `in_` has a trailing underscore because `in` is a Rust **keyword** (and the same reason the Python
    name is `in_` — `in` is a Python keyword too). PRODUCT.md §10 spells it `in_`.
  - 🔗 Maps to: PRODUCT.md §6's agent call —
    `where=[eq("segment","enterprise"), between("day","2025-10-01","2026-03-31")]` — is *exactly* a
    `Vec<Filter>` built from these helpers.
  - ✅ Done when: `cargo build -p droplet-core` is green; a tiny `#[test]` asserts e.g.
    `between("day","a","b").value == FilterValue::Range("a".into(), "b".into())`.

- [ ] Write a **`render_sql`** that turns one `Filter` into a SQL predicate string, **quoting the field as an
  identifier and binding/escaping the value safely**. For M2, single-quote and escape string literals; this
  is where you keep injection out:
  ```rust
  /// Render one filter as a SQL `WHERE` predicate. The field becomes a quoted
  /// identifier; values are single-quoted SQL string literals with `'` escaped.
  pub fn render_sql(f: &Filter) -> String {
      let col = quote_ident(&f.field);        // "day" -> "\"day\""
      match (&f.op, &f.value) {
          (FilterOp::Eq,  FilterValue::One(v)) => format!("{col} = {}", lit(v)),
          (FilterOp::Gt,  FilterValue::One(v)) => format!("{col} > {}", lit(v)),
          (FilterOp::Lt,  FilterValue::One(v)) => format!("{col} < {}", lit(v)),
          (FilterOp::Gte, FilterValue::One(v)) => format!("{col} >= {}", lit(v)),
          (FilterOp::Lte, FilterValue::One(v)) => format!("{col} <= {}", lit(v)),
          (FilterOp::In,  FilterValue::Many(vs)) => {
              let items = vs.iter().map(|v| lit(v)).collect::<Vec<_>>().join(", ");
              format!("{col} IN ({items})")
          }
          (FilterOp::Between, FilterValue::Range(lo, hi)) =>
              format!("{col} BETWEEN {} AND {}", lit(lo), lit(hi)),
          (FilterOp::Contains, FilterValue::One(v)) =>
              format!("{col} LIKE {}", lit(&format!("%{v}%"))),
          // Any other (op,value) pairing is a programming error, not agent input:
          _ => unreachable!("filter op/value shape mismatch built outside the helpers"),
      }
  }

  fn quote_ident(s: &str) -> String { format!("\"{}\"", s.replace('"', "\"\"")) }
  fn lit(s: &str) -> String { format!("'{}'", s.replace('\'', "''")) }
  ```
  - 🆕 Concept: **`match` on a tuple** `(&op, &value)` lets you pattern-match *two* values at once and bind
    their inner data (`FilterValue::One(v)` pulls out `v`). The compiler checks you covered the shapes; the
    `_ => unreachable!()` arm documents that the bad combinations can't come from the helpers. (Rust Book:
    *Enums and Pattern Matching* → "The `match` Control Flow Construct".)
  - Note on that `unreachable!`: because `Filter`'s fields are `pub`, host code *could* hand-build a mismatched
    `(op, value)` (e.g. `FilterOp::Between` with a `FilterValue::One`) and trip the panic. That can only happen
    by bypassing the eight helpers — it is a **host-side programming error**, never agent input (the agent only
    ever calls `eq`/`between`/…). If you'd rather not panic, return a `DropletError` from the `_` arm instead.
  - 🆕 Concept: `format!` builds a `String` like Python f-strings; `{col}` interpolates a named variable in
    scope (Rust 2021+ captures). (Rust Book: *Storing UTF-8 Encoded Text with Strings*; `format!` macro.)
  - ⚠️ Invariant #2: this renderer is the *only* place agent-chosen values become SQL, and it **escapes** the
    value (`''` doubling) and **quotes** the identifier (`""` doubling). The agent supplies *data*, never SQL
    structure — so a value like `x'; DROP TABLE …` becomes the harmless literal `'x''; DROP TABLE …'`. (And
    note: even this SQL only ever runs against the **local materialized copy** in later chunks, never a real
    source — the connector, not this SQL, is what fetches from the source.)
  - verify: M2 escapes values into single-quoted string literals (correct + injection-safe, but everything is
    typed as text). When real typed schemas arrive (M4 auto-bootstrap), prefer **bound parameters**
    (`params![...]`) or per-type rendering (numbers/dates unquoted) over string literals. Note this in a
    `// TODO(M4): typed/parameterized rendering` comment so the seam is visible.
  - ✅ Done when: `cargo test -p droplet-core` passes a test like
    `render_sql(&eq("region","EU")) == r#""region" = 'EU'"#` and one asserting a value containing a quote is
    escaped (`render_sql(&eq("name","O'Brien"))` contains `'O''Brien'`).

- [ ] Write a **`render_where`** that joins a `&[Filter]` into one `WHERE` clause (empty slice → empty
  string, so a no-filter load still works):
  ```rust
  pub fn render_where(filters: &[Filter]) -> String {
      if filters.is_empty() { return String::new(); }
      let preds = filters.iter().map(render_sql).collect::<Vec<_>>().join(" AND ");
      format!(" WHERE {preds}")
  }
  ```
  - 🆕 Concept: `slice.iter().map(f).collect::<Vec<_>>()` is the Rust equivalent of a Python list
    comprehension — borrow each element, transform it, gather the results. `.join(" AND ")` glues a
    `Vec<String>`. (Rust Book: *Processing a Series of Items with Iterators*.)
  - ✅ Done when: a test asserts `render_where(&[eq("a","1"), gt("b","2")])` ==
    ` WHERE "a" = '1' AND "b" = '2'`, and `render_where(&[])` is empty. **End of Chunk B — `git commit`.**

---

### Chunk C — The `Catalog`: logical name → connector + schema (hidden from the agent)

The **catalog** is *configuration* (PRODUCT.md §9): a map from a **logical dataset name** the agent uses
(`"usage_daily"`) to (a) which **connector/source** backs it and (b) its **schema** (the column names +
types). The agent never sees this map — it only ever names a dataset. This chunk builds the minimal catalog
and declares **one local dataset** so `load` has something to resolve. Keep it single-machine: the only
"connector" is M0's `LocalParquetSource` reading a Parquet file off disk.

- [ ] Define a **`ColumnSchema`** and a **`DatasetSchema`** (just names + a coarse type tag for now — the rich
  Pydantic-derived schema is M5/M4's job):
  ```rust
  #[derive(Debug, Clone, PartialEq)]
  pub enum ColumnType { Text, Int, Float, Date, Bool }

  #[derive(Debug, Clone, PartialEq)]
  pub struct ColumnSchema { pub name: String, pub ty: ColumnType }

  #[derive(Debug, Clone, PartialEq)]
  pub struct DatasetSchema { pub columns: Vec<ColumnSchema> }

  impl DatasetSchema {
      /// True if every requested column exists in the schema.
      pub fn has_columns(&self, requested: &[&str]) -> bool {
          requested.iter().all(|r| self.columns.iter().any(|c| c.name == *r))
      }
  }
  ```
  - 🆕 Concept: `iter().all(pred)` / `iter().any(pred)` are the Rust `all()`/`any()` — they short-circuit and
    return a `bool`. Nesting them checks "every requested column is present in the schema." (Rust Book:
    *Processing a Series of Items with Iterators*.)
  - 🔗 Maps to: PRODUCT.md §6 — *"`columns` and `where` are `Literal`-typed against the dataset's schema."*
    M2 checks columns at runtime; M4's auto-bootstrap will turn the schema into Python `Literal`s so a wrong
    column fails at **type-check** before the code even runs. `has_columns` is the runtime stand-in.
  - ✅ Done when: `cargo build -p droplet-core` is green; a test asserts `has_columns` is `true` for present
    columns and `false` for a typo.

- [ ] Define a **`DatasetEntry`** — one catalog row binding a logical name to *how to fetch it* and *its
  schema*. For M2 the binding is "a Parquet file name the `LocalParquetSource` can read":
  ```rust
  #[derive(Debug, Clone)]
  pub struct DatasetEntry {
      /// The agent-facing logical name, e.g. "usage_daily".
      pub name: String,
      /// HIDDEN from the agent: the source-relative path the connector reads.
      /// In prod this becomes engine-specific binding (Athena table, etc.) — M6.
      pub source_object: String,   // e.g. "usage_daily.parquet"
      pub schema: DatasetSchema,
  }
  ```
  - ⚠️ Invariant #1: `source_object` (and later the real engine binding) is **configuration the agent never
    sees**. The agent passes `name`; the catalog resolves the binding. Keep `DatasetEntry` host-side; it must
    never appear in a sandbox-facing signature.
  - 🆕 Concept: comments matter here — the field name `source_object` documents intent (it's the hidden
    binding). When M6 adds real connectors this becomes an enum (`Local{..}`, `Athena{..}`, …); for now it's
    one string. (No Book chapter — design note.)
  - ✅ Done when: `cargo build -p droplet-core` compiles with `DatasetEntry` present.

- [ ] Define the **`Catalog`** as a name → entry map, with a `register` and a `get` that errors helpfully on
  an unknown name:
  ```rust
  use std::collections::HashMap;

  #[derive(Debug, Default, Clone)]
  pub struct Catalog {
      datasets: HashMap<String, DatasetEntry>,
  }

  impl Catalog {
      pub fn new() -> Self { Self::default() }

      pub fn register(&mut self, entry: DatasetEntry) {
          self.datasets.insert(entry.name.clone(), entry);
      }

      pub fn get(&self, name: &str) -> Result<&DatasetEntry, DropletError> {
          self.datasets.get(name).ok_or_else(|| DropletError::UnknownDataset(name.to_string()))
      }
  }
  ```
  - 🆕 Concept: `HashMap<String, DatasetEntry>` is a dictionary keyed by the logical name. `.get(name)`
    returns `Option<&DatasetEntry>`; `.ok_or_else(|| …)` turns `None` into a `DropletError` — the same
    Option→Result move M0's `Registry::require` used for bad handles. (Rust Book: *Storing Keys with
    Associated Values in Hash Maps*; *Recoverable Errors with `Result`*.)
  - 🆕 Concept: `#[derive(Default)]` + `Self::default()` gives an empty catalog for free; `Clone` lets a
    `Session` hold its own copy (catalogs are small config). (Rust Book: *Appendix C — Derivable Traits*.)
  - ✅ Done when: `cargo build -p droplet-core` is green once you add the error variant (next step).

- [ ] Add an **`UnknownDataset`** variant to `DropletError` so an unknown name is a clean boundary error
  (and, while you're here, an `UnknownColumn` variant for the column check in Chunk E):
  ```rust
  #[error("unknown dataset: {0}")]
  UnknownDataset(String),
  #[error("unknown column {column:?} in dataset {dataset:?}")]
  UnknownColumn { dataset: String, column: String },
  ```
  - 🆕 Concept: a `thiserror` variant can carry **named fields** (`{ dataset, column }`) and the `#[error]`
    message interpolates them (`{column:?}` uses `Debug` formatting, which quotes strings). (Rust Book:
    *Error Handling*; the `thiserror` crate wires the `Display` impl.)
  - ⚠️ Invariant #10 (one error type): every failure on the load path — unknown dataset, unknown column,
    connector IO, DuckDB — folds into `DropletError`. No raw errors leak past the boundary.
  - ✅ Done when: a test asserts `catalog.get("nope")` returns `Err(DropletError::UnknownDataset(_))` and the
    message reads `unknown dataset: nope`.

- [ ] **Declare one local dataset.** Add a `Catalog::with_dev_dataset()` (or a test helper) that registers a
  single entry pointing at a Parquet file the dev `LocalParquetSource` can read:
  ```rust
  impl Catalog {
      /// One declared local dataset for single-machine dev. The schema below
      /// must match the fixture parquet's columns.
      pub fn with_dev_dataset() -> Self {
          let mut c = Catalog::new();
          c.register(DatasetEntry {
              name: "usage_daily".into(),
              source_object: "usage_daily.parquet".into(),
              schema: DatasetSchema { columns: vec![
                  ColumnSchema { name: "account_id".into(),     ty: ColumnType::Text },
                  ColumnSchema { name: "day".into(),            ty: ColumnType::Date },
                  ColumnSchema { name: "region".into(),         ty: ColumnType::Text },
                  ColumnSchema { name: "active_minutes".into(), ty: ColumnType::Int  },
              ] },
          });
          c
      }
  }
  ```
  - 🔗 Maps to: PRODUCT.md §6's worked example loads `"usage_daily"` with columns
    `["account_id","day","active_minutes",…]` and `where=[eq("region","EU"), …]`. This declared dataset is
    that example, shrunk to single-machine.
  - ✅ Done when: a test builds `Catalog::with_dev_dataset()` and asserts
    `catalog.get("usage_daily")?.schema.has_columns(&["account_id","region"])` is `true`. **End of Chunk C —
    `git commit`.**

---

### Chunk D — A tiny fixture + give the `Session` a catalog

`load` needs (1) a Parquet file the dev connector can fetch and (2) a `Catalog` on the `Session` to resolve
against. This chunk sets both up — small, mechanical, no new big ideas.

- [ ] Create `crates/droplet-core/tests/data/usage_daily.parquet`, a tiny fixture whose columns match the
  declared schema and whose rows have a **known answer** (so the M2 integration test can assert). Easiest with
  Python (you used the same trick for M1's `sample.parquet`):
  ```python
  import duckdb
  duckdb.sql("""
      COPY (SELECT * FROM (VALUES
          ('acct_1','2025-10-05','EU', 120),
          ('acct_1','2026-02-10','EU',  40),
          ('acct_2','2025-11-01','US', 300),
          ('acct_3','2026-01-15','EU',  90)
      ) t(account_id, day, region, active_minutes))
      TO 'crates/droplet-core/tests/data/usage_daily.parquet' (FORMAT parquet)
  """)
  ```
  - Pick the rows so a scoped load has a known result. With the data above,
    `columns=["account_id","active_minutes"]`, `where=[eq("region","EU")]` selects **3** rows (the two
    `acct_1` rows + `acct_3`), summing `active_minutes` to `120 + 40 + 90 = 250`. Write that down — your test
    asserts it.
  - 🆕 Concept: files under a crate's `tests/` directory are for **integration tests** (separate binaries
    using your crate as an outside user would); a non-`.rs` subfolder like `tests/data/` is just storage,
    Cargo won't compile it. (Rust Book: *Writing Automated Tests* → "Integration Tests".)
  - ✅ Done when: the file exists at `crates/droplet-core/tests/data/usage_daily.parquet` and is tracked by
    git (`git status` shows it staged/committed).

- [ ] Add a **`catalog`** field to `Session` (next to the four store fields from M0). Default it to
  `Catalog::with_dev_dataset()` in the dev constructor:
  ```rust
  pub struct Session {
      // … run_id, work_dir, handles, artifacts, snapshots, coord, source …
      catalog: Catalog,
  }
  ```
  - 🆕 Concept: adding a field means updating every `Session` constructor — the compiler will *list* the
    constructors you missed (a "missing field" error). Lean on that: let the build tell you what to fix.
    (Rust Book: *Using Structs to Structure Related Data*.)
  - ⚠️ Invariant #1: the catalog lives on the **host-side** `Session`, never crosses into the sandbox. The
    sandbox only ever calls `load("usage_daily", …)` by name.
  - ✅ Done when: `cargo build -p droplet-core` is green and an existing `Session` test still passes (now with
    a catalog attached).

- [ ] Add small accessors `Session::catalog(&self) -> &Catalog` and `Session::source(&self) -> &dyn Source`
  (you may already have the source accessor from M0) so the `load` function can reach both.
  - 🆕 Concept: `&dyn Source` returns a **borrowed trait object** — the caller can call `Source::load`
    without knowing the concrete type (`LocalParquetSource` now, S3 later). (Rust Book: *Using Trait Objects That
    Allow for Values of Different Types*.)
  - ✅ Done when: `cargo build -p droplet-core` is green; a test can call `session.catalog().get("usage_daily")`.
    **End of Chunk D — `git commit`.**

---

### Chunk E — The connector step: a *scoped* Parquet from the `Source`

Now the heart of invariant #2. `load` resolves the dataset, validates the requested columns against the
schema, asks the **connector** (`Source`) to produce a local Parquet for that dataset, gets back its local
**path**, and — crucially — **scopes** the result to just the requested `columns` + `where` rows. In M2 the
dev connector is M0's `LocalParquetSource`: its `load` resolves the dataset name to an existing `.parquet`
file under a base dir and returns that path (no bytes copied), and the scoping is applied **as we materialize
it** (Chunk F). The key teaching point: **the connector is the only thing that touches the "source," and the
agent's filters become the connector's `SELECT`, never a free-form query.**

> **Where the scope is applied — read this once.** In *production* (M6, Athena/Snowflake) the connector pushes
> the `columns`+`where` down into the source's native `UNLOAD (SELECT … WHERE …)` so the source itself only
> emits the scoped slice — that's what protects production. In *M2's single-machine dev*, the
> `LocalParquetSource` just hands back the path to the **whole** local file (it doesn't filter), so we apply
> the same scoped `SELECT` locally as we materialize (Chunk F). Either way the **agent expresses the scope the
> same way** (columns + typed filters) and the **shape of the result is the same** (a scoped local
> Parquet/view). The seam doesn't change when the real connectors arrive — only *who* runs the scoped SELECT
> does.

- [ ] Define a **`LoadSpec`** struct — the fully-resolved, host-side description of one load. This is what the
  agent's `(name, columns, where, as_of)` becomes *after* catalog resolution:
  ```rust
  pub struct LoadSpec {
      pub dataset: String,         // logical name (for errors/logging)
      pub source_object: String,   // resolved hidden binding (the parquet to read)
      pub columns: Vec<String>,    // validated against the schema
      pub filters: Vec<Filter>,    // the typed where
      pub as_of: AsOf,             // freshness/time-travel token (M2: just a marker)
  }

  /// Freshness selector. M2 only needs `Latest`; the cache + real tokens are M5.
  #[derive(Debug, Clone, PartialEq)]
  pub enum AsOf { Latest }
  ```
  - 🆕 Concept: separating the **agent input** (`name`, `columns`, `where`, `as_of`) from the **resolved
    spec** (`LoadSpec`, which already knows the hidden `source_object`) keeps invariant #1 visible in the
    types: agent input never contains the binding; the resolved spec does, host-side. (Rust Book: *Using
    Structs to Structure Related Data*.)
  - Note on `as_of`: PRODUCT.md §6/§13 use `as_of` as a freshness / time-travel token (e.g. an Iceberg
    snapshot id) that also keys the cache. M2 has **no cache and no real warehouse**, so `AsOf::Latest` is a
    placeholder marker — the field exists so the signature is right; M5 gives it teeth.
  - ✅ Done when: `cargo build -p droplet-core` compiles with `LoadSpec` + `AsOf`.

- [ ] Write **`resolve_spec`**: turn agent input into a `LoadSpec`, **validating columns against the schema**
  (this is the runtime stand-in for M4's type-check):
  ```rust
  pub fn resolve_spec(
      catalog: &Catalog,
      name: &str,
      columns: &[&str],
      filters: Vec<Filter>,
      as_of: AsOf,
  ) -> Result<LoadSpec, DropletError> {
      let entry = catalog.get(name)?;                 // unknown dataset -> DropletError
      // Validate every requested column exists (and every filter's field too).
      for c in columns {
          if !entry.schema.columns.iter().any(|col| &col.name == c) {
              return Err(DropletError::UnknownColumn {
                  dataset: name.to_string(), column: c.to_string() });
          }
      }
      for f in &filters {
          if !entry.schema.columns.iter().any(|col| col.name == f.field) {
              return Err(DropletError::UnknownColumn {
                  dataset: name.to_string(), column: f.field.clone() });
          }
      }
      Ok(LoadSpec {
          dataset: name.to_string(),
          source_object: entry.source_object.clone(),
          columns: columns.iter().map(|c| c.to_string()).collect(),
          filters,
          as_of,
      })
  }
  ```
  - ⚠️ Invariant #2: the agent can only name **columns** and supply **typed filters**; `resolve_spec` rejects
    anything off-schema *before* any SQL is built. There is no parameter through which a raw query could
    enter. The scope is bounded by construction.
  - 🔗 Maps to: PRODUCT.md §6 — *"The load is bounded and typed … The agent cannot express an out-of-scope
    load."* In M4 these same checks move to type-check time (a wrong column won't even compile in the
    sandbox); `resolve_spec` is the runtime version that proves the rule now.
  - ✅ Done when: a test asserts `resolve_spec(&cat, "usage_daily", &["account_id"], vec![], AsOf::Latest)`
    is `Ok`, and `resolve_spec(&cat, "usage_daily", &["nope"], …)` is
    `Err(DropletError::UnknownColumn { .. })`.

- [ ] Write **`fetch_via_connector`**: ask the connector to produce the dataset's local Parquet and return
  its path. M0's `Source::load` already returns a `PathBuf` to an existing local file — there are **no bytes
  to copy**: the dev connector points at the file, and a prod connector (M6) lands the Parquet itself before
  returning its path. The *scoping* happens in Chunk F as we build the view over that path.
  ```rust
  use std::path::PathBuf;

  /// Ask the connector to produce the dataset's local Parquet and return its path.
  /// This is the ONLY function that touches a source. No bytes are copied here —
  /// `Source::load` already hands back a local path (M0 dev connector: an existing
  /// file; M6 prod connector: one it landed via UNLOAD before returning).
  pub async fn fetch_via_connector(
      source: &dyn Source,
      spec: &LoadSpec,
  ) -> Result<PathBuf, DropletError> {
      // M0's seam takes a LoadRequest, not a path. M2 only needs the dataset name
      // for the dev connector; the scope (columns/where) is applied locally (Chunk F).
      let req = LoadRequest { dataset: spec.dataset.clone() };
      let path = source.load(&req).await?;   // <-- the connector call (returns a local PathBuf)
      Ok(path)
  }
  ```
  - 🆕 Concept: `&dyn Source` + `.load(...).await` is the **only** call in all of M2 that reaches "outside"
    for data. Everything after it operates on the **local** Parquet at the returned path. Mentally underline
    this line — it is the door. (Rust Book: *Fundamentals of Asynchronous Programming* — `.await`; *Using
    Trait Objects*.)
  - 🆕 Concept: M0 said `LoadRequest` (just `{ dataset }`) **grows in M2**. The full scope (`columns`,
    `filters`, `as_of`) lives on the host-side `LoadSpec`; for the dev connector the dataset name is enough,
    because the dev `Source::load` doesn't filter — it just resolves the name to a local file. (When M6 pushes
    the scope into a real `UNLOAD`, that's when `LoadRequest` grows to carry it.)
  - ⚠️ Invariant #2 (the whole point): `source.load(&req)` is the single guarded door. The request is built
    from the **resolved, catalog-derived** spec — never an agent string. Nothing else in M2 calls `Source`.
  - ⚠️ Invariant #1: the connector hides *which* engine it is. Today it's `LocalParquetSource`; swapping in an
    Athena connector (M6) changes nothing above this line — `load` still just calls `Source::load`.
  - verify: M0's `LocalParquetSource::load` checks `path.exists()` inside an `async fn` but does no real
    awaiting (it's "trivially async"). That's fine for the dev connector; when a real (slow) connector lands
    (M6), its `load` genuinely `.await`s the UNLOAD/download — re-check the body against M6's connector.
  - ✅ Done when: a `#[tokio::test]` builds a `Session` (dev stores, dev catalog), resolves a spec, calls
    `fetch_via_connector`, and asserts the returned path points at an existing `*.parquet`. **End of Chunk E —
    `git commit`.**

---

### Chunk F — Materialize: scoped local Parquet → a `Dataset` handle (the M1 engine)

Now we hand the local Parquet to the **M1 analyze engine** and produce a **`Dataset`** handle — but scoped to
just the requested `columns` + `where`. The trick: register a DuckDB **view** over the local file whose
`SELECT` lists exactly `columns` and whose `WHERE` is `render_where(filters)`. Heavy data stays in DuckDB
behind the handle (invariant #6); only later `to_rows`/`scalar` move capped rows into the sandbox.

> **Reuse, don't reinvent.** M1 already gave you: an ephemeral per-session `DuckEngine`, `read_parquet('…')`,
> the `MAX_RESULT_ROWS` cap + `cap_batches`, the `spawn_blocking` wrapper, and a `Dataset` handle type. M2's
> materialize step *composes* those — it builds a scoped `SELECT`, registers it as a named view, and wraps the
> view name in a `Dataset` handle. No new DuckDB machinery; just a new query built from the catalog + filters.

- [ ] Decide the materialization shape. Two equally fine options — **pick the view approach** for M2 (cheapest,
  matches the snapshot story where resume "registers views" per PRODUCT.md §12):
  - **View over the file (chosen):** `CREATE VIEW ds_<n> AS SELECT <cols> FROM read_parquet('<path>') <where>`.
    The scoped slice is logical; DuckDB reads the file lazily. Cheap, and exactly how snapshot/resume
    re-attaches cached parquet later.
  - *(Alternative, deferred):* `CREATE TABLE … AS SELECT …` materializes the scoped rows into DuckDB. Heavier;
    not needed in M2. Note it in a comment and move on.
  - 🆕 Concept: a SQL **view** is a saved query that behaves like a table when referenced, without copying
    data. `CREATE VIEW v AS SELECT …` then `SELECT … FROM v`. (No Book chapter — SQL/DuckDB.)
  - ✅ Done when: you've written one sentence in a comment saying *why* M2 uses a view (lazy + matches resume).

- [ ] Write a **`build_scoped_sql`** that assembles the scoped `SELECT` from a `LoadSpec` + the local path.
  Reuse `render_where` from Chunk B and reuse the **`quote_ident`** column quoting:
  ```rust
  /// Build `SELECT <quoted cols> FROM read_parquet('<path>') <where>`.
  /// Columns are quoted identifiers; the WHERE comes from the typed filters.
  pub fn build_scoped_sql(spec: &LoadSpec, parquet_path: &str) -> String {
      let cols = if spec.columns.is_empty() {
          "*".to_string()
      } else {
          spec.columns.iter().map(|c| quote_ident(c)).collect::<Vec<_>>().join(", ")
      };
      let where_clause = render_where(&spec.filters);
      // read_parquet path is a host-built literal (the resolved binding), not agent input.
      format!("SELECT {cols} FROM read_parquet({}){where_clause}",
              lit(parquet_path))
  }
  ```
  - ⚠️ Invariant #2: every piece of this SQL is **host-built** — column list from the validated spec, `WHERE`
    from escaped typed filters, path from the resolved binding. No substring of it comes raw from the agent.
    That is what makes "no arbitrary SQL against production" structurally true.
  - 🔗 Maps to: PRODUCT.md §6's connector line — in prod this `SELECT` is what the connector wraps in
    `UNLOAD (SELECT …) TO 's3://…'`; in M2 it's the local scoping `SELECT`. **Same scope, different runner.**
  - ✅ Done when: a test asserts `build_scoped_sql` for
    `columns=["account_id"], where=[eq("region","EU")]` produces
    `SELECT "account_id" FROM read_parquet('…') WHERE "region" = 'EU'`.

- [ ] **Reuse M1's view builder — don't add a new one.** M1 already gave you
  `DuckEngine::new_view(&mut self, select_sql: &str) -> Result<Dataset, DropletError>` (M1 Chunk D's private
  helper, the one every primitive calls). It `CREATE VIEW ds_{n}`s the SQL using the engine's own monotonic
  `next_id`, then returns a `Dataset` by value. The scoped `SELECT` you just built is *exactly* a
  `select_sql`, so materialize is one call — no caller-chosen view name, no new method:
  ```rust
  // M2 materialize = M1's new_view over the scoped SELECT. Nothing new to write.
  let scoped_sql = build_scoped_sql(&spec, parquet_path.to_str().unwrap());
  let dataset: Dataset = engine.new_view(&scoped_sql)?;   // &mut self: bumps next_id, names ds_{n}
  ```
  - 🆕 Concept: `new_view` is `&mut self` because it bumps `self.next_id` to name the next `ds_{n}`. That's
    why M2's materialize must reach the engine **mutably** — you can't register a fresh view through a `&self`
    borrow. (Rust Book: *Method Syntax* — `&mut self` for methods that change the receiver.)
  - Note: M1 wrote `new_view` as a **private** `fn`. Since `Session::load` lives in a different module, bump
    its visibility to `pub(crate) fn new_view(...)` so the load path can call it without exposing it outside
    the crate. (Rust Book: *Controlling Visibility with `pub`* — `pub(crate)`.)
  - 🆕 Concept: `new_view` generates the unique name **itself** (from the engine counter), so M2 doesn't pick
    a view name — that keeps naming in one place and means two loads never collide, the same way two M1
    primitives never collide. (No Book chapter — reuse M1's design.)
  - ⚠️ Invariant #6 (boundary discipline): the *data* stays in the view inside DuckDB. The `Dataset` handle
    the sandbox gets is just a table name behind an opaque type — rows only cross later via capped
    `to_rows`/`scalar`. M2 returns a handle, not rows.
  - ✅ Done when: a `#[test]` (feature `duckdb`) calls `engine.new_view(&build_scoped_sql(&spec, path))` over
    the fixture and a follow-up `SELECT count(*) FROM <dataset.table()>` returns the expected scoped count.

- [ ] **`load` hands the `Dataset` straight back — by value.** M1's `new_view` already produced the unique
  name and returned a `Dataset`; M2 doesn't allocate a separate `u64` handle or stash anything in a registry.
  `load` just returns that `Dataset`, exactly as M1's primitives (`filter_rows`, `group_agg`, …) return one:
  ```rust
  // Materialize and return the Dataset by value — M1's by-value handle model.
  let dataset: Dataset = engine.new_view(&scoped_sql)?;
  // … return Ok(dataset) from `load` (Chunk G).
  ```
  - 🆕 Concept: the **`Dataset`** (a tiny value wrapping the DuckDB view name) is what crosses into the
    sandbox; the actual rows stay host-side inside DuckDB. This is the same by-value handle M1's primitives
    return — `load` just produces one more `Dataset`, from a scoped view instead of a filter/group. (Rust
    Book: *Using Structs to Structure Related Data* / M1's `Dataset`.)
  - ⚠️ Invariant #1 + #6: the sandbox receives an **opaque handle** to a *logical, local* dataset — never a
    connection, a path, a view name it could inject, or rows. It cannot tell the data came from a file vs
    Athena.
  - ✅ Done when: a test calls the (still host-side) materialize path end to end and gets back a `Dataset` that
    the M1 analyze primitives can read. **End of Chunk F — `git commit`.**

---

### Chunk G — `load(...)`: the one function, tied together + the milestone test

Assemble the pieces into the single public **`load`** the rest of Droplet (and, via droplet-py/M4, the agent)
calls: resolve → fetch via connector → materialize scoped → return `Dataset` handle. It's async (the
connector + DuckDB both want it), wraps the DuckDB work in `spawn_blocking` per M1, and folds every error into
`DropletError`.

- [ ] Write the public **`load`** on `Session` (or a free function taking `&mut Session`). This is the
  agent-facing signature from PRODUCT.md §6/§10, minus the Python types (those arrive with droplet-py/M4):
  ```rust
  impl Session {
      /// The single guarded door. Resolve a catalog dataset, fetch the local
      /// parquet via its connector, materialize a scoped view, and return a Dataset.
      pub async fn load(
          &mut self,
          name: &str,
          columns: &[&str],
          filters: Vec<Filter>,
          as_of: AsOf,
      ) -> Result<Dataset, DropletError> {       // returns M1's by-value Dataset handle
          // 1. resolve + validate (no source contact yet)
          let spec = resolve_spec(self.catalog(), name, columns, filters, as_of)?;

          // 2. THE DOOR: connector produces the local parquet, hands back its path
          let parquet_path =
              fetch_via_connector(self.source(), &spec).await?;

          // 3. materialize scoped view in the per-session DuckDB (M1 engine),
          //    wrapping the DuckDB work in spawn_blocking like every M1 query.
          let scoped_sql = build_scoped_sql(&spec, parquet_path.to_str().unwrap());
          //    … run engine.new_view(&scoped_sql) inside spawn_blocking (Chunk F),
          //    which returns the Dataset by value.
          let dataset = /* spawn_blocking { engine.new_view(&scoped_sql) } (Chunk F) */;
          Ok(dataset)
      }
  }
  ```
  - 🆕 Concept: this single function *is* invariant #2 made concrete — there is **exactly one** call to a
    `Source` in the whole analyze/load surface, and it lives here, fed only by a catalog-resolved spec. (Rust
    Book: *Error Handling* — the `?` operator threads every failure into `DropletError`.)
  - 🆕 Concept: ordering matters — **validate before you fetch**. `resolve_spec` runs first so an out-of-scope
    request fails *without* touching the connector at all. (No Book chapter — boundary discipline.)
  - ⚠️ Invariant #2: keep `load` the *only* place `Source::load` is reachable on the analyze path. Don't add a
    second caller; don't expose `fetch_via_connector`/`Source` to the sandbox surface.
  - ⚠️ Invariant #9 (DuckDB is synchronous): the materialize step (`engine.new_view(&scoped_sql)`) is a
    blocking DuckDB call, so wrap it in `spawn_blocking` — and release the GIL (`py.detach(...)`) when this is
    reached from Python (M4) — exactly as every M1 query does, so it doesn't freeze the async runtime.
  - Design note (PRODUCT §14 — per-run isolation, *not* a numbered invariant): the fetched parquet path is
    *this session's* dataset, and the view lives in *this session's* ephemeral DuckDB (one run = one Session =
    ephemeral local engine + unique `work_dir`). Nothing is shared across sessions.
  - ✅ Done when: `cargo build -p droplet-core --features duckdb` is green with `load` wired end to end.

- [ ] Note explicitly, in a comment on `load`, **what M2 deliberately does NOT do** — so future-you doesn't
  think it's missing:
  ```rust
  // M2 scope notes (intentional, not TODO-rot):
  //  * NO cache: every load re-fetches + re-materializes. The content-addressed
  //    cache (hash(scoped query + source + freshness token)) arrives in M5; until
  //    then `as_of` is a marker and load is idempotent-by-recompute.
  //  * NO real warehouse: the only connector is the dev LocalParquetSource. Athena
  //    (UNLOAD -> parquet on S3) is M6; this signature does not change when it lands.
  //  * Scope is applied LOCALLY here (the dev connector returns whole-file bytes);
  //    in M6 the connector pushes the same scope into the source's native UNLOAD.
  ```
  - 🔗 Maps to: roadmap README file map — caching is **M5** (`M5-artifact-cache`), Athena is **M6**
    (`M6-connectors-athena`). M2 builds the *door*; later milestones make the door cheap (cache) and real
    (warehouse).
  - ✅ Done when: the comment is present and you can say in one sentence why re-running `load` in M2 re-reads
    the file (no cache yet).

- [ ] **The M2 integration test** (`crates/droplet-core/tests/load_boundary.rs`, gated on the `duckdb`
  feature). Build a `Session` with the dev catalog + a `LocalParquetSource` pointed at `tests/data/`, then `load`
  the scoped slice and assert the **known scoped answer**:
  ```rust
  #![cfg(feature = "duckdb")]
  // 1. Session with dev stores + Catalog::with_dev_dataset(); LocalParquetSource base = tests/data/.
  // 2. let ds = session.load("usage_daily",
  //                          &["account_id", "active_minutes"],
  //                          vec![eq("region", "EU")],
  //                          AsOf::Latest).await?;   // ds: Dataset (by value)
  // 3. Through the M1 analyze path on dataset `ds`, assert:
  //      - row count == 3   (the two acct_1 rows + acct_3; acct_2 is US, excluded)
  //      - SUM(active_minutes) == 250   (120 + 40 + 90)
  //      - the result has only the 2 requested columns (region was NOT selected)
  ```
  - 🆕 Concept: this is a real **integration test** — it drives `Session::load` exactly as an outside caller
    (eventually the agent, via droplet-py) would, and asserts an observable result, not internals. (Rust Book:
    *Writing Automated Tests* → "Integration Tests".)
  - ⚠️ Invariant #2 (proven, not asserted): the test only ever passes a **name + columns + typed filters** —
    there is no API on `Session` it *could* use to send raw SQL to the source. The scope (`region = 'EU'`,
    two columns) was expressed entirely through the helpers.
  - ⚠️ Invariant #1 (proven): the test names `"usage_daily"`, never a file path or `LocalParquetSource`. Swapping
    the connector would leave this test unchanged.
  - ✅ Done when: `cargo test -p droplet-core --features duckdb` passes with the three assertions green.
    **This is the M2 "Done when."** **End of Chunk G — `git commit`.**

- [ ] (Optional, recommended) Add a **negative test** proving the door rejects out-of-scope input *before*
  touching the connector:
  ```rust
  // load("usage_daily", &["ssn"], vec![], AsOf::Latest).await
  //   => Err(DropletError::UnknownColumn { dataset: "usage_daily", column: "ssn" })
  // The connector was never called (resolve_spec failed before fetch_via_connector).
  // Tip: a counting/spy Source whose `load` bumps a counter lets you assert it
  //      stayed at 0 — proving the door never opened.
  ```
  - ⚠️ Invariant #2: an out-of-scope load is rejected at resolution, *before* any source contact — the door
    didn't even open. Asserting the connector's `load` was never reached proves the ordering.
  - ✅ Done when: the negative test is green; an off-schema column errors and the connector was never called.

---

## M2 done checklist

Tick all of these to call M2 complete (PRODUCT.md §6/§9/§10, single-machine):

- [ ] The eight **filter helpers** (`eq`, `gt`, `lt`, `gte`, `lte`, `in_`, `between`, `contains`) exist as
  value-constructors returning a `Filter`, and `render_sql` / `render_where` turn a `&[Filter]` into a
  **quoted, escaped** `WHERE` clause — the only place agent values become SQL (invariant #2). A
  `// TODO(M4): typed/parameterized rendering` note marks the upgrade.
- [ ] A **`Catalog`** maps logical dataset name → `DatasetEntry { source_object (hidden), schema }`; `get`
  errors with `UnknownDataset`. **One local dataset** (`usage_daily`) is declared via `with_dev_dataset`. The
  catalog is host-side on the `Session` and never crosses into the sandbox (invariant #1).
- [ ] **`resolve_spec`** validates requested columns + filter fields against the schema (runtime stand-in for
  M4's type-check), erroring with `UnknownColumn` *before* any source contact.
- [ ] **`fetch_via_connector`** is the **single** call to `Source::load` on the analyze/load path — fed only a
  `LoadRequest` built from the catalog-resolved spec, and returning the connector's local parquet path
  (invariants #1, #2).
- [ ] **Materialize** reuses M1's `DuckEngine::new_view(&mut self, scoped_sql)` (`build_scoped_sql`: requested
  columns + `render_where`) over the local parquet, returning an **opaque `Dataset` handle by value** —
  data stays host-side (invariant #6).
- [ ] **`Session::load(name, columns, where, as_of) -> Dataset`** ties it together: resolve → connector fetch →
  scoped materialize → `Dataset`. It is async, wraps the DuckDB `new_view` in `spawn_blocking` (M1, invariant
  #9), folds every failure into `DropletError` (invariant #10), and carries the explicit "no cache (M5) / no
  warehouse (M6)" scope note.
- [ ] An integration test `load`s the scoped `usage_daily` slice and asserts the known answer (3 rows, sum
  250, only the 2 requested columns); a negative test proves an off-schema column is rejected before the
  connector is touched.

**Spec "Done when": a `load(...)` call resolves a catalog dataset, runs its connector to produce a scoped
local Parquet, materializes it into the analyze engine, and returns a `Dataset` handle — the connector the
only thing that touched the source.** ✅

When green, move on to [`M3-monty-driver.md`](./M3-monty-driver.md) — wire `load` + the M1 analyze
primitives into the **Monty `run_code` loop** (suspend at a tool call, run the host primitive, resume) with
type-check-before-run, for your **first working Droplet**: an agent's Python `load`s a slice, analyzes it
locally, and gets an answer back.
