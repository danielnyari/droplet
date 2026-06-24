"""Python binding tests — the M1 analyze engine driven through the PyO3 firewall.

Two kinds of fixture:

1. A real, varied Parquet written at runtime with pyarrow (``shop_parquet`` / ``big_parquet``),
   so the binding is exercised over actual int / float / bool / string / NULL columns — not just
   the one hand-crafted file.
2. The committed known-answer fixture ``crates/droplet-core/tests/data/sample.parquet`` (the same
   one the Rust tests assert against), as a cross-check that Python sees the same numbers Rust does.

Boundary discipline holds throughout (invariant #6): primitives hand back opaque ``Dataset``
handles; only ``scalar_i64`` and ``to_rows`` move values, and ``to_rows`` returns plain
``list[dict]`` — native Python types, never Arrow.
"""

from pathlib import Path

import pyarrow as pa
import pyarrow.parquet as pq
import pytest

import droplet

# --- the committed known-answer fixture (shared with the Rust tests) ----------------------------
# This file: crates/droplet-py/python/tests/test_engine.py
#   parents[0]=tests  [1]=python  [2]=droplet-py  [3]=crates
SAMPLE = (
    Path(__file__).resolve().parents[3]
    / "droplet-core"
    / "tests"
    / "data"
    / "sample.parquet"
)


# --- real generated fixtures --------------------------------------------------------------------
@pytest.fixture
def shop_parquet(tmp_path):
    """A small, multi-type table. Known aggregates:
    amount SUM=790 (EU 200, US 290, APAC 300); price SUM=11.25; active true-count=3;
    `note` has two NULLs.
    """
    table = pa.table(
        {
            "region": ["EU", "EU", "US", "US", "APAC"],  # Utf8
            "amount": pa.array([50, 150, 200, 90, 300], pa.int64()),  # Int64
            "price": pa.array([1.5, 2.0, 3.25, 0.5, 4.0], pa.float64()),  # Float64
            "active": [True, False, True, True, False],  # Boolean
            "note": ["a", None, "c", None, "e"],  # Utf8 with NULLs
        }
    )
    path = tmp_path / "shop.parquet"
    pq.write_table(table, path)
    return str(path)


@pytest.fixture
def big_parquet(tmp_path):
    """1500 rows — comfortably above the default 1000-row cap."""
    n = 1500
    table = pa.table({"id": pa.array(range(n), pa.int64())})
    path = tmp_path / "big.parquet"
    pq.write_table(table, path)
    return str(path)


# --- known-answer cross-check -------------------------------------------------------------------
def test_known_answer_fixture_matches_rust():
    assert SAMPLE.is_file(), f"missing committed fixture: {SAMPLE}"
    eng = droplet.Engine()
    usage = eng.register_parquet(str(SAMPLE))
    assert eng.scalar_i64(usage, "SUM(amount)") == 790
    agg = eng.group_agg(usage, ["category"], [("total", "CAST(SUM(amount) AS BIGINT)")])
    assert {r["category"]: r["total"] for r in eng.to_rows(agg)} == {
        "a": 200,
        "b": 290,
        "c": 300,
    }


# --- scalars over a real parquet ----------------------------------------------------------------
def test_register_then_count(shop_parquet):
    eng = droplet.Engine()
    ds = eng.register_parquet(shop_parquet)
    assert eng.scalar_i64(ds, "COUNT(*)") == 5


def test_scalar_sum_of_int_column(shop_parquet):
    eng = droplet.Engine()
    ds = eng.register_parquet(shop_parquet)
    assert eng.scalar_i64(ds, "SUM(amount)") == 790


def test_filter_then_scalar(shop_parquet):
    eng = droplet.Engine()
    ds = eng.register_parquet(shop_parquet)
    big = eng.filter_rows(ds, "amount > 100")  # 150, 200, 300
    assert eng.scalar_i64(big, "SUM(amount)") == 650


def test_group_agg_by_region(shop_parquet):
    eng = droplet.Engine()
    ds = eng.register_parquet(shop_parquet)
    agg = eng.group_agg(ds, ["region"], [("total", "CAST(SUM(amount) AS BIGINT)")])
    assert {r["region"]: r["total"] for r in eng.to_rows(agg)} == {
        "EU": 200,
        "US": 290,
        "APAC": 300,
    }


