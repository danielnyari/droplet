# M4 — `#[droplet_tool]` macro + auto-bootstrap (SKETCH)

**Milestone goal:** stop hand-maintaining the tool surface. Build a `#[droplet_tool]` **proc-macro**
that, from each fixed primitive's Rust signature, emits **both** the Monty external-function registration
**and** the Python type-stub fragment — and a **runtime schema-derived** generator that turns the
session's catalog into per-dataset field `Literal`s, row `TypedDict`s, and a specialized `load(...)`
signature. Merge the two into **one stub bundle + one external-function table** per session, and use it to
**replace M3's hand-wired surface**. This is the milestone that finally satisfies Invariant #4.

**Done when (from the spec, build-order step 2 / PRODUCT §8):** each fixed primitive carries
`#[droplet_tool]` and is registered + typed automatically; opening a session over a catalog generates the
data-shaped types; the two merge into one `type_check_stubs` + `external_functions` bundle handed to Monty,
with **no hand-maintained registry or stubs left** — and M3's milestone test still passes against the
generated surface.

**Prerequisite:** finish [`M3-monty-driver.md`](./M3-monty-driver.md). You have your **first working
Droplet**: the `run_code` loop (Monty suspends at a tool call, the host runs it, then resumes),
type-check-before-run, and a **hand-wired** stub bundle + dispatch table exposing `load` plus the analyze
primitives (`filter_rows`, `group_agg`, `to_rows`, `scalar`, `local_sql`, …). M4 does **targeted surgery**
on that working code: it *generates* what M3 wrote by hand, then deletes the hand-written copy.

**Estimate:** ~9 chunks.

> This is a **SKETCH** file: chunk-level checkboxes with concept notes and invariant callouts, *not* the
> tiny per-line steps of M0–M3. When you reach this milestone, your **first job** is to expand each chunk
> below into tiny, independently-verifiable steps the way M0/M1 are written — one new idea per `- [ ]`,
> each ending in a `cargo build` / `cargo test` you can tick.

---

## How to read this file

- Every `- [ ]` is a chunk-level task (a sitting, not a 10-minute step). Expand each into M0/M1-sized steps
  on arrival.
- `🆕 Concept:` explains a new Rust/Droplet idea the **first** time it shows up, with a Rust Book chapter
  name (run `rustup doc --book` to open the book offline) when one applies.
- `✅ Done when:` is an observable check — a command's output or a passing test.
- `⚠️ Invariant #N:` quotes a load-bearing rule from the README's Golden Rules (= `PRODUCT.md` §15) in
  plain words. Never break these.
- `🔗 Maps to:` ties a step to the real Droplet concept it unlocks.
- `verify:` flags a fact the research couldn't fully pin on the locked versions — check the crate's
  source/docs **first**, before relying on it. Proc-macro crates (`syn`/`quote`/`proc-macro2`) and the
  Monty external-fn registration API are the big ones here.

---

## The big idea, in plain words (read first, ~10 min)

Right now (end of M3) two lists are written **by hand** and must be kept in lockstep:

1. the **dispatch table** — the Rust `match call.function_name { "load" => …, "group_agg" => …, … }` that
   runs the real host work for each tool;
2. the **stub text** — the `.pyi`-style source the type checker reads, with one `def load(...) -> ...: ...`
   per tool.

If you rename `group_agg` in one and not the other, the contract silently breaks. M3 accepted that on
purpose, as a scaffold, so you could *see* what the macro will generate. M4 removes the duplication
entirely. The tool surface has **two kinds** of entries, and each gets its own generator:

- **Fixed primitives** (`load`, `filter_rows`, `group_agg`, `join`, `with_column`, `window`, `sort`,
  `distinct`, `describe`, `scalar`, `to_rows`, `local_sql`, the filter helpers `eq`/`gt`/…, and discovery
  `list_datasets`/`describe_dataset`/`search_fields`/`export`). Their **names and shapes never change at
  runtime** — they're the same for every catalog. These get a **compile-time** generator: the
  `#[droplet_tool]` **proc-macro**. Author the Rust function once; the macro emits the registration *and*
  the stub fragment from its signature.
