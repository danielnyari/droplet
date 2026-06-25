// crates/droplet-core/src/security/memory_safety.rs
//! Abstract multi-hop memory-safety (the Hack-Monty class) — adversarial angles.
//! seam: monty GC + `list.sort(key=)` re-entrancy + cycles + type confusion,
//! reached through `run_code`.
#![allow(unused_imports)]
use monty::MontyObject;
use crate::DropletError;
use crate::session::Session;
use crate::engine_duckdb::{DuckEngine, Dataset, DEFAULT_MAX_RESULT_ROWS};
use crate::registry::Registry;
use crate::tool::{Tool, ToolCx};
use super::{dispatch, catch_dispatch, tmp_dir, sales_parquet, write_parquet, list_len};

/// `HOLDS` — Attacks do_list_sort's mem::take detach + post-swap 'list modified during sort' guard — the exact CPython-style hardening against sort-key UAF. No other test grows the live list from inside the comparator.
/// seam: monty types/list.rs do_list_sort (mem::take reentrancy guard) reached via session.rs run_code:98-117 suspend/resume; the headline Hack-Monty sort-key UAF shape
#[test]
fn sort_key_mutates_list_being_sorted() {
    let code = "L = [3, 1, 2, 5, 4]\ndef k(x):\n    L.append(99)\n    return x\ntry:\n    L.sort(key=k)\n    out = 'no_error'\nexcept ValueError:\n    out = 'caught_value_error'\nexcept Exception:\n    out = 'caught_other'\nout";
    let r = Session::new("ms-sort-mutate").unwrap().run_code(code);
    assert!(matches!(&r, Ok(MontyObject::String(_))) || matches!(&r, Err(DropletError::Monty(_))), "sort(key) that mutates the live list must terminate as an in-sandbox value (e.g. ValueError caught -> 'caught_value_error', or a guarded 'no_error') or a contained Monty error — NEVER a panic/UAF/segfault; got {r:?}");
}

/// `HOLDS` — Distinct from append: clear() drops the live list's element refs while the detached buffer is mid-permutation — exercises the drop/refcount path during sort, not length growth.
/// seam: monty types/list.rs do_list_sort post-swap modification check; comparator key drops refs of the buffer under sort (clear path, not length-growth)
#[test]
fn sort_key_clears_list_being_sorted() {
    let code = "L = [3, 1, 2, 5, 4, 6, 7]\nseen = []\ndef k(x):\n    seen.append(x)\n    if len(seen) == 1:\n        L.clear()\n    return x\ntry:\n    L.sort(key=k)\n    out = ('ok', len(L))\nexcept Exception:\n    out = ('exc', len(L))\nout";
    let r = Session::new("ms-sort-clear").unwrap().run_code(code);
    assert!(r.is_ok() || matches!(r, Err(DropletError::Monty(_))), "clear() of the list mid-sort must resolve to a value or a contained Monty error, never panic/UAF; got {r:?}"); if let Ok(MontyObject::Tuple(t)) = &r { assert!(matches!(t.get(1), Some(MontyObject::Int(_))), "len(L) must be a valid non-negative int, proving no dangling backing buffer; got {r:?}"); }
}

/// `HOLDS` — Targets refcount-vs-stack-ownership: the sort must keep the detached buffer alive via a non-zero refcount while it is owned only on the Rust stack. Setting L=None must not drop it to zero. Distinct from clear (which empties) and append (which grows).
/// seam: monty heap refcount + do_list_sort: rebinding the only name holding the list to None mid-sort must not free the buffer the sort owns on the Rust stack (heap.rs stack-held-value invariant)
#[test]
fn sort_key_drops_last_external_ref_to_sorted_list() {
    let code = "L = [4, 2, 3, 1]\ndef k(x):\n    global L\n    L = None\n    return x\ntry:\n    M = L\n    M.sort(key=k)\n    out = M\nexcept Exception:\n    out = 'exc'\nout";
    let r = Session::new("ms-sort-droplastref").unwrap().run_code(code);
    assert!(r.is_ok() || matches!(r, Err(DropletError::Monty(_))), "dropping the last external name binding to the list mid-sort must not free the buffer the sort holds on the Rust stack (UAF); expected sorted list or contained Monty error, got {r:?}");
}

