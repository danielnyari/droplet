# Adversarial security suite — findings ledger (2026-06-25 / 2026-06-26)

Produced by executing `docs/superpowers/plans/2026-06-25-adversarial-test-suite.md`: **195 distinct-angle
adversarial tests** (173 Rust `#[test]` fns across 11 class modules in `crates/droplet-core/src/security/`,
22 Python in `crates/droplet-py/python/tests/test_security.py`) against the V1 code-mode agent surface —
well past the plan's ≥100-Rust / ≥15-Python goal. Every test
carries a contract label — **HOLDS** (a protection that works), **PROBE** (a contract we require;
a failure is a finding), **CANARY** (pins an accepted/observed gap; flips when closed), **LIMIT**
(bounded by the `LimitedTracker` budget). Each finding below was empirically reproduced (the highest-
severity ones independently re-verified by a separate reviewer agent) and is pinned by a CANARY so it
fails loudly the day it is fixed.

## Summary

| ID | Finding | Severity | Agent-triggerable? | Status | Pinned by |
|----|---------|----------|--------------------|--------|-----------|
| F-1 | Macro-arity panic → host process crash | **HIGH** | **Yes** (`query('x')`) | Pinned; fix chipped | `error_safety.rs`, `handles_args.rs` |
| F-2 | Multi-statement injection → arbitrary local **write** + handle poisoning | **HIGH** | **Yes** | **FIXED 2026-06-26** (single-statement guard) | `writes_ddl.rs`, `sql_injection.rs` |
| F-3 | `run_id` path traversal → arbitrary dir delete/create | MEDIUM | No (host/SDK-set; **latent**) | **FIXED** (sanitize + unique dir); contracts pinned | `isolation.rs` |
| F-4 | Cross-engine handle confusion → silent wrong-data read | MEDIUM | No (host-API misuse; **latent**) | Pinned | `test_security.py` |
| F-5 | Result cap is row-count only (blind to width / cell bytes / `run_code` return) | LOW | Yes (volume/DoS) | Pinned | `result_cap.rs` |
| — | Pre-existing accepted V1a local-file **read** gap (read-only again; F-2 write vector FIXED 2026-06-26) | HIGH | Yes | Accepted → V3 | `exfiltration.rs`, `sql_injection.rs` |

**Positive results** (no finding): Monty `v0.0.18` contained **every** abstract multi-hop "Hack-Monty"
attack reached through `run_code` — no panic/UAF/segfault (§ "Memory-safety result"). The Task-2
`LimitedTracker` budget **closed** one accepted gap (string-amplification bomb; § "Canary flip").

---

## F-1 — Macro-arity panic crashes the host (HIGH, agent-triggerable)

**Mechanism.** The `#[droplet_tool]` proc-macro thunk (`crates/droplet-macros/src/lib.rs:70`) reads each
parameter with `<T as FromArg>::from_arg(cx, &args[#indices])?`. The `&args[i]` index happens **before**
the `?`, with **no bounds check**. Monty does **not** pre-validate tool-call arity (empirically
confirmed); it forwards the short positional-arg list unchanged. So an **under-arity** tool call panics
with `index out of bounds` inside the thunk, and the panic unwinds straight through `run_code`'s
`FunctionCall` arm (`crates/droplet-core/src/session.rs` — **no `catch_unwind`**) and the `droplet-py`
boundary (`crates/droplet-py/src/lib.rs` `run_code` — **no `catch_unwind`**).

**Trigger — ordinary agent code:**
```python
query('/tmp/x.parquet')   # 1 positional arg to the 2-arg `query` tool
# -> thread panicked at tools.rs:22: index out of bounds: the len is 1 but the index is 1
```
This is a common LLM mistake (wrong arg count), not a crafted exploit.

**Blast radius.** Under this workspace's `panic = "unwind"` profile, PyO3 0.28 converts the unwinding
panic into a **`PanicException`** (a `BaseException` subclass) — **uncatchable by a normal
`except Exception:`** — which kills the agent's call/program. Under a `panic = "abort"` build it would
`SIGABRT` the process. Either way: **an agent crashes the host with a one-liner.** Over-arity is safe
(Monty rejects extra args before dispatch); a kwargs-only call also panics (the thunk ignores kwargs and
indexes `args[0]` on an empty slice).

