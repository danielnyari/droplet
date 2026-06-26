// crates/droplet-core/src/security/sandbox_escape.rs
//! Python sandbox-escape gadgets — adversarial angles. seam: monty interpreter containment (imports, builtins, dunder-introspection chains) via `Session::run_code`.
#![allow(unused_imports)]
use super::{catch_dispatch, dispatch, list_len, sales_parquet, tmp_dir, write_parquet};
use crate::DropletError;
use crate::engine_duckdb::{DEFAULT_MAX_RESULT_ROWS, Dataset, DuckEngine};
use crate::registry::Registry;
use crate::session::Session;
use crate::tool::{Tool, ToolCx};
use monty::MontyObject;

/// `HOLDS` — `import sys` succeeds (Sys is in StandardLib); escape is blocked at the unimplemented sys.exit attribute, not at import — distinct from `import os` whose getcwd is an unhandled OsCall.
/// seam: monty modules/sys.rs attribute surface reached via Session::run_code; `import sys` SUCCEEDS but sys.exit is absent
#[test]
fn import_sys_then_exit_is_blocked() {
    let mut s = Session::new("sx-sys-exit").unwrap();
    let r = s.run_code("import sys\nsys.exit(0)");
    assert!(r.is_err(), "sys.exit must not reach the host / must raise");
    assert_eq!(
        s.run_code("1+1").unwrap(),
        MontyObject::Int(2),
        "REPL survives the AttributeError"
    );
}

/// `HOLDS` — Call-stack reflection gadget; distinct from imports and from sys.exit (different missing attribute on the same module).
/// seam: monty sys module attribute resolution; frame-walk escape sys._getframe().f_globals
#[test]
fn sys_getframe_frame_walk_is_blocked() {
    let mut s = Session::new("sx-getframe").unwrap();
    let r = s.run_code("import sys\nsys._getframe(0).f_globals");
    assert!(
        r.is_err(),
        "frame introspection must raise, never expose f_globals"
    );
}

/// `HOLDS` — Type-graph traversal via dunder attributes (not import, not builtin). Monty has no __class__/__bases__/__subclasses__ on instances.
/// seam: monty instance attribute resolution via run_code; ().__class__.__bases__[0].__subclasses__() RCE chain
#[test]
fn subclasses_introspection_chain_is_blocked() {
    let mut s = Session::new("sx-subclasses").unwrap();
    let r = s.run_code("().__class__.__bases__[0].__subclasses__()");
    assert!(
        r.is_err(),
        "the __subclasses__ walk must raise AttributeError"
    );
}

/// `HOLDS` — Climbs the MRO of a concrete int type rather than the object subclass registry — separate dunder root than __subclasses__.
/// seam: monty int/type attribute resolution; (1).__class__.__mro__ MRO walk
#[test]
fn int_mro_walk_is_blocked() {
    let mut s = Session::new("sx-mro").unwrap();
    let r = s.run_code("(1).__class__.__mro__");
    assert!(r.is_err(), "__mro__ walk must raise");
}

/// `HOLDS` — Targets the function-object __globals__ escape hatch (different dunder than __mro__/__subclasses__) — historic route to __builtins__/os.
/// seam: monty function/method attribute resolution; [].__class__.__init__.__globals__ pivot to module globals
#[test]
fn init_globals_builtins_pivot_is_blocked() {
    let mut s = Session::new("sx-globals").unwrap();
    let r = s.run_code("[].__class__.__init__.__globals__");
    assert!(
        r.is_err(),
        "__globals__ on a method must raise, never yield module globals"
    );
}

/// `HOLDS` — Pokes the magic __builtins__ binding directly in the global namespace — distinct from attribute-chain walks. NameLookup resumes Undefined -> NameError.
/// seam: monty global namespace via session.rs NameLookup->Undefined arm; direct __builtins__ access
#[test]
fn builtins_dunder_poke_is_blocked() {
    let mut s = Session::new("sx-builtins").unwrap();
    let r = s.run_code("__builtins__['eval']");
    assert!(r.is_err(), "__builtins__ must be unreachable as a name");
}

