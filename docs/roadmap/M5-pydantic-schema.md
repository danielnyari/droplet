# M5 — Pydantic schema → types + stubs (SKETCH)

**Milestone goal:** Make ONE Pydantic schema the single source of truth. In `python/droplet/`,
introspect the registered Pydantic v2 models, map each field to a DuckDB column type, and emit BOTH the
typed tool signatures and the `.pyi` type stubs that are the model-facing contract — then hand that
schema + stub text across PyO3 into the Rust core so M4's *type-check-before-run* loop has something real
to check against.

**Spec's "Done when":** `Catalog.register(*models, sources=...)` turns Pydantic models into DuckDB types,
typed tool signatures, and type stubs; the generated stubs flow into the Monty type-check-before-run loop
so a wrong column/type fails *before* execution.

**Prerequisite:** finish [`M4-monty-driver.md`](./M4-monty-driver.md). By now the Monty driver loop
exists: `monty_type_checking::type_check(&src, Some(&stubs))` runs before `feed_start`, the
external-function tool surface (`run_sql` / `search_fields` / `describe_schema` / `list_tables` /
`export`) is dispatched on the host via the `ReplProgress::FunctionCall` arm, and a type error already
triggers a retry. In M4 you fed the checker a *hand-written* stub string. M5 *generates* that stub from
the Pydantic models so it can never drift from the real schema.

**Estimate:** ~9 chunks.

> This is a **SKETCH** file: chunk-level checkboxes with concept notes and invariant callouts, *not* the
> tiny per-line steps of M0/M1. Get the shape right first; expand into tiny steps when you reach this
> milestone.

---

## Where the work lives, and the one line you must not cross

This milestone straddles the two language layers, so be deliberate about which side each task lives on:

- The **introspection + mapping + stub-generation** code is **Python**, in `python/droplet/` (the SDK
  package). Pydantic is a Python library; it has no place in Rust. This is `Catalog.register(...)`'s job.
- The **type-check** that consumes the stubs is **Rust**, in `droplet-core`, via the
  `monty-type-checking` crate you wired in M4. So the schema + stub text must cross the PyO3 boundary (in
  `droplet-py`) from Python → Rust as plain owned data (strings / simple structs), never as live Pydantic
  objects.

⚠️ **Invariant #2** (framework-agnostic core; pydantic is the SDK-layer schema DSL): `pydantic` lives in
`python/droplet/`, **NOT** in the Rust core. `droplet-core` depends on the `monty` crate + engines, never
on pydantic and never on an agent framework. If you find yourself reaching for a Rust "pydantic-like"
crate in core, stop: the schema is introspected in Python and crosses the boundary as already-generated
types + stub text.

⚠️ **Invariant #7** (flat typed functions; type-check before execution): Respect Monty's limits — the tool
surface is **flat typed functions**, no classes, no module namespacing, and the type check runs **before**
execution. Every stub you emit is a top-level `def name(...) -> ...: ...`. If you catch yourself writing
`class Catalog:` or `db.run_sql` in a stub, that is the invariant breaking.

🆕 **Concept: a `.pyi` stub is a signature-only Python file.** It lists function names, argument types,
return types, and docstrings — but no bodies (each body is just `...`). It is the same idea as a C header
or a TypeScript `.d.ts`: it describes the *shape* of an API so a type checker can verify calls without
running anything. (Concept is project-side; no Rust Book chapter.)

🆕 **Concept: Monty bundles Astral's `ty` checker — you do *not* depend on `ty` separately.** The Rust
call `monty_type_checking::type_check(&src, Some(&stubs))` runs `ty` *inside* Monty against the stub
`SourceFile` you pass. Your job in M5 is to *generate honest stub text*, not to install or invoke a type
checker. (Concept is project-side; no Rust Book chapter.)

> ⚠️ **`ty` is pre-1.0 BETA.** As of writing, the bundled checker is Astral's `ty` (PyPI `ty` ≈ 0.0.49,
> a `0.0.x` beta) wired in through Monty's pinned Ruff fork. Diagnostic wording, flags, and exit
> behavior can change between releases — pin the Monty tag fleet-wide (M4 pinned `v0.0.18`) and don't
> rely on exact diagnostic strings.

---

### Chunk 1 — Introspect a Pydantic v2 model (Python side)

