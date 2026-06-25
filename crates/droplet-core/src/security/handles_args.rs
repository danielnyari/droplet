// crates/droplet-core/src/security/handles_args.rs
//! Handle/registry forgery + arg-conversion seam + macro arity — adversarial angles.
//! seam: `convert.rs` FromArg/IntoRet, `registry.rs` Registry, the `#[droplet_tool]` thunk's
//! `args[i]` direct indexing (no bounds guard) in `macros/src/lib.rs`.
#![allow(unused_imports)]
use monty::MontyObject;
use crate::DropletError;
use crate::session::Session;
use crate::engine_duckdb::{DuckEngine, Dataset, DEFAULT_MAX_RESULT_ROWS};
use crate::registry::Registry;
use crate::tool::{Tool, ToolCx};
use super::{dispatch, catch_dispatch, catch_dispatch_kw, tmp_dir, sales_parquet, write_parquet, list_len};

#[cfg(test)]
mod tests {
    use super::*;

    // ── Handle forgery: scalar Dataset arg ────────────────────────────────────────────────────────

    /// `HOLDS` — Baseline invariant #6 forgery: a valid positive i64 never issued must miss the
    /// registry → BadHandle, not panic/empty. Family anchor.
    /// seam: convert.rs Dataset::from_arg -> registry.rs Registry::require; to_rows in tools.rs
    #[test]
    fn to_rows_unissued_handle_is_bad_handle() {
        let err = dispatch("to_rows", &[MontyObject::Int(999)]).unwrap_err();
        assert!(matches!(err, DropletError::BadHandle(999)), "got {err:?}");
    }

    /// `HOLDS` — Signed->unsigned edge: a NEGATIVE Int passes i64::from_monty but fails
    /// u64::try_from BEFORE the registry lookup → BadArg('dataset handle must be non-negative'),
    /// distinct from BadHandle.
    /// seam: convert.rs Dataset::from_arg u64::try_from(i64) guard (convert.rs:192-193)
    #[test]
    fn to_rows_negative_handle_is_bad_arg_non_negative() {
        let err = dispatch("to_rows", &[MontyObject::Int(-1)]).unwrap_err();
        assert!(matches!(err, DropletError::BadArg(ref m) if m.contains("non-negative")), "got {err:?}");
    }

    /// `HOLDS` — BadArg/BadHandle boundary: 2**62 is a positive i64 so u64::try_from SUCCEEDS
    /// (no over-rejection), but registry never issued it → BadHandle. Confirms the non-negativity
    /// guard does not clamp large valid handles.
    /// seam: convert.rs Dataset::from_arg (u64::try_from succeeds) -> registry miss
    #[test]
    fn to_rows_huge_but_valid_i64_handle_is_bad_handle() {
        let err = dispatch("to_rows", &[MontyObject::Int(1i64 << 62)]).unwrap_err();
        assert!(matches!(err, DropletError::BadHandle(h) if h == (1u64 << 62)), "got {err:?}");
    }

    /// `HOLDS` — Type-confusion via integer overflow: 2**63 > i64::MAX so monty represents it as
    /// the DISTINCT MontyObject::BigInt variant; i64::from_monty's catch-all rejects it → BadArg
    /// before any handle logic. Attacks the Int-vs-BigInt variant split.
    /// seam: convert.rs Dataset::from_arg -> i64::from_monty matches ONLY Int; BigInt arm is BadArg
    #[test]
    fn to_rows_2pow63_overflows_to_bigint_is_bad_arg() {
        let big = num_bigint::BigInt::from(1u128 << 63);
        let err = dispatch("to_rows", &[MontyObject::BigInt(big)]).unwrap_err();
        assert!(matches!(err, DropletError::BadArg(ref m) if m.contains("expected int")), "got {err:?}");
    }

    /// `HOLDS` — Handle smuggled as text '0': must NOT be str->int coerced into a handle.
    /// Distinct variant arm from float/list/bytes.
    /// seam: convert.rs i64::from_monty catch-all (String arm)
    #[test]
    fn to_rows_string_handle_is_bad_arg() {
        let err = dispatch("to_rows", &[MontyObject::String("0".into())]).unwrap_err();
        assert!(matches!(err, DropletError::BadArg(ref m) if m.contains("expected int")), "got {err:?}");
    }