- **Schema-derived types** depend on **which datasets are in scope this session**. The set of valid column
  names for `usage_daily`, the row shape `to_rows` yields for it, the exact `load("usage_daily", …)`
  signature — none of that is knowable at compile time, because the catalog is configuration. These get a
  **runtime** generator: at session open, introspect the catalog and emit the data-shaped stub fragments.

The two streams **merge into one bundle** — one stub text + one external-function table — handed to Monty
as `type_check_stubs` + `external_functions`. That merged bundle is **per-session** (the surface depends on
the catalog in scope) and **versioned** (so a snapshot taken now can be resumed later and regenerate a
*byte-identical* surface; see Chunk 9 and M8).

> ⚠️ Invariant #4 (auto-generated tool surface): *"Fixed tools carry a `#[droplet_tool]` macro; the
> data-shaped types come from the catalog. No hand-maintained registry or stubs."* Everything in this file
> serves that one rule. The M3 exception ("wire a tiny surface by hand as a teaching scaffold") **expires
> here** — by the end of M4 there is no hand-maintained list anywhere.

> ⚠️ Invariant #8 (keep Python out of the core): the proc-macro and the merged-bundle builder live in
> Rust, in the `droplet-*` crates — **never** `pyo3`. The schema-derived generator reads the catalog
> *schema* (plain Rust data: dataset name → columns → types) that already crossed the PyO3 boundary as
> owned strings in M2; it does **not** import pydantic or touch a live Python object. If you reach for
> `pyo3` in the macro or the bundle builder, you've crossed the line.

---

## What a proc-macro is (your first one — gentle intro)

You've used macros already: `println!`, `vec!`, `#[derive(Debug)]`, `#[test]`. The `!` ones are
*declarative* macros; the `#[...]` ones are **procedural macros** ("proc-macros"). This milestone is the
**first time you write your own**, so go slowly.

