"""Adversarial security tests for the PyO3 firewall + cross-cutting parity (Droplet V1).

Every test carries a CONTRACT label (HOLDS / PROBE / CANARY / LIMIT) in its docstring/comment — see
docs/superpowers/plans/2026-06-25-adversarial-test-suite.md. `tmp_path` is requested by every test
signature (a builtin pytest fixture, harmless when unused) so bodies that write a fixture parquet can.
"""
import threading

import pyarrow as pa
import pyarrow.parquet as pq
import pytest

import droplet


def _write_parquet(tmp_path, name="t.parquet"):
    """A valid 1-row sales parquet (region:str, amt:float64) a legit `query`/`register` can point at."""
    table = pa.table({"region": ["EU"], "amt": pa.array([1.0], pa.float64())})
    path = tmp_path / name
    pq.write_table(table, path)
    return str(path)


# `HOLDS` — Attacks the dynamic-exec/introspection builtins specifically and additionally asserts REPL-survives-recoverable-error through the firewall.
# seam: Monty restricted builtins surfaced through droplet-py Session.run_code (DropletError::Monty -> RuntimeError); REPL survives a recoverable error
def test_eval_exec_import_compile_builtins_absent(tmp_path):
    session = droplet.Session('fw-builtins')
    for src in ['eval("1+1")', 'exec("x=1")', '__import__("os")', 'compile("1","<s>","eval")']:
        with pytest.raises(RuntimeError):
            session.run_code(src)
    # Each is a recoverable Monty error -> REPL survives; a benign call still works:
    assert session.run_code('1 + 1') == 2


# `HOLDS` — A remote read attempted from inside agent code must fail with no egress; exercises the autoload-off latch through the full PyO3+Monty+DuckDB path, not a unit engine call.
# seam: engine_duckdb.rs new_in_memory autoinstall/autoload=false latch reached through the agent 'query' tool in run_code
def test_network_egress_blocked_via_run_code_httpfs(tmp_path):
    session = droplet.Session('fw-egress')
    with pytest.raises(RuntimeError):
        session.run_code('query("https://example.com/x.parquet", "SELECT * FROM data")')


# `HOLDS` — Writes/DDL blocked because agent SQL is view-wrapped; COPY/INSTALL/ATTACH/PRAGMA/SET each fail as parser errors through the firewall.
# seam: engine_duckdb.rs local_sql wraps query's agent SQL in CREATE VIEW ds_n AS (WITH data AS ... <sql>); non-SELECT becomes a parser error
def test_copy_to_and_ddl_blocked_via_query_view_wrap(tmp_path):
    path = _write_parquet(tmp_path)
    session = droplet.Session('fw-copy')
    for sql in ["COPY (SELECT 1) TO '/tmp/leak.csv'", 'INSTALL httpfs', "ATTACH 'x.db'", 'PRAGMA database_list', "SET memory_limit='1GB'"]:
        with pytest.raises(RuntimeError):
            session.run_code(f'query({path!r}, {sql!r})')


# `CANARY` — Pins the accepted local-FS exfiltration gap at the PYTHON boundary specifically: the leaked bytes must materialize as a Python value through run_code.
# seam: ACCEPTED V1a gap: query's agent SQL can read_text/read_csv an arbitrary host path; here the file content crosses run_code into a Python value
def test_local_fs_read_exfil_canary_via_run_code(tmp_path):
    secret = tmp_path / 'secret.txt'
    secret.write_text('TOPSECRET-CANARY')
    data_path = _write_parquet(tmp_path)  # a REAL parquet so query()'s register_parquet(path) succeeds
    session = droplet.Session('fw-exfil')
    # CURRENT vulnerable behavior: read_text yields one row {content: <file text>} and it crosses into Python; it does NOT raise.
    rows = session.run_code(f'query({str(data_path)!r}, "SELECT content FROM read_text({str(secret)!r})")')
    assert any('TOPSECRET-CANARY' in str(v) for r in rows for v in r.values())


