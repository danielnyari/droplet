//! droplet-py — the PyO3 firewall. The ONLY crate allowed to depend on `pyo3`
//! (invariant #8). It compiles to a `cdylib` Python imports as `droplet._droplet`.
//!
//! It binds the pure-Rust `droplet-core` local analyze engine (M1) to Python with the same
//! boundary discipline the agent surface uses (invariant #6): opaque `Dataset` **handles** cross
//! freely; only the capped read-outs (`scalar_i64`, `to_rows`) move actual values, and `to_rows`
//! returns plain `list[dict]` — never Arrow (the roadmap endgoal keeps results small/plain; see
//! M3/M10). DuckDB is synchronous, so every engine call runs inside `py.detach(...)` to release
//! the GIL while it works (invariant #9). No pyo3 types ever leak into core — only plain
//! values/handles cross.

use monty::MontyObject;
use pyo3::IntoPyObjectExt;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyTuple};

use droplet_core::DropletError;
use droplet_core::engine_duckdb::{Dataset as CoreDataset, DuckEngine, Value};

/// Fold the one boundary error type into a Python exception (invariant #10 meets Python: every
/// `DropletError` surfaces as a catchable `RuntimeError` carrying its `Display` message).
fn to_pyerr(err: DropletError) -> PyErr {
    PyRuntimeError::new_err(err.to_string())
}

/// Map a single plain `Value` into a `dict` entry under `key`. `set_item` accepts any
/// `IntoPyObject`, so each arm hands Python its native type (None / bool / int / float / str).
fn set_cell(dict: &Bound<'_, PyDict>, key: String, value: Value) -> PyResult<()> {
    match value {
        Value::Null => dict.set_item(key, dict.py().None()),
        Value::Bool(b) => dict.set_item(key, b),
        Value::Int(i) => dict.set_item(key, i),
        Value::Float(f) => dict.set_item(key, f),
        Value::Str(s) => dict.set_item(key, s),
    }
}

/// Convert a `MontyObject` (a run_code result) into a native Python object: scalars, None, and the
/// common containers an agent program returns — `list`, `tuple`, `dict` — built recursively.
/// Anything else is an error rather than a silent guess.
fn monty_to_py(py: Python<'_>, obj: &MontyObject) -> PyResult<Py<PyAny>> {
    let out = match obj {
        MontyObject::None => py.None(),
        MontyObject::Bool(b) => b.into_py_any(py)?,
        MontyObject::Int(i) => i.into_py_any(py)?,
        MontyObject::Float(f) => f.into_py_any(py)?,
        MontyObject::String(s) => s.into_py_any(py)?,
        MontyObject::List(items) => {
            let list = PyList::empty(py);
            for it in items {
                list.append(monty_to_py(py, it)?)?;
            }
            list.into_py_any(py)?
        }
        MontyObject::Tuple(items) => {
            let elems = items
                .iter()
                .map(|it| monty_to_py(py, it))
                .collect::<PyResult<Vec<_>>>()?;
            PyTuple::new(py, elems)?.into_py_any(py)?
        }
        MontyObject::Dict(pairs) => {
            let dict = PyDict::new(py);
            for (k, v) in pairs.clone() {
                dict.set_item(monty_to_py(py, &k)?, monty_to_py(py, &v)?)?;
            }
            dict.into_py_any(py)?
        }
        other => {
            return Err(PyRuntimeError::new_err(format!(
                "run_code returned an unsupported value: {other:?}"
            )));
        }
    };
    Ok(out)
}

/// An opaque dataset handle — the Python face of a host-side DuckDB view (invariant #6). It
/// carries no rows; it only names a table inside the engine. `frozen` because a handle is
/// immutable identity, never mutated from Python.
#[pyclass(name = "Dataset", frozen)]
pub struct Dataset {
    inner: CoreDataset,
}

#[pymethods]
impl Dataset {
    fn __repr__(&self) -> String {
        format!("Dataset(table={:?})", self.inner.table())
    }
}

/// The local analyze engine (one ephemeral in-memory DuckDB) exposed to Python. Mirrors the M1
/// `DuckEngine` surface: handle-producing primitives return a `Dataset`; the two capped read-outs
/// return plain Python values.
///
/// `unsendable`: a DuckDB `Connection` is `!Sync`, but pyo3 requires a plain `#[pyclass]` be
/// `Send + Sync`. `unsendable` opts out — the engine is pinned to the thread that created it
/// (touching it from another thread panics, never UB). That matches invariant #3 exactly: one
/// run = one `Session` = one ephemeral engine, never shared across threads.
#[pyclass(name = "Engine", unsendable)]
pub struct Engine {
    inner: DuckEngine,
}

#[pymethods]
impl Engine {
    /// Open a fresh ephemeral engine. `max_result_rows` tunes the per-engine boundary cap
    /// (invariant #6); omit it to keep the core default (`DEFAULT_MAX_RESULT_ROWS`).
    #[new]
    #[pyo3(signature = (max_result_rows=None))]
    fn new(max_result_rows: Option<usize>) -> PyResult<Self> {
        let mut inner = DuckEngine::new_in_memory().map_err(to_pyerr)?;
        if let Some(n) = max_result_rows {
            inner.set_max_result_rows(n);
        }
        Ok(Self { inner })
    }

