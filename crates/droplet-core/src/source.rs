//! The `Source` connector trait + a trivial local-Parquet dev connector.
//!
//! A `Source` is the only thing that ever touches a real data engine, and its
//! job is uniform regardless of engine: **given a scoped load, produce parquet.**
//! Athena does it with `UNLOAD`, Snowflake with `COPY INTO`, BigQuery with
//! `EXPORT`; Iceberg/S3 are already parquet, read directly. The agent never
//! learns which (invariant #1).

use std::path::PathBuf;

use async_trait::async_trait;

use crate::DropletError;

/// A scoped load. M0 only carries the dataset name; M2 adds
/// columns / where-filters / as_of against the catalog schema.
pub struct LoadRequest {
    pub dataset: String,
}

/// A connector. Given a scoped load, produce parquet on local disk and return
/// its path. Real impls (M6): Athena UNLOAD / Snowflake COPY / BigQuery EXPORT,
/// or a direct read for Iceberg/S3 (already parquet). The agent never learns which.
#[async_trait]
pub trait Source: Send + Sync {
    async fn load(&self, req: &LoadRequest) -> Result<PathBuf, DropletError>;
}

/// The trivial dev connector: "produce parquet" by pointing at a local
/// `<dataset>.parquet` file under `base`. No engine, no S3 — but the trait
/// shape is identical to the real connectors (M6), so nothing upstream changes
/// when Athena plugs in behind the same trait.
pub struct LocalParquetSource {
    base: PathBuf,
}

impl LocalParquetSource {
    pub fn new(base: impl Into<PathBuf>) -> Self {
        Self { base: base.into() }
    }
}

#[async_trait]
impl Source for LocalParquetSource {
    async fn load(&self, req: &LoadRequest) -> Result<PathBuf, DropletError> {
        // "Produce parquet" = point at the local file named <dataset>.parquet.
        // The dev impl does no real awaiting; the real Athena impl will .await
        // an UNLOAD here.
        let path = self.base.join(format!("{}.parquet", req.dataset));
        if path.exists() {
            Ok(path)
        } else {
            Err(DropletError::NotFound(req.dataset.clone()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_source_resolves_existing_parquet() {
        let dir = std::env::temp_dir().join("droplet-source-test");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("sales.parquet");
        std::fs::write(&file, b"PAR1...not-real-parquet...").unwrap();

        let src = LocalParquetSource::new(&dir);
        let got = src
            .load(&LoadRequest {
                dataset: "sales".into(),
            })
            .await
            .unwrap();
        assert_eq!(got, file);

        let missing = src
            .load(&LoadRequest {
                dataset: "nope".into(),
            })
            .await;
        assert!(matches!(missing, Err(DropletError::NotFound(_))));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
