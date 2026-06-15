# M6 — Read-only SurrealDB field search (SKETCH)

**Milestone goal:** embed a read-only, in-memory **SurrealDB** inside `droplet-core`, build a
schema-derived **vector index** of every field (name + description + an **embedding**), and implement
`search_fields(nl)` — embed the natural-language query, run a **KNN** search, and return a typed
`FieldRef`. The whole index is rebuilt from the registered schema at `Session` setup and **never**
snapshotted.

**Done when (from the spec):** from a registered Pydantic schema, an agent's `run_code` step can call
`search_fields("which column holds the order amount?")` and get back the right field as a typed
`FieldRef` — the semantic-discovery half of the success criterion (find the right column, then
`run_sql` it).

**Prerequisite:** finish `docs/roadmap/M5-pydantic-schema.md` (build-order step 6). By then the Pydantic
models are registered, you have per-field metadata (name + description + table) to index, the typed
tool surface and its `.pyi` stubs exist, and the M4 Monty `run_code` loop
(`docs/roadmap/M4-monty-driver.md`) dispatches external functions — `search_fields` is the next tool
you wire into that surface. (In M4 the `search_fields` dispatch arm returned a *stub* `FieldRef`; this
milestone makes it real.)

**Estimate:** ~10 chunks.

> This is a **SKETCH** file: chunk-level checkboxes with concept notes and invariant callouts, *not*
> the tiny per-line steps of M0/M1. Get the shape right first; expand into tiny steps when you reach
> this milestone.

---

## Read first (5 min) — what is and isn't SurrealDB's job here

> 🧭 **SurrealDB only stores vectors and finds nearest neighbours.** It does **not** turn text into
> vectors. Turning `"order_total — gross amount of the order in cents"` into a `Vec<f32>` is **100% your
> app's job** — a small local model or an embedding API. This is the single most common beginner
> misconception in this milestone, so flag it loudly to yourself now: *embeddings are an APP concern,
> not a database feature.*
>
> 🆕 **Concept: a vector / embedding is just a `Vec<f32>`** — a list of numbers that represents a piece
> of text's meaning. "Semantic search" = represent each field's `name + description` as one of these
> vectors, represent the user's question as another, and return the field whose vector is **closest**.
> "Closest" is measured by a distance metric (we use **COSINE**). (No Rust Book chapter —
> project-specific.)
>
> 🧭 **How M6 fits the async story.** Every SurrealDB call is **async** (`.await`) and runs on the Tokio
> runtime `droplet-core` already owns. DuckDB (M1) was the opposite — sync, wrapped in
> `spawn_blocking`. Surreal is async: `.await` its calls; do **not** `spawn_blocking` a Surreal call.
>
> ⚠️ Invariant #1: every line in this milestone lives in `droplet-core` and **must not import `pyo3`**.
> The whole field-search path has to be exercisable from a pure-Rust `#[tokio::test]`, with no CPython
> and no wheel in the loop.

---

### Chunk 0 — Decide the embedding source first (everything depends on it)

- [ ] Choose **how text becomes a `Vec<f32>`** before writing any database code: a small **local model**
  (e.g. the `fastembed` crate with `bge-small` / `all-MiniLM-L6-v2`, dimension **384**) **or** an
  **embedding API** (e.g. OpenAI `text-embedding-3-small`, dimension **1536**). Write down the chosen
  model and its **output dimension** — the vector-index DDL and every stored vector must match it
  exactly.
  - 🆕 Concept: an **embedding model** maps text → a fixed-length numeric vector. Its **dimension**
    (vector length) is a property of the model; two models with different dimensions are not
    interchangeable. (No Rust Book chapter — project-specific.)
  - ⚠️ Invariant #5 (SurrealDB is read-only and schema-derived): this step is **not** SurrealDB.
    SurrealDB never generates embeddings — it only stores and searches the vectors you hand it. Re-read
    the 🧭 note above if this feels off.
  - 🔗 Maps to: the `embedding` you compute for each `FieldDoc` and for the `search_fields(nl)` query.
  - ✅ Done when: you have a one-line `fn embed(text: &str) -> Vec<f32>` (even a stub for now) whose
    output length is a known constant `EMBED_DIM` you can hard-code into the DDL.
  - verify: your chosen model's exact output dimension against its own docs/source before writing the
    `DIMENSION` keyword — `384` for bge-small / all-MiniLM-L6-v2, `1536` for OpenAI
    text-embedding-3-small. A wrong `DIMENSION` makes every insert/query error.

