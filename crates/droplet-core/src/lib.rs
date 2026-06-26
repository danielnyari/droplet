//! droplet-core — the pure-Rust heart of Droplet.
//!
//! No `pyo3` here (invariant #8): the Python bridge lives only in `droplet-py`.

// The DuckDB local analyze engine is a core, always-compiled module — the analyze surface
// the `droplet-py` wheel binds to. (It used to be feature-gated; it no longer is.)
pub mod convert;
pub mod engine_duckdb;
pub mod registry;
pub mod sandbox;
pub mod session;
pub mod source;
pub mod tool;
pub mod tools;

// Adversarial boundary tests for the agent surface (jailbreak / exfiltration attempts).
#[cfg(test)]
mod security;

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

    // The local analyze engine (M1). `#[from]` auto-generates From<duckdb::Error>, so a `?` on
    // any duckdb call inside a `Result<_, DropletError>` fn folds the error in (invariant #10).
    #[error("duckdb error: {0}")]
    Duckdb(#[from] duckdb::Error),

    // A column type the capped read-out doesn't yet know how to turn into a plain `Value`
    // (M1 supports the types the analyze surface produces; the richer typed-value work is later).
    #[error("unsupported column type: {0}")]
    UnsupportedType(String),

    // A tool received an argument whose MontyObject type didn't match the Rust parameter type
    // (e.g. an int where a str was expected). Surfaces from FromMonty at the sandbox boundary.
    #[error("bad tool argument: {0}")]
    BadArg(String),
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