    /// `HOLDS` — Float 0.0 numerically equals handle 0 but is a different variant; proves no
    /// float->int truncation aliases the first-issued handle. Distinct gadget from the str case.
    /// seam: convert.rs i64::from_monty catch-all (Float arm)
    #[test]
    fn to_rows_float_handle_is_bad_arg() {
        let err = dispatch("to_rows", &[MontyObject::Float(0.0)]).unwrap_err();
        assert!(matches!(err, DropletError::BadArg(ref m) if m.contains("expected int")), "got {err:?}");
    }

    /// `HOLDS` — A list wrapping a valid handle int must not be destructured into a handle. Attacks
    /// scalar-vs-sequence confusion: the scalar Dataset path must NOT route through as_seq.
    /// seam: convert.rs i64::from_monty catch-all (List arm) — scalar-vs-sequence
    #[test]
    fn to_rows_list_handle_is_bad_arg() {
        let err = dispatch("to_rows", &[MontyObject::List(vec![MontyObject::Int(0)])]).unwrap_err();
        assert!(matches!(err, DropletError::BadArg(ref m) if m.contains("expected int")), "got {err:?}");
    }

    /// `HOLDS` — Hack-Monty 'bytes object replacing a primitive': 8 zero bytes (LE u64 0) must NOT
    /// be reinterpreted/transmuted as the 8-byte handle. Distinct variant arm.
    /// seam: convert.rs i64::from_monty catch-all (Bytes arm)
    #[test]
    fn to_rows_bytes_handle_is_bad_arg() {
        let err = dispatch("to_rows", &[MontyObject::Bytes(vec![0u8, 0, 0, 0, 0, 0, 0, 0])]).unwrap_err();
        assert!(matches!(err, DropletError::BadArg(ref m) if m.contains("expected int")), "got {err:?}");
    }

    /// `HOLDS` — Tuple is the OTHER as_seq-matchable variant. The scalar handle path must still
    /// reject it → proves the Dataset arg does NOT go through as_seq. Distinct from List.
    /// seam: convert.rs i64::from_monty catch-all (Tuple arm)
    #[test]
    fn to_rows_tuple_handle_is_bad_arg() {
        let err = dispatch("to_rows", &[MontyObject::Tuple(vec![MontyObject::Int(0)])]).unwrap_err();
        assert!(matches!(err, DropletError::BadArg(ref m) if m.contains("expected int")), "got {err:?}");
    }

    // ── Registry isolation ─────────────────────────────────────────────────────────────────────────

    /// `HOLDS` — Off-by-one / first-handle forgery: 0 is the FIRST id the monotonic counter ever
    /// issues; guessing 0 before any register must miss. Distinct from the arbitrary-999 angle.
    /// seam: registry.rs Registry::require on empty registry (next==0, no items)
    #[test]
    fn fresh_session_to_rows_zero_is_bad_handle_empty_registry() {
        let err = dispatch("to_rows", &[MontyObject::Int(0)]).unwrap_err();
        assert!(matches!(err, DropletError::BadHandle(0)), "got {err:?}");
    }

    /// `HOLDS` — Cross-session handle confusion: handle 0 is valid in A but each Session owns its
    /// own Registry, so the same int must miss in B. Attacks ambient/global handle namespacing via
    /// the REAL Session surface, not the dispatch helper.
    /// seam: session.rs per-Session Registry isolation (handles field) + Dataset::from_arg
    #[test]
    fn cross_session_handle_zero_invalid_in_fresh_session() {
        let path = format!("{}/tests/data/sample.parquet", env!("CARGO_MANIFEST_DIR"));
        let mut a = Session::new("forge-a").unwrap();
        let h = a.run_code(&format!("register({path:?})")).unwrap();
        assert!(matches!(h, MontyObject::Int(0)));
        let mut b = Session::new("forge-b").unwrap();
        let err = b.run_code("to_rows(0)").unwrap_err();
        assert!(
            matches!(err, DropletError::BadHandle(0)),
            "session B must not resolve session A's handle 0, got {err:?}"
        );
    }