🆕 **Concept: a proc-macro is a Rust function that runs *at compile time* and rewrites code.** It takes a
stream of source tokens as input and returns a stream of source tokens as output. The compiler runs it
*while compiling*, splices the output back in, and then compiles *that*. So `#[droplet_tool]` on a function
is not a runtime call — it's a tiny program that, during `cargo build`, reads your function's signature and
**generates more Rust code** next to it. (Rust Book: *Macros*, the "Procedural Macros for Generating Code
from Attributes" section.)

🆕 **Concept: a proc-macro must live in its own crate, marked `proc-macro = true`.** A normal library crate
can't define proc-macros; the crate that *exports* the `#[droplet_tool]` attribute must be a dedicated
`proc-macro` crate (its `Cargo.toml` has `[lib] proc-macro = true`). Other crates then depend on it like
any library and `use` the attribute. So even though PRODUCT §17 lists "the `#[droplet_tool]` macro" under
`droplet-core`'s responsibilities, **physically** it needs a separate crate — e.g.
`crates/droplet-macros/` — that `droplet-core` depends on. (Rust Book: *Macros*; and *Managing Growing
Projects with Packages, Crates, and Modules*, ch. 7, for the crate split.)

🆕 **Concept: the three crates that make proc-macros bearable.**
- **`proc-macro2`** — a wrapper over the compiler's raw `proc_macro` token type that you can also use
  *outside* the compiler (e.g. in unit tests), so your macro logic is testable.
- **`syn`** — *parses* a token stream into a typed syntax tree. You'll parse your tool function into a
  `syn::ItemFn` and read its name, arguments, and return type as structured data instead of raw tokens.
- **`quote`** — the reverse: a `quote! { … }` block writes Rust code as a template, splicing in values with
  `#name`. It returns a token stream you hand back to the compiler.

A useful mental model: **`syn` reads, `quote` writes**, `proc-macro2` is the token type both speak.

> 📚 **Read these before writing a line:** the Rust Book *Macros* chapter (the attribute-macro section),
> and **"The Little Book of Rust Macros"** (the *Procedural Macros* chapters). Proc-macros are advanced
> Rust — the README warns you don't meet them until here on purpose. Budget time to read first.

> verify: the **exact versions** of `syn` / `quote` / `proc-macro2`. The current major lines are
> `syn = "2.0"` (use `features = ["full"]` to parse whole functions), `quote = "1.0"`,
> `proc-macro2 = "1.0"` — but these are **not** on this roadmap's locked-pin list, so confirm the exact
> patch versions on crates.io and pin them in `[workspace.dependencies]` before you start. The `syn` 1.x →
> 2.x API changed shape; ignore any 1.x tutorial.

> verify: the **Monty external-function registration API** at tag `v0.0.18`. M3 dispatched purely by
> matching `call.function_name` (Monty *suspends*; there's no up-front registry on the Rust side). So
> "register an external function" may mean *nothing more than* "the dispatch arm exists + the name appears
> in the stub the checker sees" — there may be no separate Rust registration call at all. **Read
> `crates/monty/src/` at the tag** to confirm what (if anything) `external_functions` means on the Rust
> embed path before you design what the macro emits. The macro's job is fixed regardless: emit (a) the
> stub `def` fragment and (b) whatever the host needs to route the name to the function.

---

### Chunk 1 — Create the `droplet-macros` proc-macro crate

- [ ] Add a new crate `crates/droplet-macros/` with `[lib] proc-macro = true`, depending on
  `syn` (features `["full"]`), `quote`, and `proc-macro2`. Add the three to `[workspace.dependencies]`
  (pinning exact `verify:`'d versions) and opt the crate in with `.workspace = true`. Make `droplet-core`
  depend on `droplet-macros`.
  - 🆕 Concept: a **`proc-macro = true` crate** can export *only* proc-macros from its root, and it's
    compiled to run inside the compiler — so it can't be used as a normal runtime library. Keep it tiny:
    just the macro. (Rust Book: *Macros*.)
  - ⚠️ Invariant #8: `droplet-macros` must **not** depend on `pyo3` (and neither does `droplet-core`).
    It depends only on the three proc-macro crates. The macro generates *text and tokens*, never touches
    Python.
  - ✅ Done when: `cargo build -p droplet-macros` is green (an empty crate with the right `Cargo.toml`),
    and `droplet-core`'s `Cargo.toml` lists `droplet-macros` as a dependency.

- [ ] Write the **simplest possible** attribute macro to prove the toolchain works: a `#[droplet_tool]`
  that parses the function and emits it **unchanged**, plus one extra trivial item (e.g. a `const` named
  after the function) so you can see the macro actually generated something.
  ```rust
  // verify: exact syn/quote/proc-macro2 versions — see the verify note above
  use proc_macro::TokenStream;
  use quote::quote;
  use syn::{parse_macro_input, ItemFn};

  #[proc_macro_attribute]
  pub fn droplet_tool(_attr: TokenStream, item: TokenStream) -> TokenStream {
      let func = parse_macro_input!(item as ItemFn);
      let name = &func.sig.ident;
      let marker = quote::format_ident!("__droplet_tool_{}", name);
      quote! {
          #func
          #[allow(non_upper_case_globals)]
          const #marker: &str = stringify!(#name);
      }.into()
  }
  ```
  - 🆕 Concept: a `#[proc_macro_attribute]` fn takes **two** token streams — the attribute's own args
    (`_attr`, empty for now) and the item it's attached to (`item`) — and returns the replacement tokens.
    `parse_macro_input!(item as ItemFn)` turns the raw tokens into a typed function node and auto-emits a
    `compile_error!` on a parse failure. (Rust Book: *Macros*.)
  - verify: the `syn` 2.x node names — `ItemFn`, `func.sig.ident`, `func.sig.inputs`, `func.sig.output`.
    Confirm against the pinned `syn` docs; 1.x differs.
  - ✅ Done when: a throwaway `#[droplet_tool] fn ping() {}` in `droplet-core` still compiles and is
    callable, proving the attribute round-trips through the compiler.

### Chunk 2 — Macro emits the **stub fragment** from the signature

- [ ] Teach the macro to read the function's argument names + types and its return type, and emit a Python
  **`.pyi` stub fragment** — `def name(arg: PyType, …) -> PyRet: ...` — as a string the host can collect.
  Map Rust types → Python stub types with a small fixed table (`String`/`&str → str`, `i64 → int`,
  `f64 → float`, `bool → bool`, your `Dataset` handle type → its Python class name, `Vec<T> → list[…]`,
  the filter type → its Python name). Unknown → a **loud** fallback (emit `object` *and* a compile-time
  warning), never a silent guess.
  - 🆕 Concept: the macro walks `func.sig.inputs` (each a `syn::FnArg` → `PatType` with a `pat` name and a
    `ty` type) and `func.sig.output` (a `syn::ReturnType`), turning each into its Python spelling. This is
    the compile-time half of "one signature, two outputs" — same idea as the runtime schema-derived
    generator later in this milestone (Chunk 6), where the *source* is a Pydantic field rather than a Rust
    signature. (Rust Book: *Macros*; the iteration is plain Rust over `syn` nodes.)
  - 🆕 Concept: a **`.pyi` stub** is a signature-only Python file — names, arg types, return types,
    docstrings, but every body is literally `...`. It's the same idea as a C header or a TypeScript `.d.ts`:
    it describes the *shape* of an API so a type checker can verify calls without running anything.
    (Concept is project-side; no Rust Book chapter.)
  - ⚠️ Invariant #4: the stub fragment is **generated from the signature**, so it can't drift from the real
    function — that's the whole point of the macro.
  - verify: how Monty's checker wants the stub formatted at the tag (a real `.pyi`? a plain `def … : ...`
    string?). M3 already pinned this when it hand-wrote stubs — reuse that exact format here.
  - ✅ Done when: `#[droplet_tool] fn scalar(d: Dataset, col: &str) -> f64` makes the macro emit the
    fragment `def scalar(d: Dataset, col: str) -> float: ...` (assert it in a macro unit test, see Chunk 4).

- [ ] Carry the function's Rust doc-comment (`///`) into the stub as the Python docstring. Short, literal,
  one line — the model leans on these.
  - 🆕 Concept: doc-comments arrive as `#[doc = "..."]` attributes on the `ItemFn`; the macro reads them
    from `func.attrs`. (Rust Book: *Macros*.)
  - ✅ Done when: a doc-commented tool's emitted stub fragment includes a matching `"""…"""` docstring.

### Chunk 3 — Macro emits the **registration / dispatch hook**

- [ ] Teach the macro to *also* emit whatever the host needs to route `call.function_name == "scalar"` to
  the real `scalar` fn — so the dispatch table is **generated**, not hand-typed. Concretely: each
  `#[droplet_tool]` registers its `(name, stub_fragment, dispatch_thunk)` into a **compile-time-collected
  inventory** the bundle builder reads in Chunk 5.
  - 🆕 Concept: a proc-macro can't directly append to a `match` in another file, so the standard trick is a
    **distributed-slice / inventory pattern**: each macro expansion emits a registration item, and a
    collector gathers them all at startup. (Concept is project-side; covered by the chosen registry crate's
    docs, not the Rust Book.)
  - verify: the **mechanism** — options are the `inventory` crate (runtime collection at startup),
    `linkme` (`distributed_slice`, link-time), or a plainer route: have the macro push the name into a
    generated module the bundle builder iterates. Pick one, pin its exact version (`verify:`), and confirm
    it works under your link setup before committing. The simplest *correct* option wins; don't over-build.
  - ⚠️ Invariant #4: this is the step that lets you **delete the hand-written dispatch `match`** from M3 —
    the names now come from the macro, in lockstep with the stubs.
  - ✅ Done when: two `#[droplet_tool]`-annotated fns both appear in the collected inventory with their
    name + stub fragment, verified by a test that enumerates the inventory.

- [ ] Decide how the dispatch thunk **adapts arguments**: Monty hands you `MontyObject` args by name/order,
  and your real fn wants typed Rust args (a `Dataset` handle, a `&str`, an `f64`). The thunk the macro
  emits must unpack the `MontyObject`s into the fn's parameter types and pack the return back into a
  `MontyObject`/`ExtFunctionResult`. Keep the conversion rules in **one** place the macro calls into.
  - verify: the `MontyObject` ↔ Rust conversions and the `ExtFunctionResult` construction at the tag (M3
    already used these in its hand dispatch — reuse the same helpers; the macro just calls them).
  - ⚠️ Invariant #6 (boundary discipline): the thunk for a result-returning tool (`to_rows`, `scalar`,
    `describe`) must return **capped** rows; handle-returning tools (`load`, `filter_rows`, `group_agg`,
    `join`, …) return an **opaque handle**, never bulk data. The macro emits the wiring; the cap lives in
    the analyze prims from M1.
  - ✅ Done when: a `#[droplet_tool]` tool dispatched through the generated thunk produces the same result
    M3's hand-wired arm did for the same call.

### Chunk 4 — Unit-test the macro in isolation

- [ ] Add macro-expansion tests so the generator is verifiable on its own (this is *why* you depend on
  `proc-macro2`: the parsing/emitting logic can run outside the compiler). Use a snapshot test
  (`verify:` the crate, e.g. `macrotest` / `trybuild` / `insta`) or assert the emitted stub-fragment
  string directly for a handful of representative signatures (a handle-returner, a scalar-returner, a
  filter helper, a `Vec`-returner).
  - 🆕 Concept: **golden / snapshot testing** pins the macro's output to a checked-in expected string, so
    any accidental change to what it emits fails loudly. (Concept is project-side; tool-specific docs.)
  - verify: pin whichever snapshot/expansion crate you choose (exact version `verify:`).
  - ✅ Done when: the macro's emitted stub fragments for the representative signatures match their golden
    expectations, and deliberately changing the Rust→Python type map turns a snapshot red.

- [ ] Annotate **one real fixed primitive** end to end with `#[droplet_tool]` (start with `scalar` — it's
  the simplest: handle in, capped scalar out) and confirm its generated stub + dispatch match what M3 had
  by hand for `scalar`.
  - 🔗 Maps to: the first brick of replacing the hand-wiring — one tool now fully auto-generated.
  - ✅ Done when: `scalar` works through the *generated* path, and you've removed `scalar`'s hand-written
    stub line + dispatch arm from M3's code.

### Chunk 5 — The compile-time half of the bundle (annotate every fixed primitive)

- [ ] Put `#[droplet_tool]` on **every** fixed primitive: the analyze prims (`filter_rows`, `group_agg`,
  `join`, `with_column`, `window`, `sort`, `distinct`, `describe`, `scalar`, `to_rows`), `local_sql`, the
  filter helpers (`eq`, `gt`, `lt`, `gte`, `lte`, `in_`, `between`, `contains`), discovery
  (`list_datasets`, `describe_dataset`, `search_fields`), `export`, and the **base** `load`. Then write a
  `fixed_stub_fragments() -> String` + `fixed_dispatch()` that reads the inventory from Chunk 3.
  - 🆕 Concept: this is "author once, registered + typed automatically" (PRODUCT §8) applied across the
    whole fixed surface — the macro you built in Chunks 2–3, used everywhere.
  - ⚠️ Invariant #4: after this step, the **entire fixed surface** is macro-generated. Delete the rest of
    M3's hand-written stub block and dispatch `match`. Nothing fixed is hand-maintained anymore.
  - ⚠️ Practical rule (not a numbered invariant): Monty is a *subset* of Python with no class/module
    namespacing, so the tool surface is **flat typed functions** — every emitted `def` is a **top-level**
    `def name(...) -> ...`, never `tools.group_agg`. If a tool's signature can't be a flat free function, the
    design is wrong.
  - verify: `search_fields`'s return type (`FieldRef`) is real in **M9**, not here — in M4 emit its stub
    against a placeholder `FieldRef` type and a stub dispatch, exactly as M3 did. Keep the `FieldRef` shape
    recorded so M9 matches it.
  - ✅ Done when: `fixed_stub_fragments()` returns a stub block containing a `def` for **every** fixed tool
    name, sourced entirely from the macro inventory, and `cargo test -p droplet-core` passes with the
    hand-written fixed surface deleted.

### Chunk 6 — The runtime half: schema-derived **field `Literal`s** and **row `TypedDict`s**

- [ ] Write a runtime generator `schema_stub_fragments(catalog) -> String` that, for each dataset in the
  session's catalog scope, emits the **data-shaped** stub fragments. First: a **field `Literal`** per
  dataset — a `Literal["account_id", "day", "active_minutes", …]` of that dataset's real column names, so
  the type checker can reject a column that doesn't exist.
  - 🆕 Concept: a Python **`Literal[...]`** type pins a value to an exact set of allowed strings. A
    `where`/`columns` argument typed `Literal["account_id", "day", …]` makes a hallucinated column name a
    **type error caught before the code runs** — there's nothing for the bad name to bind to. (Concept is
    project-side; no Rust Book chapter.)
  - 🆕 Concept: this generator reads the catalog **schema** that M2 stored on the `Session` as plain owned
    Rust (`dataset → [(column, type)]`). It does **not** introspect Pydantic — that introspection already
    happened on the Python side and crossed the boundary as text/data. (Rust Book: *Managing Growing
    Projects…*, ch. 7, for where this module lives.)
  - ⚠️ Invariant #8: this is Rust, in `droplet-core`, over already-owned schema data — **no pyo3, no
    pydantic**. `droplet-core` must still build as pure Rust.
  - 🔗 Maps to: the v1 success criterion — *a wrong field caught by the type checker before execution*.
  - ✅ Done when: for a catalog with a `usage_daily(account_id, day, active_minutes)` dataset,
    `schema_stub_fragments` emits a `Literal["account_id","day","active_minutes"]` for it.

