// crates/droplet-core/src/security/isolation.rs
//! Session / work_dir isolation + run_id path traversal — adversarial angles. seam: `session.rs` Session::new/Drop, the `temp_dir().join(format!("droplet-{run_id}"))` path build.
#![allow(unused_imports)]
use super::{catch_dispatch, dispatch, list_len, sales_parquet, tmp_dir, write_parquet};
use crate::DropletError;
use crate::engine_duckdb::{DEFAULT_MAX_RESULT_ROWS, Dataset, DuckEngine};
use crate::registry::Registry;
use crate::session::Session;
use crate::tool::{Tool, ToolCx};
use monty::MontyObject;

#[cfg(test)]
mod tests {
    use super::*;

    /// `CONTRACT` (was a CANARY for F-3) — Core traversal: a host-supplied run_id with `../` is
    /// flattened to a single in-temp component before the `temp_dir().join`, so the work_dir can
    /// never climb above temp_dir. The only FS mutations Session::new makes are on work_dir itself
    /// (create_dir_all + remove_dir_all), so asserting work_dir's containment is sufficient proof
    /// nothing escaped — no out-of-temp probe path needed (keeps this test portable).
    /// seam: session.rs Session::new — sanitize_run_id drops `..`/separators ahead of create_dir_all.
    #[test]
    fn run_id_dotdot_traversal_stays_inside_temp_dir() {
        let base = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let run_id = "../../../../../../tmp/droplet-evil-traversal-probe";
        let sess = Session::new(run_id).expect("a traversing run_id is sanitized, not rejected");
        let canon = std::fs::canonicalize(sess.work_dir()).unwrap();
        // CONTRACT: work_dir is a strict child of temp_dir — no escape.
        assert!(
            canon.starts_with(&base) && canon != base,
            "run_id traversal escaped temp_dir: work_dir={canon:?} base={base:?}"
        );
    }

    /// `CONTRACT` (was a CANARY for F-3) — Distinct from creation: the pre-create remove_dir_all must
    /// target only the sanitized in-temp work_dir, so a victim dir OUTSIDE temp_dir is never deleted.
    /// Unix-only (uses `/var/tmp` as a real out-of-temp location) with a PER-RUN-UNIQUE victim name,
    /// so the test only ever creates/removes its OWN freshly-minted dir — never a pre-existing one.
    /// seam: session.rs Session::new — `fs::remove_dir_all(&work_dir)` after run_id is flattened.
    #[cfg(unix)]
    #[test]
    fn run_id_traversal_does_not_remove_dir_outside_temp_dir() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let name = format!(
            "droplet-victim-delete-probe-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        );
        let victim = std::path::Path::new("/var/tmp").join(&name);
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::write(victim.join("keepme.txt"), b"do-not-delete").unwrap();
        // The un-sanitized run_id used to resolve out of temp_dir; sanitize keeps the wipe in-temp.
        let run_id = format!("../../../../../../tmp/{name}");
        let _sess = Session::new(&run_id).unwrap();
        let survived = victim.join("keepme.txt").exists();
        let _ = std::fs::remove_dir_all(&victim); // cleanup (only ever our own unique dir)
        assert!(
            survived,
            "Session::new wiped a dir OUTSIDE temp_dir via run_id traversal"
        );
    }

    /// `HOLDS` — Embedded-NUL injection: distinct OS-level rejection path; confirms the error folds cleanly instead of panicking inside the constructor.
    /// seam: session.rs Session::new — create_dir_all with NUL byte in run_id; OS rejects the path
    #[test]
    fn run_id_null_byte_errs_not_panics() {
        let result = std::panic::catch_unwind(|| Session::new("ab\0cd"));
        // A NUL byte in the path must surface as a contained DropletError::Io, never panic/UB.
        let result = result.expect("Session::new must not panic on a NUL-byte run_id");
        let is_io_err = matches!(result, Err(DropletError::Io(_)));
        assert!(
            is_io_err,
            "NUL-byte run_id must fold into DropletError::Io (got a non-Io variant or Ok)"
        );
    }

    /// `HOLDS` — Negative control proving the boundary fails ONLY on real '/'+'..' — pins encoder behavior so a future 'decode run_id' change is caught.
    /// seam: session.rs Session::new — %2f is NOT a path separator; literal-filename negative control
    #[test]
    fn run_id_url_encoded_slash_does_not_traverse() {
        let base = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let sess = Session::new("..%2f..%2f..%2fetc%2fevil")
            .expect("a literal (non-separator) run_id should construct");
        let canon = std::fs::canonicalize(sess.work_dir()).unwrap();
        let inside = canon.starts_with(&base);
        drop(sess);
        // %2f is a literal filename byte, not a separator, so this must stay safely inside temp_dir.
        assert!(
            inside,
            "URL-encoded slashes must NOT traverse; work_dir {canon:?} escaped temp_dir {base:?}"
        );
    }