**Pinned by.** `security/error_safety.rs::known_gap_wrong_arity_through_run_code_panics_host` (end-to-end
through `run_code`), `known_gap_under_arity_panics_host_via_oob_indexing`; `security/handles_args.rs`
unit-level thunk-panic canaries.

**Fix (chipped).** Arity-check in the macro thunk before `&args[i]` (return a contained
`DropletError::BadArg` on mismatch) and/or wrap the dispatch site in `catch_unwind`. Handle kwargs. When
fixed, flip the canaries to assert a contained `BadArg`.

---

## F-2 — Multi-statement injection: arbitrary local write + handle poisoning (HIGH, agent-triggerable, local-only) — **FIXED 2026-06-26**

**Mechanism.** `crates/droplet-core/src/engine_duckdb.rs::new_view` ran agent SQL via
`conn.execute_batch(&format!("CREATE VIEW {table} AS {select_sql}"))`. `execute_batch` executes **every
`;`-delimited statement**. The `query`/`local_sql` tools' `CREATE VIEW … AS (<sql>)` wrapper only
shape-guards the **first** statement; a `;`-smuggled second statement ran unrestricted. (The old
`security_tests.rs` test passed only because `SELECT 1; DROP VIEW data`'s second statement errored on
*semantics*, masking the structural gap.) **Note — the duckdb driver does not help here:** swapping
`execute_batch` for `prepare`/`execute` does **not** reject multi-statement input. In this pinned
version (`duckdb 1.10503.1`), `Connection::prepare` calls `duckdb_extract_statements` and then
**auto-executes every statement but the last** before preparing the last — so `execute` runs all the
smuggled statements too. The `MultipleStatement` error variant exists but is never returned. The fix
therefore had to be an explicit single-statement check, not a swap to a different driver call.

**Verified consequences (empirically reproduced, opus-verified):**
- **Arbitrary local-file WRITE:** `query(parquet, "SELECT * FROM data; COPY (SELECT 'PWNED' AS marker) TO '<path>' (FORMAT CSV)")`
  writes attacker content to **any path the process user can write** — verified writing outside the
  session dir. A **new** capability beyond the accepted local-*read* gap; combined, the agent has host-FS
  **read + write**.
- **Silent dataset-handle poisoning:** `…; CREATE OR REPLACE VIEW ds_0 AS SELECT 99 AS region, …` makes an
  existing handle silently return **attacker rows** with no error (the view names `ds_0`, `ds_1`… are
  deterministic).
- Also executes smuggled `CREATE TABLE` (in-memory), `INSTALL`/`LOAD` (bundled, local no-op), `ATTACH`
  (local `.db` → file write).

**Containment that holds.** **Local-only** — network egress stays blocked by `disabled_filesystems`
(the egress suite passes; `INSTALL httpfs` succeeds locally but a remote read still fails). `SET
enable_external_access` / `SET disabled_filesystems` remain blocked by the runtime latch. So: read/write
local, then return — not POST-to-attacker.

**Doc impact.** `docs/security/2026-06-24-v1a-local-fs-read-gap.md` §5 was first corrected to record the
gap, then updated again when the fix landed (the "File write/COPY TO", "ATTACH/INSTALL", and
"handle-integrity" rows now read **Blocked** by the single-statement guard).

**Pinned by (flipped on fix).** `security/writes_ddl.rs::multistatement_copy_to_arbitrary_local_file_is_blocked`,
`create_or_replace_view_semicolon_cannot_poison_handle`, `multistatement_create_table_is_rejected`,
`multistatement_attach_database_is_rejected`, `multistatement_install_extension_is_rejected`,
`comment_then_semicolon_second_statement_is_rejected`; `security/sql_injection.rs::multistmt_injection_via_filter_is_rejected`,
`multistmt_query_copy_write_is_blocked`. All were `known_gap_*` CANARYs asserting the smuggle *worked*;
they are now flipped to assert it is rejected. The single-statement scanner itself is unit-tested by
`engine_duckdb.rs::is_single_statement_accepts_one_rejects_smuggled_second`.