    // ── Multi-handle / compound arg forgery ───────────────────────────────────────────────────────

    /// `HOLDS` — Partial-validity forgery: one real + one forged handle. Each Dataset param
    /// resolves independently; a valid left must NOT launder a forged right. Attacks multi-handle
    /// tools where a good handle might mask a bad one.
    /// seam: convert.rs Dataset::from_arg per-arg in the join thunk (left valid, right forged)
    #[test]
    fn join_mixed_valid_and_forged_handle_is_bad_handle() {
        let path = format!("{}/tests/data/sample.parquet", env!("CARGO_MANIFEST_DIR"));
        let mut engine = DuckEngine::new_in_memory().unwrap();
        let mut handles = Registry::new();
        let mut cx = ToolCx { engine: &mut engine, handles: &mut handles };
        let reg = inventory::iter::<Tool>().find(|t| t.name == "register").unwrap();
        let h = (reg.dispatch)(&mut cx, &[MontyObject::String(path)], &[]).unwrap();
        let jn = inventory::iter::<Tool>().find(|t| t.name == "join").unwrap();
        let err = (jn.dispatch)(
            &mut cx,
            &[h, MontyObject::Int(999), MontyObject::String("l.id = r.id".into())],
            &[],
        )
        .unwrap_err();
        assert!(
            matches!(err, DropletError::BadHandle(999)),
            "forged RIGHT handle must fail even with a valid left, got {err:?}"
        );
    }

    /// `HOLDS` — Forgery laundered through the COMPOUND arg list[tuple[str,Dataset]]; the handle is
    /// nested two levels deep and resolution must still hit the registry and miss. Distinct seam from
    /// the scalar Dataset arg.
    /// seam: convert.rs Vec<(String,Dataset)>::from_arg -> Dataset::from_arg on the nested handle
    #[test]
    fn local_sql_forged_handle_in_dataset_list_is_bad_handle() {
        let arg = MontyObject::List(vec![MontyObject::Tuple(vec![
            MontyObject::String("usage".into()),
            MontyObject::Int(4242),
        ])]);
        let err = dispatch("local_sql", &[MontyObject::String("SELECT 1".into()), arg]).unwrap_err();
        assert!(
            matches!(err, DropletError::BadHandle(4242)),
            "forged handle inside the dataset list must surface BadHandle, got {err:?}"
        );
    }

    // ── Vec<String> / Vec<(String,String)> arg-conversion seam ───────────────────────────────────

    /// `HOLDS` — Shape confusion: a str where list[str] is required. as_seq rejects String (only
    /// List|Tuple) → BadArg BEFORE any SQL. Uses a REAL handle so the failure is provably the arg
    /// shape, not the handle.
    /// seam: convert.rs Vec<String>::from_arg via as_seq (String is not List|Tuple)
    #[test]
    fn group_agg_str_for_by_list_is_bad_arg() {
        let path = format!("{}/tests/data/sample.parquet", env!("CARGO_MANIFEST_DIR"));
        let mut engine = DuckEngine::new_in_memory().unwrap();
        let mut handles = Registry::new();
        let mut cx = ToolCx { engine: &mut engine, handles: &mut handles };
        let reg = inventory::iter::<Tool>().find(|t| t.name == "register").unwrap();
        let h = (reg.dispatch)(&mut cx, &[MontyObject::String(path)], &[]).unwrap();
        let ga = inventory::iter::<Tool>().find(|t| t.name == "group_agg").unwrap();
        let metrics = MontyObject::List(vec![MontyObject::Tuple(vec![
            MontyObject::String("t".into()),
            MontyObject::String("SUM(amount)".into()),
        ])]);
        let err = (ga.dispatch)(
            &mut cx,
            &[h, MontyObject::String("region".into()), metrics],
            &[],
        )
        .unwrap_err();
        assert!(
            matches!(err, DropletError::BadArg(ref m) if m.contains("list[str]")),
            "a bare str for the `by` list must be BadArg, got {err:?}"
        );
    }

