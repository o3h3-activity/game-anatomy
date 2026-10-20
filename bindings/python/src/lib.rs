use std::path::PathBuf;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::PyDict;

/// Return platform, serial, identity_md5, and retroachievements_platform_id.
///
/// Accepts str or os.PathLike paths. Serial is None when unavailable. Releases
/// the GIL while reading media. Hashing failures raise RuntimeError.
#[pyfunction]
fn generate_hash(py: Python<'_>, path: PathBuf) -> PyResult<Py<PyDict>> {
    let info = py
        .detach(move || ::game_anatomy::game_info(&path))
        .map_err(|error| PyRuntimeError::new_err(error.to_string()))?;
    let result = PyDict::new(py);
    result.set_item("platform", info.platform)?;
    result.set_item("serial", info.serial)?;
    result.set_item("identity_md5", info.identity_md5)?;
    result.set_item(
        "retroachievements_platform_id",
        info.retroachievements_platform_id,
    )?;
    Ok(result.unbind())
}

#[pymodule]
fn game_anatomy(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(generate_hash, module)?)
}
