//! droplet-core — the pure-Rust heart of Droplet.
//!
//! No `pyo3` here (invariant #8): the Python bridge lives only in `droplet-py`.

// The DuckDB local analyze engine is conditionally compiled: included only when the
// `duckdb` feature is active, so the default build never pulls in the C++ engine.
#[cfg(feature = "duckdb")]
pub mod engine_duckdb;
pub mod registry;
pub mod sandbox;
pub mod session;
pub mod source;

/// The one boundary error type. Every engine error in Droplet (Monty, DuckDB,
/// SurrealDB, S3, Redis, IO…) eventually folds into this single enum
/// (invariant #10: thiserror in libraries, anyhow at binaries).
#[derive(thiserror::Error, Debug)]
pub enum DropletError {
    #[error("no such handle: {0}")]
    BadHandle(u64),

    #[error("io error")]
    Io(#[from] std::io::Error),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("monty error")]
    Monty(#[from] monty::MontyException),

    // A blocking analyze task (spawn_blocking) panicked or was cancelled. Folding JoinError in
    // lets the async entrypoint end in `.await??` with no manual mapping (invariant #10).
    #[error("blocking task failed: {0}")]
    Join(#[from] tokio::task::JoinError),

    // The local analyze engine (M1). Feature-gated so the default build never references
    // `duckdb`. `#[from]` auto-generates From<duckdb::Error>, so a `?` on any duckdb call
    // inside a `Result<_, DropletError>` fn folds the error in (invariant #10).
    #[cfg(feature = "duckdb")]
    #[error("duckdb error: {0}")]
    Duckdb(#[from] duckdb::Error),
}

// Future #[from] variants fold in as engines arrive (invariant #10):
//   Surreal(#[from] surrealdb::Error)        // M9 (read-only field search)
//   S3 / Redis / DynamoDB / postcard / zstd / tokio::task::JoinError  // M5/M7/M8

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bad_handle_displays_id() {
        let err = DropletError::BadHandle(7);
        assert_eq!(err.to_string(), "no such handle: 7");
    }

    #[test]
    fn io_error_folds_in() {
        fn might_fail() -> Result<(), DropletError> {
            let _ = std::fs::read("definitely-not-a-real-file")?; // io::Error -> DropletError
            Ok(())
        }
        assert!(might_fail().is_err());
    }
}
