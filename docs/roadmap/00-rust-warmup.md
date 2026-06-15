# 00 — Rust Warm-Up (before M0)

**Milestone goal:** learn *just enough* Rust to start building Droplet v1 — not all of Rust, only the
handful of concepts the build leans on hardest. You'll do tiny exercises in the throwaway scratch crate
you already created (`droplet-warmup`). Two ideas get extra depth because v1 rests on them: **traits +
trait objects** (Droplet has a `Source` connector trait plus *three* store traits, and each has several
backends) and **async/await + Tokio** (the connector IO, S3, DynamoDB, Redis, and SurrealDB clients are
all async; DuckDB is sync and must be fenced off with `spawn_blocking`).

**Done when:** you can write a small Rust program that uses structs + `impl`, `enum` + `match`,
`Option`/`Result` with `?`, a `HashMap` registry, **defines a trait, implements it for two types, and
stores them as `Box<dyn Trait>`**, **runs an `async fn` under `#[tokio::main]` with `.await` and
`spawn_blocking`**, shares one value across tasks with `Arc`, and hashes some bytes into a
content-addressed key — i.e. you can tick every box in the "Ready for M0" checklist at the bottom.

**Prerequisite:** none — this is the first file. Do the **Recommended setup** in
[`README.md`](./README.md) first (it installs `rustup` and the rust-analyzer editor extension). Next
file: [`M0-skeleton.md`](./M0-skeleton.md). The spec is at [`PRODUCT.md`](../../PRODUCT.md) (repo root,
**not** `docs/PRODUCT.md`).

**Estimate:** ~10 chunks (a sitting each). You already program in Python, so the *new* parts are
ownership/borrowing, the trait/`dyn` machinery behind the `Source` connector + the store seams, and the
async/Tokio model — everything else maps onto things you already know.

---

## How to read this file (read first, 2 min)