    /// `HOLDS` — Element-type confusion: correctly-shaped List but ints not strs.
    /// String::from_monty rejects each in the collect() → BadArg. Distinct from the wrong-container
    /// angle; attacks per-element conversion.
    /// seam: convert.rs Vec<String>::from_arg element loop (String::from_monty on Int)
    #[test]
    fn group_agg_ints_for_str_list_is_bad_arg() {
        let path = format!("{}/tests/data/sample.parquet", env!("CARGO_MANIFEST_DIR"));
        let mut engine = DuckEngine::new_in_memory().unwrap();
        let mut handles = Registry::new();
        let mut cx = ToolCx { engine: &mut engine, handles: &mut handles };
        let reg = inventory::iter::<Tool>().find(|t| t.name == "register").unwrap();
        let h = (reg.dispatch)(&mut cx, &[MontyObject::String(path)], &[]).unwrap();
        let ga = inventory::iter::<Tool>().find(|t| t.name == "group_agg").unwrap();
        let by = MontyObject::List(vec![MontyObject::Int(1), MontyObject::Int(2)]);
        let metrics = MontyObject::List(vec![MontyObject::Tuple(vec![
            MontyObject::String("t".into()),
            MontyObject::String("SUM(amount)".into()),
        ])]);
        let err = (ga.dispatch)(&mut cx, &[h, by, metrics], &[]).unwrap_err();
        assert!(
            matches!(err, DropletError::BadArg(ref m) if m.contains("expected str")),
            "int elements in the `by` list must be BadArg, got {err:?}"
        );
    }

    /// `HOLDS` — Over-arity inner tuple: the refutable `let [a,b] = pair else {..}` must reject a
    /// 3-element tuple as 'expected a 2-tuple' (NOT panic, NOT silently take first two). Attacks the
    /// irrefutable-pattern assumption.
    /// seam: convert.rs Vec<(String,String)>::from_arg slice-pattern `let [a,b] = pair`
    #[test]
    fn group_agg_three_tuple_metric_is_bad_arg() {
        let path = format!("{}/tests/data/sample.parquet", env!("CARGO_MANIFEST_DIR"));
        let mut engine = DuckEngine::new_in_memory().unwrap();
        let mut handles = Registry::new();
        let mut cx = ToolCx { engine: &mut engine, handles: &mut handles };
        let reg = inventory::iter::<Tool>().find(|t| t.name == "register").unwrap();
        let h = (reg.dispatch)(&mut cx, &[MontyObject::String(path)], &[]).unwrap();
        let ga = inventory::iter::<Tool>().find(|t| t.name == "group_agg").unwrap();
        let by = MontyObject::List(vec![MontyObject::String("region".into())]);
        let metrics = MontyObject::List(vec![MontyObject::Tuple(vec![
            MontyObject::String("t".into()),
            MontyObject::String("SUM(amount)".into()),
            MontyObject::String("extra".into()),
        ])]);
        let err = (ga.dispatch)(&mut cx, &[h, by, metrics], &[]).unwrap_err();
        assert!(
            matches!(err, DropletError::BadArg(ref m) if m.contains("2-tuple")),
            "a 3-tuple metric must be rejected, got {err:?}"
        );
    }

    /// `HOLDS` — Under-arity inner tuple (mirror of the 3-tuple): the slice pattern must also
    /// reject a 1-element tuple. Under-indexing is where an args[1]-style bug would hide; here
    /// the refutable pattern protects it. Distinct boundary.
    /// seam: convert.rs Vec<(String,String)>::from_arg slice-pattern on a 1-element tuple
    #[test]
    fn group_agg_one_tuple_metric_is_bad_arg() {
        let path = format!("{}/tests/data/sample.parquet", env!("CARGO_MANIFEST_DIR"));
        let mut engine = DuckEngine::new_in_memory().unwrap();
        let mut handles = Registry::new();
        let mut cx = ToolCx { engine: &mut engine, handles: &mut handles };
        let reg = inventory::iter::<Tool>().find(|t| t.name == "register").unwrap();
        let h = (reg.dispatch)(&mut cx, &[MontyObject::String(path)], &[]).unwrap();
        let ga = inventory::iter::<Tool>().find(|t| t.name == "group_agg").unwrap();
        let by = MontyObject::List(vec![MontyObject::String("region".into())]);
        let metrics = MontyObject::List(vec![MontyObject::Tuple(vec![MontyObject::String("only".into())])]);
        let err = (ga.dispatch)(&mut cx, &[h, by, metrics], &[]).unwrap_err();
        assert!(
            matches!(err, DropletError::BadArg(ref m) if m.contains("2-tuple")),
            "a 1-tuple metric must be rejected, got {err:?}"
        );
    }