- [ ] Add the **row `TypedDict`** per dataset: the shape `to_rows(dataset)` yields — a `TypedDict` whose
  keys are the dataset's columns and whose value types are the Python spellings of the DuckDB column types
  (reuse this milestone's column-type map — the Pydantic/DuckDB → Python spellings the schema-derived
  generator builds — in reverse: DuckDB type → Python type). So iterating `to_rows(usage)` gives
  the model `r["active_minutes"]` typed as `float`, and `r["nope"]` is a type error.
  - 🆕 Concept: a **`TypedDict`** describes a dict with known string keys and per-key value types — exactly
    a "row." It lets the checker verify `r["active_minutes"]` is valid and `r["typo"]` is not. (Concept is
    project-side; no Rust Book chapter.)
  - verify: the DuckDB-type → Python-type mapping (`BIGINT → int`, `DOUBLE → float`, `VARCHAR → str`,
    `BOOLEAN → bool`, `TIMESTAMP → datetime`, …) against what the analyze prims actually return into the
    sandbox; keep it consistent with the forward map this milestone defines.
  - ✅ Done when: `to_rows` over `usage_daily` is typed by a generated `TypedDict` with the right key→type
    pairs, and a test asserts the emitted `TypedDict` text.

### Chunk 7 — The specialized `load(...)` signature for in-scope datasets