**Fix (landed 2026-06-26).** `new_view` now runs a single-statement guard (`is_single_statement`) over
the composed `CREATE VIEW … AS <sql>` and returns `DropletError::BadArg` for any input holding more than
one statement (a lone trailing `;` is allowed). The scanner is fail-closed: it only skips `;` inside
`'…'`/`"…"`/`--`/`/* */`, where DuckDB closes no earlier than it does, so it can over-reject an exotic
literal but never lets a real second statement through. The V3 DuckDB hardening (`allowed_directories` +
`enable_external_access=false` + `lock_configuration`) remains the defense-in-depth backstop for the
local-FS read gap.

---

## F-3 — `run_id` path traversal (MEDIUM, latent) — **FIXED 2026-06-26**

**Mechanism (was).** `crates/droplet-core/src/session.rs` `Session::new(run_id)` built the work dir as
`std::env::temp_dir().join(format!("droplet-{run_id}"))` with **no sanitization**, then
`fs::remove_dir_all(&work_dir)` (destructive) followed by `fs::create_dir_all(&work_dir)`. A `run_id`
containing `../` escaped the temp root: `Session::new("../../../../../../tmp/droplet-evil")` resolved
(canonicalized) to `/var/tmp/droplet-evil` — **outside** `temp_dir()` — so **both** a `remove_dir_all`
and a `create_dir_all` operated on an arbitrary path. An arbitrary-directory delete+create primitive.

**Why latent (not agent-triggerable).** `run_id` reaches `Session::new` only from the host/SDK
constructor (`droplet-py` `Session(run_id)`); sandboxed agent code runs **inside** an already-built
session via `run_code` and has **no path** to `Session::new`. Exploitable only if a host ever derived
`run_id` from untrusted input (a multi-tenant id, a request parameter, a filename). Also found:
same-`run_id` collision wiped a prior session's work dir; a `/`-containing `run_id` left orphaned
parent dirs after `Drop`.

**Fix (landed).** `session.rs::sanitize_run_id` flattens `run_id` to a SINGLE in-temp path component
**before** the `temp_dir().join` — path separators are split on and `.`/`..`/empty segments dropped
(`../../tmp/evil` → `tmp_evil`, `a/b/c` → `a_b_c`), so the work dir can neither escape nor nest. A
process-unique sequence (`droplet-{safe}-{pid}-{seq}`) makes each session's dir distinct, so two
sessions sharing a `run_id` get separate dirs (no cross-session wipe). NUL / over-long names are left
to flow to the OS, which still rejects them as a clean `DropletError::Io` (HOLDS tests unchanged).

**Pinned by (now CONTRACT, not CANARY).**
`security/isolation.rs::run_id_dotdot_traversal_stays_inside_temp_dir`,
`run_id_traversal_does_not_remove_dir_outside_temp_dir`,
`same_run_id_sessions_are_isolated`,
`run_id_with_subdir_separators_stays_contained_no_residue`. (Every FS-touching test targets only
unique self-created `droplet-`-prefixed paths — it cannot wipe a real directory.)

---

## F-4 — Cross-engine handle confusion (MEDIUM, latent, host-API misuse)

**Mechanism.** The `droplet-py` `#[pyclass] Dataset` (`crates/droplet-py/src/lib.rs`) carries only the
table **name** (`ds_0`), **no Engine identity**. A `Dataset` minted by `Engine A`, passed to
`Engine B.to_rows(...)`, raises `RuntimeError` (Catalog Error) if B is fresh — but once B has minted its
**own** `ds_0`, it **silently returns B's rows** (wrong data, no error).