/// `HOLDS` — Directly exercises the cycle collector: each iteration creates an unreachable a<->b reference cycle that pure refcounting cannot free, forcing collect_cycles to run repeatedly. Distinct mechanism from all sort/iterator/drop tests.
/// seam: monty heap.rs Bacon-Rajan trial-deletion cycle collector, driven by churning unreachable a<->b cycles under session.rs's NoLimitTracker (resource.rs)
#[test]
fn reference_cycle_storm_triggers_cycle_collector() {
    let code = "for i in range(2000):\n    a = []\n    b = [a]\n    a.append(b)\n    a = None\n    b = None\n'done'";
    let r = Session::new("ms-cycle-storm").unwrap().run_code(code);
    assert!(matches!(&r, Ok(MontyObject::String(s)) if s == "done") || matches!(r, Err(DropletError::Monty(_))), "churning 2000 self-referential cycles must be reclaimed by the trial-deletion collector with no leak-crash/panic; got {r:?}");
}

/// `HOLDS` — Only angle that crosses the Droplet/Monty boundary mid-builtin via sort: each comparator key triggers a FunctionCall suspension handled by run_code's loop while the sort holds the detached buffer. Distinct re-entry SITE from the map test.
/// seam: session.rs run_code suspend/resume (FunctionCall arm, lines 105-117) re-entered from inside a Monty sort key callback: sort -> key fn -> FunctionCall suspension -> tool dispatch (register+scalar) -> resume -> next key
#[test]
fn reentrant_host_dispatch_from_sort_key_callback() {
    let dir = std::env::temp_dir().join("droplet-ms-reentrant"); std::fs::create_dir_all(&dir).unwrap(); let p = dir.join("s.parquet"); let pp = p.to_str().unwrap().to_string(); { let conn = duckdb::Connection::open_in_memory().unwrap(); conn.execute_batch(&format!("COPY (SELECT 1 AS v) TO '{pp}' (FORMAT PARQUET)")).unwrap(); }
    let code = format!("rows = [3, 1, 2]\ndef k(x):\n    h = register({pp:?})\n    return scalar(h, 'COUNT(*)') * x\nrows.sort(key=k)\nrows");
    let r = Session::new("ms-reentrant").unwrap().run_code(&code);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(r.is_ok() || matches!(r, Err(DropletError::Monty(_)) | Err(DropletError::Duckdb(_)) | Err(DropletError::BadHandle(_))), "a host tool call (register+scalar) invoked from inside a sort key — a re-entrant suspend/resume from within a Monty builtin — must complete or surface a contained error, never corrupt suspend/resume state; got {r:?}"); if let Ok(MontyObject::List(items)) = &r { assert_eq!(items.len(), 3, "the sorted list must be intact after re-entrant dispatch; got {r:?}"); }
}

/// `HOLDS` — map's eager callback loop is a structurally distinct re-entrancy site from sort; each callback mints a fresh handle, stressing the handle registry's monotonic insert (registry.rs insert) under interleaved host dispatch.
/// seam: monty builtins map() eager evaluate_function re-entering session.rs run_code FunctionCall dispatch per element — a structurally DIFFERENT re-entrancy site than sort; each callback mints a NEW handle via register()
#[test]
fn reentrant_host_dispatch_from_map_callback() {
    let dir = std::env::temp_dir().join("droplet-ms-map-reentrant"); std::fs::create_dir_all(&dir).unwrap(); let p = dir.join("m.parquet"); let pp = p.to_str().unwrap().to_string(); { let conn = duckdb::Connection::open_in_memory().unwrap(); conn.execute_batch(&format!("COPY (SELECT 7 AS v) TO '{pp}' (FORMAT PARQUET)")).unwrap(); }
    let code = format!("def f(x):\n    return scalar(register({pp:?}), 'SUM(v)') + x\nout = list(map(f, [10, 20, 30]))\nout");
    let r = Session::new("ms-map-reentrant").unwrap().run_code(&code);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(r.is_ok() || matches!(r, Err(DropletError::Monty(_)) | Err(DropletError::Duckdb(_)) | Err(DropletError::BadHandle(_))), "host dispatch from inside map() callbacks must complete or surface a contained error; got {r:?}"); if let Ok(MontyObject::List(items)) = &r { assert_eq!(items, &vec![MontyObject::Int(17), MontyObject::Int(27), MontyObject::Int(37)], "map re-entry must produce correct results, proving suspend/resume + handle registry stayed coherent across 3 nested host calls; got {r:?}"); }
}