    /// `HOLDS` — Tuple-position confusion: alias is str (correct) but the 2nd element is a str
    /// instead of an int handle. Dataset::from_arg -> i64::from_monty rejects the wrong VARIANT.
    /// Distinct from the forged-int-handle angle (valid int that missed the registry).
    /// seam: convert.rs Vec<(String,Dataset)>::from_arg -> Dataset::from_arg on a String 2nd elem
    #[test]
    fn local_sql_second_elem_wrong_type_str_not_handle_is_bad_arg() {
        let arg = MontyObject::List(vec![MontyObject::Tuple(vec![
            MontyObject::String("usage".into()),
            MontyObject::String("not_a_handle".into()),
        ])]);
        let err = dispatch("local_sql", &[MontyObject::String("SELECT 1".into()), arg]).unwrap_err();
        assert!(
            matches!(err, DropletError::BadArg(ref m) if m.contains("expected int")),
            "a str where a Dataset handle is required must be BadArg, got {err:?}"
        );
    }

    /// `HOLDS` — Structural confusion one level up: outer list is fine but its element is a bare Int
    /// instead of a (str, handle) tuple. The inner as_seq must reject the scalar. Distinct from
    /// wrong-element-inside-tuple; here there is no tuple at all.
    /// seam: convert.rs Vec<(String,Dataset)>::from_arg inner as_seq
    #[test]
    fn local_sql_dataset_list_inner_not_a_tuple_is_bad_arg() {
        let arg = MontyObject::List(vec![MontyObject::Int(0)]);
        let err = dispatch("local_sql", &[MontyObject::String("SELECT 1".into()), arg]).unwrap_err();
        assert!(
            matches!(err, DropletError::BadArg(ref m) if m.contains("tuple[str, Dataset]")),
            "a non-tuple element in the dataset list must be BadArg, got {err:?}"
        );
    }

    // ── Macro arity probes (catch_dispatch / catch_dispatch_kw) ───────────────────────────────────
    //
    // The `#[droplet_tool]` macro thunk indexes `args[i]` directly (macros/src/lib.rs:70):
    //   `let #arg_idents = <#arg_types as FromArg>::from_arg(cx, &args[#indices])?;`
    // There is NO bounds guard before the index, so a short args slice panics. All four tests below
    // were originally PROBE (desired contract: no panic, contained Err). After running, all four
    // observed a PANIC (res.is_err()) → converted to CANARY pinning the observed behavior.
    // End-to-end reachability (does this panic cross `run_code`?) is tested in the error_safety
    // class (Task 9).

    /// `CANARY` (was PROBE) — FINDING: macro thunk panics on missing arg (args[1] out of bounds).
    /// OBSERVED: res.is_err() — the thunk panics on the OOB index of args[1] for `query`'s `sql` arg.
    /// DESIRED CONTRACT: res.is_ok() + inner Err(BadArg) — a contained error, not a host panic.
    /// FINDING: macros/src/lib.rs:70 thunk `&args[#indices]` direct indexing; no arity guard.
    /// E2E reachability tested in error_safety (Task 9).
    /// seam: macros/src/lib.rs:70 thunk `&args[#indices]`; session.rs dispatch has NO catch_unwind
    #[test]
    #[allow(non_snake_case)]
    fn CANARY_query_missing_arg_panics_at_thunk_level() {
        // DESIRED (when macro gains an arity guard): res.is_ok() && res.unwrap().is_err()
        let res = catch_dispatch("query", &[MontyObject::String("/tmp/x.parquet".into())]);
        assert!(
            res.is_err(),
            "CANARY: macro thunk must panic on missing `sql` arg (args[1] OOB); \
             if this flips to is_ok() the arity finding is fixed — update to PROBE/HOLDS"
        );
    }