def test_grand_total_with_empty_grouping(shop_parquet):
    eng = droplet.Engine()
    ds = eng.register_parquet(shop_parquet)
    total = eng.group_agg(ds, [], [("total", "CAST(SUM(amount) AS BIGINT)")])
    rows = eng.to_rows(total)
    assert len(rows) == 1
    assert rows[0]["total"] == 790


def test_filter_then_group_chain(shop_parquet):
    eng = droplet.Engine()
    ds = eng.register_parquet(shop_parquet)
    active = eng.filter_rows(ds, "active = true")  # EU/50, US/200, US/90
    agg = eng.group_agg(active, ["region"], [("n", "CAST(COUNT(*) AS BIGINT)")])
    assert {r["region"]: r["n"] for r in eng.to_rows(agg)} == {"EU": 1, "US": 2}


# --- type fidelity & NULLs across the boundary --------------------------------------------------
def test_to_rows_preserves_native_python_types(shop_parquet):
    eng = droplet.Engine()
    ds = eng.register_parquet(shop_parquet)
    eu = eng.filter_rows(ds, "region = 'EU' AND amount = 50")
    (row,) = eng.to_rows(eu)
    # `type(...) is` (not isinstance) so bool isn't accepted as int.
    assert type(row["region"]) is str and row["region"] == "EU"
    assert type(row["amount"]) is int and row["amount"] == 50
    assert type(row["price"]) is float and row["price"] == pytest.approx(1.5)
    assert type(row["active"]) is bool and row["active"] is True


def test_to_rows_renders_sql_null_as_none(shop_parquet):
    eng = droplet.Engine()
    ds = eng.register_parquet(shop_parquet)
    missing = eng.filter_rows(ds, "note IS NULL")
    rows = eng.to_rows(missing)
    assert len(rows) == 2
    assert all(r["note"] is None for r in rows)


def test_float_aggregate_crosses_as_float(shop_parquet):
    eng = droplet.Engine()
    ds = eng.register_parquet(shop_parquet)
    agg = eng.group_agg(ds, [], [("revenue", "SUM(price)")])
    (row,) = eng.to_rows(agg)
    assert type(row["revenue"]) is float
    assert row["revenue"] == pytest.approx(11.25)


# --- the configurable cap (invariant #6) --------------------------------------------------------
def test_default_cap_is_1000(big_parquet):
    eng = droplet.Engine()
    assert eng.max_result_rows == 1000
    ds = eng.register_parquet(big_parquet)  # 1500 rows
    assert len(eng.to_rows(ds)) == 1000  # clamped to the default


def test_custom_cap_limits_the_readout(big_parquet):
    eng = droplet.Engine(max_result_rows=7)
    assert eng.max_result_rows == 7
    ds = eng.register_parquet(big_parquet)
    assert len(eng.to_rows(ds)) == 7


# --- handles: opaque & independent --------------------------------------------------------------
def test_handles_are_opaque_and_independent(shop_parquet):
    eng = droplet.Engine()
    ds = eng.register_parquet(shop_parquet)  # 5 rows
    big = eng.filter_rows(ds, "amount > 100")  # 3 rows
    assert isinstance(ds, droplet.Dataset)
    assert "Dataset(table=" in repr(ds)
    # The two handles name different views and read back independently.
    assert len(eng.to_rows(ds)) == 5
    assert len(eng.to_rows(big)) == 3


# --- errors fold into catchable Python exceptions (invariant #10) -------------------------------
def test_bad_scalar_expr_raises_runtimeerror(shop_parquet):
    eng = droplet.Engine()
    ds = eng.register_parquet(shop_parquet)
    with pytest.raises(RuntimeError):
        eng.scalar_i64(ds, "SUM(nonexistent_column)")


def test_filter_on_unknown_column_raises_runtimeerror(shop_parquet):
    eng = droplet.Engine()
    ds = eng.register_parquet(shop_parquet)
    with pytest.raises(RuntimeError):
        # Whether DuckDB binds at view-creation or read time, the chain must surface a clean error.
        eng.to_rows(eng.filter_rows(ds, "nonexistent_column > 1"))


def test_register_missing_file_raises_runtimeerror(tmp_path):
    eng = droplet.Engine()
    missing = str(tmp_path / "does_not_exist.parquet")
    with pytest.raises(RuntimeError):
        eng.scalar_i64(eng.register_parquet(missing), "COUNT(*)")