- Each `- [ ]` is a tiny task (~10–30 min, one idea). Do them in order; tick a box only when its **✅** is true.
- **🆕 Concept** = a one-time plain-English explanation of a new idea, with a Rust Book chapter *name* to read.
- **✅ Done when** = how you know the task worked (a command's output or a passing test).
- **⚠️ Invariant** = a Droplet rule (one of the **10** in [`PRODUCT.md`](../../PRODUCT.md), §15 — the
  same 10 "Golden rules" listed in [`README.md`](./README.md)) the exercise quietly prepares you for,
  quoted in plain words with its number.
- **🔗 Maps to** = the real Droplet v1 concept this exercise unlocks — so you know *why* you're learning it.
- **verify:** = a fact the research couldn't fully pin on the locked version; check the crate source/docs before relying on it.
- The Rust Book is free (`rustup doc --book`, or search "The Rust Programming Language book"). Chapter
  *names* below match that book; chapter *numbers* drift between editions, so trust the **name**.

> You don't need to memorise anything. The goal is to *recognise* these shapes when you meet them in
> M0/M1. Come back here whenever a later step says "🆕" for something you skipped.

> This is the **scratch crate** `droplet-warmup`, not part of Droplet proper. It's a workspace member
> *for now* so `cargo` commands work; you'll drop it from the workspace `members` once you finish this
> file (M0 notes exactly where). Nothing you write here ships.

---

### Chunk 1 — Confirm the toolchain and reset the scratch crate

> You already installed `rustup` + rust-analyzer (README "Recommended setup") and ran `cargo new
> droplet-warmup`. This chunk confirms the tools, adds the two components Droplet pins, and clears the
> leftover warm-up code so you start from a clean `main`.

- [ ] Confirm the compiler and build tool: run `rustc --version` and `cargo --version`.
  - 🆕 Concept: `rustup` manages compiler versions (like `pyenv` manages Pythons); `cargo` is Rust's
    build tool + package manager (like `pip` + `venv` + `make` in one). (Rust Book: Getting Started)
  - ✅ Done when: both print a version. Edition 2024 needs Rust **≥ 1.85.0**, and later milestones push
    the floor to **≥ 1.89** (maturin in M0), so any recent stable **≥ 1.89** is fine. (M0 adds a
    `rust-toolchain.toml` that pins one exact version for everyone.)
- [ ] Add the linter: run `rustup component add clippy`.
  - 🆕 Concept: `clippy` is a Rust-aware linter that catches common mistakes (like `ruff`/`flake8` for
    Python). (Rust Book: Appendix D — Useful Development Tools)
  - ✅ Done when: `cargo clippy --version` prints a version.
- [ ] Add the formatter: run `rustup component add rustfmt`.
  - 🆕 Concept: `rustfmt` auto-formats to one canonical style (like `black` for Python).
  - ✅ Done when: `cargo fmt --version` prints a version.
  - 🔗 Maps to: Droplet pins these exact components in `rust-toolchain.toml`
    (`components = ["rustfmt", "clippy"]`).
- [ ] Open `droplet-warmup/src/main.rs` and confirm it's a `--bin` crate (it has a `fn main`).
  - 🆕 Concept: a `--bin` crate is an *executable* (`src/main.rs`, has `fn main`); a `--lib` crate is a
    library (`src/lib.rs`, **no** `fn main`). Droplet's `droplet-core` is a `--lib`; `xtask` is a
    `--bin`. (Rust Book: Hello, Cargo!)
  - ✅ Done when: you can point to the `fn main` and say which kind of crate this is.
- [ ] Reset the scratch `main` so it's clean: replace the body of `fn main` with `println!("warmup");`
  and delete any leftover `double` code from earlier tinkering (the crate currently has a `double`
  function — you'll re-add a `double` in Chunk 2 with the *right* type, so clearing it now keeps `main`
  from getting cluttered).
  - ✅ Done when: `cargo run` inside `droplet-warmup/` prints `warmup` with no warnings.
- [ ] Confirm rust-analyzer is alive: hover a `let` binding and look for the inline type hint.
  - 🆕 Concept: rust-analyzer is the language server — it shows each variable's *inferred type* inline,
    the fastest way to learn the type system. (Rust Book: Appendix D)
  - ✅ Done when: you see a grey type hint (e.g. `: &str`) next to a binding, or on hover.
- [ ] Run the three commands you'll use constantly: `cargo build`, `cargo run`, `cargo test`.
  - ✅ Done when: all three succeed. With no tests yet, `cargo test` reports `0 passed; 0 failed`.

---

### Chunk 2 — Variables, mutability, and your first test

- [ ] In `main`, bind `let x = 5;`, then try `x = 6;`. Watch it fail to compile.
  - 🆕 Concept: bindings are **immutable by default**. You must write `let mut x = 5;` to reassign.
    This default catches a whole class of bugs. (Rust Book: Variables and Mutability)
  - ✅ Done when: the immutable version errors with "cannot assign twice to immutable variable", and
    adding `mut` makes it compile.
- [ ] Notice types are usually inferred (`let x = 5;` is `i32`) but you can annotate them.
  - 🆕 Concept: a *type annotation* like `let x: i32 = 5;` spells out the type when inference can't, or
    when you want to be explicit. (Rust Book: Data Types)
  - ✅ Done when: hovering `x` shows `i32`, and adding `: i32` changes nothing.
- [ ] Try an unsigned 64-bit integer: `let h: u64 = 42;`.
  - 🆕 Concept: `u64` is an unsigned (non-negative) 64-bit integer. (Rust Book: Data Types)
  - 🔗 Maps to: **every engine object the sandbox sees is referenced by a `u64` handle** — DuckDB
    connections, capped result sets, materialized artifacts all live host-side and the sandbox only
    holds the `u64`. This is *the* Droplet primitive, so you'll type `u64` a lot.
  - ⚠️ Invariant (#6): boundary discipline — engine objects live behind a handle registry; the sandbox
    sees only opaque handles, never the engine object.
- [ ] Add a tiny pure function above `main`: `fn double(n: u64) -> u64 { n * 2 }`, and call it.
  - 🆕 Concept: a function signature names each parameter's type and the return type after `->`.
    (Rust Book: Functions)
  - ✅ Done when: `cargo build` is green and `main` calls `double` (so it isn't flagged unused).
- [ ] Write your first test in a test module:
    ```rust
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn doubles() {
            assert_eq!(double(21), 42);
        }
    }
    ```
  - 🆕 Concept: `#[test]` marks a test; `assert_eq!(a, b)` fails it if `a != b`; `#[cfg(test)]` means
    "only compile this when testing". (Rust Book: How to Write Tests)
  - ✅ Done when: `cargo test` shows `test tests::doubles ... ok`.
- [ ] **Test-first habit:** change the assertion to `assert_eq!(double(21), 99)`, run `cargo test`,
  watch it FAIL, then change it back to `42`. Get used to the red → green loop.
  - ✅ Done when: you've seen one failing run and one passing run.

---

### Chunk 3 — Ownership, borrowing & references (the big one)

> This is the concept Python never taught you, and the one thing that makes Rust feel alien for a day
> or two. Spend real time here — every later milestone depends on it. Don't rush.

- [ ] Reproduce a **move**: `let s1 = String::from("hi");`, then `let s2 = s1;`, then try to use `s1`
  again (e.g. `println!("{s1}")`). Watch it fail.
  - 🆕 Concept: every value has exactly ONE owner. Assigning a heap value (like `String`) *moves*
    ownership; the old name dies. This is how Rust frees memory with no garbage collector and no
    use-after-free bugs. (Rust Book: Understanding Ownership)
  - ✅ Done when: you see "borrow of moved value: `s1`" and you understand *why* `s1` is dead.
- [ ] Fix it with a **clone**: change `let s2 = s1;` to `let s2 = s1.clone();` and use both.
  - 🆕 Concept: `.clone()` duplicates the underlying data so you get a second independent owner —
    sometimes cheap, sometimes expensive. (Rust Book: Understanding Ownership)
  - 🔗 Maps to: in Droplet you'll `.clone()` cheap handles (a `u64`, an `Arc<dyn ArtifactStore>`)
    freely, but you'll *avoid* cloning big things — capped result rows stay capped precisely so a clone
    is small.
  - ✅ Done when: both `s1` and `s2` print, each owning its own copy.
- [ ] Now fix it with a **reference** instead: `let s2 = &s1;` and use both `s1` and `*s2`.
  - 🆕 Concept: `&s1` *borrows* the value without taking ownership, so `s1` still owns the data and
    stays usable. Reach for `&` first; reach for `.clone()` only when you genuinely need a second owner.
    (Rust Book: References and Borrowing)
  - ✅ Done when: both `s1` and the borrow work, and you can say in one sentence how this differs from
    `.clone()`.
- [ ] Write a borrowing function: `fn len_of(s: &String) -> usize { s.len() }`, call it as
  `len_of(&s1)`, and confirm `s1` is still usable afterward.
  - 🆕 Concept: a function taking `&T` *borrows* (caller keeps ownership); one taking `T` *consumes* it
    (caller loses it). This `&`-vs-no-`&` distinction in signatures is the heart of the language.
    (Rust Book: References and Borrowing)
  - ✅ Done when: `len_of(&s1)` returns `2` and `println!("{s1}")` after it still compiles.
- [ ] Mutate through a reference: `fn push_bang(s: &mut String) { s.push('!'); }`, called as
  `push_bang(&mut s1)` (so `s1` must be `let mut s1`).
  - 🆕 Concept: `&mut T` is a *mutable* (exclusive) borrow — Rust allows many `&T` readers OR exactly
    one `&mut T` writer at a time, never both. This rule prevents data races *at compile time*. (Rust
    Book: References and Borrowing)
  - 🔗 Maps to: a store impl mutating its own state inside a method takes `&mut self`; later you'll see
    why sharing one store across tasks (Chunk 9) needs `Arc`/`Mutex` instead of `&mut`.
  - ✅ Done when: after the call, `s1` is `"hi!"`.
- [ ] Optional but recommended: read about slices and try `let first = &s1[0..1];`.
  - 🆕 Concept: a slice (`&str` / `&[T]`) borrows a *part* of a collection without copying it. (Rust
    Book: The Slice Type)
  - 🔗 Maps to: you'll hash bytes as a `&[u8]` slice in Chunk 10 — `blake3::hash` takes exactly that.
  - ✅ Done when: `first` is `"h"`.

---

### Chunk 4 — Structs, enums, and pattern matching

- [ ] Define a struct with one field and an `impl` block:
    ```rust
    struct Session { step: u64 }
    impl Session {
        fn new() -> Self { Session { step: 0 } }
        fn advance(&mut self, by: u64) -> u64 { self.step += by; self.step }
    }
    ```
  - 🆕 Concept: a `struct` groups named fields (like a Python `@dataclass`). An `impl` block holds its
    methods; `&self` borrows the instance to read it, `&mut self` borrows it to *mutate* it, and `Self`
    is the type itself. (Rust Book: Defining and Instantiating Structs; Method Syntax)
  - 🔗 Maps to: this rehearses Droplet's `Session` — one durable, ephemeral analysis context per run
    that owns an ephemeral DuckDB, a read-only Surreal handle, its manifest, and its snapshot lifecycle.
  - ✅ Done when: `cargo build` is green with `Session` defined.
- [ ] Write a test that creates a `Session::new()`, calls `advance(1)` twice, and asserts `2`.
  - ⚠️ Invariant (#6): boundary discipline — keep values that cross the host↔sandbox line *small*;
    `advance` returns one `u64`, not a blob. Heavy data stays in the engines.
  - ⚠️ Invariant (#3): analyze is local & ephemeral — one run = one `Session`. This tiny struct stands
    in for that one-per-run context (the real one owns an ephemeral *local* DuckDB + a working dir wiped
    on close).
  - ✅ Done when: `cargo test` is green and you can explain that `&mut self` is what let `advance`
    change the field.
- [ ] Define an enum whose variants carry data:
    ```rust
    enum FreshnessPolicy { Versioned, Ttl(u64), Passthrough }
    ```
  - 🆕 Concept: an `enum` is a type that is *exactly one of* several variants, each able to carry its
    own data. This is how Rust models "one of these" safely. (Rust Book: Defining an Enum)
  - 🔗 Maps to: this is literally Droplet's cache freshness policy (PRODUCT.md §13): `Versioned`
    (default), `Ttl(duration)`, `Passthrough` (never cache).
  - ✅ Done when: `cargo build` is green with `FreshnessPolicy` defined.
- [ ] Write a function that `match`es on the enum:
    ```rust
    fn caches(p: &FreshnessPolicy) -> bool {
        match p {
            FreshnessPolicy::Versioned => true,
            FreshnessPolicy::Ttl(_secs) => true,
            FreshnessPolicy::Passthrough => false,
        }
    }
    ```
  - 🆕 Concept: `match` compares a value against patterns and runs the first arm that fits; it *forces*
    you to handle every variant (the compiler errors if you miss one). (Rust Book: The `match` Control
    Flow Construct)
  - 🔗 Maps to: Monty's `ReplProgress` (M3) is exactly this shape — `Complete { value, .. }` vs
    `FunctionCall(call)` vs `OsCall` / `NameLookup` / `ResolveFutures`. The Monty driver loop is a
    `match` over those variants, so this exercise *is* the warm-up for it.
  - verify: the exact `ReplProgress` variant names (`Complete` / `FunctionCall` / `OsCall` /
    `NameLookup` / `ResolveFutures`) are from Monty `v0.0.18` and may have changed — read
    `crates/monty/src/` for the current enum before relying on them in M3.
  - ✅ Done when: a quick `#[test]` confirms `caches(&FreshnessPolicy::Passthrough) == false`.
- [ ] Add a new variant `FreshnessPolicy::Manual` but DON'T update `caches`. Watch the compiler force
  you to handle it, then add the missing arm.
  - 🆕 Concept: compiler-enforced completeness is called *exhaustiveness* — `match` can't silently
    forget a case. (Rust Book: The `match` Control Flow Construct)
  - ✅ Done when: you see "non-exhaustive patterns", then a green build after adding the arm. (This is
    exactly why the Monty driver loop in M3 can't silently forget a `ReplProgress` case.)

---

### Chunk 5 — Option and the `?` operator

- [ ] Use `Option<T>` for "maybe a value": `fn first_char(s: &str) -> Option<char> { s.chars().next() }`
  and `match` on the result.
  - 🆕 Concept: `Option<T>` is either `Some(T)` or `None` — Rust has no `null`, so "might be missing"
    lives in the type and the compiler makes you handle the `None` case. (Rust Book: The `Option` Enum
    and Its Advantages Over Null Values)
  - 🔗 Maps to: a cache-index lookup (`cache_key → artifact_key`) returns `Option<String>` — `None` is
    a cache miss. You'll see this exact shape in the content-addressed cache (M5) and the cache index in
    the coordination store (M7).
  - ✅ Done when: `first_char("hi")` matches `Some('h')` and `first_char("")` matches `None`.
- [ ] Use `?` on an `Option`:
  `fn first_upper(s: &str) -> Option<char> { let c = s.chars().next()?; Some(c.to_ascii_uppercase()) }`.
  - 🆕 Concept: `?` means "if this is `None`, return `None` from the whole function now; otherwise
    unwrap the inner value and keep going". It removes mountains of `match` boilerplate. (Rust Book:
    Recoverable Errors with Result)
  - ✅ Done when: `first_upper("hi")` is `Some('H')` and `first_upper("")` is `None`.

---

### Chunk 6 — Result, `?`, and a first taste of `thiserror`

- [ ] Use `Result<T, E>` for "success or error": a function returning `Result<u64, String>` that returns
  `Err("nope".to_string())` on bad input and `Ok(n)` otherwise, then `match` on it.
  - 🆕 Concept: `Result<T, E>` is `Ok(T)` or `Err(E)` — recoverable errors live in the type, so the
    caller must deal with them. (Rust Book: Recoverable Errors with Result)
  - ✅ Done when: both the `Ok` and `Err` paths are reachable from a `#[test]`.
- [ ] See the difference between a recoverable error and a **panic**: call `.unwrap()` on an `Err` once
  and watch it crash.
  - 🆕 Concept: a *panic* (`.unwrap()` / `panic!`) aborts like an unhandled exception. Use `Result` for
    expected failures; reserve `.unwrap()` for tests and truly-impossible cases. (Rust Book:
    Unrecoverable Errors with `panic!`)
  - ✅ Done when: you've seen the `thread 'main' panicked` message, then removed the `.unwrap()`.
- [ ] Chain `?` on `Result`: two functions returning `Result<_, String>` where one calls the other with
  `?`, so the error bubbles up automatically.
  - 🆕 Concept: `?` on a `Result` returns early on the first `Err` and unwraps `Ok` otherwise — the
    same shortcut you saw for `Option`, now for errors. (Rust Book: Recoverable Errors with Result)
  - ✅ Done when: feeding bad input to the inner function makes the outer one return that same `Err`
    without you writing a `match`.
- [ ] Add the `thiserror` crate to the scratch crate: run `cargo add thiserror` inside
  `droplet-warmup/`.
  - 🆕 Concept: `cargo add` writes a dependency into this crate's `Cargo.toml` (the warm-up crate has an
    empty `[dependencies]` table, so this is your first dep). Pin the **2.x** major (so `Cargo.toml` shows
    `thiserror = "2"`). (Lots of older snippets show `1.x` — the previous major; don't follow them.) (Rust
    Book: Hello, Cargo!)
  - verify: confirm the current 2.x patch of `thiserror` on crates.io before pinning — only the **2.x**
    major is approved here, not any specific patch.
  - ✅ Done when: `Cargo.toml` shows a `thiserror = "2..."` entry and `cargo build` downloads it cleanly.
- [ ] Define a real error enum with `thiserror`:
    ```rust
    #[derive(thiserror::Error, Debug)]
    enum WarmupError {
        #[error("no such handle: {0}")]
        BadHandle(u64),
        #[error("io error")]
        Io(#[from] std::io::Error),
    }
    ```
  - 🆕 Concept: `thiserror` *derives* the boilerplate for a custom error type — `#[error("...")]` writes
    the human-readable message, and `#[from]` auto-generates a `From` conversion. (`thiserror` is a
    crate, not part of the Book — read its crates.io README; the Book's Error Handling chapter covers
    the `Result`/`?` background.)
  - 🔗 Maps to: this is exactly `DropletError`'s shape. M0 builds the real one and folds every engine
    error (DuckDB, SurrealDB, Monty, S3/DynamoDB/Redis, IO) into it; you're rehearsing it.
  - ✅ Done when: `cargo build` is green with `WarmupError` defined.
- [ ] Prove `#[from]` lets `?` convert errors for free:
  `fn open_it(p: &str) -> Result<(), WarmupError> { std::fs::File::open(p)?; Ok(()) }`, called with a
  path that doesn't exist.
  - 🆕 Concept: because of `#[from] std::io::Error`, the `?` on `File::open` turns the `io::Error` into
    a `WarmupError::Io` automatically — no manual conversion. (Rust Book: Recoverable Errors with Result)
  - ⚠️ Invariant (#10): **one error type at the boundary** — every engine error folds into one
    `DropletError` via `#[from]`. Use `thiserror` in libraries (`droplet-core`), `anyhow` only at binary
    edges (`xtask`/CLI).
  - ✅ Done when: `open_it("nope.txt")` returns `Err(WarmupError::Io(_))` without you writing any
    conversion code.

> ℹ️ **Version pins to remember for later (don't act now, just know):** Droplet pins crate versions and
> commits the lockfile. `thiserror` (the `2.x` major) is the library error crate; `anyhow` (the `1.x`
> major) is its binary-side counterpart (used in `xtask`/CLI, *never* in `droplet-core` — invariant #10).
> *verify: confirm the current 2.x patch of `thiserror` / 1.x patch of `anyhow` on crates.io before
> pinning — only the 2.x (thiserror) and 1.x (anyhow) majors are approved.* For the snapshot manifest
> (M8) the compact serializer is **`postcard`** (the
> maintained, Monty-consistent choice — Monty itself uses `postcard 1.1`, and `bincode` is now
> officially **unmaintained**, so don't pick it for new code). The content-addressing hash is
> **`blake3`** (Chunk 10). You don't need any of these now.

---

### Chunk 7 — Vec, HashMap, and the handle registry

- [ ] Use a `Vec<T>`: build `let mut v: Vec<u64> = Vec::new();`, push `1` and `2`, iterate with
  `for x in &v { ... }`.
  - 🆕 Concept: `Vec<T>` is Rust's growable array (like a Python `list`). Iterating with `&v` *borrows*,
    so the vec stays usable after the loop. (Rust Book: Storing Lists of Values with Vectors)
  - 🔗 Maps to: a capped query result comes back as a `Vec<RecordBatch>` (Arrow); DuckDB's
    `query_arrow` collects into exactly that. Capping it small with SQL `LIMIT` is the boundary
    discipline that keeps snapshots small.
  - ✅ Done when: the loop prints `1` then `2`, and `v` is still usable afterward.
- [ ] Use a `HashMap<K, V>`: add `use std::collections::HashMap;`, build a `u64 → String` map, insert a
  couple of entries, look one up with `.get(&key)`.
  - 🆕 Concept: `HashMap<K, V>` is a key→value dictionary (like a Python `dict`). `.get` returns an
    `Option<&V>` because the key might be missing. (Rust Book: Storing Keys with Associated Values in
    Hash Maps)
  - 🔗 Maps to: **the handle registry is a `HashMap<u64, EngineObject>`** — the structure that keeps
    engine objects host-side while the sandbox holds only the `u64` key.
  - ⚠️ Invariant (#6): engine objects live host-side behind the registry; the sandbox only ever gets a
    `u64` handle back, never the object behind it.
  - ✅ Done when: `.get(&existing)` is `Some(&value)` and `.get(&missing)` is `None`.
- [ ] Build a tiny registry: a struct holding a `HashMap<u64, String>` plus a `next: u64` counter, with
  one method `insert(&mut self, val: String) -> u64` (stores the value, returns its new id).
  - 🆕 Concept: a `HashMap` + a counter is the idiom for "hand out incrementing ids" — each `insert`
    bumps `next` and uses the old value as the key. (Rust Book: Storing Keys with Associated Values in
    Hash Maps)
  - ⚠️ Invariant (#7): distributed by default — state lives in the shared plane and is reconstructable;
    a plain incrementing `u64` keyed in a map is the simplest such handle (M8 rebuilds engine state from
    the manifest, never from serialized engine heaps).
  - ✅ Done when: `cargo build` is green with `insert` defined.
- [ ] Add `get(&self, id: u64) -> Option<&String>`, then a test that inserts two values, gets distinct
  ids, and reads both strings back.
  - ✅ Done when: the test inserts, sees two different ids, and reads the right strings. You just built a
    miniature handle registry.

---

### Chunk 8 — Traits, generics, and trait objects (the `Source` connector + store seams)

> Droplet v1 leans on this shape in two places. First, the **`Source` connector trait** (M0): every
> engine — Athena, Snowflake, BigQuery, Iceberg, plain S3 — is reached through one `Source` impl that
> turns a scoped load into local Parquet, and M0 ships a **trivial local-Parquet dev connector** behind
> it so the rest of the build runs with no cloud. Second, the **three store traits** —
> `ArtifactStore` (M5), `CoordinationStore` (M7), `SnapshotStore` (M8) — each with *several* backends
> (S3 + local-for-dev; Redis + DynamoDB + in-memory-for-dev). In every case the code that uses the seam
> doesn't care which backend it got, so the backend is chosen at runtime and held behind a trait object.
> This chunk builds exactly that shape in miniature. *(These traits are built across M0 and M5–M8, not
> all at once in M0 — here you're just rehearsing the pattern they all share.)*

- [ ] Define a trait and implement it for two types:
    ```rust
    trait Store {
        fn put(&mut self, key: String, bytes: Vec<u8>);
        fn get(&self, key: &str) -> Option<Vec<u8>>;
    }
    ```
  - 🆕 Concept: a `trait` is a shared interface (like a Python `Protocol`/ABC). Any type can `impl` it
    to promise it has that behaviour. (Rust Book: Traits: Defining Shared Behavior)
  - ✅ Done when: `cargo build` is green with the trait defined (no impls yet — expect "trait is never
    used" until the next step).
- [ ] Implement it for an **in-memory** backend (a struct wrapping a `HashMap<String, Vec<u8>>`):
    ```rust
    struct MemStore { map: std::collections::HashMap<String, Vec<u8>> }
    impl Store for MemStore {
        fn put(&mut self, key: String, bytes: Vec<u8>) { self.map.insert(key, bytes); }
        fn get(&self, key: &str) -> Option<Vec<u8>> { self.map.get(key).cloned() }
    }
    ```
  - 🔗 Maps to: this is the dev/in-memory `ArtifactStore` you build in M5 — the simplest backend behind
    the trait, used so tests run without S3.
  - ✅ Done when: a `#[test]` does `put("k", vec![1,2])` then `get("k") == Some(vec![1,2])`.
- [ ] Implement the **same trait** for a second backend that logs every write (a struct wrapping a
  `MemStore` plus a `Vec<String>` of "audit" lines). Prove both satisfy `Store`.
  - 🆕 Concept: one trait, many implementations — that's how Droplet swaps a backend (in-memory vs S3)
    without changing the calling code. (Rust Book: Traits: Defining Shared Behavior)
  - 🔗 Maps to: the *same* `ArtifactStore` trait will have an S3/MinIO impl in M5 and an in-memory impl
    for dev — different code, identical interface. (The `Source` connector trait in M0 is the very same
    pattern: a local-Parquet dev impl first, real engine impls like Athena later in M6.)
  - ✅ Done when: a test exercises both impls through the same `put`/`get` calls.
- [ ] Take a `Store` **by generic** (static dispatch): write
  `fn round_trip<S: Store>(s: &mut S, k: &str)` that puts then gets a value.
  - 🆕 Concept: `<S: Store>` is a *generic* bound — the function works for *any* type that implements
    `Store`, and the compiler stamps out a specialised copy per type (monomorphization, fast, no runtime
    cost). (Rust Book: Generic Types, Traits, and Lifetimes)
  - ✅ Done when: `round_trip` compiles and runs against *both* your backends.
- [ ] Now hold mixed backends behind a **trait object**:
    ```rust
    let stores: Vec<Box<dyn Store>> = vec![
        Box::new(MemStore { map: Default::default() }),
        // Box::new(your_logging_store),
    ];
    ```
  - 🆕 Concept: `Box<dyn Store>` is a *trait object* — a heap pointer to "some type that implements
    `Store`, decided at runtime". Unlike the generic above, the concrete type is *erased*, so one `Vec`
    can hold different backends. `Box<T>` on its own just means "owned heap allocation". (Rust Book:
    Using Trait Objects That Allow for Values of Different Types; Box<T> to Point to Data on the Heap)
  - 🔗 Maps to: a `Session`/`Catalog` holds each store as a trait object (e.g. `Box<dyn ArtifactStore>`
    or `Arc<dyn CoordinationStore>`) chosen from config at startup — that's how the *same* core code
    runs against in-memory stores in tests and S3/Redis/DynamoDB in production.
  - ✅ Done when: you loop over `stores`, calling `put`/`get` on each through the `dyn Store` interface,
    and it compiles and runs.
  - verify: when these traits go **async** (Chunk 9, and for real starting with the `Source` connector
    in M0/M2 and the store traits in M5–M8), native `async fn` in traits is stable but **not**
    dyn-compatible, so a `Box<dyn ArtifactStore>` with an `async fn` method won't compile on its own.
    Droplet's plan is to annotate each such trait with `#[async_trait]` from the `async-trait` crate
    (digest pins `0.1.89`) so `Box<dyn ArtifactStore>` keeps working. Confirm the dyn-vs-async rule
    against the pinned `async-trait` docs before you wire the real `Source` trait in M0.
- [ ] Modules & visibility: move the registry/store types into `mod stores { ... }` and notice `main`
  can no longer reach them; then fix it by marking the types and methods `pub`.
  - 🆕 Concept: `mod` makes a namespace; items are *private by default*. `pub` exposes an item across
    the module boundary — this is how a crate chooses its public API and hides the rest. (Rust Book:
    Defining Modules to Control Scope and Privacy; Paths for Referring to an Item in the Module Tree)
  - 🔗 Maps to: `droplet-core` keeps registry internals private and exposes only the `Source`/store
    traits + the flat tool surface as `pub`.
  - ℹ️ Practical note (not a numbered invariant, but it shapes the design — see the README "Practical
    note" under the Golden rules): the model-facing tool surface is **flat typed functions** (Monty is a
    *subset* of Python — no classes / module namespacing) — so `droplet-core`'s public API stays a flat
    set of `pub` functions, not nested modules the sandbox would have to navigate.
  - ✅ Done when: the build fails on a privacy error, then goes green once you add `pub`.

---

### Chunk 9 — Async/await, Tokio, `spawn_blocking`, and `Arc`

> This is the second pillar of v1. Droplet's connector IO, S3, DynamoDB, Redis, and SurrealDB clients
> are **all async** (`.await`), while DuckDB (the *local* analyze engine) is **synchronous and
> blocking**. You'll see the whole shape here: run async code under Tokio, fence the blocking
> DuckDB-style call off with `spawn_blocking`, and share one store across tasks with `Arc`.

- [ ] Read these five facts (no code yet), then do the exercises below:
  1. An `async fn` returns a **`Future`** — a lazy computation that does nothing until it's `.await`ed.
     (Unlike a Python coroutine, a Rust future is *inert* until something polls it.)
  2. `.await` drives a future to completion *without blocking the OS thread* — other tasks make
     progress meanwhile. You can only `.await` inside an `async fn` or `async` block.
  3. Rust's std has no runtime to *drive* futures, so you bring one. **Tokio** is the de-facto runtime;
     `#[tokio::main]` turns `async fn main` into a normal `main` that starts the runtime.
  4. **Connector IO (the `load` boundary), S3 (`aws-sdk-s3`), DynamoDB (`aws-sdk-dynamodb`), Redis
     (`redis`), and SurrealDB (`surrealdb`, `Mem` engine) are all async** — every call ends in `.await`.
     That's why `droplet-core` owns a Tokio runtime.
  5. **DuckDB — the _local_ analyze engine that reads the Parquet `load` already pulled down — is the
     opposite: synchronous, blocking CPU/IO work.** You must NOT run it on the async executor; you wrap
     it in `tokio::task::spawn_blocking` so it runs on a separate thread pool and doesn't freeze the
     runtime.
  - 🆕 Concept: the above is the whole mental model. Async/Tokio is beyond the Book, but the idea is
    "don't do blocking work on the async threads." (Rust Book: Fearless Concurrency — for the
    threads/`Send`/`Sync` background async builds on.)
  - ⚠️ Invariant (#5): the SurrealDB handle in fact #4 is **read-only and schema-derived** — built once
    from the schema at session start, queried (never written) after that, and **rebuilt** on resume
    rather than snapshotted (snapshots are just REPL bytes + a manifest; engines are reconstructed, never
    serialized).
- [ ] Add Tokio to the scratch crate: `cargo add tokio --features rt-multi-thread,macros`.
  - 🆕 Concept: `--features` turns on optional parts of a crate. Tokio is modular: `macros` gives you
    `#[tokio::main]`, `rt-multi-thread` gives the multi-threaded runtime. (Rust Book: More About Cargo
    and Crates.io — for the feature-flag idea)
  - ✅ Done when: `Cargo.toml` shows `tokio = { version = "1", features = ["rt-multi-thread", "macros"] }`.
- [ ] Make `main` async and await one future: replace `fn main` with
  `#[tokio::main] async fn main()`, and inside it
  `tokio::time::sleep(std::time::Duration::from_millis(100)).await;`, printing before and after.
  - ✅ Done when: it compiles and prints with a ~100 ms pause between the two lines.
- [ ] Write and call your own `async fn`: e.g.
  `async fn greet(name: &str) -> String { format!("hi {name}") }`, then `let s = greet("droplet").await;`.
  - 🆕 Concept: calling an `async fn` *returns the future*; nothing runs until you `.await` it. (Async is
    beyond the Book — read the Tokio tutorial's "Hello Tokio" / "Async in depth" pages.)
  - 🔗 Maps to: every connector method (`Source`, M0/M2) and every store method (M5–M8) is an
    `async fn ... -> Result<_, DropletError>` you'll `.await`. This is the exact call shape.
  - ✅ Done when: `s == "hi droplet"`.
- [ ] Fence off a **blocking** call with `spawn_blocking`: simulate DuckDB's sync work and double-`?`
  the result:
    ```rust
    let n: u64 = tokio::task::spawn_blocking(move || -> u64 {
        // pretend this is the sync DuckDB query (no .await allowed in here)
        std::thread::sleep(std::time::Duration::from_millis(50));
        42
    })
    .await
    .expect("blocking task panicked");
    ```
  - 🆕 Concept: `spawn_blocking` moves a closure onto a dedicated blocking-thread pool so a slow sync
    call never stalls the async runtime. Its `.await` yields a `Result` whose `Err` is a `JoinError`
    (did the task panic?). When the closure *also* returns a `Result`, you write `.await??` — the first
    `?` unwraps the `JoinError`, the second the inner result. (Async is beyond the Book; see the Tokio
    docs for `spawn_blocking`.)
  - 🔗 Maps to: this is *exactly* how Droplet runs its **local** DuckDB analyze engine (M1) — each
    analyze primitive (`local_sql`, `group_agg`, `to_rows`, …) connects + queries the **downloaded
    Parquet** + collects Arrow inside `spawn_blocking(...)`, then `.await??`s it. (Remember: DuckDB never
    touches a source — it only crunches the local copy `load` already pulled down.) The closure is `move`
    and must own its data (a DuckDB `Connection` is created and owned *inside* the closure, never shared
    across threads).
  - ⚠️ Invariant (#9): DuckDB is synchronous → run it inside `spawn_blocking`, never on the async
    executor; and (in the PyO3 layer in `droplet-py`, much later) release the GIL during query
    execution. NOTE invariant (#8): the GIL-release step lives **only** in `droplet-py` —
    `droplet-core` never imports `pyo3`, so the warm-up and `droplet-core` itself stay pure Rust.
  - ✅ Done when: `n == 42` and you can explain why the closure must be `move`.
- [ ] Share one value across two tasks with `Arc`: wrap a value in
  `let shared = std::sync::Arc::new(MemStore { map: Default::default() });`, `.clone()` the `Arc` into a
  spawned task with `tokio::spawn`, and read it from both.
  - 🆕 Concept: `Arc<T>` is an *atomically reference-counted* shared pointer — many owners, freed when
    the last drops. Cloning an `Arc` is cheap (it just bumps a counter); it does *not* deep-copy the
    value. This is how one immutable value is shared across async tasks. (Rust Book: Shared-State
    Concurrency — Atomic Reference Counting with `Arc<T>`)
  - 🔗 Maps to: a `Session`/`Catalog` holds each store as `Arc<dyn Store>` and clones the `Arc` into
    every task that needs it, so one store backs the whole run.
  - ✅ Done when: both tasks observe the same shared value and the program exits cleanly.
- [ ] Add a touch of `Mutex` for *shared mutation*: wrap a counter in
  `let m = std::sync::Arc::new(tokio::sync::Mutex::new(0u64));`, clone the `Arc` into a task, and in
  both places do `*m.lock().await += 1;`.
  - 🆕 Concept: `Arc` alone gives shared *read* access; to *mutate* shared state you also need a lock.
    `Mutex<T>` grants one writer at a time (`.lock()` returns a guard you deref). Use `tokio::sync::Mutex`
    when the lock is held across an `.await`; `std::sync::Mutex` for short non-async critical sections.
    (Rust Book: Shared-State Concurrency — Using Mutexes to Allow Access to Data from One Thread at a
    Time)
  - 🔗 Maps to: the in-memory dev `CoordinationStore` (run registry, leases, cache index) is an
    `Arc<Mutex<HashMap<...>>>` so concurrent tasks coordinate safely; the prod backends (Redis/DynamoDB)
    do this server-side instead.
  - ⚠️ Invariant (#7): mutable coordination (registry, leases, cache index) lives in the consistent
    store; the in-memory dev version uses a `Mutex` to stand in for that consistency.
  - ✅ Done when: after both tasks finish, the counter reads `2`.

---

### Chunk 10 — A light intro to hashing & content-addressing

> Droplet's artifact cache and snapshot store are **content-addressed**: the storage key *is* the hash
> of the bytes. Same bytes → same key, so identical results dedupe automatically across the fleet and a
> scan that already happened isn't repeated. This chunk gives you just enough hashing to get that idea.

- [ ] Read the one idea, then build it: *content-addressing* means you don't pick a name for a blob —
  you **hash its bytes** and use that fixed-length digest as the key. Identical inputs always produce
  the identical key, so you can ask "do we already have this?" without re-reading or re-computing it.
  - 🔗 Maps to: PRODUCT.md §11–§13 — the ArtifactStore (materialized Parquet) and SnapshotStore (REPL +
    manifest blobs) are *both* immutable and content-addressed; the CoordinationStore's cache index maps
    `cache_key → artifact_key`, where the cache key is `hash(scoped query + source + freshness_token)`.
  - ⚠️ Invariant (#7): immutable data is content-addressed in the object store; mutable coordination is
    in the consistent store.
- [ ] Add the hash crate: `cargo add blake3@1`.
  - 🆕 Concept: BLAKE3 is a fast cryptographic hash. Droplet uses **`blake3` (1.x)** rather than `sha2`
    because it's much faster and its `Hash` prints as lowercase hex out of the box. (Not in the Book —
    read the `blake3` crates.io README.)
  - ✅ Done when: `Cargo.toml` shows `blake3 = "1"` and `cargo build` is green.
- [ ] Write a content-addressing helper:
    ```rust
    fn artifact_key(bytes: &[u8]) -> String {
        blake3::hash(bytes).to_hex().to_string() // 64-char lowercase hex
    }
    ```
  - 🆕 Concept: feed bytes in, get a fixed digest out; `blake3::hash` takes a `&[u8]` slice and returns
    a `Hash`, and `.to_hex().to_string()` gives you an owned `String` key for a `HashMap`/object-store
    key. (`Hash::to_hex` returns a `blake3::Hex`, an `ArrayString`, **not** a `String` — so call
    `.to_string()`.)
  - 🔗 Maps to: this *single* helper is what the content-addressed cache (M5) and the snapshot store
    (M8) use for **both** the artifact cache key and the content-addressed snapshot key — same bytes,
    same key, automatic dedup across pods.
  - ✅ Done when: `cargo build` is green with `artifact_key` defined.
- [ ] Test that it's deterministic and collision-sensitive:
    ```rust
    #[test]
    fn same_bytes_same_key() {
        assert_eq!(artifact_key(b"abc"), artifact_key(b"abc"));
        assert_ne!(artifact_key(b"abc"), artifact_key(b"abd"));
    }
    ```
  - ✅ Done when: `cargo test` shows `same_bytes_same_key ... ok`. You can now explain why "same bytes →
    same key" means two pods that compute the same result write to the same key and dedupe for free.

---

## ✅ You're ready for M0 when you can…

Tick these off honestly. If one isn't true, revisit the chunk that taught it.

- [ ] Run `rustc --version` and see a recent stable (**≥ 1.89** is the safe floor once M0's maturin
  lands), and have `cargo fmt` and `cargo clippy` both work. *(Chunk 1 — M0's `rust-toolchain.toml` pins
  one exact version + `rustfmt` + `clippy`.)*
- [ ] Explain in one sentence the difference between a `--lib` and a `--bin` crate. *(`droplet-core` is a
  lib; `xtask` is a bin.)*
- [ ] Explain **move vs borrow vs clone**, and predict whether a given signature
  (`fn f(x: T)` vs `fn f(x: &T)` vs `fn f(x: &mut T)`) consumes, reads, or mutates its argument.
  *(The one that unblocks everything — Chunk 3.)*
- [ ] Define a `struct` with an `impl` block that has a `&mut self` method, and a `#[test]` that drives
  it. *(That's the `Session` shape — Chunk 4; invariant #3 — one local, ephemeral run.)*
- [ ] Write a `match` over an `enum` with data and have the compiler catch a missing variant. *(That's
  the Monty `ReplProgress` driver loop — Chunk 4; M3.)*
- [ ] Use `Option` with the `?` operator. *(That's a cache-index miss returning `None`.)*
- [ ] Use `Result` with the `?` operator, and define a `thiserror` error enum with a `#[from]` variant.
  *(That's `DropletError` — invariant #10.)*
- [ ] Build a `HashMap<u64, _>` registry that hands out incrementing `u64` ids and looks values back up.
  *(That's the handle registry — invariants #6 and #7.)*
- [ ] **Define a trait, implement it for two backends, take it by generic, and store mixed backends in a
  `Vec<Box<dyn Trait>>`.** *(That's the `Source` connector trait (M0) plus the three store traits —
  `ArtifactStore` (M5), `CoordinationStore` (M7), `SnapshotStore` (M8) — invariant #7. They're built
  across M0 and M5–M8, not all at once in M0. Remember: the real traits are async, so they'll need
  `#[async_trait]` to stay `dyn`-compatible.)*
- [ ] Mark items `pub` to expose them across a `mod` boundary, and keep internals private. *(That's how
  `droplet-core` exposes only its `Source`/store traits + flat tool surface — see the README "Practical
  note": Monty is a Python *subset*, so the surface is flat typed functions.)*
- [ ] **Run an `async fn` under `#[tokio::main]`, `.await` it, fence a blocking call with
  `spawn_blocking(...).await??`, and share one value across tasks with `Arc` (plus a `Mutex` for shared
  mutation).** *(That's the connector/S3/Redis/DynamoDB/Surreal async story + the local DuckDB engine's
  `spawn_blocking` — invariants #5, #9, #7.)*
- [ ] **Hash some bytes with `blake3` into a hex key and explain why "same bytes → same key" means the
  cache and snapshots dedupe across pods.** *(That's content-addressing — invariant #7; Chunk 10.)*

> When every box above is ticked, drop `droplet-warmup` from the workspace `members` (M0 tells you
> exactly where) and open [`M0-skeleton.md`](./M0-skeleton.md). M0 builds the handle registry,
> `DropletError`, the `Session`, and the **`Source` connector trait** (with a trivial local-Parquet dev
> connector) **for real** — and the three store traits (`ArtifactStore`, `CoordinationStore`,
> `SnapshotStore`) arrive later in M5–M8 — but now they'll all feel familiar instead of foreign.