    /// `CANARY` (was PROBE) — FINDING: kwargs-only call panics (thunk ignores _kwargs, indexes
    /// empty args[0] → OOB).
    /// OBSERVED: res.is_err() — the thunk panics because positional args is empty and args[0] OOB.
    /// DESIRED CONTRACT: res.is_ok() + inner Err(BadArg) — kwargs silently dropped AND empty-args
    /// index must not panic.
    /// FINDING: macros/src/lib.rs thunk ignores _kwargs (line 68) + args[0] OOB on empty args.
    /// seam: macros/src/lib.rs thunk ignores _kwargs + args[0] indexing on empty args
    #[test]
    #[allow(non_snake_case)]
    fn CANARY_query_kwargs_only_panics_at_thunk_level() {
        // DESIRED (when fixed): res.is_ok() && res.unwrap() is Err(BadArg) — kwargs surfaced as error
        let res = catch_dispatch_kw(
            "query",
            &[],
            &[(
                MontyObject::String("sql".into()),
                MontyObject::String("SELECT 1".into()),
            )],
        );
        assert!(
            res.is_err(),
            "CANARY: kwargs-only call must panic (thunk ignores kwargs, args[0] OOB on empty args); \
             if this flips the finding is fixed — update to PROBE/HOLDS"
        );
    }

    /// `CANARY` (was PROBE) — FINDING: extra (over-arity) args to `query` are silently dropped;
    /// the 3-arg call succeeds without any arity error.
    /// OBSERVED: res.is_ok() + inner Ok(_) — the thunk indexes only args[0]/args[1] and ignores
    /// args[2], so the call SUCCEEDS (not even a BadArg is returned).
    /// DESIRED CONTRACT: res.is_ok() + inner Err(BadArg) — a 3-arg call to a 2-arg tool must be
    /// a contained arity error, not silent truncation.
    /// FINDING: macros/src/lib.rs thunk indexes only args[0..N]; extra args silently dropped (no
    /// len check). The successful call is also an exfiltration angle (extra arg may carry smuggled
    /// data the tool ignores — addressed in context of the accepted V1a gap).
    /// seam: macros/src/lib.rs thunk indexes only args[0..N]; no len check
    #[test]
    #[allow(non_snake_case)]
    fn CANARY_query_too_many_args_extra_arg_silently_dropped() {
        let path = format!("{}/tests/data/sample.parquet", env!("CARGO_MANIFEST_DIR"));
        // DESIRED (when fixed): inner Err(BadArg) — over-arity must be a contained error
        let res = catch_dispatch(
            "query",
            &[
                MontyObject::String(path),
                MontyObject::String("SELECT * FROM data".into()),
                MontyObject::String("EXTRA".into()),
            ],
        );
        assert!(res.is_ok(), "over-arity must not panic (thunk ignores the extra arg)");
        let inner = res.unwrap();
        assert!(
            inner.is_ok(),
            "CANARY: a 3-arg call to 2-arg `query` currently SUCCEEDS (extra arg silently dropped); \
             DESIRED: inner Err(BadArg) arity error. Flips when macro gains a len check."
        );
    }

    /// `HOLDS` — (Originally PROBE → CANARY; observed no panic; re-upgraded to HOLDS.)
    /// group_agg with 2 args (missing metrics): the thunk evaluates args[0] (Dataset handle) FIRST;
    /// an unissued handle returns BadHandle(0) via `?` BEFORE the thunk ever reaches args[2].
    /// The early-error short-circuit makes this a CONTAINED error, not a host panic — the arity
    /// bug is masked by the earlier handle miss. The systemic arity-panic finding is still real
    /// (demonstrated by to_rows_zero_args and query_missing_arg where no early-exit exists),
    /// but this specific invocation happens to be safe because the first arg fails first.
    /// OBSERVED: res.is_ok() + inner Err(BadHandle(0)) — no panic.
    /// seam: convert.rs Dataset::from_arg (args[0]) fails before args[2] is reached in thunk
    #[test]
    #[allow(non_snake_case)]
    fn group_agg_missing_metrics_short_circuits_at_bad_handle() {
        let by = MontyObject::List(vec![MontyObject::String("region".into())]);
        let res = catch_dispatch("group_agg", &[MontyObject::Int(0), by]);
        assert!(res.is_ok(), "must not panic — BadHandle(0) short-circuits before args[2] OOB");
        let inner = res.unwrap();
        assert!(
            matches!(inner, Err(DropletError::BadHandle(0))),
            "must return BadHandle(0) from args[0] resolution, got {inner:?}"
        );
    }

