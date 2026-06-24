# Droplet roadmap replan — vertical value slices

> **Status:** design, awaiting review.
> **Supersedes:** the milestone *structure* of `docs/roadmap/` (00 + M0–M10). Does **not** touch
> `PRODUCT.md` — that is and remains the source of truth.

---

## 1. Problem

The current roadmap is built as **horizontal layers**: `M0` skeleton → `M1` engine → `M2` load →
`M3` wire-together. Consequences:

- **Nothing runs until M3.** `M0`/`M1`/`M2` each ship *infrastructure*, not value. A milestone's
  "done" is a passing unit test on an inert layer, not a thing you can run.
- **Throwaway scaffolds are scheduled on purpose.** `M3` hand-wires the external-function dispatch
  table *and* hand-writes the `.pyi` stubs **explicitly so `M4` can delete them**. That is a direct
  violation of PRODUCT.md **invariant #4** ("the tool surface is auto-bootstrapped … no
  hand-maintained registry or stubs"), waved through as a "teaching scaffold."
- **The scaffolds become false truth.** A later session (or a future reader) opens the roadmap, finds
  "build a hand-wired stub here," and treats the throwaway as the intended design.

This is the failure mode being eliminated: **a fast/green milestone that ships nothing usable, and a
scaffold that outlives its excuse.**

## 2. Principles (the replan rules)

1. **Every milestone ends in something you can run and demo.** "Done" = a command that produces value.
2. **Infrastructure rides in attached to the first feature that needs it** — never as its own milestone.
3. **No throwaway scaffolds across milestones.** If a surface is needed, build the permanent version
   once. (Where seeing-then-replacing genuinely aids learning, it happens *within* one milestone and
   only the permanent version ships — nothing hand-written survives the milestone boundary.)
4. **Learnability lives inside a milestone, not between milestones.** Milestone = value; the tiny
   Rust-newbie steps are its internal build order. Small steps remain — they just don't end at
   "nothing works yet."

## 3. PRODUCT.md is the source of truth — followed at all times

This replan reorganizes *sequencing only*. The product vision is non-negotiable and every milestone is
checked against it:

- **The north star is §20** (the v1 success criteria). The spine is the staircase to §20; the final
  milestone *is* that acceptance test passing.
- **Scope is §16.** Nothing in the spine is outside v1 scope. **§16 "Deferred" stays deferred**:
  Snowflake/BigQuery connectors, Iceberg write-back on export, metric/semantic modeling beyond field
  search, managed-tier features — not built; only their seams exist (the `Source` trait, the backend
  config).
- **The invariants (§15) are honored from the first slice**, not retrofitted. In particular invariant
  #4 (auto-bootstrap, no hand-maintained registry/stubs) means the `#[droplet_tool]` macro is built
  **for real in V1** — there is no hand-wired interim surface to delete later.
- **§21 is a seed, not a contract.** Its *capabilities* are all delivered; its *horizontal ordering*
  (and its "build all four store traits up front" step 1) is reorganized into vertical slices. The
  vision (§1–§16, §20) is followed exactly; only the build sequence changes.

## 4. The new spine

Each milestone lists **Ships** (the value), **Done when** (the runnable demo), and **PRODUCT.md**
(the sections + invariants it delivers). `Vn` numbering is deliberately distinct from the old `Mn`.

### V1 — Code-mode over a local file (the walking skeleton)

- **Ships:** agent Python runs in the Monty sandbox, calls a typed, **macro-generated** tool that
  analyzes a local Parquet, and gets a real answer back. Single process, local file, no
  cloud/cache/snapshot.
- **Done when:** `run_code("rows = query('sales.parquet','SELECT region, SUM(amt) AS t FROM data GROUP BY region'); print(rows)")`
  returns the real aggregates to the agent's code.
- **PRODUCT.md:** §1, §2 (analyze half), §7 (local analyze surface, `local_sql`, `to_rows`/`scalar`),
  §8 (the `#[droplet_tool]` macro + suspend/resume execution), §3 (Monty driver). Invariants #3, #4
  (macro is real from day one), #6 (handles + capped readout), #8 (pyo3 only in `droplet-py`), #9, #10.
  Build-order §21 steps 2–4, vertically sliced.
- **Why it's value, not infra:** this is the smallest thing that is *actually Droplet* — code mode
  producing an answer. The error type, `Session`, DuckDB, the Monty driver, and the macro all ride in
  **because this slice needs them**, not as separate milestones.