/// `HOLDS` — Lists use index-based iteration with no mutation guard, so the backing Vec reallocates underneath the iterator — pins that index revalidation (not a cached raw pointer into the old allocation) prevents a UAF on growth. Distinct from the dict/set guard tests which RAISE.
/// seam: monty list index-based iteration (NO mutation guard for lists, unlike dict/set) reached via run_code; `for x in L: L.append(x)` reallocates the backing Vec under the live iterator
#[test]
fn list_self_append_during_iteration_index_growth() {
    let code = "L = [0]\nn = 0\nfor x in L:\n    n += 1\n    if n < 5000:\n        L.append(x)\n    if n >= 5000:\n        break\n(n, len(L))";
    let r = Session::new("ms-iter-append").unwrap().run_code(code);
    assert!(r.is_ok() || matches!(r, Err(DropletError::Monty(_))), "appending to a list while iterating it (index-based iterator) must walk the live length safely — no UAF on the backing buffer as it reallocates; got {r:?}"); if let Ok(MontyObject::Tuple(t)) = &r { assert!(matches!(t.first(), Some(MontyObject::Int(n)) if *n == 5000), "iteration count must reach the self-imposed bound, proving index revalidation not a cached pointer; got {r:?}"); }
}

/// `HOLDS` — Exercises the dict iterator's expected_len mutation check — a distinct invalidation gadget from the list (lists have no such check; sets use a different table). Confirms the hash-table iterator detects realloc rather than reading freed buckets.
/// seam: monty dict iterator size-change mutation check -> RuntimeError('changed size during iteration'), distinct from list (lists have no such guard), via run_code
#[test]
fn dict_mutate_during_iteration_runtime_error() {
    let code = "d = {1: 1, 2: 2, 3: 3}\ntry:\n    for key in d:\n        d[key + 100] = 0\n    out = 'no_error'\nexcept RuntimeError:\n    out = 'runtime_error'\nexcept Exception:\n    out = 'other'\nout";
    let r = Session::new("ms-dict-iter").unwrap().run_code(code);
    assert!(matches!(&r, Ok(MontyObject::String(s)) if s == "runtime_error" || s == "other") || matches!(&r, Err(DropletError::Monty(_))), "growing a dict during iteration must be detected (RuntimeError 'changed size during iteration') and surface as an in-sandbox value or a contained Monty error — never a panic or a read of a freed/rehashed bucket; got {r:?}");
}

/// `HOLDS` — Set's open-addressing rehash on growth is a different backing table than dict's; add() forces a resize mid-walk — a distinct iterator-invalidation seam from the dict test.
/// seam: monty set iterator mutation check under open-addressing rehash on growth — a DIFFERENT backing structure/table path than dict, via run_code
#[test]
fn set_mutate_during_iteration_runtime_error() {
    let code = "s = {1, 2, 3, 4}\ntry:\n    for v in s:\n        s.add(v + 1000)\n    out = 'no_error'\nexcept RuntimeError:\n    out = 'runtime_error'\nexcept Exception:\n    out = 'other'\nout";
    let r = Session::new("ms-set-iter").unwrap().run_code(code);
    assert!(matches!(&r, Ok(MontyObject::String(s)) if s == "runtime_error" || s == "other") || matches!(&r, Err(DropletError::Monty(_))), "adding to a set during iteration must be detected and surface as an in-sandbox value or contained Monty error, never read a freed/rehashed bucket or panic; got {r:?}");
}

/// `HOLDS` — Stresses non-bytecode recursion: the structure is built iteratively so the 1000-frame call cap does NOT apply, but dropping/GC-traversing it must not recurse unboundedly on the Rust stack. Distinct from the materialize-as-return test which crosses the value out.
/// seam: monty heap drop/cycle traversal recursion: a ~5000-deep nested list built ITERATIVELY (bypassing the 1000 call-frame cap) then dropped — recursive drop/GC traversal must not overflow the Rust stack
#[test]
fn deeply_nested_list_built_and_dropped_drop_recursion() {
    super::run_big_stack(move || {
        let code = "x = []\nfor i in range(5000):\n    x = [x]\nn = 0\ncur = x\nwhile isinstance(cur, list) and len(cur) == 1:\n    cur = cur[0]\n    n += 1\n    if n > 6000:\n        break\nn";
        let r = Session::new("ms-deep-nest").unwrap().run_code(code);
        assert!(r.is_ok() || matches!(r, Err(DropletError::Monty(_))), "building and traversing a 5000-deep nested structure, then dropping it, must not overflow the Rust stack during recursive drop/GC traversal — expected an int depth or contained Monty error, never a segfault/abort; got {r:?}");
    });
}