### Chunk 1 — Add the `surrealdb` dependency (embedded, in-memory)

- [ ] Add `surrealdb = { version = "3.1.4", features = ["kv-mem"] }` to `[workspace.dependencies]` in
  the root `Cargo.toml`, then opt `droplet-core` in with `surrealdb.workspace = true`. `kv-mem` is the
  **in-process, in-memory** engine — no separate `surreal` server, no port, no network. (`tokio` and
  `serde` are already workspace deps from earlier milestones; Surreal needs both.)
  - 🆕 Concept: with the `kv-mem` feature, `Surreal::new::<Mem>(())` runs the **whole database inside
    your Rust process in RAM**. This is exactly what Droplet needs: a schema-derived index rebuilt per
    `Session` and thrown away. (Rust Book: *More About Cargo and Crates.io*, ch. 14 — feature flags on a
    dependency.)
  - ⚠️ Expect a **slow first build** — `surrealdb` is a heavy dependency (it pulls in its own
    storage/parsing machinery even with only `kv-mem`). That's normal, not a misconfig. Keep
    `Cargo.lock` committed.
  - ✅ Done when: `cargo build -p droplet-core` is green with `surrealdb` added, and `grep surrealdb
    Cargo.lock` shows `3.1.4`.

### Chunk 2 — Smoke-test the embedded engine in isolation

- [ ] Write a throwaway `#[tokio::test]` that starts the engine and selects a namespace + database:
  ```rust
  use surrealdb::engine::local::Mem;   // the in-memory engine type (feature = "kv-mem")
  use surrealdb::Surreal;
  let db = Surreal::new::<Mem>(()).await?;
  db.use_ns("droplet").use_db("fields").await?;
  ```
  - 🆕 Concept: a SurrealDB connection lives inside a **namespace** and a **database** (`use_ns` /
    `use_db`) — two levels of grouping you select before any query runs. (No Rust Book chapter —
    SurrealDB-specific.)
  - ⚠️ Import-path trap: the canonical type for 3.1.4 is `surrealdb::engine::local::Mem`. Older
    snippets show `surrealdb::engine::mem::Mem` — that path does **not** exist in 3.1.4. (The dynamic
    `surrealdb::engine::any::connect("memory")` also works, but prefer the typed
    `Surreal::new::<Mem>(())` for a fixed in-process engine.)
  - verify: the exact handle type `Surreal::new::<Mem>(())` returns at 3.1.4 (the local engine alias —
    `Surreal<Db>` via `surrealdb::engine::local::Db`) before you name it in a function signature; read
    `docs.rs/surrealdb/3.1.4/surrealdb/engine/local/`.
  - ✅ Done when: the test connects, selects ns/db, and returns `Ok(())` with no server running and no
    panic.

### Chunk 3 — Define the schema-derived vector index (DDL, run once)

- [ ] Build the field table + index **once** with a single `db.query(...)`, using your real `EMBED_DIM`
  from Chunk 0:
  ```surql
  DEFINE TABLE field;
  DEFINE FIELD name        ON field TYPE string;
  DEFINE FIELD description ON field TYPE string;
  DEFINE FIELD table       ON field TYPE string;
  DEFINE FIELD embedding   ON field TYPE array<float>;
  DEFINE INDEX field_vec ON field FIELDS embedding HNSW DIMENSION 384 DIST COSINE;
  ```
  - 🆕 Concept: **DDL** (Data Definition Language). `DEFINE TABLE` / `DEFINE FIELD` / `DEFINE INDEX`
    are SurrealQL statements you send as strings via `db.query("...")`. You build the index once at
    setup, then only **read** from it. (No Rust Book chapter — SurrealDB-specific.)
  - ⚠️ **MTREE is gone in 3.x.** MTREE was deprecated in 2.x and **removed in 3.0** (auto-converted to
    HNSW on migration). In `surrealdb = "3.1.4"` the vector-index choices are **HNSW** (in-memory ANN
    graph — correct for Droplet's small in-RAM field index) and **DISKANN** (3.1+, for larger-than-RAM
    corpora). Do **not** write MTREE DDL; it fails.
  - ⚠️ Keyword exactness: the distance keyword is **`DIST`** (not `DISTANCE`); HNSW metrics are
    `EUCLIDEAN`, `COSINE`, `MANHATTAN`. `DIMENSION` must **exactly equal** your model's output length
    and the length of every inserted vector.
  - verify: the full set of optional HNSW params in 3.1.4 (`EFC`, `M`, `M0`, `TYPE`) against
    `docs.rs/surrealdb/3.1.4` before relying on them — `M0` appears in one example but not every
    reference. Treat **`DIMENSION` + `DIST`** as the only must-haves.
  - ✅ Done when: the `DEFINE` batch runs without error against the in-memory DB.