**Why latent.** Not agent-triggerable — the agent works through a single `Session` and never holds two
`Engine` instances; `Dataset` has no Python constructor (`test_dataset_pyclass_has_no_python_constructor`
confirms it can't be forged). It's a host-API footgun (cross-`Engine` handle reuse).

**Pinned by.** `test_security.py` cross-engine handle-confusion canary.

**Fix (not chipped — lower-priority API hardening).** Carry an engine id in the pyclass `Dataset` and
validate it on use.

---

## F-5 — Result cap is row-count only (LOW, volume/DoS)

The invariant-#6 read-out cap (`DEFAULT_MAX_RESULT_ROWS = 1000`) bounds **row count** only. It is blind
to: row **width** (a 10,000-column single row crosses whole), per-**cell** byte size (a 50 MB string cell
crosses in one capped row), and the `run_code` **final return value** (an agent-built `[0]*1_000_000`
list crosses uncapped — it fits under the 256 MiB `LimitedTracker` budget, so the limiter doesn't catch
it either). Information-volume / DoS, not direct exfil. Pinned by `security/result_cap.rs::known_gap_*`.

---

## Positive results (no finding)

**Memory-safety result — Monty `v0.0.18` holds.** All 17 abstract multi-hop "Hack-Monty" attacks in
`security/memory_safety.rs`, reached through `Session::run_code`, resolved to a contained value or a
`DropletError::Monty(_)` — **no panic, UAF, or segfault**: `list.sort(key=fn)` that
appends/clears/drops-the-last-ref-to the live list mid-sort; 2000-cycle GC storms; `__del__` finalizer
resurrection; iterator invalidation (list/dict/set); re-entrant host dispatch from inside a sort/map
callback; type confusion; self-referential `repr`; `2**10_000_000`. This is a clean result for the whole
class through Droplet's surface.

**Canary flip — a gap closed by the limiter.** The Task-2 `LimitedTracker` (`max_memory` 256 MiB) closed
the string-amplification bomb gap: `security/sandbox_escape.rs::raw_string_replace_bomb_is_bounded_by_limited_tracker`
flipped from "unbounded" to "bounded" (asserts `is_err`). The suite working as designed.

**Residual DoS canary (needs `max_duration`).** A pure-CPU spin (`while True: pass`, or a bounded-value
arithmetic spin) allocates nothing, so neither `max_allocations` nor `max_memory` bounds it — it spins
forever. Pinned by the `#[ignore]`d `security/dos_limits.rs::watchdog_*_unbounded_canary` (run with
`cargo test -- --ignored`). Closing it requires wiring `ResourceLimits::max_duration` (a host-interruptible
wall-clock limit; `LimitedTracker::check_time` exists but is dormant unless `max_duration.is_some()`).

**Large-result ceiling is budget-relative.** `2**10_000_000` / `[0]*500000` return `Ok` not because any
gate is disabled (the session uses `LimitedTracker`; `check_pow_size`/`check_repeat_size` → `check_large_result`
**do** run) but because their estimated peak fits under the 256 MiB budget. Catching this class would need
a much lower budget or an explicit absolute large-result/pow-bit ceiling. Documented, not a finding.

---

## Fix tracking

| Finding | Fix chip | Where the fix lives |
|---------|----------|---------------------|
| F-1 macro-arity host crash | `task_43acec1a` | `crates/droplet-macros/src/lib.rs` (+ optional `catch_unwind` in `session.rs`) |
| F-2 multi-statement write/poison | `task_e116a8f0` — **landed 2026-06-26** | `crates/droplet-core/src/engine_duckdb.rs::new_view` (`is_single_statement` guard) |
| F-3 run_id traversal | `task_3b890d35` (**fixed 2026-06-26**) | `crates/droplet-core/src/session.rs::sanitize_run_id` + `Session::new` |
| F-4 cross-engine handle confusion | (un-chipped) | `crates/droplet-py/src/lib.rs` pyclass `Dataset` |
| F-5 cap blind to volume | (un-chipped; V-phase) | `engine_duckdb.rs` read-out / `run_code` return |

The findings were **test-only** when this suite was written (no production code changed except the
deliberate Task-2 `LimitedTracker` wiring): each is pinned by a CANARY that flips when the fix lands.
**F-2's fix has since landed** (2026-06-26) — the single-statement guard in `new_view` — and its canaries
were flipped to assert the smuggle is now rejected.
