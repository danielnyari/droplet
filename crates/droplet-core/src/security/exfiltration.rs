// crates/droplet-core/src/security/exfiltration.rs
//! The canonical V1a known-gap canary: arbitrary local-file read is currently possible (host-data
//! exfiltration). Asserts the CURRENT vulnerable behavior on purpose, so it FAILS LOUDLY the day
//! local-FS scoping lands (V3). Full writeup: docs/security/2026-06-24-v1a-local-fs-read-gap.md.
#![allow(unused_imports)]
use super::{sales_parquet, tmp_dir};
use crate::DropletError;
use crate::session::Session;
use monty::MontyObject;

/// `CANARY` — agent SQL reads a host file it was never handed, via `read_csv`, and the contents
/// cross back into the sandbox. Flip to assert-blocked when V3 scopes the local filesystem.
#[test]
fn known_gap_local_file_read_is_currently_possible() {
    let dir = tmp_dir("exfil");
    let p = sales_parquet(&dir);
    let secret = dir.join("secret.txt");
    std::fs::write(&secret, "TOPSECRET").unwrap();
    let mut s = Session::new("exfil").unwrap();
    let code = format!(
        "query({p:?}, \"SELECT * FROM read_csv('{}', header=false)\")",
        secret.to_str().unwrap()
    );
    let out = s
        .run_code(&code)
        .expect("KNOWN GAP: local read currently succeeds");
    let leaked = format!("{out:?}");
    assert!(
        leaked.contains("TOPSECRET"),
        "KNOWN GAP canary: expected leak, got {leaked}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