### Chunk 4 — Define the `FieldDoc` record and build one per field

- [ ] Define a `#[derive(Serialize)]` row struct, one record per schema field, with an **app-computed**
  embedding:
  ```rust
  #[derive(serde::Serialize)]
  struct FieldDoc {
      name: String,        // e.g. "order_total"
      description: String, // e.g. "Gross amount of the order in cents"
      table: String,       // which source table the field belongs to
      embedding: Vec<f32>, // length == EMBED_DIM; YOU compute this, not Surreal
  }
  ```
  Then, for each field in the registered (M5) Pydantic-derived schema, embed `name + " " + description`
  with your `embed(...)` fn and push a `FieldDoc`.
  - ⚠️ Invariant #5 (read-only, schema-derived): every `FieldDoc` comes **from the registered
    schema** — names, descriptions, and tables are derived from the Pydantic models, not from user data
    or DB writes. Nothing about this index is user-authored.
  - 🆕 Concept: `#[derive(Serialize)]` auto-generates the struct → DB-value conversion, the same way it
    does for JSON; SurrealDB consumes the serde value. (Rust Book: *Generic Types, Traits, and
    Lifetimes*, ch. 10 — derive macros implement traits for you.)
  - ✅ Done when: a Rust test builds a `Vec<FieldDoc>` from a tiny mock schema and every
    `embedding.len() == EMBED_DIM`.

### Chunk 5 — Bulk-insert the field records (the one and only write)

- [ ] Insert all `FieldDoc`s in a single bound statement:
  `db.query("INSERT INTO field $docs").bind(("docs", docs)).await?;`. After this line, the handle is
  treated **read-only** — no other code path issues a write.
  - 🆕 Concept: **parameter binding** (`.bind(("docs", docs))`) hands serde values to SurrealQL as the
    `$docs` variable, instead of string-concatenating data into the query. Safer and avoids escaping.
    (No Rust Book chapter — SurrealDB-specific.)
  - ⚠️ Invariant #5 (read-only): SurrealDB has **no read-only handle flag** — read-only is a
    **discipline you enforce in `droplet-core`**, not a constructor option. This `INSERT` is the *only*
    write; the build steps (Chunks 2–5) run once at `Catalog`/`Session` setup, and nothing writes after.
  - ⚠️ Invariant #9 (per-run isolation): the index belongs to **one** `Session` — built from that
    session's registered schema, thrown away on close. Don't share one Surreal handle across sessions.
  - ✅ Done when: a test inserts N `FieldDoc`s and a follow-up `SELECT count() FROM field GROUP ALL`
    returns N.

### Chunk 6 — Run a KNN search: the `<|K|>` operator

- [ ] Embed a natural-language query into a `Vec<f32>`, then run a KNN search comparing the stored
  `embedding` field to your query vector, ordering by the computed distance:
  ```rust
  let q: Vec<f32> = embed("which column holds the order amount?");
  let mut res = db.query(
      "SELECT name, table, vector::distance::knn() AS distance
       FROM field
       WHERE embedding <|3,COSINE|> $q
       ORDER BY distance",
  ).bind(("q", q)).await?;
  ```
  - 🆕 Concept: **KNN** = K-Nearest-Neighbours ("give me the K most similar rows"). The `<|...|>` thing
    is SurrealDB's KNN operator, used in a `WHERE` clause comparing the vector field to your query
    vector. (No Rust Book chapter — SurrealDB-specific.)
  - ⚠️ `vector::distance::knn()` in the `SELECT` list returns the distance the KNN operator **already
    computed** for that row — it does **not** recompute, and only works in a `SELECT` whose `WHERE` used
    the `<|...|>` operator. Pair it with `ORDER BY distance` for nearest-first.
  - ✅ Done when: a test inserts a handful of fields and the query returns the expected field first for
    an obvious query (e.g. "order amount" → `order_total`).