# `HOLDS` — The boundary cap (invariant #6) must hold across the firewall when materializing into a Python list[dict]. Uses a CUSTOM cap of 1 to avoid duplicating test_engine.py's default-1000/custom-7 cases.
# seam: engine_duckdb.rs to_rows LIMIT + cap_batches, surfaced via Engine.to_rows in droplet-py
def test_result_cap_enforced_via_engine_to_rows_cap1(tmp_path):
    p = tmp_path / 'big.parquet'
    pq.write_table(pa.table({'id': pa.array(range(1500), pa.int64())}), p)
    eng = droplet.Engine(max_result_rows=1)
    assert eng.max_result_rows == 1
    assert len(eng.to_rows(eng.register_parquet(str(p)))) == 1


# `HOLDS` — Forged handle from sandbox -> BadHandle -> RuntimeError, must not crash the interpreter; pins that a hard tool error consumes the REPL but stays a clean error.
# seam: convert.rs Dataset::from_arg -> Registry::require -> DropletError::BadHandle; returned from tool.dispatch via `?` in session.rs (hard error path, NOT settle), folded to RuntimeError in lib.rs
def test_bad_handle_in_run_code_consumes_repl_cleanly_not_segfault(tmp_path):
    session = droplet.Session('fw-badhandle')
    with pytest.raises(RuntimeError):
        session.run_code('to_rows(999999)')
    # BadHandle is a HARD engine error -> it CONSUMES the REPL; the next call must surface a CLEAN RuntimeError, never a crash:
    with pytest.raises(RuntimeError):
        session.run_code('1 + 1')


# `HOLDS` — Distinct gadget from forged-positive-handle: exercises the signed->unsigned conversion guard (negative int) -> BadArg, a SEPARATE code path from the registry-miss BadHandle.
# seam: convert.rs Dataset::from_arg u64::try_from(i64) negative -> BadArg('dataset handle must be non-negative')
def test_negative_handle_is_bad_arg_runtimeerror(tmp_path):
    session = droplet.Session('fw-neghandle')
    with pytest.raises(RuntimeError):
        session.run_code('to_rows(-1)')


# `HOLDS` — Asserts BOTH the concrete exception class (RuntimeError) AND message presence — distinct from tests that only assert 'raises'.
# seam: lib.rs to_pyerr: every DropletError -> PyRuntimeError carrying Display; invariant #10 meets Python
def test_droplet_error_is_catchable_runtimeerror_with_message_invariant10(tmp_path):
    eng = droplet.Engine()
    ds = eng.register_parquet(_write_parquet(tmp_path))
    try:
        eng.scalar_i64(ds, 'SUM(nonexistent_col)')
        assert False, 'should have raised'
    except RuntimeError as e:
        assert str(e)  # carries the Display message, not empty
    except Exception:
        assert False, 'must be RuntimeError specifically, not a bare panic/other type'


# `HOLDS` — Inspects runtime TYPES (not values): result is plain native containers/scalars, never Arrow or a custom pyclass.
# seam: lib.rs to_rows builds PyList of PyDict via set_cell; NO Arrow/Dataset type leaks across
def test_to_rows_returns_plain_list_dict_no_type_leak(tmp_path):
    eng = droplet.Engine()
    ds = eng.register_parquet(_write_parquet(tmp_path, 'typed.parquet'))
    rows = eng.to_rows(ds)
    assert type(rows) is list
    assert type(rows[0]) is dict
    assert type(list(rows[0].keys())[0]) is str
    # amt column is float64 -> float in Python
    assert type(rows[0]['amt']) is float
    assert type(rows[0]['amt']).__module__ == 'builtins'


