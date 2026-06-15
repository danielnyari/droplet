//! droplet-py — the PyO3 firewall. The ONLY crate allowed to depend on `pyo3`
//! (invariant #8). It compiles to a `cdylib` Python imports as `droplet._droplet`.
//!
//! M0 ships a trivial `add` to prove the Python toolchain end-to-end. Real calls
//! into `droplet-core` (load/analyze) arrive in later milestones — and only plain
//! values/handles will cross, never pyo3 types leaking into core.

use pyo3::prelude::*;

#[pyfunction]
fn add(a: u64, b: u64) -> u64 {
    a + b
}

// Function-style #[pymodule]: the param is &Bound<'_, PyModule> (the 0.29 API).
#[pymodule]
fn _droplet(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(add, m)?)?;
    Ok(())
}