- **Cost (honest):** V1 is large — it absorbs old `M0`+`M1`+`M2`-core+`M3`-driver, and the proc-macro
  is advanced Rust for a newbie. It is large *because* invariant #4 forbids a hand-wired shortcut. If
  the swallow is too big, the internal split is **V1a** (`query` a local file from code-mode, macro for
  one tool) → **V1b** (the full local analyze surface: `filter_rows`/`group_agg`/`join`/`with_column`/
  `window`/… as handles, boundary discipline) — both still end in a runnable demo.

### V2 — Wrong code caught before it runs

- **Ships:** type-check-before-execute against the macro-generated stub bundle; a bad column/arg is
  rejected with a retryable error and **zero** queries run.
- **Done when:** feeding code that references a non-existent field returns the type-check *retry*
  error and the analyze engine was never touched; fixing it runs to an answer.
- **PRODUCT.md:** §4, §8 (the stub-bundle half of auto-bootstrap, fed to Monty's `ty` type checker),
  §14 ("a wrong field fails at type-check before anything runs"). Invariant #4. §21 step 4 (the
  type-check half).
- **Why it's value:** the self-correcting loop and "the agent can't mess up" — a distinct, demoable
  capability. It consumes the *stub* half of the macro V1 already generates; nothing throwaway.

### V3 — The governed door (load + catalog + schema-derived types)

- **Ships:** the agent references **logical datasets** (not file paths); `load(name, columns, where,
  as_of)` pulls a bounded, typed slice through a `Source` connector → local Parquet; the agent never
  sees the source. Catalog-derived types make `columns`/`where` `Literal`-typed per dataset.
- **Done when:** register a catalog, run `load(...)` + a multi-step analysis; swap the dev connector
  underneath with **zero agent-code change**; a wrong field is now caught against the *catalog* schema.
- **PRODUCT.md:** §6, §9, §10 (load + discovery typing), §8 (runtime schema-derived stub fragments),
  §14 (load is the governed boundary). Invariants #1, #2, #4. §21 step 5 (load half).
- **Why it's value:** the actual security model and the engine-agnostic abstraction become real and
  demonstrable.

### V4 — Pull once, reuse (content-addressed cache)

- **Ships:** the unloaded Parquet is the content-addressed cache artifact, keyed by `hash(scoped
  query + source + freshness token)`; a repeated `load` reuses it. Per-dataset freshness
  (Versioned/TTL/Passthrough).
- **Done when:** run the same `load` twice, assert the source was hit **exactly once** (instrumented
  counter), with the cache index resolving the second call.
- **PRODUCT.md:** §6 (cache artifact), §13 (freshness), §11 (cache index). Invariant #7 (partial:
  content-addressed immutable data). §21 step 5 (cache).
- **Why it's value:** the core thesis — one bounded pull, then free local code — made measurable.

### V5 — Real warehouse (Athena / S3 / Iceberg connectors)

- **Ships:** `Source` impls — S3 + Iceberg direct read, and **Athena** (`UNLOAD` → Parquet on S3),
  behind the same trait, cached.
- **Done when:** `load` against a real Athena/S3 (or the documented local stand-in) materializes and
  caches a slice; the local analysis is identical to V3's.
- **PRODUCT.md:** §6, §9, §16 (v1 connectors: S3 + Iceberg + Athena). **§16 Deferred stays deferred**
  (no Snowflake/BigQuery). §21 step 5 (connectors).
- **Why it's value:** it works on real production data, not a fixture.

### V6 — Survive a crash (snapshot/resume, single machine)

- **Ships:** snapshot = Monty REPL bytes + manifest (never engine heaps); resume rebuilds DuckDB from
  the manifest by re-attaching cached Parquet.
- **Done when:** snapshot after a `run_code` step, restart the process, resume, reach the **same**
  final answer.
- **PRODUCT.md:** §12, §14. Invariant #5. §21 step 7 (single-pod first).
- **Why it's value:** durability you can demonstrate by killing a run and continuing it.

### V7 — Across machines (distributed state plane)

- **Ships:** `ArtifactStore` (S3) + `SnapshotStore` (S3) + `CoordinationStore` (Redis, then DynamoDB);
  run registry + **leases** + cache index; cross-pod resume.
- **Done when:** two pods (two processes) on a shared plane — pod B reuses pod A's cached unload (zero
  re-hit) **and** resumes pod A's run under a lease.
- **PRODUCT.md:** §3 (stateless pods), §11, §12 (cross-pod), §13. Invariant #7. §21 steps 6–7.
- **Why it's value:** the distributed promise, proven with two real processes.

### V8 — Write results out (governed export)

