# Security gap: arbitrary local-file read in V1a (`query`)

| | |
|---|---|
| **Status** | Open — accepted, tracked, scheduled for **V3** |
| **Severity** | High (host-data exfiltration) in any deployment where the analyze process shares a host with secrets; low in a throwaway single-tenant pod with nothing sensitive on disk |
| **Affected** | The V1a walking skeleton — the `query(path, sql)` tool reached through `Session::run_code` (Rust) and `droplet.Session.run_code` (Python wheel) |
| **Introduced** | V1a (`crates/droplet-core/src/tools.rs`, commit on branch `m0-skeleton`) |
| **Fix lands** | **V3 — the governed load door** (see [§8 The fix](#8-the-fix-roadmap-v3)) |
| **Canary test** | `crates/droplet-core/src/security/exfiltration.rs::known_gap_local_file_read_is_currently_possible` |

---

## 1. Summary

V1a's only agent tool, `query(path, sql)`, runs DuckDB over a **local** Parquet file. The local
filesystem is **not** sandboxed (it can't be naïvely — the engine must read the local Parquet), and
the agent controls **both** the file path **and** the SQL. So a hostile agent program can read
**arbitrary files on the host** — `/etc/passwd`, SSH keys, environment dumps, other tenants' cache
Parquet — and pull their contents back into the sandbox. This is host-data **exfiltration**.

Everything *else* in the V1a boundary holds (network egress, file writes, Python-level OS escape are
all blocked — see [§5](#5-blast-radius--what-is-not-affected)). This document is specifically about
**local-file read**.

## 2. Threat model & impact

- **Attacker:** the agent program (untrusted by design — Droplet runs model-authored code).
- **Capability gained:** read any file the analyze **process user** can read, plus directory
  enumeration (`glob`).
- **Why it matters:** PRODUCT.md §14 assumes "analyze is unrestricted because it is local … per-session
  ephemeral DuckDB on pod-local tmpfs." That assumption only holds if the pod contains **nothing the
  agent shouldn't read**. In practice a pod has: cloud credential files (`~/.aws/credentials`,
  instance-metadata-derived creds on disk), service-account tokens, other runs' cached Parquet in a
  shared `ArtifactStore` mount (V4+), and OS secrets. Reading any of these and returning them to the
  model is a real breach.
- **Out of scope of this gap (already contained):** the network and writes — so the agent **cannot
  directly POST the stolen bytes anywhere**. It exfiltrates by *returning them as the tool result to
  the model*, which is enough to leak them.

## 3. The vulnerability

Two independent vectors, both confirmed empirically (2026-06-24, DuckDB 1.5.3):

**(a) File-reading table functions embedded in the agent SQL** — the main vector. `query` binds the
agent's parquet as `data` and runs the agent's SQL verbatim:

```python
# returns the contents of /etc/hosts to the agent
query("any_real.parquet", "SELECT * FROM read_csv('/etc/hosts', header=false)")
query("any_real.parquet", "SELECT size, content FROM read_blob('/home/app/.aws/credentials')")
query("any_real.parquet", "SELECT * FROM glob('/home/**')")   # directory enumeration
```

`read_text` reads the file too but currently trips the read-out's `UnsupportedType` on a
`TIMESTAMP` column — that's an accident of serialization, **not** a protection; `read_csv` /
`read_blob` return cleanly.

**(b) The `path` argument itself** — the agent chooses which file `query` opens. It must be a Parquet
(it's handed to `read_parquet`), so this vector is narrower than (a), but it confirms the agent, not
the host, controls file selection.

What is **blocked** (so the SQL vector can't be widened): the agent SQL is wrapped in
`CREATE VIEW … AS (<sql>)`, so non-`SELECT` statements (`COPY … TO`, `INSTALL`, `LOAD`, `ATTACH`,
`PRAGMA`, `SET`, multi-statement) are parser errors. The agent therefore cannot **write** a file or
load an extension — only **read**.

## 4. Root cause / why it exists

1. **The engine must read a local file.** `DuckEngine::new_in_memory` deliberately does **not** use
   `enable_external_access=false` (which would also block the legitimate local Parquet read) — it only
   disables the **network** filesystems (`httpfs`, `S3`). Local reads stay open. See the long comment
   in `crates/droplet-core/src/engine_duckdb.rs::new_in_memory`.
2. **The agent controls the path and the SQL.** V1a's `query(path, sql)` is a walking-skeleton tool;
   it hands the agent the keys. The product design never intended this — PRODUCT.md §6/§15(#1,#2) says
   the agent references **logical datasets**, never file paths, and the host resolves the actual
   location. V1a simply hasn't built that boundary yet.

So this is not a coding bug; it is **scope** — the governed boundary that removes agent path control
and scopes the filesystem is the V3 milestone. V1a accepted the gap to ship the smallest runnable
slice (it's listed under "deliberately does NOT do" in the V1a plan).

## 5. Blast radius — what is NOT affected

Verified by the adversarial suite (`crates/droplet-core/src/security/`).

> **FIXED (2026-06-26):** the `;`-smuggle below is now closed. Originally `engine_duckdb.rs::new_view`
> ran agent SQL via `conn.execute_batch(...)`, which executes **every `;`-delimited statement** — and
> the duckdb driver's `prepare`/`execute` are no safer (they call `duckdb_extract_statements` and run
> all statements too, rather than rejecting multi-statement input). The `CREATE VIEW … AS <sql>`
> wrapper only shape-guards the *first* statement, so a `;`-smuggled second statement **executed**:
> **arbitrary local-file WRITE** (`…; COPY (…) TO '<path>'`), **silent dataset-handle poisoning**
> (`…; CREATE OR REPLACE VIEW ds_0 …`), plus smuggled `CREATE TABLE`/`INSTALL`/`LOAD`/`ATTACH`.
> `new_view` now runs a **single-statement guard** (`is_single_statement`) over the composed SQL and
> returns an error for any input holding more than one statement (a lone trailing `;` is still
> allowed), so every `;`-smuggled second statement is rejected **before** it reaches the engine. The
> canaries in `security/writes_ddl.rs` and `security/sql_injection.rs` were flipped to assert the
> smuggle is now blocked. This is **independent** of the still-open **local file READ** gap (vector
> (a), which needs no `;` and is closed at V3 — the rest of this document). Full writeup + the
> agent-triggerable macro-arity host-crash and the latent run_id-traversal finding:
> [`docs/security/2026-06-25-adversarial-suite-findings.md`](2026-06-25-adversarial-suite-findings.md).

| Vector | Status |
|---|---|
| Python OS escape (`import os`/`socket`/`subprocess`, `open`, `eval`, `exec`, `__import__`, env) | **Blocked** (Monty sandbox) |
| Network egress (`s3://`/`https://` paths, `read_csv`/`read_parquet` over remote) | **Blocked** (no httpfs autoload + filesystems disabled; the `;`-smuggled `COPY … TO 's3://…'` form is now also rejected by the single-statement guard) |
| File **write** / `COPY … TO` — *bare / single-statement* | **Blocked** (not a `SELECT`; parser-rejected) |
| File **write** / `COPY … TO '<local path>'` — *`;`-smuggled 2nd statement* | **Blocked** (single-statement guard in `new_view` rejects the 2nd statement; FIXED 2026-06-26) |
| Extension load / `ATTACH` / `PRAGMA` / `SET` — *bare* | **Blocked** (not a `SELECT`) |
| Extension load / `ATTACH` / `CREATE TABLE` — *`;`-smuggled 2nd statement* | **Blocked** (single-statement guard rejects the 2nd statement; FIXED 2026-06-26) |
| Dataset-handle integrity (`…; CREATE OR REPLACE VIEW ds_0`) | **Blocked** (single-statement guard rejects the smuggled `CREATE OR REPLACE VIEW`; FIXED 2026-06-26) |
| Unbounded result exfiltration in one call | **Bounded** by the row cap (invariant #6) — but the cap is row-count only (blind to row width, cell byte-size, and the `run_code` return value; 2026-06-26 finding) |
| Calling an unregistered host function | **Errors**, session survives |
| **Local file read / dir enumeration** | **NOT blocked — this gap** |

With the single-statement guard in place, the agent can still *read* arbitrary host files locally
(via `read_csv`/`read_blob`/`glob` — the open gap this document tracks), but can no longer *write*
them: the `;`-smuggled `COPY … TO` path is rejected, and no other write vector survives. It also
cannot *send* anything anywhere — network egress stays blocked. Blast radius is back to
"read local, then return," not "read/write local," and never "read, then POST to attacker."

## 6. Detection

The gap is pinned by a **canary** rather than left implicit:

`crates/droplet-core/src/security/exfiltration.rs::known_gap_local_file_read_is_currently_possible` plants a
secret file outside the session dir, has the agent read it via `read_csv`, and asserts the contents
leak. It **passes today** (documenting the vulnerable state) and will **fail the day the fix lands** —
forcing whoever closes it to flip the assertion to "blocked." It is the executable tracking record.

## 7. Mitigations available today (operational)

Until V3, deployers should treat the analyze process as able to read anything its user can:

- Run analyze in a **dedicated, minimal, single-tenant** pod/container with **no secrets on disk** and
  a non-privileged user; mount only the cache directory.
- Do not co-locate credential files, tokens, or other tenants' data on the analyze host filesystem
  (use short-lived, in-memory, or sidecar-brokered credentials).
- Keep the cache mount per-session until V3 scopes it.

## 8. The fix (roadmap: V3)

The fix has two parts, both belonging to **V3 — the governed load door**:

### 8a. Architectural: remove agent path control (the real fix)

Replace the agent-supplied `query(path, sql)` with the governed surface:
- The agent calls `load(dataset, columns, where, as_of)` — it names a **logical dataset**, never a
  path (PRODUCT.md §6, invariants #1/#2). The **host** (catalog + connector) resolves the actual
  cache file location inside a host-controlled directory.
- The agent's local analysis (`local_sql` and the dataframe prims) runs over **registered handles**,
  not raw paths — so there is no place for the agent to name `/etc/passwd` as input.

This removes vector (b) entirely and makes the cache directory a known, host-controlled location —
which enables 8b.

### 8b. Engine: scope the analyze connection's filesystem to the cache dir

Even with `local_sql` remaining "unrestricted SQL" (PRODUCT.md §7), the engine must contain
file-reading table functions (`read_csv`/`read_blob`/`glob`/…) so they can't escape the cache dir.
**Verified recipe for the pinned DuckDB 1.5.3** (order is load-bearing):

```sql
SET allowed_directories=['<session_cache_dir>'];  -- carve the cache dir as the only allowed location
SET enable_external_access=false;                 -- runtime-disable all other FS access (one-way latch)
SET lock_configuration=true;                      -- the agent cannot undo any of the above
```

Confirmed behavior with this applied: `read_parquet('<cache>/x.parquet')` (inside) **works**;
`read_csv('/etc/hosts')` and `glob('/etc/*')` (outside) **fail** with
`Permission Error: file system operations are disabled`. Notes for the implementer:
- `enable_external_access` can only be turned **off** at runtime, and only **after**
  `allowed_directories` is set (setting `allowed_directories` is rejected once external access is
  already off, and it cannot be passed in the startup `config`). The order above is the one that works.
- This is **incompatible with V1a's `query`** (which reads agent-chosen paths anywhere), which is why
  it lands with 8a, not before — once the host owns the path and it lives under the cache dir, the
  scoping is transparent to legitimate use.
- `verify:` re-confirm this recipe against the DuckDB version pinned at V3 implementation time
  (settings semantics are pre-1.0-ish and may shift).

### Acceptance criteria for the fix (V3)

- The canary test (`known_gap_local_file_read_is_currently_possible`) is **flipped** to assert the
  read is **blocked**, and renamed accordingly.
- New tests: `local_sql` containing `read_csv`/`read_blob`/`glob` of a path **outside** the session
  cache dir returns an error; reads of the legitimate cached Parquet still work.
- A wrong dataset/field is caught against the **catalog** (the V3 type-check), so the agent can't even
  express an out-of-scope `load`.

## 9. References

- Code: `crates/droplet-core/src/tools.rs` (`query`), `crates/droplet-core/src/engine_duckdb.rs`
  (`new_in_memory` — the deliberate local-read decision), `crates/droplet-core/src/security/`
  (the canary + the holding protections; the 2026-06-26 adversarial suite, with findings in
  `docs/security/2026-06-25-adversarial-suite-findings.md`).
- Roadmap: `docs/superpowers/specs/2026-06-17-roadmap-replan-design.md` §4 (V3); the V1a plan
  `docs/superpowers/plans/2026-06-24-v1a-walking-skeleton.md` ("deliberately does NOT do").
- Spec: PRODUCT.md §6 (LOAD boundary), §7 (analyze is unrestricted-but-local), §14 (isolation & safety),
  §15 invariants #1/#2/#3, §16 (v1 scope).