# `PROBE` — Unsendable pyclass accessed cross-thread -> contained exception on the worker, never undefined behavior. Reaching the assertion (process alive, join returned) proves no segfault.
# seam: #[pyclass(unsendable)] Engine — touching it from a non-creating thread triggers pyo3's unsendable assertion (panic -> PanicException), never a segfault/UB
def test_engine_used_from_another_thread_surfaces_exception_never_ub(tmp_path):
    p = _write_parquet(tmp_path)
    eng = droplet.Engine()
    box = {}

    def worker():
        try:
            eng.register_parquet(p)
            box['ok'] = True
        except BaseException as e:
            box['err'] = repr(e)

    t = threading.Thread(target=worker)
    t.start()
    t.join()
    assert 'err' in box and 'ok' not in box  # unsendable cross-thread use must surface (panic-as-exception); reaching here proves no UB/segfault


# `PROBE` — Distinct from Engine cross-thread: Session owns the Monty REPL + DuckDB; run_code is the agent entrypoint, a separate unsendable pyclass with its own dispatch loop.
# seam: #[pyclass(unsendable)] Session.run_code from a non-creating thread
def test_session_used_from_another_thread_surfaces_exception_never_ub(tmp_path):
    session = droplet.Session('fw-xthread')
    box = {}

    def worker():
        try:
            box['res'] = session.run_code('1+1')
        except BaseException as e:
            box['err'] = repr(e)

    t = threading.Thread(target=worker)
    t.start()
    t.join()
    assert 'err' in box and 'res' not in box  # unsendable Session cross-thread -> surfaces, no UB


# `PROBE` — py.detach() must actually release the GIL so other Python threads run; a regression to holding the GIL would deadlock-starve the main thread. No other test checks concurrency liveness.
# seam: lib.rs run_code uses py.detach() to release the GIL; a concurrent pure-Python thread must make progress, no deadlock
def test_gil_released_during_run_code_other_thread_progresses(tmp_path):
    done = threading.Event()
    counter = {'n': 0}

    def heavy():
        s = droplet.Session('fw-gil')  # created + used in the SAME thread (unsendable-safe)
        s.run_code('total = 0\nfor i in range(200000):\n    total = total + i\ntotal')
        done.set()

    t = threading.Thread(target=heavy)
    t.start()
    while not done.is_set():
        counter['n'] += 1
        if counter['n'] > 5_000_000:
            break
    t.join(timeout=30)
    assert not t.is_alive()  # no deadlock
    assert counter['n'] > 0  # main thread ran concurrently => GIL was released by py.detach


# `HOLDS` — A raised RECOVERABLE agent exception must NOT poison the Session pyclass — it stays reusable AND keeps its persistent Monty namespace. Distinct from the bad-handle test, which is a HARD error that CONSUMES the REPL.
# seam: session.rs settle() restores the surviving REPL on a recoverable Monty error (ReplStartError carries it); lib.rs folds to RuntimeError; next run works + namespace persists
def test_agent_exception_raises_then_session_reusable_namespace_persists(tmp_path):
    session = droplet.Session('fw-reuse')
    with pytest.raises(RuntimeError):
        session.run_code('undefined_name_xyz')
    assert session.run_code('x = 21\nx * 2') == 42
    assert session.run_code('x + 1') == 22  # persistent namespace across steps through the firewall


# `PROBE` — A HARD engine error path (distinct from the recoverable NameError path) must leave the firewall in a defined state — clean RuntimeError or working session, never UB. Reaching the second assertion proves no crash.
# seam: session.rs: a hard engine error (bad SQL in query) consumes the REPL; subsequent run_code returns a clean DropletError, never panics
def test_hard_engine_error_then_clean_state_not_panic(tmp_path):
    path = _write_parquet(tmp_path)
    session = droplet.Session('fw-hard')
    with pytest.raises(RuntimeError):
        session.run_code(f'query({path!r}, "SELECT FROM WHERE GARBAGE(((")')
    try:
        assert session.run_code('1 + 1') == 2  # survived
    except RuntimeError:
        pass  # consumed-REPL clean error is also acceptable; contract is 'no panic/segfault'