    /// The current row cap a `to_rows` read-out clamps to.
    #[getter]
    fn max_result_rows(&self) -> usize {
        self.inner.max_result_rows()
    }

    /// Register a LOCAL Parquet file as a `Dataset` handle (no rows copied).
    fn register_parquet(&mut self, py: Python<'_>, path: &str) -> PyResult<Dataset> {
        let inner = py
            .detach(|| self.inner.register_parquet(path))
            .map_err(to_pyerr)?;
        Ok(Dataset { inner })
    }

    /// `WHERE` over a handle → a new handle. Predicate is raw local SQL (safe: local & ephemeral).
    fn filter_rows(&mut self, py: Python<'_>, ds: &Dataset, where_sql: &str) -> PyResult<Dataset> {
        let inner = py
            .detach(|| self.inner.filter_rows(&ds.inner, where_sql))
            .map_err(to_pyerr)?;
        Ok(Dataset { inner })
    }

    /// `GROUP BY` over a handle → a new handle. `by` is the grouping columns; `metrics` is a list
    /// of `(alias, sql_expr)` pairs (e.g. `[("total", "SUM(amount)")]`).
    fn group_agg(
        &mut self,
        py: Python<'_>,
        ds: &Dataset,
        by: Vec<String>,
        metrics: Vec<(String, String)>,
    ) -> PyResult<Dataset> {
        // Borrow the owned Python-side strings as the &str slices the core API takes.
        let by_refs: Vec<&str> = by.iter().map(String::as_str).collect();
        let metric_refs: Vec<(&str, &str)> = metrics
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let inner = py
            .detach(|| self.inner.group_agg(&ds.inner, &by_refs, &metric_refs))
            .map_err(to_pyerr)?;
        Ok(Dataset { inner })
    }

    /// Pull a single integer out of a handle (e.g. a `SUM`/`COUNT`) — the narrowest capped
    /// boundary crossing (invariant #6).
    ///
    /// Takes `&mut self` even though it only reads: a DuckDB `Connection` is `!Sync`, so the
    /// `py.detach` closure that releases the GIL must capture the engine by `&mut` (which is
    /// `Send`) rather than by `&` (which would require `Sync`). Binding `&mut self.inner` and
    /// `move`-ing it in forces that unique borrow even though `scalar_i64` itself only needs `&`.
    /// The mutability is invisible to Python callers.
    fn scalar_i64(&mut self, py: Python<'_>, ds: &Dataset, expr: &str) -> PyResult<i64> {
        let eng = &mut self.inner;
        py.detach(move || eng.scalar_i64(&ds.inner, expr))
            .map_err(to_pyerr)
    }

    /// Move up to `max_result_rows` rows of a handle into Python as a `list[dict]` (invariant #6).
    /// The Arrow→plain-rows conversion happens in core; here we just build the dicts.
    ///
    /// `&mut self` + `move`-ed `&mut` borrow for the same `!Sync`-connection reason as `scalar_i64`.
    // `to_*` conventionally takes `&self`, but the GIL-release borrow forces `&mut` here (see above).
    #[allow(clippy::wrong_self_convention)]
    fn to_rows<'py>(&mut self, py: Python<'py>, ds: &Dataset) -> PyResult<Bound<'py, PyList>> {
        let eng = &mut self.inner;
        let rows = py
            .detach(move || eng.to_rows_values(&ds.inner))
            .map_err(to_pyerr)?;
        let out = PyList::empty(py);
        for row in rows {
            let dict = PyDict::new(py);
            for (col, value) in row {
                set_cell(&dict, col, value)?;
            }
            out.append(dict)?;
        }
        Ok(out)
    }
}

/// A Droplet run: drives agent code in the Monty sandbox via `run_code`. `unsendable` for the same
/// `!Sync` reason as `Engine` (it owns the ephemeral DuckDB connection) — pinned to its thread.
#[pyclass(name = "Session", unsendable)]
pub struct Session {
    inner: droplet_core::session::Session,
}

#[pymethods]
impl Session {
    #[new]
    fn new(run_id: &str) -> PyResult<Self> {
        let inner = droplet_core::session::Session::new(run_id).map_err(to_pyerr)?;
        Ok(Self { inner })
    }

    /// Run one agent program in the sandbox; return its result as native Python. The GIL is released
    /// for the duration (invariant #9): Monty + DuckDB are pure Rust and don't need it, so other
    /// Python threads run meanwhile.
    fn run_code(&mut self, py: Python<'_>, code: &str) -> PyResult<Py<PyAny>> {
        let result = py.detach(|| self.inner.run_code(code)).map_err(to_pyerr)?;
        monty_to_py(py, &result)
    }
}

// Function-style #[pymodule]: the param is &Bound<'_, PyModule>.
#[pymodule]
fn _droplet(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Engine>()?;
    m.add_class::<Dataset>()?;
    m.add_class::<Session>()?;
    Ok(())
}