- [ ] In `python/droplet/`, write a tiny function that takes a `BaseModel` subclass and iterates
  `Model.model_fields.items()` → `(name, FieldInfo)`. For each field read `field_info.annotation` (the
  Python type object) and `field_info.is_required()` (nullability). Print them to see the shape.
  - 🆕 Concept: **Pydantic v2 introspection.** `model_fields` is the v2 way; the v1 `__fields__` /
    `.outer_type_` API is **gone** — ignore v1 tutorials. `FieldInfo.annotation` gives the *type object*
    (e.g. `<class 'int'>`); `is_required()` tells you whether the field may be absent/null. (Concept is
    project-side; no Rust Book chapter.)
  - ⚠️ Pin pydantic in the SDK package: `pip install "pydantic>=2.13,<3"` (v2 requires Python ≥ 3.9).
    The introspection API differs across v1/v2 — make sure you are on v2.
  - ✅ Done when: running it on a 3-field model prints each field name, its annotation, and
    required/nullable.

- [ ] Handle the optional case as its OWN step: a `str | None` / `Optional[str]` field surfaces as a
  union you must unwrap to get the inner scalar type. Add a helper that, given an annotation, returns
  `(inner_type, nullable)`.
  - 🆕 Concept: an **`Optional[X]` / `X | None`** field is a *union* of `X` and `NoneType`. To map it to
    a DuckDB column you want the inner `X` plus a "nullable" flag — so you unwrap the union first.
    (Concept is project-side; no Rust Book chapter.)
  - verify: the exact unwrap on Python 3.10+ (`typing.get_args` / `typing.get_origin` /
    `types.UnionType`) against your target Python — `X | None` (PEP 604) and `Optional[X]` can surface as
    *different* objects, so test both forms.
  - ✅ Done when: the helper returns `(str, True)` for `str | None` and `(int, False)` for a plain `int`.

- [ ] For anything you can't map to a scalar (nested models, `list[...]`, `dict[...]`), fall back to
  `Model.model_json_schema()` and note it. Leave a `TODO` for complex types rather than guessing — v1
  only needs the flat scalar path to work end to end.
  - 🆕 Concept: `model_json_schema()` returns the model's full JSON-Schema description — a richer fallback
    when a field isn't a simple scalar. (Concept is project-side; no Rust Book chapter.)
  - ✅ Done when: a nested-model field hits the fallback and logs a `TODO` instead of crashing.

### Chunk 2 — Map Python/pydantic types → DuckDB column types

- [ ] Write a single mapping dict and a `to_duckdb_type(annotation) -> str` function. Start from the
  digest's safe set: `{int: "BIGINT", float: "DOUBLE", str: "VARCHAR", bool: "BOOLEAN"}`. Unknown →
  `VARCHAR` **plus a `TODO` log** so it fails *visibly*, not silently.
  - 🆕 Concept: **this map is the contract between two outputs.** The same field type drives the DuckDB
    `CREATE`/view column type *and* the stub's Python type. One source (the Pydantic field), two outputs.
    (Concept is project-side; no Rust Book chapter.)
  - ✅ Done when: `to_duckdb_type` returns the right string for each of `int/float/str/bool` and logs a
    `TODO` for an unmapped type.

- [ ] Extend the map for the common extras you'll actually hit: `datetime.datetime → TIMESTAMP`,
  `datetime.date → DATE`, `bytes → BLOB`, `decimal.Decimal → DECIMAL`.
  - verify: DuckDB **1.5.3**'s exact spelling for these (the `duckdb` crate `1.10503` bundles upstream
    DuckDB 1.5.3) — especially `DECIMAL` precision/scale — against the DuckDB type reference before
    relying on it. Bare `DECIMAL` defaults to a fixed precision/scale you may not want.
  - ✅ Done when: each extra type maps to a verified DuckDB type string.

- [ ] Write a tiny test that maps a model with one field of each supported type and asserts the DuckDB
  type string for each. This is the column-type half of "one schema, many outputs".
  - ✅ Done when: the test asserts the right DuckDB type string for every supported Python type.

### Chunk 3 — Generate the typed tool SIGNATURES