/// `HOLDS` — globals() is a distinct namespace-leak builtin (separate from the __builtins__ binding).
/// seam: monty BuiltinsFunctions enum — Globals commented out in builtins/mod.rs
#[test]
fn globals_builtin_is_blocked() {
    let mut s = Session::new("sx-globals-fn").unwrap();
    let r = s.run_code("globals()");
    assert!(r.is_err(), "globals() builtin must be absent -> NameError");
}

/// `HOLDS` — Local-scope dict leak — different reflection primitive than globals()/__builtins__.
/// seam: monty BuiltinsFunctions enum — Locals commented out
#[test]
fn locals_builtin_is_blocked() {
    let mut s = Session::new("sx-locals").unwrap();
    let r = s.run_code("locals()");
    assert!(r.is_err(), "locals() must be absent");
}

/// `HOLDS` — compile() forges code objects bypassing source filtering — distinct dynamic-code vector from eval/exec (already in security_tests).
/// seam: monty BuiltinsFunctions — Compile commented out; code-object forging path
#[test]
fn compile_builtin_is_blocked() {
    let mut s = Session::new("sx-compile").unwrap();
    let r = s.run_code("compile('1+1','<s>','eval')");
    assert!(r.is_err(), "compile() must be absent");
}

/// `HOLDS` — breakpoint() routes through sys.breakpointhook/pdb and can import arbitrary modules — different escape than eval/compile.
/// seam: monty BuiltinsFunctions — Breakpoint commented out; PYTHONBREAKPOINT/pdb host pivot
#[test]
fn breakpoint_builtin_is_blocked() {
    let mut s = Session::new("sx-breakpoint").unwrap();
    let r = s.run_code("breakpoint()");
    assert!(r.is_err(), "breakpoint() must be absent");
}

/// `HOLDS` — help() spawns a pager and input() reads host stdin — homogeneous I/O-builtin family, each a distinct host-channel gadget; sub-family table permitted.
/// seam: monty BuiltinsFunctions — Help, Input commented out; pager + stdin host I/O
#[test]
fn help_and_input_builtins_are_blocked() {
    for (i, code) in ["help(str)", "input()"].iter().enumerate() {
        let mut s = Session::new(&format!("sx-io-{i}")).unwrap();
        assert!(
            s.run_code(code).is_err(),
            "interactive I/O builtins must be NameErrors: {code}"
        );
    }
}

