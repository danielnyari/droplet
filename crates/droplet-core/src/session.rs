//! `Session` — the per-run context (PRODUCT.md §14 isolation).
//!
//! One run = one `Session`. It owns a unique working directory (wiped on close)
//! and the handle registry. The ephemeral DuckDB connection, the read-only
//! Surreal handle, and the deferred store backends get added in later milestones.

use std::fs;
use std::path::{Path, PathBuf};

use crate::DropletError;
use crate::registry::Registry;
use crate::source::{LocalParquetSource, Source};

pub struct Session {
    run_id: String,
    work_dir: PathBuf,
    // One registry per session. The stored type is a placeholder for now;
    // it becomes the real engine-handle type when DuckDB lands in M1.
    handles: Registry<()>,
    // The connector seam (invariant #1): any backend plugs in unchanged.
    // Dev impl now; Athena/S3 later (M6).
    source: Box<dyn Source>,
    // Later milestones add (NOT in M0):
    //   duck: duckdb::Connection             // M1 — ephemeral per-session local analyze engine
    //   surreal: read-only Surreal<Mem>      // M9 — schema-derived field search (read-only)
    //   artifacts: Box<dyn ArtifactStore>    // M5 — content-addressed load cache
    //   coord:     Box<dyn CoordinationStore>// M7 — run registry / leases / cache index
    //   snapshots: Box<dyn SnapshotStore>    // M8 — REPL+manifest blobs
}

impl Session {
    pub fn new(run_id: &str) -> Result<Self, DropletError> {
        // Unique per run so two sessions never collide (§14 isolation).
        let work_dir = std::env::temp_dir().join(format!("droplet-{run_id}"));
        // Wipe any stale dir from a previous run, then recreate it empty.
        let _ = fs::remove_dir_all(&work_dir); // ignore "not found"
        fs::create_dir_all(&work_dir)?; // io::Error -> DropletError via #[from]
        // Default the connector to the local-Parquet dev impl, looking in the
        // session's own work_dir. M2's catalog decides this properly.
        let source: Box<dyn Source> = Box::new(LocalParquetSource::new(work_dir.clone()));
        Ok(Self {
            run_id: run_id.to_string(),
            work_dir,
            handles: Registry::new(),
            source,
        })
    }

    pub fn work_dir(&self) -> &Path {
        &self.work_dir
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Borrow the session's handle registry.
    pub fn handles(&self) -> &Registry<()> {
        &self.handles
    }

    /// Borrow the session's connector (the only thing that touches a source).
    pub fn source(&self) -> &dyn Source {
        self.source.as_ref()
    }

    /// Consume the session and surface a teardown error, for callers who want
    /// the wipe to be loud rather than best-effort. `Drop` still runs as a
    /// backstop if you don't call this.
    pub fn close(self) -> Result<(), DropletError> {
        fs::remove_dir_all(&self.work_dir)?;
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Best-effort cleanup; never panic in a destructor.
        let _ = fs::remove_dir_all(&self.work_dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_creates_a_fresh_work_dir() {
        let s = Session::new("run-123").unwrap();
        assert!(s.work_dir().is_dir()); // the dir exists on disk
    }

    #[test]
    fn drop_wipes_the_work_dir() {
        let path = {
            let s = Session::new("run-drop").unwrap();
            s.work_dir().to_path_buf()
        }; // session dropped here
        assert!(!path.exists(), "Drop should have wiped {path:?}");
    }

    #[tokio::test]
    async fn session_carries_a_working_connector() {
        use crate::source::LoadRequest;

        let s = Session::new("run-src").unwrap();
        // The dev connector looks in work_dir for <dataset>.parquet.
        let file = s.work_dir().join("orders.parquet");
        std::fs::write(&file, b"PAR1...not-real-parquet...").unwrap();

        let got = s
            .source()
            .load(&LoadRequest {
                dataset: "orders".into(),
            })
            .await
            .unwrap();
        assert_eq!(got, file);
        assert!(got.exists());
    }
}