- [ ] Decide the *shape* of the flat tool surface from **PRODUCT.md §7**: `run_sql(sql)`,
  `search_fields(nl)`, `describe_schema()` / `list_tables()`, `export(query, dest)`. These names are
  fixed; M5 fills in their *types* from the schema. Write them down as a **single flat list in one place**
  (you'll reuse it twice: once for stubs, once for the host dispatch list — see Chunk 9).
  - ⚠️ **Invariant #7** (flat typed functions): Monty has no class/module namespacing, so it is bare
    `run_sql(...)` in the sandbox — never `schema.run_sql`. The supported Python subset also has no
    third-party imports and no classes, so the surface MUST be free functions.
  - ✅ Done when: there is one canonical list/constant of the five flat tool names that both the stub
    generator and the dispatch check read from.

- [ ] Define the typed result / `FieldRef` shapes the tools return as its OWN step. `search_fields(nl)`
  returns a typed `FieldRef` (PRODUCT.md §7) — give it fields like `name: str`, `table: str` (and
  optionally `dtype: str`). `run_sql` returns capped rows (e.g. `list[dict]`). Express these as small
  Python type names the stub can import or define inline.
  - 🆕 Concept: **`FieldRef`** is the small typed handle `search_fields` returns — a field name + its
    table, NOT rows of data. It is the discovery result the agent then feeds into `run_sql`. (Concept is
    project-side; no Rust Book chapter.)
  - verify: the exact element type each row-returning tool yields against your M2/M4 host code (a `dict`,
    a typed row, or a `Table`/handle) so the stub's return type is honest. The M6 `FieldRef` Rust return
    type MUST match the `FieldRef` you declare here, or M6's `search_fields` calls fail the type check.
  - ✅ Done when: `FieldRef` and the `run_sql` row type are written down as named Python types the stub
    generator can emit.

- [ ] Make the row cap visible in the signature where rows cross the boundary: e.g.
  `run_sql(sql: str, *, limit: int = ...) -> list[dict]`. The cap is not decoration — it is boundary
  discipline.
  - ⚠️ **Invariant #4** (boundary discipline): only result-returning tools move *capped* rows into the
    sandbox; `describe_schema` / `list_tables` move handles/metadata, not bulk rows. Engine objects live
    behind handles; keep bulk data out of these signatures so snapshots stay small.
  - ✅ Done when: every row-returning tool's stub signature shows a `limit` cap; discovery tools return
    metadata/handles only.

### Chunk 4 — Generate the `.pyi` type stubs (the model-facing contract)

- [ ] Write `generate_stubs(models, sources) -> str` that emits one `.pyi` text: a module docstring, the
  `FieldRef` type, and every flat tool as `def name(...) -> ...: ...` (body is literally `...`). This is
  the *single artifact the LLM is type-checked against*.
  - 🆕 Concept: you **write this generator yourself** — iterate `model_fields`, reuse Chunk 2's mapping,
    and emit `def`-lines as text. There is no official "pydantic → .pyi" tool for this direction
    (`datamodel-code-generator` solves the reverse). (Concept is project-side; no Rust Book chapter.)
  - ✅ Done when: `generate_stubs(...)` returns a non-empty string containing every flat tool `def`.

- [ ] Add a one-line docstring to every stubbed function (`"""Run DuckDB SQL; returns capped rows."""`).
  Short, literal, no marketing — the model leans on these.
  - ✅ Done when: each generated `def` carries a single-line docstring.

- [ ] Emit a per-table view of the registered schema *inside* the stub (or a companion describe-string)
  so the checker — and the model — know the real column names/types. This is what makes "a wrong column
  name is caught before execution" possible: the names in the stub come from `model_fields`, so a
  hallucinated column has nothing to bind to.
  - 🔗 **Maps to:** the v1 Success Criterion — *a wrong column name caught by the type checker before
    execution*.
  - ✅ Done when: the generated stub (or companion describe-string) lists the real per-table column names
    and their types, sourced from `model_fields`.

- [ ] Round-trip test on the Python side: generate the stub for a sample model and assert (a) it parses
  as valid Python via `ast.parse`, and (b) it contains exactly the expected flat `def` names.
  - 🆕 Concept: `ast.parse(stub_text)` compiles the stub to a syntax tree without running it — a cheap
    "is this valid Python?" check. (Concept is project-side; no Rust Book chapter.)
  - ✅ Done when: `ast.parse(stub_text)` succeeds and the set of top-level `def` names equals your fixed
    tool list from Chunk 3.

### Chunk 5 — Wire `Catalog.register(*models, sources=...)`

- [ ] Implement `Catalog.register(*models, sources=...)` so one call produces the three outputs from one
  schema: (1) the DuckDB column types per table, (2) the typed tool signatures, (3) the `.pyi` stub text.
  Return them in a small **plain object** (a dataclass / dict of strings) — owned data, ready to cross the
  boundary.
  - 🔗 **Maps to:** the central abstraction in PRODUCT.md §4 — *Pydantic models + S3 sources → DuckDB
    types, the read-only field-search index, typed tool signatures, type stubs*. (The field-search index
    is **M6**; M5 produces the schema + stubs it will consume.)
  - ⚠️ **Invariant #2** (pydantic = SDK-layer DSL, not a core dep): all of this is Python
    (`python/droplet/`). Nothing here imports or touches Rust; the result is just strings + simple
    structs.
  - ✅ Done when: one `Catalog.register(MyModel, sources=...)` call returns an object holding the per-table
    DuckDB types, the signatures, and the stub text — all as plain Python data.

### Chunk 6 — Hand the schema + stubs across PyO3 into the core

- [ ] In `droplet-py` (the PyO3 `cdylib`), add a thin `#[pyfunction]` that accepts the generated stub
  text + the per-table DuckDB column types as plain `String` / simple owned data and passes them into a
  `droplet-core` API. Keep this layer paper-thin — translate at the boundary, do no logic here.
  - ⚠️ **Invariant #1** (core never imports pyo3): the `#[pyfunction]` and `wrap_pyfunction!` live **ONLY**
    in `droplet-py`. `droplet-core` receives plain Rust types and never sees a Python object. Do NOT add
    `pyo3` to `droplet-core`.
  - 🆕 Concept: a `#[pyfunction]` is the PyO3 attribute that exposes a Rust function to Python; you
    register it on the module with `m.add_function(wrap_pyfunction!(name, m)?)?`. (Concept is project-side;
    PyO3 is covered by its own guide, not the Rust Book.)
  - verify: the exact module-arg type on `pyo3 = "0.29"` is `&Bound<'_, PyModule>` (the current `Bound`
    API). Older tutorials show `&PyModule` / `Py<...>` (the pre-0.21 "GIL Refs" API) — do not copy those.
  - ✅ Done when: from Python you can call the new `#[pyfunction]` with the generated stub text + column
    types and it returns `Ok`.

- [ ] In `droplet-core`, store the incoming stub text + schema on the `Session` (or a `Catalog` struct it
  owns) as owned `String`s, so the Monty driver can reach them at type-check time. No pydantic types
  cross — only the already-generated stub text and a simple `Vec<(table, column, duckdb_type)>`.
  - 🆕 Concept: **the seam carries text, not objects.** The whole point of generating stubs in Python is
    that the Rust side only ever handles inert strings — no pydantic, no PyO3-in-core. (Rust Book:
    *Managing Growing Projects with Packages, Crates, and Modules*, ch. 7 — for where this lives in
    `droplet-core`.)
  - ⚠️ **Invariant #2** (framework-agnostic core): the stored data is plain owned Rust — `droplet-core`
    must compile and test as pure Rust with no pyo3 and no pydantic in its dependency tree.
  - ✅ Done when: a `droplet-core`-only test constructs a `Session`/`Catalog`, sets the stub text + column
    list, and reads them back — with `droplet-core` building as pure Rust (`cargo build -p droplet-core`).

### Chunk 7 — Feed the generated stubs into type-check-before-run

- [ ] In `droplet-core`, change M4's type-check call so the stub `SourceFile` is built from the
  *Session-stored generated stub text*, not a hard-coded string:
  ```rust
  let src   = SourceFile::new("session.py", agent_code);
  let stubs = SourceFile::new("stubs.pyi", session.stub_text()); // generated in M5, not hand-written
  match monty_type_checking::type_check(&src, Some(&stubs)) {
      Ok(None)        => { /* clean → proceed to feed_start */ }
      Ok(Some(diags)) => { /* fold into DropletError → model retry */ }
      Err(internal)   => { /* internal checker error → distinct DropletError */ }
  }
  ```
  Now the contract the checker enforces is the *real* schema.
  - ⚠️ **Invariant #7** (type-check **before** execution): this call still runs ahead of `feed_start`; M5
    only changes *where the stub text comes from* (generated, not hand-written).
  - verify: the exact `type_check` / `SourceFile` signatures against the pinned Monty tag
    (`monty = { git = "https://github.com/pydantic/monty", tag = "v0.0.18" }`). The observed shape is
    `pub fn type_check(python_source: &SourceFile, stubs_file: Option<&SourceFile>) -> Result<Option<TypeCheckingDiagnostics>, String>` and `SourceFile::new(name, code)` — but the API is pre-1.0 and churns;
    read `crates/monty-type-checking/src/` at the tag before writing this.
  - ✅ Done when: feeding a correct call returns `Ok(None)` and proceeds to `feed_start`.

- [ ] Decide what source the checker sees in a *persistent* REPL as its OWN step.
  - verify: whether the persistent `MontyRepl` must be type-checked against the *cumulative* session
    source (names defined in earlier `run_code` steps) or just the new chunk — `type_check` takes one
    source, so for a multi-step session you may need to prepend prior definitions or keep stub continuity.
    This is unspecified in Monty's docs; test both and keep what the checker accepts.
  - ✅ Done when: a two-step session where step 2 references a name defined in step 1 type-checks the way
    you chose (cumulative or per-chunk), and you've recorded which the checker accepts.

### Chunk 8 — Prove "wrong column caught before execution"

- [ ] Write an end-to-end test (Python SDK driving the wheel, or a Rust integration test feeding generated
  stubs): register a model with a `revenue` column, then feed agent code that selects a *non-existent*
  column / passes a wrong-typed arg, and assert `type_check` returns `Ok(Some(diags))` → a retry, with
  **no DuckDB query ever running**.
  - 🔗 **Maps to:** the v1 Success Criterion in miniature — wrong field fails before execution, model
    self-corrects.
  - ✅ Done when: the bad-column case returns type diagnostics (and triggers a retry) while a
    correct-column case returns `Ok(None)` and proceeds — and the bad case never reaches DuckDB.

- [ ] Confirm the diagnostics are turned into a `DropletError` that the driver maps to a model retry (the
  M4 plumbing), and that an *internal* checker error (`Err(String)`) surfaces **distinctly** (not as a
  retry-able type error).
  - ⚠️ **Invariant #10** (one boundary error type): all engine/checker errors fold into `DropletError`
    (`thiserror` in libraries, `anyhow` only at binaries); keep the boundary to one error type.
  - 🆕 Concept: `type_check` returns `Ok(Some(diags))` for *real* type errors but `Err(String)` for an
    *internal* checker failure — two different `DropletError` variants, because only the first should
    trigger a model retry. (Concept is project-side; no Rust Book chapter.)
  - ✅ Done when: a type error becomes a retry-mapped `DropletError`, and a simulated internal checker
    failure becomes a distinct, non-retry `DropletError`.

### Chunk 9 — No-drift guard: stub names == host dispatch list

- [ ] Add a test that asserts the flat `def` names in the *generated* stub equal the flat
  external-function names the Monty driver actually registers/dispatches (the canonical list from Chunk 3,
  shared with M4's `ReplProgress::FunctionCall` dispatch). Mismatch fails loudly, **naming the offending
  function** — so renaming a tool in only one place can't silently break the contract.
  - ⚠️ **Invariant #7** (flat typed functions, type-checked before run): the stub *is* the flat tool
    surface — this test is what keeps "flat typed functions, type-checked before run" honest as the
    surface grows (M6 adds the real `search_fields`).
  - ✅ Done when: the test is green, and deliberately renaming a tool in only one place turns it red with a
    message naming the function.

---

## Notes carried forward (don't act yet)

- **The seam is text, not objects.** Everything M5 sends into `droplet-core` is owned strings + a simple
  `Vec<(table, column, duckdb_type)>`. No pydantic, no PyO3 in core — Invariants #1 and #2 both ride on
  this. If a Rust type in core ever needs `pyo3`, you've crossed the line.
- **The stub is the single contract.** The `.pyi` the LLM is checked against, the host dispatch list, and
  (in M6) the `FieldRef` Rust return type must all agree. Chunk 9's drift test guards the names; keep the
  `FieldRef` shape in sync with M6 when you wire `search_fields`.
- **`ty` and Monty are pre-1.0.** Don't assert exact diagnostic strings or exact `type_check` signatures —
  pin the Monty tag fleet-wide, read the source at that tag, and leave `verify:` notes where the API
  could move.
- **Mapping unknowns must be loud.** An unmapped Python type falling back to `VARCHAR` must log a `TODO`,
  never pass silently — a silent fallback is how a wrong column type sneaks past the "one schema, many
  outputs" promise.

---

> 📌 When you reach this milestone, expand each chunk into tiny steps the way M0/M1 are written.
