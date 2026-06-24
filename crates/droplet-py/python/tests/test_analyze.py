"""V1b: the handle-based local analyze surface, driven from the Python wheel.

The agent registers a local Parquet (-> opaque handle), chains handle-based primitives, and only
`to_rows`/`scalar` move values back. Then the agent's OWN Python derives, branches, and ranks —
the §20 analyze shape, local-only (no load/cache/cross-pod yet). No wheel changes were needed: the
macro-generated tools auto-register and `run_code` dispatches them.
"""

import pyarrow as pa
import pyarrow.parquet as pq

import droplet


def _write_demo(tmp_path):
    table = pa.table(
        {
            "region": ["EU", "EU", "US", "APAC", "APAC"],
            "amt": pa.array([100.0, 50.0, 200.0, 300.0, 0.0], pa.float64()),
        }
    )
    path = tmp_path / "demo.parquet"
    pq.write_table(table, path)
    return str(path)


def test_multi_step_analysis_over_handles(tmp_path):
    path = _write_demo(tmp_path)
    session = droplet.Session("py-v1b")
    code = f"""ds = register({path!r})
agg = group_agg(ds, ['region'], [('total', 'SUM(amt)'), ('n', 'CAST(COUNT(*) AS BIGINT)')])
ranked = []
for r in to_rows(agg):
    avg = r['total'] / r['n']
    if avg >= 100:
        ranked.append({{'region': r['region'], 'avg': avg}})
ranked.sort(key=lambda x: -x['avg'])
ranked"""
    ranked = session.run_code(code)
    # EU avg 75 dropped; US 200 and APAC 150 kept, ranked descending.
    assert ranked == [{"region": "US", "avg": 200.0}, {"region": "APAC", "avg": 150.0}]


def test_intermediate_ops_return_opaque_handles(tmp_path):
    path = _write_demo(tmp_path)
    session = droplet.Session("py-v1b-handles")
    # register and group_agg hand back opaque integer handles, not rows (invariant #6).
    h = session.run_code(f"register({path!r})")
    assert isinstance(h, int)
    g = session.run_code(f"group_agg(register({path!r}), ['region'], [('t', 'SUM(amt)')])")
    assert isinstance(g, int)


def test_handles_persist_across_steps(tmp_path):
    path = _write_demo(tmp_path)
    session = droplet.Session("py-v1b-persist")
    session.run_code(f"ds = register({path!r})")
    rows = session.run_code("to_rows(filter_rows(ds, 'amt >= 100'))")
    # amt>=100 keeps EU 100, US 200, APAC 300 -> 3 rows; the step-1 handle still resolves.
    assert len(rows) == 3
