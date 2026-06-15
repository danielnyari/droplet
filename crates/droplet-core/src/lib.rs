//! droplet-core — the pure-Rust heart of Droplet.
//!
//! No `pyo3` here (invariant #8): the Python bridge lives only in `droplet-py`.

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
}

// Future #[from] variants fold in as engines arrive (invariant #10):
//   DuckDb(#[from] duckdb::Error)            // M1 (local analyze engine)
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