- [ ] Emit a **specialized `load` overload** per in-scope dataset so `load("usage_daily", columns=…,
  where=…)` is typed against *that* dataset's fields. The dataset name argument becomes a
  `Literal["usage_daily", "orders", …]`; `columns` becomes `list[<that dataset's field Literal>]`; the
  `where` filters reference the same field `Literal`. The base `load` came from the macro (Chunk 5); this
  step **specializes its types** from the catalog.
  - 🆕 Concept: this is the join point of the two generators — the **macro** gave you `load`'s *fixed*
    shape (it's a function that takes a name, columns, filters, `as_of`); the **catalog** gives you the
    *types* that fill those slots for the datasets actually in scope. PRODUCT §8 calls this "the specialized
    `load(...)` signature for the datasets in scope." (Concept is project-side.)
  - 🆕 Concept (typing mechanism): with multiple datasets you likely want `@overload`-style stubs (one
    typed `load` per dataset literal) or a single `load` whose `columns`/`where` are typed by a union of the
    per-dataset `Literal`s. `verify:` which form Monty's bundled `ty` accepts cleanly at the tag, and keep
    the one it accepts.
  - ⚠️ Invariant #2 (only `load` touches the source, bounded + typed): the *typing* is what makes the load
    bounded — the agent **cannot express an out-of-scope load** because an out-of-scope column/dataset is a
    type error. The macro+catalog typing is the enforcement mechanism for the boundary, not decoration.
  - ✅ Done when: `load("usage_daily", columns=["active_minutes"])` type-checks, while
    `load("usage_daily", columns=["nope"])` and `load("not_a_dataset", …)` are **type errors** — proven by
    a test feeding each through the M3 type-check seam.

### Chunk 8 — Merge into ONE bundle + table; feed Monty; delete the hand-wiring

- [ ] Write the **bundle builder**: `build_tool_bundle(catalog) -> ToolBundle`, where
  `ToolBundle { stub_text: String, external_functions: … }` is the merge of (a) `fixed_stub_fragments()`
  (Chunk 5) + `schema_stub_fragments(catalog)` (Chunks 6–7) concatenated into **one** stub source, and
  (b) the fixed dispatch inventory keyed for host routing. Build it **once at session open** and store it
  on the `Session`.
  - 🆕 Concept: this single `ToolBundle` is exactly PRODUCT §8's *"one stub bundle + one external-function
    table, handed to Monty as `type_check_stubs` + `external_functions`."* Macro output + schema-derived
    types are now indistinguishable to Monty — it just sees one typed surface. (Concept is project-side.)
  - ⚠️ Invariant #4: with the bundle assembled from the macro inventory + the catalog, there is **no
    hand-maintained registry or stub** left. Grep the codebase for the old hand-written stub string and
    dispatch `match` and confirm they're gone.
  - ✅ Done when: `build_tool_bundle(catalog).stub_text` contains every fixed `def` **and** the
    per-dataset `Literal`/`TypedDict`/specialized-`load` fragments, in one string.

- [ ] Point M3's type-check-before-run and suspend/resume loop at the bundle: the checker's stub
  `SourceFile` is built from `session.tool_bundle().stub_text`, and the `FunctionCall` dispatch routes
  through the bundle's external-function table — **not** the hand-written versions. Run M3's milestone test
  unchanged against the generated surface.
  - ⚠️ Practical rule (not a numbered invariant): the type check still runs **ahead** of `feed_start`; M4
    only changes *where the stub text and dispatch come from* (generated, not hand-written).
  - 🔗 Maps to: closing the loop on M3 — the "first working Droplet" now runs on an auto-bootstrapped
    surface, satisfying Invariant #4 it was scaffolding toward.
  - ✅ Done when: M3's end-to-end test (wrong column caught pre-execution; correct multi-step analysis runs
    and returns capped rows) **passes against the generated bundle**, with the hand-wired surface deleted.