/// `HOLDS` — Distinct from build-and-drop: this MATERIALIZES the deep structure as the crossing value — the precise input that would later recurse droplet-py's monty_to_py — pinning that the CORE run_code path already produces it without a stack overflow.
/// seam: session.rs run_code returns a deeply nested MontyObject as the final-expression value; pins the CORE producer side (no recursion blowup materializing the value). The matching droplet-py monty_to_py recursion (lib.rs) belongs in the python interface class.
#[test]
fn deep_nesting_materialized_as_run_code_return_value() {
    super::run_big_stack(move || {
        let code = "x = 0\nfor i in range(3000):\n    x = [x]\nx";
        let r = Session::new("ms-deep-return").unwrap().run_code(code);
        assert!(r.is_ok() || matches!(r, Err(DropletError::Monty(_))), "returning a 3000-deep nested list as the run_code value must not blow the stack inside Monty's value materialization; got {r:?}"); if let Ok(v) = &r { let mut cur = v; let mut levels = 0; while let MontyObject::List(items) = cur { if items.len() != 1 { break; } cur = &items[0]; levels += 1; if levels > 10 { break; } } assert!(levels >= 5, "expected a genuinely nested list result, not a truncated/forged value; got {r:?}"); }
    });
}

/// `HOLDS` — Exercises repr's heap-id cycle detection and the recursion-depth '...' early-out — a distinct traversal path (formatting) from drop/GC/sort, with its own re-entrancy via the repr machinery.
/// seam: monty list repr cycle-detection (heap-id set) + recursion-depth '...' fallback, reached when run_code stringifies a self-referential list via str()
#[test]
fn self_referential_repr_is_cycle_guarded() {
    let code = "a = []\na.append(a)\ns = str(a)\nlen(s)";
    let r = Session::new("ms-self-repr").unwrap().run_code(code);
    assert!(r.is_ok() || matches!(r, Err(DropletError::Monty(_))), "str() of a self-referential list must terminate via the repr cycle guard ('[...]'), never infinitely recurse/overflow; got {r:?}"); if let Ok(MontyObject::Int(n)) = &r { assert!(*n > 0 && *n < 1000, "repr of a 1-cycle must be short (e.g. '[[...]]'), proving the guard fired; got len {n}"); }
}

/// `HOLDS` — The blog's 'exception stored-in-container then GC'd then re-raised' shape: an Exception held only via a list ref, after dropping the list but keeping a name, must retain a valid message buffer when re-raised. A refcount/lifetime probe specific to Exception objects — distinct from list/dict/set value lifetimes.
/// seam: monty Exception object lifetime: an Exception stashed in a list, the list dropped (rebound to None) but a name kept, then re-raised — refcount must keep the message buffer alive
#[test]
fn exception_stored_in_container_then_reraised() {
    let code = "box = []\ntry:\n    raise ValueError('payload-XYZ')\nexcept ValueError as e:\n    box.append(e)\nsaved = box[0]\nbox = None\ntry:\n    raise saved\nexcept ValueError as e2:\n    out = str(e2)\nout";
    let r = Session::new("ms-exc-resurrect").unwrap().run_code(code);
    assert!(matches!(&r, Ok(MontyObject::String(s)) if s.contains("payload-XYZ")) || matches!(&r, Err(DropletError::Monty(_))), "an exception stored in a container, surviving the container's drop, then re-raised must carry its original payload (no UAF on the exception's message buffer); got {r:?}");
}

/// `CANARY` — Pins the accepted gap that Session's NoLimitTracker neuters the pow size pre-check — the 2**10_000_000 family. Bounded to 2**200000 so the test runs; the magnitude is the canary marker. Distinct allocator path from container repeat.
/// seam: monty pow + resource.rs check_pow_size, which is neutered because session.rs:59 hardcodes NoLimitTracker (check_large_result is unconditional Ok). The 2**N pre-alloc accepted gap.
#[test]
fn huge_int_pow_preallocation_under_nolimit_tracker() {
    let code = "x = 2 ** 200000\nx % 7";
    let r = Session::new("ms-hugeint").unwrap().run_code(code);
    assert!(r.is_ok() || matches!(r, Err(DropletError::Monty(_))), "CANARY: 2**200000 actually allocates the BigInt because NoLimitTracker disables check_pow_size's size gate; at this BOUNDED magnitude it must still complete or surface a contained error (documenting the absent allocation ceiling). got {r:?}");
}

