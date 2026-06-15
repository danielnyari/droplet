# Droplet — Product Spec (full)

> Supersedes all earlier specs. This is the source of truth.
> Personal / open-source project. Distributed from day one. The "v1 scope" callout (§16) marks
> what ships first; the rest is the target the architecture is built toward.

---

## 1. What Droplet is

Droplet is a runtime that lets AI agents do **real data analysis in code mode** over an
organization's data, while touching the production data engines **as little as possible**. It does
this with a hard split: a **bounded, engine-agnostic load** pulls a scoped slice of data out of the
source (Athena, Snowflake, BigQuery, Iceberg, S3 — the agent never knows which) into a **local,
ephemeral analysis environment**, where the agent then writes **free-form analysis code** against
that local copy without ever touching the source again. It is framework-agnostic, ships its own
Python SDK, runs agent code in [Monty](https://github.com/pydantic/monty), and is distributed by
default.

It is explicitly **not** a text-to-SQL layer. The agent does not send questions to production; it
pulls data once and computes locally.

Name: the Trisolaran probe. Package: `droplet`.

---

## 2. The core idea — load / analyze separation

Everything follows from one separation:

- **LOAD** is the only thing that touches a source engine. It is bounded, governed, typed,
  engine-agnostic, and content-addressed-cached. Each scoped slice is unloaded **once** and reused
  across runs and pods.
- **ANALYZE** runs against the *local materialized copy* in an ephemeral DuckDB. It is deliberately
  **unrestricted** — arbitrary local SQL, a dataframe API, and the agent's own Python — because
  there is nothing to protect: it is local, ephemeral, per-session-isolated, and resource-capped.

The inversion is the value. Text-to-SQL sends every question to production. Droplet sends **one
bounded unload** to production and then runs **unlimited code locally**. Restrict the boundary,
liberate the analysis.

---

## 3. Architecture

```
┌────────────────────────────────────────────────────────────────────────────────┐
│  consumer agent — ANY framework (pydantic-ai / langchain / raw tool calls)       │
│  writes code-mode Python  ───────────────►  Droplet Python SDK                   │
└──────────────────────────────────────┬───────────────────────────────────────────┘
                                        │  PyO3
┌────────────────────────────────────────▼──────────────────────────────────────────┐
│  DROPLET RUNTIME  —  Rust core, embeds the `monty` crate  —  STATELESS POD          │
│                                                                                     │
│   Session mgr ──► per run: ephemeral DuckDB + read-only SurrealDB (field search)    │
│                                                                                     │
│   ┌── Monty sandbox (agent code) ──┐     ◄── AUTO-BOOTSTRAP ──┐                      │
│   │  calls external fns            │      #[droplet_tool] macros → regs + stubs     │
│   └──────────────┬─────────────────┘      catalog schemas → Literal / TypedDict     │
│                  │ external-fn calls (suspend / resume at boundary)                  │
│        ┌─────────┴───────────────────────────────────────────────────────────┐     │
│        │  ANALYZE  (local, UNRESTRICTED)                                       │     │
│        │   DuckDB over local parquet · dataframe prims · local_sql · Python    │     │
│        └─────────▲─────────────────────────────────────────────────────────────┘    │
│                  │ local parquet                                                     │
│        ┌─────────┴───────────────────────────────────────────────────────────┐     │
│        │  LOAD  (engine-agnostic, GOVERNED)                                    │     │
│        │   catalog → connector → source-native UNLOAD to parquet → pick up     │     │
│        └─────────┬───────────────────────────────────────────────────────────┘     │
└──────────────────┼───────────────────────────────────────────────────────────────────┘
                   │ unload / read parquet
        ┌──────────▼──────────┐   ┌──────── STATE PLANE (shared, distributed) ────────────┐
        │  SOURCE ENGINES     │   │  ArtifactStore (S3): content-addressed parquet         │
        │   Athena   UNLOAD   │   │     = load cache  +  analysis intermediates            │
        │   Snowflake COPY    │   │  SnapshotStore (S3): REPL + manifest blobs (zstd)      │
        │   BigQuery EXPORT   │   │  CoordinationStore (Redis / DynamoDB):                 │
        │   Iceberg/S3 read   │   │     run registry · leases · cache index                │
        └─────────────────────┘   └──────────────────────────────────────────────────────────┘

  Request lifecycle:
    load(dataset, scope)  ─► [connector unload → parquet → local DuckDB]   once; cached fleet-wide
        └─► free-form analysis code over local Dataset handles             unlimited; no engine contact
              └─► export(result → parquet / Iceberg)                        governed
                  + per-step snapshot → state plane                         resume on any pod
```

Pods are stateless and interchangeable. A run executes on one pod at a time; its state (snapshot +
materialized parquet) lives in the shared state plane, so any pod can resume it. No session affinity.

---

## 4. Why it exists

- **Code mode beats querying for analysis.** Real analysis is an algorithm — iteration, branching,
  custom scoring, multi-step derivation — not a single query. The agent writes that algorithm.
- **Take load off production.** The source engine performs one native bulk unload (its cheapest
  operation), to object storage; the agent then iterates locally without ever re-hitting it.
- **The agent must not know or care which engine backs the data.** One unified API over all sources.
- **The agent must not be able to mess up.** Typed, schema-derived tools; a wrong field fails at
  type-check before anything runs; the load boundary is scoped and governed.

---

## 5. Who it's for

Builders adding data analysis to AI agents over an org's warehouse/lakehouse data, who need to
protect production from agent query load and want the agent to analyze freely and safely. Any agent
framework; Monty is the only framework-ish dependency.

---

## 6. LOAD — the engine-agnostic production boundary

- The agent references **logical datasets** from the catalog. The engine binding is configuration it
  never sees.
- A **connector** translates a scoped load into the source's **native bulk unload to parquet** on
  object storage, which Droplet then picks up into local DuckDB:
  - Athena → `UNLOAD (SELECT …) TO 's3://…' WITH (format='PARQUET')`
  - Snowflake → `COPY INTO 's3://…' FROM (SELECT …) FILE_FORMAT=(TYPE=PARQUET)`
  - BigQuery → `EXPORT DATA OPTIONS(format='PARQUET', uri='gs://…') AS SELECT …`
  - Iceberg / S3 → already parquet, read directly (no unload)
- Uniform output (parquet on object storage) → everything downstream is identical regardless of
  source.
- The unloaded parquet **is** the content-addressed cache artifact, keyed by `hash(scoped query +
  source + freshness token)`, reused fleet-wide. One unload, unlimited local reuse.
- The load is bounded and typed: `columns` and `where` are `Literal`-typed against the dataset's
  schema (auto-bootstrapped, §8). The agent cannot express an out-of-scope load.

Agent-facing call (engine-agnostic):

```python
usage = load("usage_daily",
             columns=["account_id", "day", "active_minutes", "feature_adopts"],
             where=[eq("segment","enterprise"), eq("region","EU"),
                    between("day","2025-10-01","2026-03-31")],
             as_of="latest")          # → Dataset(local)
```

---

## 7. ANALYZE — local, unrestricted code mode

Once data is local, the agent writes free-form analysis. The surface is wide open because there is
no production to protect:

- **Dataframe primitives** over local `Dataset` handles: `filter_rows`, `group_agg`, `join`,
  `with_column`, `window`, `sort`, `distinct`, `describe`, `scalar`, `to_rows`. Heavy ops run in
  local DuckDB over handles; data stays host-side.
- **`local_sql(sql, datasets=…)`** — arbitrary DuckDB SQL over local datasets. Unrestricted; it is
  local and ephemeral.
- **The agent's own Python** — loops, branches, custom math, accumulation, ranking — over small
  results pulled into the sandbox via `to_rows` / `scalar`.

This is the code mode and where DX matters. The agent can iterate, recompute, branch, and join freely
without ever contacting a source engine again.

```python
agg = group_agg(usage, by=["account_id"], metrics={
    "base":   ("active_minutes","mean", between("day","2025-10-01","2025-12-31")),
    "recent": ("active_minutes","mean", between("day","2026-01-01","2026-03-31")),
    "adoption": ("feature_adopts","mean")})
ranked = []
for r in to_rows(agg):
    if not r["base"]: continue
    drop  = (r["base"] - r["recent"]) / r["base"]
    score = 0.7*max(drop,0) + 0.3*(1 - min(r["adoption"]/5.0, 1.0))
    if score > 0.4:
        ranked.append({**r, "score": round(score,3)})
ranked.sort(key=lambda x: x["score"], reverse=True)
```

---

## 8. Auto-bootstrap — how the tool surface gets into Monty

No hand-maintained registry or stubs. Two generators feed Monty per session:

- **Fixed primitives** carry a `#[droplet_tool]` proc-macro that emits, at compile time, both the
  Monty external-function registration **and** the Python type-stub fragment from the Rust signature.
  Author the function once; it is registered and typed automatically.
- **Schema-derived types** are generated at session open: the runtime introspects the session's
  catalog (dataset schemas + Pydantic models) and emits the data-dependent stub fragments — per-dataset
  field `Literal`s, row `TypedDict`s, and the specialized `load(...)` signature for the datasets in
  scope.

The two merge into one stub bundle + one external-function table, handed to Monty as
`type_check_stubs` + `external_functions`. The agent gets a fully-typed, schema-aware API with zero
parallel maintenance. The bundle is per-session (the surface depends on the catalog in scope) and
versioned (so snapshot/resume regenerates an identical surface).

**Execution:** when agent code calls an external fn, Monty suspends at the boundary, the host runs
the primitive against the session's local DuckDB state (keyed by handle args), and resumes. Data
stays host-side; only handles and capped results cross — which is also what keeps snapshots small.

---

## 9. Catalog & connectors

- **Catalog (config; hidden from the agent):** logical datasets, each bound to a source connector +
  scope/policy + schema (introspected from the source or declared via Pydantic). Powers `load`
  typing, semantic field search, and governance.
- **Connectors (per engine):** implement a `Source` trait — given a scoped load, produce parquet on
  object storage (native unload, or direct read for Iceberg/S3). Adding an engine = one connector;
  nothing else changes.
- **Semantic field search:** a read-only, schema-derived SurrealDB vector index over field
  names/descriptions/types, exposed as `search_fields`. Rebuilt from the catalog; shared read-only.

---

## 10. Tool surface (reworked)

Discovery: `list_datasets()`, `describe_dataset(name)`, `search_fields(q)`.
Load (engine-agnostic, governed): `load(name, columns, where, as_of) -> Dataset`.
Analyze (local, unrestricted): `filter_rows`, `group_agg`, `join`, `with_column`, `window`, `sort`,
`distinct`, `describe`, `scalar`, `to_rows`, `local_sql`.
Filters: `eq`, `gt`, `lt`, `gte`, `lte`, `in_`, `between`, `contains` (first arg is a `Literal`
field name).
Export: `export(source, dest, schema) -> ExportResult` — parquet on object storage or Iceberg
write-back; governed, schema-validated.

Every name above is auto-bootstrapped (§8) and typed against the session's catalog.

---

## 11. Distributed state plane

Three stores; immutable data is content-addressed in object storage, mutable coordination is in a
strongly consistent store:

- **ArtifactStore (S3):** immutable, content-addressed parquet — the load cache + analysis
  intermediates.
- **SnapshotStore (S3):** immutable, content-addressed, zstd-compressed snapshot blobs.
- **CoordinationStore (Redis or DynamoDB):** run registry (`run_id` → snapshot pointer, status),
  **leases** (one active worker per run, short TTL, reassignable — not affinity), and the **cache
  index** (`cache_key` → `artifact_key`).

---

## 12. Snapshot / resume

- Snapshot = **Monty REPL bytes + session manifest** (catalog ref, loaded-dataset refs + their cache
  keys, materialized intermediate keys). **Never serialize engine heaps.** Resume rebuilds the local
  DuckDB from the manifest by re-attaching the cached parquet (cheap; registers views).
- Immutable, content-addressed, versioned (snapshot-format + catalog version), zstd-compressed.
- Write-behind during execution; durability barrier at step boundary / suspend.
- Per `run_code` step; lease-guarded; resumable on any pod.

---

## 13. Freshness / caching

Per-dataset, configurable, one cache key — `hash(scoped query + source + freshness_token)` — where
only the token varies:

- **Versioned** (default) — token from the source's version signal: Iceberg snapshot id, S3
  ETags/version-ids, or a watermark. Auto-invalidates when the data changes.
- **TTL(duration)** — token = `floor(now / ttl)`; reuse for the window (good when the upstream
  already lags).
- **Passthrough** — never cache.

Iceberg snapshots double as the `as_of` token for reproducible, cacheable time-travel.

---

## 14. Isolation & safety

- **Load is the only governed boundary** — scoped, typed, policy-checked, credential-scoped; the only
  thing that touches a source. There is no arbitrary SQL against production, ever.
- **Analyze is unrestricted because it is local** — per-session ephemeral DuckDB on pod-local tmpfs,
  wiped on close; resource-capped; no cross-session shared mutable state.
- **Export is governed** — destination allow-list, scoped creds, schema-validated, audited.
- Per-run isolation: one run = one Session = ephemeral local engine + a unique working dir.

---

## 15. Invariants — DO NOT VIOLATE

1. The agent never sees the source engine. All sources are reached through connectors that normalize
   to parquet; the agent works against logical, local datasets only.
2. Only `load` touches a source engine, and only via a bounded, typed, cached unload. No arbitrary
   SQL against production.
3. Analyze runs solely against the local materialized copy; it is unrestricted but cannot reach a
   source.
4. The tool surface is auto-bootstrapped: `#[droplet_tool]` macros for fixed primitives + catalog-
   derived types for the rest. No hand-maintained registry or stubs.
5. Snapshot = REPL bytes + manifest only; never serialize engine heaps; reconstruct DuckDB from the
   manifest on resume. Snapshots immutable, content-addressed, versioned, compressed.
6. Boundary discipline: only `to_rows`/`scalar`/load-samples move rows into the sandbox, capped;
   everything else is handles. Keeps snapshots small.
7. Distributed by default: immutable data content-addressed in object storage, mutable coordination in
   the consistent store; resume lease-guarded; no affinity.
8. `droplet-core` (Rust) does not depend on `pyo3`; PyO3 lives only in `droplet-py`. Framework-agnostic
   core (depends on `monty` + engines, never an agent framework).
9. DuckDB is synchronous → `spawn_blocking`; release the GIL at the PyO3 boundary during query work.
10. One error type at the boundary: `thiserror` in libraries, `anyhow` at binaries; all engine errors
    fold into `DropletError`.

---

## 16. v1 scope

**In v1:**
- Connectors: S3 + Iceberg (direct parquet) and **Athena** (`UNLOAD` to S3) — proves the unified
  abstraction with one real unload engine.
- Local analyze surface (dataframe prims + `local_sql` + Python), auto-bootstrap, `load`, discovery,
  `search_fields`, `export` to S3 parquet.
- Distributed state plane (S3 artifact + snapshot stores; Redis + DynamoDB coordination); content-
  addressed cache with per-dataset freshness; lease-guarded cross-pod resume.
- Python SDK + one example framework adapter (pydantic-ai).

**Deferred:** Snowflake + BigQuery connectors; Iceberg write-back on export; the metric/semantic
modeling layer beyond field search; managed-tier features (build only the pluggable seams).

---

## 17. Repo structure

```
droplet/
├── Cargo.toml                # virtual workspace (resolver = "3", edition 2024)
├── crates/
│   ├── droplet-core/         # Session, Monty driver + auto-bootstrap, DuckDB, read-only Surreal,
│   │                         #   Arrow, snapshot subsystem, Source/ArtifactStore/SnapshotStore/
│   │                         #   CoordinationStore traits, the #[droplet_tool] macro. NO pyo3.
│   ├── droplet-connectors/   # Source impls: s3, iceberg, athena (snowflake/bq deferred)
│   └── droplet-py/           # PyO3 cdylib → Python SDK wheel (maturin)
├── python/droplet/           # Python API (Catalog, Session, backend + connector config)
├── adapters/droplet-pydantic-ai/
└── xtask/
```

---

## 18. Tech stack

- Rust edition 2024, resolver 3.
- **Monty** (`monty` crate) — sandbox, embedded; snapshot/resume; type-check stubs (ty).
- **DuckDB** (duckdb-rs) — local analysis engine + parquet pickup; prefer prebuilt lib over `bundled`;
  load ICU at runtime.
- **SurrealDB** (embedded, read-only) — field-search vector index.
- **Arrow** (arrow-rs) — interchange.
- **Tokio** — async (connectors/IO async; DuckDB sync via `spawn_blocking`).
- **serde** + compact binary + **zstd** — snapshots.
- **thiserror / anyhow** — errors.
- **PyO3 + maturin** — Python SDK wheel.
- **pydantic** — SDK schema DSL.
- Clients: object store (S3), Redis, DynamoDB, Athena (+ Snowflake/BigQuery later).

---

## 19. Positioning

- **vs ClickHouse MCP / text-to-SQL tools:** they send every question to production as SQL. Droplet
  sends one bounded unload to production, then runs unlimited local code. Different problem solved:
  load protection + real analysis, not query forwarding.
- **vs Snowflake semantic layer:** warehouse-locked text-to-SQL as a Q&A service. Droplet is
  engine-agnostic, the agent writes typed code-mode analysis over local data, and no question ever
  reaches the source after the load.
- **vs Moose:** a framework for engineers to build analytical backends. Droplet is for agents to
  analyze existing data safely at run time, engine-agnostic, with load protection.

One line: an engine-agnostic, load-protecting, code-mode data-analysis runtime for agents — pull a
bounded slice once, analyze it freely and safely in local code.

---

## 20. Success criteria (v1)

In a multi-pod deployment, from the Python SDK with no agent framework: register a catalog with an
Athena-backed dataset, then have an agent `load` a scoped slice (one Athena `UNLOAD`, cached
fleet-wide) and write a multi-step analysis program over the local copy — group, derive, branch,
score, rank — touching Athena zero further times, with a wrong field caught at type-check before
execution, the session snapshotted to the shared store and resumable on a different pod that rebuilds
DuckDB from the manifest, and a second run on another pod reusing the cached unload instead of
re-hitting Athena.

---

## 21. Build order (seed for the roadmap)

1. Workspace skeleton; `Session` + handle registry + `DropletError`; the store traits
   (`Source`, `ArtifactStore`, `SnapshotStore`, `CoordinationStore`) with local/in-memory dev impls.
2. `#[droplet_tool]` macro + the auto-bootstrap pipeline (compile-time stub fragments + runtime
   schema-derived types → Monty stub bundle + external-fn table).
3. Local analyze engine: DuckDB over local parquet; dataframe primitives + `local_sql`; capped
   results; `spawn_blocking`.
4. Monty driver: `run_code` loop, external-fn dispatch, type-check-before-run.
5. Connectors: S3 + Iceberg (direct), then Athena (`UNLOAD` → parquet); `load` + content-addressed
   cache + freshness policy + cache index.
6. ArtifactStore (S3) + CoordinationStore (Redis, then DynamoDB): registry + leases.
7. SnapshotStore (S3): REPL + manifest, zstd, content-addressed, write-behind; cross-pod resume
   rebuilding DuckDB from the manifest.
8. Read-only SurrealDB field search + `search_fields`; catalog-derived typing end to end.
9. Python SDK polish + pydantic-ai adapter; distributed integration test (two pods, shared cache,
   cross-pod resume, one Athena unload).
```