- **Ships:** `export(source, dest, schema)` writes a validated local result to an allow-listed S3
  destination, scoped creds, audited. The mirror of `load`.
- **Done when:** an allow-listed export writes readable Parquet + an audit record; an off-list dest is
  rejected before any byte moves, also audited.
- **PRODUCT.md:** §10 (export), §14 (export governed). §16 (export to S3 parquet; **Iceberg write-back
  deferred**). Invariants #2-spirit (governed boundary), #6, #10.
- **Why it's value:** closes the loop — read → analyze → write — under governance.

### V9 — Discovery (field search + dataset introspection)

- **Ships:** read-only SurrealDB vector field-search index, `search_fields`, plus `list_datasets` /
  `describe_dataset`; catalog-derived typing end to end.
- **Done when:** the agent finds a dataset/field it wasn't told about and uses it in a typed `load`.
- **PRODUCT.md:** §9 (semantic field search), §10 (discovery). Invariant #5 (Surreal rebuilt, never
  written after build). §21 step 8.
- **Why it's value:** usability against a real catalog the agent doesn't have memorized.

### V10 — Real agent framework + the acceptance gate

- **Ships:** Python SDK polish (`Catalog`/`Session`/`run_code`/`run_async`, backend config) + a thin,
  separate **pydantic-ai** adapter; then the **§20** two-pod acceptance test.
- **Done when:** the full §20 sentence runs green from the SDK with no framework, **and** a pydantic-ai
  `Agent` drives the same surface.
- **PRODUCT.md:** §3, §16 (SDK + one adapter), **§20** (the gate). Invariant #8 (framework lives only
  in the adapter package).
- **Why it's value:** the product, usable by others, with the v1 success criteria satisfied.

## 5. What changed from the current roadmap

- **First runnable Droplet moves from M3 → V1.**
- **The throwaway scaffold is deleted.** Old `M3` hand-wiring + hand-written `.pyi` (invariant-#4
  violation) and old `M4` "rewrite it with a macro" collapse into **V1 building the macro for real**;
  type-check + catalog-derived typing become **V2/V3**.
- **No standalone "refactor the wiring" milestone.**
- **The four store traits are not all built up front** (old `M0`/§21-step-1). Only the `Source` seam
  appears early (V3/V5); `ArtifactStore`/`SnapshotStore`/`CoordinationStore` arrive in the slice that
  first ships their value (V4/V6/V7) — no inert trait stubs in milestone 1.
- **Snapshot splits** into single-machine (V6) then distributed (V7), each with its own demo.
- **Unchanged:** `00-rust-warmup.md` (learning, not a product slice) and `PRODUCT.md` (source of truth).

## 6. Current state vs the new spine

Work already done this session maps to **V1, partially**:

- Have: `DropletError`, `Registry`, `Session` (wiped work dir), `Source` trait + local-Parquet dev
  connector, the DuckDB analyze engine (`register_parquet`/`filter_rows`/`group_agg`/`local_sql`,
  capped `to_rows`/`scalar`, configurable cap), the Arrow→plain-rows readout, a Monty suspend/resume
  *seam* (test-only), and a **direct** `droplet-py` `Engine` binding.
- **Not yet V1:** the Monty **driver** (`run_code` loop), the `#[droplet_tool]` macro + auto-bootstrap,
  the external-fn dispatch. The direct `Engine` binding is an **SDK/test convenience**, not the
  code-mode product surface (per §1/§8 the agent reaches tools *through Monty*, not via an imperative
  host API) — it stays as a test seam, clearly labeled, and is not mistaken for the product.

## 7. Risks / honest costs

- **V1 is big.** Mitigation: the internal V1a/V1b split (§4), and tiny per-step build order inside the
  milestone. The size is a *deliberate* consequence of refusing a throwaway scaffold.
- **Proc-macro early.** Advanced Rust arrives in V1 instead of M4. PRODUCT.md §21 already sequences the
  macro early (step 2); the principle forces it. V1 includes a from-scratch macro intro.
- **The `duckdb` build cost** (always-on C++ compile) is paid from V1 — already the case after this
  session's change; absorbed by `Cargo.lock` + CI caching.

## 8. Next steps (after approval)

1. Rewrite `docs/roadmap/README.md` to the V1–V10 spine + the principles in §2 and the PRODUCT.md
   anchoring in §3.
2. Rewrite the per-milestone files (`M0`–`M10` → `V1`–`V10`), DEEP for V1–V3 (the path to a working,
   safe, governed slice), SKETCH for V4–V10, each ending in a runnable **Done when**.
3. Update the `droplet-roadmap` memory (it still says "first working agent at M3").