- [ ] No-drift guard (carry M3's spirit forward): add a test asserting the set of flat `def` names in the
  generated `stub_text` equals the set of names the dispatch table routes — so a tool present in one but
  not the other fails loudly, **naming the offending function**. With generation this should be impossible
  by construction; the test proves it stays that way as the surface grows (M9 adds the real
  `search_fields`).
  - ⚠️ Invariant #4: this is the guard that keeps "no hand-maintained registry or stubs" honest over time.
  - ✅ Done when: stub `def` names == dispatch names, and deliberately dropping one tool from one side turns
    the test red with the function name in the message.

### Chunk 9 — Make the bundle **versioned** (for snapshot/resume identity)

- [ ] Stamp the `ToolBundle` with a **version** = `hash(fixed-surface version + catalog version +
  generator version)`, and record it in the session (and later, in the snapshot manifest). The bundle is
  **per-session** because the surface depends on the catalog in scope; it must be **regenerable
  identically** so a session snapshotted on one pod and resumed on another rebuilds a **byte-identical**
  tool surface.
  - 🆕 Concept: a snapshot only stores REPL bytes + a tiny manifest (Invariant #5) — **not** the tool
    surface. On resume, the surface is **regenerated** from the same catalog + same generator version. The
    version stamp is what lets resume **refuse** to load a snapshot whose surface would differ (mismatched
    catalog/generator), rather than silently mis-bind. (Concept is project-side; the snapshot subsystem is
    M8.)
  - ⚠️ Invariant #5 (snapshots tiny + versioned): record the bundle version in the manifest so cross-
    version resume fails loudly. Never serialize the bundle itself into the snapshot — regenerate it.
  - ⚠️ Invariant #7 (distributed by default): because resume happens on **any** pod with no affinity, the
    surface must be reproducible from shared, versioned inputs (the catalog), not from pod-local state.
  - 🔗 Maps to: M8's cross-pod resume — the bundle version is part of the manifest's identity check.
  - ✅ Done when: building the bundle twice for the same catalog yields the **same version** and identical
    `stub_text`; changing the catalog (or the generator) changes the version. A test asserts both
    directions.

---

## Notes carried forward (don't act yet)

- **The macro is the contract enforcer, not a convenience.** Invariant #4 is *satisfied* here: by the end
  of M4 there is **no hand-maintained registry or stubs** — fixed tools come from `#[droplet_tool]`, data-
  shaped types come from the catalog, merged into one versioned bundle. M3's hand-wiring was always a
  scaffold; deleting it is a deliverable, not cleanup.
- **Two generators, one bundle.** Compile-time (`#[droplet_tool]` → fixed `def`s + dispatch) and runtime
  (catalog → field `Literal`s, row `TypedDict`s, specialized `load`) merge into a single
  `stub_text` + external-fn table. Keep the merge in one `build_tool_bundle` so there's exactly one place
  the surface is assembled.
- **The bundle is per-session and versioned.** It depends on the catalog in scope and must regenerate
  identically for snapshot/resume (M8). Version = `hash(fixed-surface + catalog + generator)`; record it in
  the manifest; never serialize the bundle itself.
- **`search_fields`/`FieldRef` is M9; the real `load` connectors + cache are M5–M6.** In M4 the macro emits
  the *typed surface*; the bodies behind `search_fields` (read-only SurrealDB, M9) and the real cached
  `load` (M5 artifact cache, M6 Athena/S3 connectors) land later. Keep the `FieldRef` stub shape recorded
  so M9 matches it, exactly as M3 noted.
- **Pin the proc-macro crates and the registry crate.** `syn`/`quote`/`proc-macro2` and whichever
  inventory/`linkme` crate you pick are **not** on this roadmap's locked-pin list — pin exact versions in
  `[workspace.dependencies]`, keep `Cargo.lock` committed, and leave `verify:` notes where the API could
  move. Proc-macro builds are slow; a fixed lockfile keeps them predictable.
- **Read the Monty embed source before designing registration.** What `external_functions` means on the
  Rust embed path at `v0.0.18` (vs. a pure dispatch-by-name model) decides what the macro's registration
  hook actually emits. Confirm in `crates/monty/src/` at the tag — don't infer it from the `monty-python`
  convenience API.

---

> 📌 When you reach this milestone, expand each chunk into tiny steps the way M0/M1 are written.