    /// `CONTRACT` (was a CANARY for F-3) — Run_id collision: two sessions sharing a run_id must get
    /// DISTINCT work dirs (per-instance unique suffix), so the second constructor's remove_dir_all
    /// cannot wipe the first live session's on-disk state.
    /// seam: session.rs Session::new — per-instance sequence in the work_dir name; cross-session isolation.
    #[test]
    fn same_run_id_sessions_are_isolated() {
        let a = Session::new("collide-xyz").unwrap();
        let marker = a.work_dir().join("a_private.txt");
        std::fs::write(&marker, b"A-owns-this").unwrap();
        assert!(marker.exists());
        let b = Session::new("collide-xyz").unwrap();
        assert_ne!(
            a.work_dir(),
            b.work_dir(),
            "two sessions with the same run_id must get DISTINCT work dirs"
        );
        let a_state_survived = marker.exists();
        drop(a);
        drop(b);
        assert!(
            a_state_survived,
            "second Session::new wiped the first live session's work_dir"
        );
    }

    /// `HOLDS` — Handle-namespace isolation: handles are per-session integers from 0; the same int in another session must not silently resolve to anything (no shared Registry).
    /// seam: session.rs Session — per-session handles: Registry<Dataset>; cross-session handle isolation
    #[test]
    fn two_sessions_do_not_share_handle_registries() {
        let dir = std::env::temp_dir().join("droplet-iso-handles");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("s.parquet");
        let p = p.to_str().unwrap().to_string();
        {
            let conn = duckdb::Connection::open_in_memory().unwrap();
            conn.execute_batch(&format!(
                "COPY (SELECT 'EU' AS region, CAST(1.0 AS DOUBLE) AS amt) TO '{p}' (FORMAT PARQUET)"
            ))
            .unwrap();
        }
        let mut a = Session::new("iso-a").unwrap();
        let ha = a.run_code(&format!("register({p:?})")).unwrap(); // first handle in A == Int(0)
        let mut b = Session::new("iso-b").unwrap();
        let b_resolve = b.run_code("to_rows(0)");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(matches!(ha, MontyObject::Int(0)), "first handle in A is 0");
        // Same integer handle value, but B's registry is independent: handle 0 is unknown in B.
        assert!(
            b_resolve.is_err(),
            "a handle registered only in session A must NOT resolve in session B (got {b_resolve:?})"
        );
    }

    /// `HOLDS` — Teardown completeness: ensures Drop cleans residue regardless of live host-side state, closing the disk-residue window between runs. Extends existing drop_wipes_the_work_dir by holding live handles + a spilled file.
    /// seam: session.rs Drop — fs::remove_dir_all(work_dir) best-effort; lifecycle teardown with live state
    #[test]
    fn drop_wipes_work_dir_even_with_live_handles() {
        // Use a SEPARATE fixture dir — not the same name as the session's work_dir,
        // because Session::new("iso-drop") creates droplet-iso-drop and wipes it first.
        let fixture_dir = std::env::temp_dir().join("droplet-iso-drop-fixture");
        std::fs::create_dir_all(&fixture_dir).unwrap();
        let p = fixture_dir.join("s.parquet");
        let p = p.to_str().unwrap().to_string();
        {
            let conn = duckdb::Connection::open_in_memory().unwrap();
            conn.execute_batch(&format!(
                "COPY (SELECT 'EU' AS region, CAST(1.0 AS DOUBLE) AS amt) TO '{p}' (FORMAT PARQUET)"
            ))
            .unwrap();
        }
        let work_path = {
            let mut s = Session::new("iso-drop").unwrap();
            s.run_code(&format!("register({p:?})")).unwrap();
            std::fs::write(s.work_dir().join("spill.tmp"), b"residue").unwrap();
            s.work_dir().to_path_buf()
        }; // <- Drop runs here
        let _ = std::fs::remove_dir_all(&fixture_dir);
        assert!(
            !work_path.exists(),
            "Drop must wipe the session work_dir (and its residue) even when handles/datasets were live; {work_path:?} still exists"
        );
    }