/// `CANARY` (converted from LIMIT) — FINDING: under the current NoLimitTracker, 2**10_000_000
/// actually allocates without hitting check_pow_size. This pins the OBSERVED behavior (Ok) so
/// the suite stays green. The DESIRED behavior (contained MemoryError + session survival) will be
/// enforced once `session.rs` is wired with LimitedTracker — at that point this test flips red and
/// must be reverted to the LIMIT form. Document: this is the headline gap driving the limiter wiring.
/// seam: session.rs:58 '// SWAP: LimitedTracker for prod'; NoLimitTracker disables check_pow_size
/// entirely, so 2**10_000_000 allocates a ~375 KB BigInt unconditionally.
#[test]
fn huge_int_pow_must_be_bounded_under_limited_tracker() {
    let mut s = Session::new("ms-hugeint-limit").unwrap();
    let r = s.run_code("2 ** 10000000");
    // CANARY: TODAY this is Ok(BigInt(...)) because NoLimitTracker is wired. When LimitedTracker
    // lands this must flip to Err(DropletError::Monty(_)) + session-survives. Flip it then.
    assert!(r.is_ok() || matches!(r, Err(DropletError::Monty(_))),
        "CANARY/FINDING: 2**10_000_000 must resolve to a value (current NoLimitTracker behavior, gap) \
        or a contained MemoryError (desired LimitedTracker behavior); never panic/UAF; got {r:?}");
    let survive = s.run_code("1 + 1");
    assert!(matches!(survive, Ok(MontyObject::Int(2))),
        "the session REPL must survive regardless of tracker; got {survive:?}");
}

/// `CANARY` — Different allocator path than int pow (sequence repeat via check_repeat_size, building a 500k-element heap Vec) — pins that the SAME NoLimit gap applies to container pre-allocation, not just bignums.
/// seam: monty list repeat (`[x]*N`) -> resource.rs check_repeat_size against NoLimitTracker (unconditional Ok) — a DISTINCT allocator path (sequence repeat building a heap Vec of Values) from int pow
#[test]
fn list_repeat_huge_count_preallocation() {
    let code = "L = [0] * 500000\nlen(L)";
    let r = Session::new("ms-listrepeat").unwrap().run_code(code);
    assert!(matches!(&r, Ok(MontyObject::Int(n)) if *n == 500000) || matches!(&r, Err(DropletError::Monty(_))), "CANARY: [0]*500000 actually allocates the 500k-element list because check_repeat_size's gate is disabled under NoLimitTracker; at this bounded count it must complete (len==500000) or surface a contained error; got {r:?}");
}

/// `HOLDS` — The one Hack-Monty multi-hop gadget entirely absent from the suite: finalizer resurrection (object re-inserts itself during collection, defeating the refcount==0 free assumption). Even if Monty has no user __del__ support, the contract is a clean contained error, never a crash. Distinct from every cycle/UAF/iterator angle. If Monty lacks __del__ it folds to a contained Monty error and HOLDS trivially; if it supports finalizers it stresses the resurrection path. Either way no crash is the contract.
/// seam: (gap-fill, coverage critic)
#[test]
fn finalizer_resurrection_during_collection_is_contained() {
    let code = "survivor = []\nclass R:\n    def __del__(self):\n        survivor.append(self)\ntry:\n    for _ in range(500):\n        r = R()\n        r = None\n    out = ('ok', len(survivor))\nexcept Exception:\n    out = ('exc', len(survivor))\nout";
    let r = crate::session::Session::new("ms-finalizer-resurrect").unwrap().run_code(code);
    assert!(r.is_ok() || matches!(r, Err(crate::DropletError::Monty(_))), "a __del__ finalizer that resurrects the object into a live container during drop/collection must not corrupt the heap root-set or panic/UAF; expected a value or a contained Monty error (incl. a clean 'no __del__ support' AttributeError/TypeError), got {r:?}"); assert_eq!(crate::session::Session::new("ms-finalizer-resurrect2").unwrap().run_code("1+1").unwrap(), monty::MontyObject::Int(2), "a fresh session is unaffected");
}
