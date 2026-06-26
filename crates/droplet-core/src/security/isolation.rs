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

    /// `CANARY` — Core traversal: agent-influenced run_id with ../ climbs above temp_dir and create_dir_all lands a real dir outside the sandbox root.
    /// FINDING: Session::new creates a work_dir OUTSIDE temp_dir via run_id traversal (confirmed on macOS).
    /// Pinning OBSERVED behavior: Session::new returns Ok and work_dir canonicalizes to /private/var/tmp/droplet-evil-traversal-probe (outside temp_dir).
    /// seam: session.rs Session::new — temp_dir().join(format!("droplet-{run_id}")) + create_dir_all; path traversal via run_id
    // Platform-gated: this CANARY's payload is tuned to macOS path semantics (temp_dir lives under
    // /var/folders/... and `/tmp` -> `/private/tmp`, so `../../tmp/...` escapes temp_dir). On Linux
    // temp_dir IS `/tmp`, so the same run_id resolves back *inside* it and the escape assert can't
    // hold — it would false-fail, not catch a regression. The F-3 run_id-traversal gap is STILL OPEN
    // on both platforms (Session::new does not sanitize run_id); a cross-platform pin + the real fix
    // (sanitize_run_id) are tracked separately, out of scope for this F-1 change.
    #[test]
    #[cfg_attr(
        not(target_os = "macos"),
        ignore = "macOS-only path semantics; F-3 gap still open, cross-platform pin tracked separately"
    )]
    fn known_gap_run_id_dotdot_traversal_creates_dir_outside_temp_dir() {
        let base = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let run_id = "../../../../../../tmp/droplet-evil-traversal-probe";
        let sess = Session::new(run_id);
        let made = sess.as_ref().ok().map(|s| s.work_dir().to_path_buf());
        let canon_made = made
            .as_ref()
            .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()));
        if let Some(c) = &canon_made {
            let _ = std::fs::remove_dir_all(c);
        }
        // belt-and-suspenders cleanup at both possible resolutions:
        let _ = std::fs::remove_dir_all(std::path::Path::new(
            "/private/var/tmp/droplet-evil-traversal-probe",
        ));
        let _ = std::fs::remove_dir_all(std::path::Path::new(
            "/var/tmp/droplet-evil-traversal-probe",
        ));
        // FINDING: CANARY pinning OBSERVED behavior — Session::new succeeds and work_dir escapes temp_dir.
        // If this test ever starts PASSING the original assert (i.e., work_dir stays inside temp_dir or Err),
        // the gap is fixed — delete this CANARY and re-enable the CONTRACT assert.
        match &canon_made {
            None => { /* Err would mean the gap is fixed — update this test */ }
            Some(c) => assert!(
                !c.starts_with(&base),
                "CANARY: Session::new now refuses the traversal (work_dir={c:?} is inside temp_dir) — the gap is FIXED, update this test"
            ),
        }
    }

    /// `CANARY` — Distinct from creation: the remove_dir_all-before-create gives a DESTRUCTIVE primitive (delete any reachable dir), not just a benign mkdir.
    /// FINDING: Session::new's remove_dir_all deletes a directory OUTSIDE temp_dir when run_id contains path traversal.
    /// Pinning OBSERVED behavior: keepme.txt in the victim dir is deleted by Session::new's internal remove_dir_all.
    /// seam: session.rs Session::new — `let _ = fs::remove_dir_all(&work_dir)` runs BEFORE create; destructive arbitrary-dir deletion
    // Platform-gated for the same reason as the creation canary above: the victim is planted at
    // `/var/tmp` to collide with where the run_id resolves on macOS; on Linux the run_id resolves to
    // `/tmp/...` instead, so the victim is never touched and `keepme.txt` survives — a false-fail, not
    // a real regression. F-3 remains open on both platforms; tracked separately.
    #[test]
    #[cfg_attr(
        not(target_os = "macos"),
        ignore = "macOS-only path semantics; F-3 gap still open, cross-platform pin tracked separately"
    )]
    fn known_gap_run_id_traversal_removes_dir_outside_temp_dir() {
        // CRITICAL: plant the victim where the traversing run_id ACTUALLY resolves.
        // On macOS the run_id below resolves to /var/tmp (canon /private/var/tmp), NOT /tmp (which is /private/tmp).
        // The designer's original /tmp plant does NOT collide and would give a false PASS.
        let victim = std::path::Path::new("/var/tmp").join("droplet-victim-delete-probe");
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::write(victim.join("keepme.txt"), b"do-not-delete").unwrap();
        let run_id = "../../../../../../tmp/droplet-victim-delete-probe";
        let _sess = Session::new(run_id); // remove_dir_all inside new() targets the resolved /var/tmp/... path
        let still_there = victim.join("keepme.txt").exists();
        let _ = std::fs::remove_dir_all(&victim); // cleanup
        // FINDING: CANARY pinning OBSERVED behavior — Session::new deletes the victim dir outside temp_dir.
        // still_there == false means the file was wiped: that is the OBSERVED gap.
        assert!(
            !still_there,
            "CANARY: keepme.txt survived — Session::new no longer wipes outside temp_dir (gap may be FIXED, update this test)"
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

    /// `CANARY` — Run_id collision: two concurrent sessions sharing a run_id silently corrupt each other's on-disk isolation — a lifecycle/isolation bug distinct from path traversal.
    /// FINDING: identical run_id => identical work_dir => second Session::new's remove_dir_all wipes first live session's private state.
    /// Pinning OBSERVED behavior: A's marker is deleted by B's constructor.
    /// seam: session.rs Session::new — remove_dir_all on a colliding work_dir path; cross-session isolation of on-disk state
    #[test]
    fn known_gap_same_run_id_second_session_wipes_first_sessions_work_dir() {
        let a = Session::new("collide-xyz").unwrap();
        let marker = a.work_dir().join("a_private.txt");
        std::fs::write(&marker, b"A-owns-this").unwrap();
        assert!(marker.exists());
        let _b = Session::new("collide-xyz").unwrap();
        let a_state_survived = marker.exists();
        drop(a);
        // FINDING: CANARY pinning OBSERVED behavior — B's constructor wipes A's marker (survived=false).
        assert!(
            !a_state_survived,
            "CANARY: A's marker survived — same-run_id collision no longer wipes the first session (gap may be FIXED, update this test)"
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

    /// `CANARY` — Separator injection (no ..): run_id with '/' nests work_dir, and Drop's remove of the deepest path leaves orphaned parent dirs — a contained but distinct residue/lifecycle defect.
    /// FINDING: Drop removes only the deepest work_dir path, leaving empty parent dirs (droplet-sub, droplet-sub/deep) as orphaned residue.
    /// Pinning OBSERVED behavior: leftover == true (parent dirs remain after Drop).
    /// Note: the work_dir itself stays INSIDE temp_dir (no path-escape here — purely a teardown completeness gap).
    /// seam: session.rs Session::new — run_id 'a/b/c' nests work_dir; Drop removes only the deepest path, leaving parent segments
    #[test]
    fn known_gap_run_id_with_subdir_separators_leaves_parent_residue() {
        let base = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let run_id = "sub/deep/leaf";
        let work_canon = {
            let s = Session::new(run_id).expect("nested run_id constructs");
            std::fs::canonicalize(s.work_dir()).unwrap()
        }; // Drop here removes temp_dir/droplet-sub/deep/leaf only
        let first_seg = std::env::temp_dir().join("droplet-sub");
        let leftover = first_seg.exists();
        let _ = std::fs::remove_dir_all(&first_seg);
        // The work_dir stays inside temp_dir (good — no escape).
        assert!(
            work_canon.starts_with(&base),
            "nested run_id must stay under temp_dir, got {work_canon:?}"
        );
        // FINDING: CANARY pinning OBSERVED behavior — parent dirs are NOT removed by Drop (leftover==true).
        assert!(
            leftover,
            "CANARY: no leftover parent dirs found — Drop now fully cleans the nested tree (gap may be FIXED, update this test)"
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