    /// `CANARY` (was PROBE) — FINDING: minimal arity panic: zero args to a 1-arg tool (to_rows).
    /// Smallest reproduction — isolates the args[0] OOB crash from any handle/registry logic.
    /// OBSERVED: res.is_err() — the thunk panics on args[0] OOB on an EMPTY args slice.
    /// DESIRED CONTRACT: res.is_ok() + inner Err(BadArg).
    /// seam: macros/src/lib.rs thunk args[0] on an EMPTY args slice (1-param tool)
    #[test]
    #[allow(non_snake_case)]
    fn CANARY_to_rows_zero_args_panics_at_thunk_level() {
        // DESIRED (when macro gains an arity guard): res.is_ok() && inner is Err(BadArg)
        let res = catch_dispatch("to_rows", &[]);
        assert!(
            res.is_err(),
            "CANARY: to_rows() with no args must panic (args[0] OOB on empty args); \
             if this flips the finding is fixed — update to PROBE/HOLDS"
        );
    }

    // ── Miscellaneous arg-conversion corners ──────────────────────────────────────────────────────

    /// `HOLDS` — String-param confusion at the entry tool: an int where the parquet PATH (str) is
    /// required must be BadArg before any FS touch. Confirms register doesn't treat an int as a
    /// pre-existing handle.
    /// seam: convert.rs String::from_monty (Int arm) via register's first visible param
    #[test]
    fn register_int_path_is_bad_arg_not_panic() {
        let err = dispatch("register", &[MontyObject::Int(7)]).unwrap_err();
        assert!(
            matches!(err, DropletError::BadArg(ref m) if m.contains("expected str")),
            "got {err:?}"
        );
    }

    /// `HOLDS` — Resolution-ordering: scalar takes (Dataset, expr). The forged handle must fail at
    /// from_arg (BadHandle) BEFORE the agent SQL expr compiles — proving handle validation gates SQL
    /// execution. Distinct from to_rows (scalar carries a 2nd SQL arg that must NOT run first).
    /// seam: convert.rs from_arg ordering: handle resolved (arg 0) BEFORE the expr str reaches the engine
    #[test]
    fn scalar_forged_handle_is_bad_handle_before_sql() {
        let err = dispatch(
            "scalar",
            &[MontyObject::Int(31337), MontyObject::String("SUM(amount)".into())],
        )
        .unwrap_err();
        assert!(
            matches!(err, DropletError::BadHandle(31337)),
            "a forged handle must fail at resolution, not as a SQL error, got {err:?}"
        );
    }

    /// `HOLDS` — BOTH handles forged, pinning that the LEFT (first-evaluated) fails first →
    /// confirms left-to-right positional resolution and that join with neither issued never
    /// silently joins empty views. Distinct from the mixed valid/invalid test (left valid, right
    /// forged).
    /// seam: convert.rs Dataset::from_arg on join's FIRST (left) param via fresh empty registry;
    /// left-to-right ordering
    #[test]
    fn join_first_handle_forged_is_bad_handle_left_arg() {
        let err = dispatch(
            "join",
            &[
                MontyObject::Int(1),
                MontyObject::Int(2),
                MontyObject::String("l.id = r.id".into()),
            ],
        )
        .unwrap_err();
        assert!(
            matches!(err, DropletError::BadHandle(1)),
            "the LEFT forged handle must fail first, got {err:?}"
        );
    }
}
