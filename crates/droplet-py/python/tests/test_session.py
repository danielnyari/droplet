"""run_code through the PyO3 firewall — V1a's "Done when", driven from Python.

Agent code runs in the Monty sandbox, calls the macro-generated `query` tool over a local Parquet,
and the real aggregates come back into Python as a plain list[dict] (invariant #6: capped, no Arrow).
"""

import pyarrow as pa
import pyarrow.parquet as pq
import pytest

import droplet


def _write_sales(tmp_path):
    # amt is float64: an un-cast SUM over an int/decimal column would be a DuckDB HUGEINT/Decimal
    # the read-out doesn't decode yet (see droplet-core/src/tools.rs); float64 crosses cleanly.
    table = pa.table(
        {
            "region": ["EU", "EU", "US"],
            "amt": pa.array([100.0, 50.0, 200.0], pa.float64()),
        }
    )
    path = tmp_path / "sales.parquet"
    pq.write_table(table, path)
    return str(path)


def test_run_code_returns_aggregates(tmp_path):
    path = _write_sales(tmp_path)
    session = droplet.Session("run-py-v1a")
    code = (
        f"rows = query({path!r}, 'SELECT region, SUM(amt) AS t FROM data GROUP BY region')\n"
        "print(rows)\n"
        "rows"
    )
    rows = session.run_code(code)
    assert {r["region"]: r["t"] for r in rows} == {"EU": 150.0, "US": 200.0}


def test_run_code_unknown_tool_raises():
    session = droplet.Session("run-py-v1a-unknown")
    with pytest.raises(RuntimeError):
        session.run_code("not_a_real_tool(1)")