### Chunk 7 — Pick the right KNN operator form (exact vs approximate)

- [ ] Decide which of the three KNN forms `search_fields` uses, and write it down:
  - `embedding <|3|> $q` — uses the index's own configured distance.
  - `embedding <|3,40|> $q` (numeric 2nd arg) — **approximate** HNSW search with effort `ef=40`.
  - `embedding <|3,COSINE|> $q` (metric keyword 2nd arg) — **brute-force exact** search with that
    metric, **bypassing** the index.
  For Droplet's tiny per-session field index (tens/hundreds of rows), default to **brute-force exact
  (`<|K,COSINE|>`)** — no approximate-recall surprises. Switch to `<|K,EF|>` only if the field count
  ever grows large.
  - 🆕 Concept: "approximate" KNN trades a little accuracy for speed at large scale; "exact"
    (brute-force) always returns the true nearest neighbours but scans every row. For a small index,
    exact is both correct and fast. (No Rust Book chapter — SurrealDB-specific.)
  - 🔗 Maps to: `search_fields` correctness — a discovery tool wants the *truly* closest field, so exact
    is the safe v1 default.
  - ✅ Done when: a test confirms swapping `<|3,COSINE|>` for `<|3|>` returns the same top hit on your
    mock schema, and you've committed to `<|K,COSINE|>` as the v1 form with a comment explaining why.

### Chunk 8 — Return a typed `FieldRef` (not a raw `Value`)

- [ ] Pull the rows out of the result and map them onto the typed `FieldRef` your tool surface returns:
  `let hits: surrealdb::Value = res.take(0)?;` then deserialize/convert into `Vec<FieldRef>` (or one
  `FieldRef` for the top hit). Keep `K` small — this is a discovery tool, not a data tool.
  - 🆕 Concept: `res.take(0)` pulls the rows of the **first statement** (by position, `0` = first), not
    by table name. You then turn the dynamic `Value` into your strongly-typed `FieldRef`. (No Rust Book
    chapter — SurrealDB-specific.)
  - ⚠️ Invariant #4 (boundary discipline): `search_fields` returns a **small typed handle/result**
    (`FieldRef` — a field name + table), never rows of user data. It's a discovery tool; the heavy data
    lives in DuckDB behind handles. Keep the return tiny so snapshots stay small.
  - 🔗 Maps to: the `FieldRef` type the M5 stubs declare as `search_fields`'s return type — this is
    where the host actually produces it (M4 only stubbed it).
  - verify: the exact way to turn the `surrealdb::Value` (or `res.take::<Vec<FieldRef>>(0)?`) into your
    `FieldRef` at 3.1.4 — whether you can `take` straight into a `#[derive(Deserialize)]` struct or must
    go via `Value`. The 3.x deserialize API differs from 2.x snippets; read
    `docs.rs/surrealdb/3.1.4` for `Response::take`.
  - verify: the exact shape `FieldRef` must match the M5-generated stub signature (field name? table?
    confidence/distance?). Make the Rust return type and the `.pyi` agree, or the M4
    type-check-before-run loop will reject correct calls.
  - ✅ Done when: a test asserts `search_fields(...)` returns a `FieldRef` whose `name`/`table` match
    the expected field.

### Chunk 9 — Wire `search_fields` into the Monty tool surface + fold errors