    /// `HOLDS` — Lifecycle double-teardown: close() + Drop both remove the same dir; asserts no panic from the second best-effort wipe (idempotent teardown).
    /// seam: session.rs close(self) + Drop — both call remove_dir_all; double-teardown-of-dir lifecycle
    #[test]
    fn close_then_drop_double_wipe_does_not_panic() {
        let s = Session::new("iso-close").unwrap();
        let work_path = s.work_dir().to_path_buf();
        let close_res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.close()));
        // close() consumes self (remove_dir_all) then Drop runs on the moved-out value — must not panic or error spuriously.
        let close_res = close_res.expect("Session::close must not panic");
        assert!(
            close_res.is_ok(),
            "close() on a fresh session should succeed: {close_res:?}"
        );
        assert!(!work_path.exists(), "close() must wipe the work_dir");
    }

    /// `CONTRACT` (was a CANARY for F-3) — Separator injection (no ..): a run_id with '/' is flattened
    /// to a single component, so the work_dir neither nests under temp_dir nor leaves orphaned parent
    /// dirs after Drop.
    /// seam: session.rs Session::new — run_id 'a/b/c' flattened to one segment; no nested mkdir, no residue.
    #[test]
    fn run_id_with_subdir_separators_stays_contained_no_residue() {
        let base = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        // Clean any pre-fix residue so the no-orphan assert below reflects only THIS run.
        let _ = std::fs::remove_dir_all(std::env::temp_dir().join("droplet-sub"));
        let run_id = "sub/deep/leaf";
        let work_canon = {
            let s = Session::new(run_id).expect("nested run_id constructs");
            let c = std::fs::canonicalize(s.work_dir()).unwrap();
            // CONTRACT: a single child of temp_dir — its parent IS temp_dir (no intermediate dirs).
            assert_eq!(
                c.parent().map(std::path::Path::to_path_buf),
                Some(base.clone()),
                "run_id separators created nested dirs under temp_dir: {c:?}"
            );
            c
        }; // Drop runs here
        assert!(!work_canon.exists(), "Drop must fully wipe the work_dir");
        // The pre-fix orphan path must not exist.
        assert!(
            !std::env::temp_dir().join("droplet-sub").exists(),
            "orphaned parent dir 'droplet-sub' left behind"
        );
    }

    /// `HOLDS` — Length/DoS edge: a 5000-char single component exceeds NAME_MAX; tests the constructor degrades to a clean Err (or contained dir) rather than panicking.
    /// seam: session.rs Session::new — create_dir_all with an over-long path component (ENAMETOOLONG)
    #[test]
    fn very_long_run_id_errs_not_panics() {
        let res = std::panic::catch_unwind(|| Session::new(&"L".repeat(5000)));
        // An over-long run_id must surface a contained DropletError (Io: name too long), never panic.
        let res = res.expect("Session::new must not panic on a very long run_id");
        // Precise contract: if the OS rejects the name it must be DropletError::Io; if the OS accepts it, work_dir must exist & sit under temp_dir.
        match res {
            Err(DropletError::Io(_)) => { /* acceptable: name-too-long rejected cleanly */ }
            Err(other) => panic!("long run_id should fail (if at all) as Io, got {other:?}"),
            Ok(s) => {
                let base = std::fs::canonicalize(std::env::temp_dir()).unwrap();
                let c = std::fs::canonicalize(s.work_dir()).unwrap();
                assert!(
                    c.starts_with(&base),
                    "long run_id work_dir escaped temp_dir: {c:?}"
                );
            }
        }
    }

    /// `HOLDS` — Empty/degenerate run_id: the format!("droplet-{run_id}") prefix means even "" yields a distinct child droplet-; confirms remove_dir_all can never be aimed at temp_dir root or siblings.
    /// seam: session.rs Session::new — run_id "" => temp_dir/droplet- (a distinct child, never temp_dir itself)
    #[test]
    fn empty_run_id_does_not_wipe_or_target_bare_temp_dir() {
        let base = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let sentinel = std::env::temp_dir().join("droplet-empty-runid-sentinel.txt");
        std::fs::write(&sentinel, b"keep").unwrap();
        let work_canon = {
            let s = Session::new("").expect("empty run_id constructs");
            std::fs::canonicalize(s.work_dir()).unwrap()
        }; // Drop here removes temp_dir/droplet-
        let sentinel_survived = sentinel.exists();
        let _ = std::fs::remove_file(&sentinel);
        // Empty run_id must resolve to a NON-ROOT child (temp_dir/droplet-), never temp_dir itself,
        // so neither create nor the Drop wipe can touch temp_dir root or its other contents.
        assert_ne!(
            work_canon, base,
            "empty run_id must not make work_dir == temp_dir root"
        );
        assert!(
            work_canon.starts_with(&base) && work_canon != base,
            "work_dir {work_canon:?} must be a strict child of temp_dir"
        );
        assert!(
            sentinel_survived,
            "empty run_id teardown must not wipe temp_dir contents (sentinel deleted!)"
        );
    }
}