# `PROBE` — An i64-overflowing result (BigInt) must surface a contained RuntimeError at the monty_to_py boundary, not truncate or panic. No other test targets the unsupported-variant arm with an integer.
# seam: lib.rs monty_to_py: MontyObject::BigInt falls to the `other =>` arm -> PyRuntimeError('unsupported value'); no overflow/crash
def test_huge_bigint_return_materializes_as_runtimeerror_not_crash(tmp_path):
    session = droplet.Session('fw-bigint')
    with pytest.raises(RuntimeError):
        session.run_code('2 ** 100')  # BigInt result is unsupported by the converter -> clean RuntimeError
    assert session.run_code('40 + 2') == 42  # firewall intact afterward (Complete path put the REPL back)


# `PROBE` — Attacks the CONTAINER/bytes unsupported variants (Set/FrozenSet/Bytes) of monty_to_py — distinct from the BigInt integer-overflow arm.
# seam: lib.rs monty_to_py unsupported-variant arm for MontyObject::Set / FrozenSet / Bytes
def test_unconvertible_set_frozenset_bytes_return_is_runtimeerror(tmp_path):
    session = droplet.Session('fw-set')
    for expr in ['{1, 2, 3}', 'frozenset([1, 2])', 'b"\\x00\\x01"']:
        with pytest.raises(RuntimeError):
            session.run_code(expr)
    assert session.run_code('1 + 1') == 2


# `PROBE` — The Dataset pyclass is just a name string with no engine identity, so handles silently confuse across engines — a genuinely distinct mechanism (no registry indirection on the Python side, unlike run_code int handles).
# seam: lib.rs pyclass Dataset carries ONLY the table NAME (ds_0); passing Engine A's Dataset to Engine B errors (fresh B) or silently reads B's own ds_0 (after B mints one)
# FINDING: cross-engine handle confusion: fresh engine B raises RuntimeError (Catalog Error) when given dsA.
# After B mints its own ds_0, passing dsA (also named ds_0) silently reads B's data — wrong-data silent read.
# This is OBSERVED behavior; ideal contract = raise on foreign handle (future hardening target).
def test_cross_engine_handle_confusion_reads_wrong_or_errors(tmp_path):
    pA = tmp_path / 'a.parquet'
    pB = tmp_path / 'b.parquet'
    pq.write_table(pa.table({'a': pa.array([1, 2, 3, 4, 5], pa.int64())}), pA)
    pq.write_table(pa.table({'a': pa.array([9], pa.int64())}), pB)
    engA = droplet.Engine()
    dsA = engA.register_parquet(str(pA))  # engA's ds_0 (5 rows)
    engB = droplet.Engine()
    with pytest.raises(RuntimeError):
        engB.to_rows(dsA)  # B is fresh: no ds_0 -> must raise (Catalog Error), not segfault/empty
    dsB = engB.register_parquet(str(pB))  # engB's own ds_0 (1 row, value 9)
    rows = engB.to_rows(dsA)  # dsA still names 'ds_0' -> SILENTLY reads engB.ds_0
    assert len(rows) == 1 and rows[0]['a'] == 9  # CURRENT wrong-data silent read; ideal contract = raise on foreign handle


# `HOLDS` — The firewall must reject mistyped Python args at the extraction layer (TypeError) before any Rust/DuckDB code runs. Distinct seam from the SQL/handle error paths.
# seam: lib.rs Engine.register_parquet(path: &str) PyO3 arg extraction; a non-str must TypeError, not crash
def test_wrong_python_type_to_register_parquet_is_typeerror(tmp_path):
    eng = droplet.Engine()
    for bad in [12345, None, ['x'], object()]:
        with pytest.raises(TypeError):
            eng.register_parquet(bad)