- [ ] Replace the stub `search_fields` arm in the M4 `run_code` dispatch loop with the real host call:
  in the `match call.function_name` arm for `"search_fields"`, embed the arg, run the KNN query on the
  host Tokio runtime, build the `FieldRef`, and `call.resume(...)` it back to the sandbox.
  - ⚠️ Invariant #7 (flat typed functions): Monty has no class/module namespacing, so it is a bare
    `search_fields(...)` in the sandbox namespace — never `surreal.search(...)`. It lives in the same
    flat external-fn dispatch table as `run_sql` / `describe_schema` / `list_tables` / `export`.
  - ⚠️ Invariant #6 / async boundary: run the Surreal KNN query by **`.await`ing** it on the host
    runtime. Don't `spawn_blocking` it (that's DuckDB's pattern, M1). This is the async side of the host.
  - [ ] Fold `surrealdb::Error` into `DropletError` with a `thiserror` `#[from]` variant so every `?` on
    a Surreal call converts cleanly at the boundary.
    - ⚠️ Invariant #10 (one boundary error type): `thiserror` in libraries, `anyhow` at binaries;
      all engine errors fold into `DropletError`. Keep `surrealdb::Error` from leaking past the host.
    - 🆕 Concept: `thiserror`'s `#[from]` generates a `From<surrealdb::Error>` impl, which is what lets
      `?` auto-convert the error. (Rust Book: *Error Handling*, ch. 9.)
  - ✅ Done when: a Monty script that calls `search_fields("order amount")` end-to-end through the M4
    `run_code` start/resume loop gets back a `FieldRef`, and a deliberately bad query surfaces as a
    `DropletError`, not a raw `surrealdb::Error`.

### Chunk 10 — Enforce rebuild-not-snapshotted (the distributed invariant)

- [ ] Make index construction a single **`build_field_index(schema) -> Surreal<Db>`** path that runs at
  `Session`/`Catalog` setup (Chunks 2–5), and ensure the Surreal handle is **never** written to the
  snapshot manifest. On resume (M7, `docs/roadmap/M7-snapshot-store.md`), the field index is **rebuilt
  from the schema ref in the manifest**, not restored — on whatever pod picks the run up.
  - ⚠️ Invariant #5 (read-only, schema-derived, rebuilt not snapshotted): "Read-only Surreal is
    schema-derived and rebuilt, never snapshotted." Because the whole index is a pure function of the
    registered schema, there is nothing to serialize — re-running `build_field_index` on **any pod**
    reproduces it exactly.
  - ⚠️ Invariant #3 (snapshot = REPL bytes + manifest only): the content-addressed snapshot blob never
    serializes the Surreal engine heap, just like it never serializes DuckDB's. The manifest records the
    *schema ref*; the index is reconstructed from it.
  - ⚠️ Invariant #8 (distributed by default): the snapshot lives in the **shared, content-addressed**
    store, and a different stateless pod resumes the run — so the field index *must* be cheaply
    rebuildable from the manifest, with no pod-local state. This is why "rebuild, never snapshot" is
    non-negotiable, not just an optimization.
  - 🔗 Maps to: M7's resume path rebuilds DuckDB **and** the Surreal field index from the manifest's
    schema ref on a different pod — this chunk is the Surreal half of "rebuild engines on resume."
  - ✅ Done when: dropping the Surreal handle and calling `build_field_index(schema)` again yields an
    index that answers `search_fields` identically — proving resume needs no snapshot of it.

---

## Notes carried forward (don't act yet)

- **Embeddings stay an APP concern.** If you started with a stub `embed(...)`, swapping in a real local
  model (`fastembed`) or an API is isolated to that one function — the DB code is unchanged as long as
  `EMBED_DIM` matches. SurrealDB is never in the embedding business.
- **Read-only is a discipline, not a flag.** There is no constructor option that makes the embedded
  Surreal read-only. Invariant #5 is enforced by *your code never writing after the one-time build* —
  guard it (e.g. by only exposing query helpers, not insert helpers, past setup).
- **Rebuilt, never snapshotted — because Droplet is distributed.** The index is a pure function of the
  registered schema, so M7 stores only the schema ref in the content-addressed manifest; any pod
  rebuilds the index from it (Invariants #3, #5, #8). Never put the Surreal handle in the snapshot.
- **DISKANN is the escape hatch, not the default.** If the field corpus ever outgrows RAM, 3.1 added
  DISKANN (and `INNER_PRODUCT` / `COSINE_NORMALIZED` metrics). For v1's small schema-derived index,
  HNSW + brute-force exact KNN is correct and simpler — don't reach for DISKANN prematurely.
- **Verify against 3.1.4 when you pin.** The surrealdb 3.x public API jumped from 2.x; most online
  snippets are 2.x and will mislead you (especially the `engine::local::Mem` path, the `DIST` keyword,
  and the `Response::take` → typed-struct deserialize). Read the 3.1.4 docs/source for exact signatures
  rather than trusting 2.x snippets.

---

> 📌 When you reach this milestone, expand each chunk into tiny steps the way M0/M1 are written.