/// `PROBE` — open() is the ONE builtin monty implements as a host OsCall; session.rs blindly resumes every OsCall with None. None-where-FileHandle-expected must surface as Err, never a host read.
/// seam: monty builtins/open.rs CallResult::OsCall(Open); session.rs OsCall arm resumes MontyObject::None
#[test]
fn open_oscall_resumes_none_no_host_read() {
    let dir = std::env::temp_dir().join("droplet-sx-open");
    std::fs::create_dir_all(&dir).unwrap();
    let secret = dir.join("s.txt");
    std::fs::write(&secret, "TOPSECRET").unwrap();
    let mut s = Session::new("sx-open").unwrap();
    let code = format!("open({:?}).read()", secret.to_str().unwrap());
    let r = s.run_code(&code);
    assert!(
        r.is_err(),
        "open() OsCall resumed with None so .read() must raise; got {r:?}"
    );
    assert!(
        !format!("{r:?}").contains("TOPSECRET"),
        "host file content must never appear"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `PROBE` — Distinct from the read angle: a filesystem-mutation invariant (no host file created), not just an Err.
/// seam: monty open(mode='w') -> OsCall(Open) truncate effect; session.rs OsCall arm never writes
#[test]
fn open_write_oscall_creates_no_host_file() {
    let dir = std::env::temp_dir().join("droplet-sx-openw");
    std::fs::create_dir_all(&dir).unwrap();
    let victim = dir.join("created.txt");
    let mut s = Session::new("sx-openw").unwrap();
    let code = format!(
        "f = open({:?}, 'w')\nf.write('x')",
        victim.to_str().unwrap()
    );
    let _ = s.run_code(&code);
    assert!(
        !victim.exists(),
        "open('w')/write must not create or truncate a host file via the OsCall path"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `PROBE` — A second, independent host-FS channel beyond builtin open(): pathlib read_text -> OsFunctionCall::ReadText. Different code path reaching the same OsCall arm.
/// seam: monty modules/pathlib.rs Path.read_text -> OsCall(ReadText); session.rs OsCall arm resumes None
#[test]
fn pathlib_read_text_returns_none_not_host_content() {
    let mut s = Session::new("sx-pathlib").unwrap();
    let r = s.run_code("import pathlib\npathlib.Path('/etc/passwd').read_text()");
    if let Ok(v) = r {
        assert_eq!(
            v,
            MontyObject::None,
            "read_text OsCall resumed with None must yield None, not host file content"
        )
    }
    assert_eq!(
        s.run_code("1+1").unwrap(),
        MontyObject::Int(2),
        "session survives the pathlib OsCall path"
    );
}

/// `PROBE` — `import os` succeeds and os.getenv is a real OsCall — distinct from security_tests' 'read env' row (os.environ.get). Pins that the None-resume severs the single-key env-exfil channel.
/// seam: monty modules/os.rs getenv -> OsCall(Getenv); session.rs OsCall arm resumes None
#[test]
fn os_getenv_oscall_does_not_leak_real_env() {
    unsafe {
        std::env::set_var("DROPLET_SX_SECRET", "LEAKME");
    }
    let mut s = Session::new("sx-getenv").unwrap();
    let out = s.run_code("import os\nos.getenv('DROPLET_SX_SECRET')");
    if let Ok(v) = out {
        assert!(
            !format!("{v:?}").contains("LEAKME"),
            "os.getenv must not return the real host env value"
        )
    }
    unsafe {
        std::env::remove_var("DROPLET_SX_SECRET");
    }
}

/// `PROBE` — os.environ is a property-backed OsCall distinct from getenv (whole-environment dump vs single key). Confirms the None-resume severs the bulk channel even after dict().
/// seam: monty os.environ property -> ZeroArgOsProperty::GetEnviron OsCall; session.rs resumes None
#[test]
fn os_environ_oscall_does_not_leak_full_env() {
    unsafe {
        std::env::set_var("DROPLET_SX_ENVIRON", "LEAKALL");
    }
    let mut s = Session::new("sx-environ").unwrap();
    let out = s.run_code("import os\ndict(os.environ)");
    unsafe {
        std::env::remove_var("DROPLET_SX_ENVIRON");
    }
    if let Ok(v) = out {
        assert!(
            !format!("{v:?}").contains("LEAKALL"),
            "os.environ must not materialize the host environment"
        )
    }
}

/// `PROBE` — Exercises the asyncio coroutine/future machinery and the ResolveFutures suspension boundary — a different arm than FunctionCall/OsCall. Contract: no-panic + REPL survival regardless of whether the coroutine body runs.
/// seam: monty modules/asyncio.rs gather + session.rs ResolveFutures arm (resumes pending futures with Return(None))
#[test]
fn asyncio_gather_through_resolvefutures_is_contained() {
    let dir = std::env::temp_dir().join("droplet-sx-async");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("sales.parquet");
    {
        let c = duckdb::Connection::open_in_memory().unwrap();
        c.execute_batch(&format!(
            "COPY (SELECT 'EU' r, 1.0::DOUBLE a) TO '{}' (FORMAT PARQUET)",
            p.to_str().unwrap()
        ))
        .unwrap();
    }
    let mut s = Session::new("sx-async").unwrap();
    let code = format!(
        "import asyncio\nasync def f():\n    return query({:?}, 'SELECT * FROM data')\nasyncio.gather(f())",
        p.to_str().unwrap()
    );
    let r = s.run_code(&code);
    assert!(r.is_ok() || r.is_err(), "must terminate without panic");
    assert_eq!(
        s.run_code("1+1").unwrap(),
        MontyObject::Int(2),
        "session must survive the async/ResolveFutures path"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `PROBE` — Materialization bomb via BigInt->decimal: str() forces the digits. Distinct from recursion and the raw string bomb. Contained by the CPython-style int-string-conversion cap, independent of NoLimitTracker.
/// seam: monty int->str digit-limit guard (4300, sys.int_info) reached via run_code; 2**10_000_000 bomb
#[test]
fn huge_int_str_materialization_is_contained() {
    let mut s = Session::new("sx-bigint").unwrap();
    let r = s.run_code("len(str(2 ** 10000000))");
    assert!(
        r.is_err(),
        "str() of an astronomically large int must hit the 4300-digit int-string-conversion limit (ValueError), not OOM/abort the host; got {r:?}"
    );
    assert_eq!(
        s.run_code("1+1").unwrap(),
        MontyObject::Int(2),
        "session survives the big-int attempt"
    );
}

/// `CANARY` — Memory-amplification gadget ('a'*1000 each replaced by 'b'*1M). Distinct from big-int (digit cap) and recursion (stack cap): attacks raw heap allocation.
/// seam: monty resource.rs check_replace_size/check_estimated_size -> LimitedTracker::check_large_result via run_code
/// FINDING (2026-06-25): original CANARY expected the ~1GB string to be materialized (NoLimitTracker = no cap), but the LimitedTracker
/// wired in Task 2 DOES cap this via max_memory. The gap originally noted here is now CLOSED. The test now pins the current behavior:
/// string-amplification bomb raises MemoryError (bounded). Original intent: assert!(matches!(r, Ok(MontyObject::Int(1_000_000_000)))).
#[test]
fn raw_string_replace_bomb_is_bounded_by_limited_tracker() {
    let mut s = Session::new("sx-bigstr").unwrap();
    let r = s.run_code("len(('a' * 1000).replace('a', 'b' * 1000000))");
    // FINDING: LimitedTracker (max_memory) catches the ~1GB string via check_replace_size/check_estimated_size.
    // This gap is CLOSED: the string-amplification bomb raises MemoryError, not returns Int(1_000_000_000).
    assert!(
        r.is_err(),
        "CANARY (gap closed): string-amplification bomb is bounded by LimitedTracker max_memory — raises MemoryError; got {r:?}"
    );
    assert!(
        matches!(r.as_ref().unwrap_err(), crate::DropletError::Monty(_)),
        "breach surfaces as Monty error path, got {r:?}"
    );
    assert_eq!(
        s.run_code("1+1").unwrap(),
        MontyObject::Int(2),
        "session survives"
    );
}

/// `CANARY` — Distinct allocation gadget from replace-amplification: a plain str*int with no replace machinery — the simplest direct heap-allocation bomb. Pins that even the most basic memory DoS is uncapped.
/// seam: monty str __mul__ allocation through NoLimitTracker::on_allocate (always Ok) via run_code
#[test]
fn python_string_repeat_is_currently_unbounded() {
    let mut s = Session::new("sx-strmul").unwrap();
    let r = s.run_code("len('x' * 50000000)");
    assert!(
        matches!(r, Ok(MontyObject::Int(50_000_000))),
        "CANARY: agent-level string repeat is unbounded under NoLimitTracker (50M-char string built). Flip to is_err() when a LimitedTracker is wired; got {r:?}"
    );
    assert_eq!(
        s.run_code("1+1").unwrap(),
        MontyObject::Int(2),
        "session survives"
    );
}

/// `PROBE` — Reference-cycle / GC-root gadget: builds a->b->a across the run boundary. Distinct from UAF and allocation — targets cycle collection / root-set traversal.
/// seam: monty heap/GC root-set + cycle collector via run_code; self-referential list cycle
#[test]
fn reference_cycle_does_not_leak_or_crash() {
    let mut s = Session::new("sx-cycle").unwrap();
    let code = "a = []\nb = [a]\na.append(b)\nlen(a)";
    let r = s.run_code(code);
    assert!(
        matches!(r, Ok(MontyObject::Int(1))),
        "a reference cycle must be built and measured without panic; got {r:?}"
    );
    assert_eq!(
        s.run_code("1+1").unwrap(),
        MontyObject::Int(2),
        "session survives a cyclic structure"
    );
}

/// `PROBE` — Different consequence of cycles than construction: forces the formatter to traverse the cycle. repr_sequence_fmt has an incr_recursion_depth guard returning '...'.
/// seam: monty list.rs repr_sequence_fmt incr_recursion_depth cycle guard when repr() walks a cyclic list
#[test]
fn repr_of_self_referential_list_is_contained() {
    let mut s = Session::new("sx-cycle-repr").unwrap();
    let code = "a = []\na.append(a)\nrepr(a)";
    let r = s.run_code(code);
    assert!(
        matches!(r, Ok(MontyObject::String(ref v)) if v.contains("...")),
        "repr of a self-referential list must use the cycle guard ('...'), not infinite-recurse the host; got {r:?}"
    );
    assert_eq!(
        s.run_code("1+1").unwrap(),
        MontyObject::Int(2),
        "session survives"
    );
}

/// `HOLDS` — Type-confusion gadget: a mutating list method on an immutable bytes object probes whether per-type method tables are disjoint (bytes-as-list confusion would be memory-unsafe).
/// seam: monty types/bytes.rs attribute_error path; calling a list method on a bytes object
#[test]
fn bytes_type_confusion_method_call_is_blocked() {
    let mut s = Session::new("sx-typeconf").unwrap();
    let r = s.run_code("b'abc'.append(1)");
    assert!(
        r.is_err(),
        "a bytes object must reject a list method (no type confusion); AttributeError expected"
    );
}

/// `HOLDS` — setattr IS available (unlike eval/exec) — probes monkey-patching a builtin type to inject a gadget. Distinct from getattr-dynamic and from read-only dunder walks.
/// seam: monty builtins/setattr.rs against an immutable builtin type; __dict__-less mutation probe
#[test]
fn setattr_on_int_cannot_inject_attribute() {
    let mut s = Session::new("sx-setattr").unwrap();
    let r = s.run_code("setattr((1).__class__, 'x', 5)");
    assert!(
        r.is_err(),
        "setattr on a builtin type/instance must raise (no __dict__), preventing attribute injection / type patching"
    );
}

/// `HOLDS` — getattr() with a runtime-built name is the standard way to bypass static string scanners — distinct from the literal ().__class__ chain. Confirms enforcement at the attribute layer, not source filtering.
/// seam: monty builtins/getattr.rs delegating to the same attribute resolution; runtime-built dunder name
#[test]
fn getattr_dynamic_dunder_bypass_is_blocked() {
    let mut s = Session::new("sx-getattr-dyn").unwrap();
    let r = s.run_code("getattr((), '__cl' + 'ass__')");
    assert!(
        r.is_err(),
        "dynamic getattr must not resolve a dunder that direct attribute access also lacks"
    );
}

/// `HOLDS` — Format-string attribute access reaches object internals through the formatter rather than direct attribute syntax — a separate eval path that must also enforce the missing-dunder rule.
/// seam: monty str.format field attribute-access path; the {0.__class__} formatter leak
#[test]
fn format_spec_class_leak_is_blocked() {
    let mut s = Session::new("sx-format").unwrap();
    let r = s.run_code("'{0.__class__}'.format(())");
    assert!(
        r.is_err(),
        "format() field attribute access to __class__ must raise, not leak the type object"
    );
}

/// `PROBE` — Probes the hashing seam: a confused hash path treating a list as hashable would enable re-entrant heap access during rehash. Different seam than sort/cycle.
/// seam: monty builtins/hash.rs + set insertion via run_code; unhashable-mutable probe
#[test]
fn set_add_unhashable_list_is_contained() {
    let mut s = Session::new("sx-set-hash").unwrap();
    let r = s.run_code("s = set()\na = []\ntry:\n    s.add(a)\n    out='added'\nexcept Exception:\n    out='raised'\nout");
    assert!(
        matches!(r, Ok(MontyObject::String(ref v)) if v=="raised"),
        "adding an unhashable list to a set must raise TypeError, contained; got {r:?}"
    );
    assert_eq!(
        s.run_code("1+1").unwrap(),
        MontyObject::Int(2),
        "session survives"
    );
}

/// `HOLDS` — Exception-state-corruption angle: a raised exception escaping run_code must leave the REPL+namespace clean, so an attacker can't wedge the session into a half-dead exploitable state. Distinct from tool-Err which consumes the REPL.
/// seam: monty Exception handling + session.rs settle() REPL restoration after a raised exception
#[test]
fn unhandled_exception_does_not_poison_namespace() {
    let mut s = Session::new("sx-exc-resurrect").unwrap();
    let _ = s.run_code("raise ValueError('x')");
    assert_eq!(
        s.run_code("saved = 7\nsaved").unwrap(),
        MontyObject::Int(7),
        "REPL usable after an uncaught raise"
    );
    assert_eq!(
        s.run_code("saved").unwrap(),
        MontyObject::Int(7),
        "namespace persists across a prior raise (settle restored the REPL)"
    );
}