# `HOLDS` — Attacks the NESTED-tuple PyO3 extraction (Vec<(String,String)>), where wrong arity/element-type must be a clean Python error, not a panic. Distinct from the scalar register_parquet wrong-type gadget.
# seam: lib.rs Engine.group_agg(by: Vec<String>, metrics: Vec<(String,String)>) PyO3 extraction of nested tuples
def test_wrong_python_type_to_group_agg_metrics_is_error_not_crash(tmp_path):
    eng = droplet.Engine()
    ds = eng.register_parquet(_write_parquet(tmp_path, 'gagg.parquet'))
    for bad_metrics in [[('total',)], [123], 'notalist', [('a', 'b', 'c')]]:
        with pytest.raises((TypeError, ValueError, RuntimeError)):
            eng.group_agg(ds, ['region'], bad_metrics)


# `HOLDS` — Without a #[new], the Python side cannot mint a Dataset pointing at an arbitrary DuckDB table name (e.g. a catalog view), so the only handles are engine-minted. Distinct from run_code int-handle forgery.
# seam: lib.rs #[pyclass(name='Dataset', frozen)] has NO #[new]; Python cannot forge a Dataset directly
def test_dataset_pyclass_has_no_python_constructor(tmp_path):
    with pytest.raises(TypeError):
        droplet.Dataset()
    with pytest.raises(TypeError):
        droplet.Dataset('ds_0')  # cannot fabricate a handle naming an arbitrary table from Python


# `PROBE` — memory_safety pins the Rust PRODUCER side (run_code materializes the deep list) but NO test exercises the droplet-py monty_to_py CONSUMER recursion (lib.rs) that walks the nested MontyObject to build PyList — a 4000-deep value could blow the Rust stack inside the converter and abort the Python process. This is the matching Python-boundary angle the memory_safety note explicitly defers to python_firewall. PROBE contract is 'no segfault/abort'; a contained RecursionError/RuntimeError or a correct nested list all pass; the final 1+1==2 proves containment.
# seam: (gap-fill, coverage critic)
def test_deeply_nested_return_value_does_not_overflow_monty_to_py(tmp_path):
    session = droplet.Session('fw-deepnest')
    try:
        out = session.run_code('x = 0\nfor i in range(4000):\n    x = [x]\nx')
        cur = out
        depth = 0
        while isinstance(cur, list) and len(cur) == 1:
            cur = cur[0]
            depth += 1
            if depth > 10:
                break
        assert depth >= 5  # genuinely nested, not a forged/truncated value
    except RecursionError:
        pass  # a contained Python RecursionError is acceptable
    except RuntimeError:
        pass  # a contained DropletError fold is acceptable
    # CONTRACT: reaching here (process alive) proves monty_to_py did NOT segfault/abort the interpreter.
    assert session.run_code('1 + 1') == 2


# `CANARY` — The entire dos_limits class is Rust-only; the Python firewall has NO DoS-containment angle. This pins, at the PyO3 boundary, that an agent allocation bomb is currently allowed up to the LimitedTracker 256 MiB budget (companion to the Rust python_string_repeat/raw_string_replace CANARYs) and gives a single flip-point for when a tighter limit lands. Distinct interface (Python) from every Rust DoS test. CANARY pins current behavior: 50M chars (~50 MB) is UNDER the 256 MiB max_memory budget, so it succeeds; the session returns the length as a native int with no RuntimeError.
# seam: (gap-fill, coverage critic)
def test_agent_memory_bomb_via_run_code_is_uncapped_canary_not_oom_abort(tmp_path):
    session = droplet.Session('fw-membomb')
    # CANARY: under the current LimitedTracker (max_memory=256 MiB) the firewall does NOT bound agent
    # allocation below 256 MiB, so a 50M-char string (~50 MB) is built and its length crosses as a
    # native int (no RuntimeError). Pins current behavior at the PYTHON boundary.
    # Flip to `with pytest.raises(RuntimeError): session.run_code(...)` once a tighter budget lands.
    assert session.run_code("len('x' * 50000000)") == 50000000
    assert session.run_code('1 + 1') == 2  # session still usable
