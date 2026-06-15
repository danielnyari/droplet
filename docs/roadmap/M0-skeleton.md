# M0 — Skeleton

**Milestone goal:** stand up the Droplet **virtual workspace**, build `droplet-core` (the one boundary error type + a generic handle registry + a per-run `Session`), define the **four pluggable store traits** (`Source`, `ArtifactStore`, `SnapshotStore`, `CoordinationStore`) with trivial in-memory/local dev impls, ship `droplet-py` as a Python wheel via **maturin**, wire **CI** — and prove a trivial **Monty** (sandboxed Python) run can call **one** host function over shared `Session` state.

**Done when (from the spec, BUILD ORDER step 1):** `cargo build` and `maturin develop` are green **and** a trivial Monty run calls one host function over shared session state; the four store traits exist with dev impls behind `Box<dyn …>`.

**Prerequisite:** finish [`00-rust-warmup.md`](./00-rust-warmup.md) first. You should be comfortable with `cargo build`/`cargo test`, `let`, functions, `struct`, `enum`, `match`, `Result`, the `?` operator, **traits + generics**, and **`async`/`await` + Tokio + `Arc`/`Mutex`** before starting here — those last two are exactly what the store traits and the async backends use.

**Estimate:** ~9 chunks (A–I). Each chunk is one sitting. Do them in order — later chunks depend on earlier ones.

The spec is at [`PRODUCT.md`](../../PRODUCT.md) (repo root). When a task says "⚠️ Invariant #N", N is one of the 10 numbered invariants in PRODUCT.md §8.

---

## How to read this file

- Every `- [ ]` is a tiny task (~10–30 minutes). Check it off only when its **✅** is true.
- `🆕 Concept:` explains a new Rust idea the **first** time it shows up. (Rust Book chapter **names** are given — run `rustup doc --book` to open the book offline; chapter *numbers* drift between editions, so trust the name.)
- `✅ Done when:` is an observable check — usually a command's output or a passing test. Don't move on until you see it.
- `⚠️ Invariant:` quotes a load-bearing rule from `PRODUCT.md` §8 (by its number 1–10) in plain words. Never break these.
- `🔗 Maps to:` ties a tiny exercise to the real Droplet concept it unlocks.
- `verify:` flags a fact the research couldn't fully pin on the locked version (especially Monty's fast-moving API). Check it against the real crate source/docs **before** relying on it.
- Code snippets are **anchors**, not full solutions. You write the real code.

> **Three `cargo add` traps to internalize before you start** (each is detailed in the chunk where it bites):
> 1. `cargo add monty` grabs **`monty 0.0.0`**, a *"Coming soon" placeholder* — not the interpreter. You will pull `monty` from **git, pinned to tag `v0.0.18`** (Chunk H). verify: re-check the latest tag at github.com/pydantic/monty before pinning — the digest verified `v0.0.18` (released 2026-05-29) as newest, but it's a pre-1.0 repo.
> 2. `cargo add` for serialization is **deferred** — no `postcard`/`zstd`/`blake3` in M0. Those arrive in **M7** (snapshot store). M0 needs only `thiserror` + `serde` + `async-trait` + `tokio`.
> 3. PyO3 lives **only** in `droplet-py` (Chunk G), **never** in `droplet-core` — invariant #1. Don't let an editor "add import" suggestion sneak `pyo3` into core.

> **What M0 deliberately does NOT touch** (so you don't over-build): no DuckDB (that's M1), no real S3/Redis/DynamoDB clients (M2–M4/M7 — M0 ships *in-memory/local* dev impls only), no read-only SurrealDB field search (M6), no snapshot serialization (M7), no Pydantic schema codegen (M5). M0 is the bones.

> **Sibling files this one links to** (names per the roadmap structure): [`M1-duckdb.md`](./M1-duckdb.md), and later [`M4-monty-driver.md`](./M4-monty-driver.md), [`M5-pydantic-schema.md`](./M5-pydantic-schema.md), [`M7-snapshot-store.md`](./M7-snapshot-store.md). If a file doesn't exist yet, the link is a forward reference — you'll create it when you reach that milestone.

---

### Chunk A — Workspace hygiene: make the existing skeleton green and intentional

You already did the warm-up and the first M0 steps under the *old* spec, so a partial workspace is on disk: a root `Cargo.toml` (with `[workspace]`/`[workspace.package]` but **no** `[workspace.dependencies]`), a `crates/droplet-core/` library crate whose `src/lib.rs` still holds **leftover guessing-game `fn main` code**, and a throwaway `droplet-warmup/` member. This chunk verifies what's right, adds the dependency table, and clears the leftover.

- [ ] Run `rustc --version` and `cargo --version` at the repo root.
  - 🆕 Concept: `rustup` manages compiler versions; `cargo` is the build tool + package manager (like `pip` + `venv` + `make` in one). (Rust Book: Getting Started)
  - ✅ Done when: both print a version. Edition 2024 needs Rust **≥ 1.85.0**; the repo machine has `1.96.0`, which is fine.

- [ ] Create `rust-toolchain.toml` at the repo root pinning the compiler for everyone who builds this repo:
  ```toml
  [toolchain]
  channel    = "1.96.0"
  components = ["rustfmt", "clippy"]
  ```
  - 🆕 Concept: `rust-toolchain.toml` pins one compiler version *per repo*; `rustup` auto-installs it the first time you run any cargo command here, making builds reproducible across machines and time. (Rust Book: Appendix E — Editions covers editions; toolchain pinning is a Cargo/rustup feature.)
  - Note: any stable `≥ 1.85.0` supports edition 2024; `1.96.0` is current (June 2026). Pinning an exact version is more deterministic than `"stable"`. (The whole later stack clears it: PyO3 0.29's MSRV is 1.83, an *edition-2024* consumer crate needs ≥ 1.85, `maturin 1.14` wants ≥ 1.89, and `redis 1.2` wants ≥ 1.88 — `1.96.0` clears all of them.)
  - ✅ Done when: `rustc --version` prints `1.96.0` inside the repo (rustup may download it on first use).

- [ ] Confirm `rustfmt` and `clippy` are available now that the toolchain lists them.
  - 🆕 Concept: `rustfmt` auto-formats code (`cargo fmt`); `clippy` is the Rust-aware linter (`cargo clippy`). Listing them as `components` guarantees they install with the pinned toolchain. (Rust Book: Appendix D — Useful Development Tools)
  - ✅ Done when: `cargo fmt --version` and `cargo clippy --version` both print a version.

- [ ] Open the root `Cargo.toml` and confirm it is a **virtual** manifest: a `[workspace]` table and **no** `[package]` table.
  - 🆕 Concept: a **virtual workspace** root builds nothing itself — it only groups member crates. `[workspace]` *without* `[package]` is what makes it virtual. (Rust Book: Cargo Workspaces)
  - Note: PRODUCT.md §9 says the root `Cargo.toml` is the virtual manifest. Do not add a `[package]` to the root.

- [ ] In the root `[workspace]` table, confirm `resolver = "3"` is set **explicitly** (it already is).
  - 🆕 Concept: the dependency *resolver* decides which versions/features each crate gets. Edition 2024 wants resolver `"3"`, and a virtual (package-less) workspace does **not** infer it from members — you must write it at the root. (Rust Book: Appendix E — Editions; the resolver detail is in the Rust 2024 edition guide, *Cargo: the resolver*.)
  - ⚠️ Gotcha: deleting this line on edition-2024 members produces a confusing resolver/edition error. Keep it.

- [ ] Confirm the root `[workspace.package]` table is present (it already ships these — keep the values as-is):
  ```toml
  [workspace.package]
  edition    = "2024"
  version    = "0.0.1"
  license    = "MIT"
  repository = "https://github.com/you/droplet"
  ```
  - 🆕 Concept: `[workspace.package]` holds values member crates inherit with `edition.workspace = true`. Define once, inherit everywhere. (Rust Book: Cargo Workspaces)

- [ ] **Add an empty `[workspace.dependencies]` table to the root `Cargo.toml`** (it does not exist yet). Just the header for now — you'll fill it one crate at a time, starting in the next step.
  ```toml
  [workspace.dependencies]
  ```
  - 🆕 Concept: `[workspace.dependencies]` declares versions in one place; members then write `<crate>.workspace = true` to opt in. No version drift across crates. (Rust Book: Cargo Workspaces)
  - ✅ Done when: the table header exists and `cargo build` still parses the manifest (it's empty, so nothing changes yet).

- [ ] Declare just `thiserror` in `[workspace.dependencies]` — one crate at a time:
  ```toml
  thiserror = "2.0.18"
  ```
  - ⚠️ Gotcha: pin the **2.x** major (`2.0.18`). The 1.x line is still widely downloaded so search results mix the two; the `#[error(...)]`/`#[from]` syntax is the same, but use 2.x.
  - ✅ Done when: the line is present. You'll add `serde` in Chunk B and `async-trait`/`tokio`/`monty`/`pyo3`/`anyhow` in later chunks.

- [ ] **Clear the leftover `fn main` out of `crates/droplet-core/src/lib.rs`.** It currently holds warm-up guessing-game code (`use std::io; fn main() { … }`). A **library crate has no `fn main`** — replace the whole file body with an empty-but-yours starting point (a single `//! droplet-core` doc comment is fine).
  - 🆕 Concept: a `--lib` crate's entry file is `src/lib.rs` and it exposes *items* (functions, structs) for other crates to use — it never has a `fn main`. Only `--bin` crates (`src/main.rs`) do. (Rust Book: Packages and Crates)
  - 🔗 Maps to: `droplet-core` is the pure-Rust heart of Droplet; everything else (the wheel, the adapter) calls into it. It must stay a clean library.
  - ✅ Done when: `cargo build -p droplet-core` is green on the cleared file (no `main`, no warnings about unused `std::io`).

- [ ] Note for later: **`droplet-warmup` can be removed from `members`** once you've finished the warm-up. You don't have to remove it yet (a stray member that still compiles is harmless), but plan to drop it before M1 so the workspace contains only real Droplet crates. Leave it for now if you're still poking at it.
  - ✅ Done when: you've decided — either it's gone from `members` and its folder deleted, or you've left a TODO to remove it before M1.

- [ ] Run `cargo build` at the repo root with the workspace as-is.
  - 🆕 Concept: `cargo metadata --no-deps` (a handy alternative) parses the manifests without compiling and prints the member list as JSON — a fast sanity check that the workspace resolves.
  - ✅ Done when: `cargo build` prints `Finished` with no errors. This green baseline is what every later chunk builds on.

- [ ] Confirm a `.gitignore` at the repo root ignores `target/` (it already does).
  - 🆕 Concept: `target/` holds all compiled output, is huge, and is regenerated by `cargo build`, so it never belongs in git. (Cargo/Git hygiene — no Rust Book chapter.)
  - ✅ Done when: `git status` lists nothing under `target/`.

---

### Chunk B — `droplet-core` dependencies: `thiserror` + `serde`

`droplet-core` is the heart of Droplet and must stay **pure Rust** — usable and testable with no Python. This chunk wires in the first two library dependencies: `thiserror` (the boundary error type) and `serde` with `derive` (you'll serialize `Session`/manifest-ish structs later). **No serialization-format crates yet** — `postcard`/`zstd`/`blake3` all arrive in M7, and DuckDB/Arrow in M1.

- [ ] Confirm `crates/droplet-core/Cargo.toml` inherits the workspace package values (it already does — verify the shape):
  ```toml
  [package]
  name    = "droplet-core"
  edition.workspace    = true
  version.workspace    = true
  license.workspace    = true
  repository.workspace = true

  [dependencies]
  ```
  - 🆕 Concept: `edition.workspace = true` pulls the value from `[workspace.package]` — one source of truth. (Rust Book: Cargo Workspaces)
  - ✅ Done when: `cargo build -p droplet-core` is green.

- [ ] Add `serde` to the **root** `[workspace.dependencies]` — **with the `derive` feature**:
  ```toml
  serde = { version = "1.0.228", features = ["derive"] }
  ```
  - ⚠️ Gotcha: `serde`'s `#[derive(Serialize)]` only works with `features = ["derive"]`. A bare `serde = "1"` gives "cannot find derive macro `Serialize`" — a classic beginner stumble.
  - ✅ Done when: the line is present in the workspace table.

- [ ] In `crates/droplet-core/Cargo.toml`, opt into both library deps under `[dependencies]`:
  ```toml
  [dependencies]
  thiserror.workspace = true
  serde.workspace     = true
  ```
  - 🆕 Concept: `thiserror.workspace = true` pulls the pin from `[workspace.dependencies]`. (Rust Book: Cargo Workspaces)
  - ⚠️ Invariant #1: "droplet-core must not depend on pyo3." Notice there is **no** `pyo3` here, and there never will be. Also note: **no `anyhow`** in a library — invariant #10 reserves `anyhow` for binaries.
  - ✅ Done when: `cargo build -p droplet-core` prints `Finished`.

---

### Chunk C — `DropletError`: the one boundary error type

Every engine error in Droplet (Monty, DuckDB, SurrealDB, S3, Redis, IO…) eventually folds into **one** error type, per invariant #10. Start it now with two variants; you'll add `#[from]` variants as you wire each engine in later milestones. **Test-first** throughout.

- [ ] Write a **failing** test first. In `src/lib.rs` add:
  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;

      #[test]
      fn bad_handle_displays_id() {
          let err = DropletError::BadHandle(7);
          assert_eq!(err.to_string(), "no such handle: 7");
      }
  }
  ```
  - 🆕 Concept: `#[cfg(test)]` marks code compiled only during `cargo test`; `mod tests { use super::*; }` is the conventional home for unit tests. (Rust Book: Writing Automated Tests)
  - 🆕 Concept: **test-first** — write the test, watch it fail (it won't even compile, since `DropletError` doesn't exist yet), then make it pass. This is the loop you'll repeat all milestone. (Rust Book: How to Write Tests)
  - ✅ Done when: `cargo test -p droplet-core` **fails to compile** (no `DropletError`). That failure is the goal of this step.

- [ ] Define `DropletError` as a `thiserror` enum with just the `BadHandle` variant:
  ```rust
  #[derive(thiserror::Error, Debug)]
  pub enum DropletError {
      #[error("no such handle: {0}")]
      BadHandle(u64),
  }
  ```
  - 🆕 Concept: an `enum` is a type that is exactly one of several variants, each optionally carrying data (`BadHandle(u64)` carries a `u64`). Rust's tagged-union / sum type. (Rust Book: Enums and Pattern Matching)
  - 🆕 Concept: `#[derive(...)]` auto-generates trait impls. `thiserror::Error` generates a real `std::error::Error`; `#[error("…")]` is the human message; `{0}` interpolates the first field. (Rust Book: Traits: Defining Shared Behavior)
  - 🆕 Concept: `pub` makes an item visible outside its module — required so other crates (and Python via `droplet-py`) can name `DropletError`. (Rust Book: Defining Modules to Control Scope and Privacy)
  - ✅ Done when: `cargo test -p droplet-core` compiles and `bad_handle_displays_id` **passes**.

- [ ] Add a second variant wrapping `std::io::Error` with `#[from]`:
  ```rust
  #[error("io error")]
  Io(#[from] std::io::Error),
  ```
  - 🆕 Concept: `#[from]` on a variant tells `thiserror` to generate `From<std::io::Error> for DropletError`, which is what makes `?` auto-convert. (Rust Book: Recoverable Errors with `Result`)
  - ✅ Done when: `cargo build -p droplet-core` is still green with both variants.

- [ ] Add a test proving `#[from]` auto-converts via `?`:
  ```rust
  #[test]
  fn io_error_folds_in() {
      fn might_fail() -> Result<(), DropletError> {
          let _ = std::fs::read("definitely-not-a-real-file")?; // io::Error -> DropletError
          Ok(())
      }
      assert!(might_fail().is_err());
  }
  ```
  - 🆕 Concept: `?` returns early on `Err`, calling `From` to convert when error types differ. `thiserror`'s `#[from]` *generates* that `From`, so `?` "just works". (Rust Book: Recoverable Errors with `Result`)
  - 🆕 Concept: `Result<T, E>` is recoverable error handling (caller decides); `panic!`/`.unwrap()` is unrecoverable. Libraries return `Result`; reserve `.unwrap()` for tests. (Rust Book: To `panic!` or Not to `panic!`)
  - ⚠️ Invariant #10: "One error type at the boundary: thiserror in libraries, anyhow at binaries; all engine errors fold into DropletError." This enum *is* that one type.
  - ✅ Done when: `cargo test -p droplet-core` shows both tests passing.

- [ ] Leave a comment listing the **future `#[from]` variants** so the design intent is visible — you do **not** add the deps yet:
  ```rust
  // Future #[from] variants fold in as engines arrive (invariant #10):
  //   DuckDb(#[from] duckdb::Error)            // M1
  //   Monty(#[from] monty::MontyException)     // Chunk H (this milestone) — verify the type name
  //   Surreal(#[from] surrealdb::Error)        // M6 (read-only field search)
  //   S3 / Redis / DynamoDB / postcard / zstd / tokio::task::JoinError  // M2/M4/M7
  ```
  - ⚠️ Invariant #10 again: each engine you wire later gets exactly one `#[from]` variant here, never a second ad-hoc error type leaking to the boundary.
  - Note: SurrealDB here is the **read-only, schema-derived field-search** engine (invariant #5), not a write/storage engine. It folds in at M6.
  - ✅ Done when: the comment is in place and `cargo build -p droplet-core` is still green.

---

### Chunk D — The generic handle registry

The registry is Droplet's **boundary seam**: engine objects (a DuckDB connection, a materialized result) live **host-side** inside the registry; the sandbox only ever receives an opaque `u64`. You build a small **generic** struct wrapping a `HashMap` plus a monotonic counter — test-first.

- [ ] Create `crates/droplet-core/src/registry.rs` and declare it from `lib.rs` with `pub mod registry;`.
  - 🆕 Concept: a `mod` is Rust's namespace/module. `pub mod registry;` loads `registry.rs` as a public child module. (Rust Book: Defining Modules to Control Scope and Privacy)
  - ✅ Done when: `cargo build -p droplet-core` is green with the empty module declared.

- [ ] Write a **failing** test in `registry.rs` describing the behavior:
  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;

      #[test]
      fn insert_then_get_roundtrips() {
          let mut reg: Registry<String> = Registry::new();
          let h = reg.insert("hello".to_string());
          assert_eq!(reg.get(h), Some(&"hello".to_string()));
      }
  }
  ```
  - ✅ Done when: `cargo test -p droplet-core` fails to compile (no `Registry` yet).

- [ ] Define the registry struct — fields only:
  ```rust
  use std::collections::HashMap;

  pub struct Registry<T> {
      next: u64,
      items: HashMap<u64, T>,
  }
  ```
  - 🆕 Concept: `HashMap<K, V>` is a key→value dictionary (like a Python `dict`). Keys are `u64` handles; values are host-side engine objects. (Rust Book: Storing Keys with Associated Values in Hash Maps)
  - 🆕 Concept: `<T>` is a **generic type parameter** — `Registry` works for *any* stored type `T` (a DuckDB `Connection` later, a `String` in this test). (Rust Book: Generic Data Types)
  - 🆕 Concept: `next: u64` is a *monotonic counter* — only ever increases, so every handle is unique and never reused within a run.
  - ⚠️ Invariant #4: "Engine objects live behind a handle registry; the sandbox sees only opaque handles." This struct *is* that boundary.

- [ ] Add an `impl` block with `new` and `insert`:
  ```rust
  impl<T> Registry<T> {
      pub fn new() -> Self {
          Self { next: 0, items: HashMap::new() }
      }
      pub fn insert(&mut self, value: T) -> u64 {
          let id = self.next;
          self.next += 1;          // monotonic: never hand out the same id twice
          self.items.insert(id, value);
          id
      }
  }
  ```
  - 🆕 Concept: `impl<T> Registry<T>` adds methods to the struct. `&mut self` borrows it mutably (the method may modify it). `Self` (capital S) is shorthand for `Registry<T>`. (Rust Book: Method Syntax)

- [ ] Add `get` to the same `impl` block:
  ```rust
  pub fn get(&self, handle: u64) -> Option<&T> {
      self.items.get(&handle)
  }
  ```
  - 🆕 Concept: `&self` borrows immutably (read-only); contrast with `&mut self`. (Rust Book: References and Borrowing)
  - 🆕 Concept: `Option<&T>` is "maybe a reference to `T`" — `Some(&value)` if present, `None` if not. No nulls, no exceptions; the caller must handle both. (Rust Book: The `Option` Enum)
  - ✅ Done when: `insert_then_get_roundtrips` passes.

- [ ] Add a test that handles are unique and a bogus handle is `None`:
  ```rust
  #[test]
  fn handles_are_unique_and_missing_is_none() {
      let mut reg: Registry<u32> = Registry::new();
      let a = reg.insert(1);
      let b = reg.insert(2);
      assert_ne!(a, b);                 // monotonic counter never repeats
      assert_eq!(reg.get(999), None);   // never-issued handle
  }
  ```
  - ✅ Done when: it passes.

- [ ] Add `remove`, returning the owned value, plus a test:
  ```rust
  pub fn remove(&mut self, handle: u64) -> Option<T> {
      self.items.remove(&handle)
  }
  ```
  - 🆕 Concept: `remove` returns `Option<T>` (owned) — `Some(value)` if present. This is how an engine handle is cleaned up when the session is done with it.
  - ✅ Done when: a `remove`-then-`get`-is-`None` test passes and `cargo test -p droplet-core` is green.

- [ ] Add a `require` helper that connects the registry to `DropletError` — the exact move engine functions make when the sandbox passes a bad handle:
  ```rust
  use crate::DropletError;

  impl<T> Registry<T> {
      pub fn require(&self, handle: u64) -> Result<&T, DropletError> {
          self.get(handle).ok_or(DropletError::BadHandle(handle))
      }
  }
  ```
  - 🆕 Concept: `Option::ok_or` turns `Some(v)` into `Ok(v)` and `None` into `Err(…)`. A "missing handle" becomes a `DropletError::BadHandle` the boundary can report. (Rust Book: Recoverable Errors with `Result`)
  - ⚠️ Invariant #4: engine functions will call `require(h)?` so a bad `u64` from the sandbox is rejected cleanly, never dereferenced.
  - ✅ Done when: a one-line test asserts `reg.require(999)` returns `Err`, and `cargo test -p droplet-core` is green.

---

### Chunk E — `Session`: the per-run context

A **`Session`** is the durable-but-ephemeral context for one run (invariant #9): it owns a unique working directory (wiped on close) and the handle registry. M0 keeps it minimal — the ephemeral DuckDB connection, the read-only Surreal handle, and the store backends get added in later milestones. The big new ideas here are `PathBuf`, `std::fs`, and the `Drop` trait.

- [ ] Create `crates/droplet-core/src/session.rs` and declare `pub mod session;` in `lib.rs`.
  - ✅ Done when: `cargo build -p droplet-core` is green with the empty module.

- [ ] Define the `Session` struct — fields only (just the two isolation fields + the registry for now):
  ```rust
  use std::path::PathBuf;
  use crate::registry::Registry;

  pub struct Session {
      run_id: String,
      work_dir: PathBuf,
      // One registry per session. The stored type is a placeholder for now;
      // it becomes the real engine-handle type when DuckDB lands in M1.
      handles: Registry<()>,
  }
  ```
  - 🆕 Concept: `PathBuf` is an **owned, growable filesystem path** (like `String` is to `&str`). Use `PathBuf` when the struct owns the path; `&Path` is the borrowed view. (Rust Book: the standard library; paths are in the std docs, not a numbered chapter.)
  - 🆕 Concept: `String` is an owned, UTF-8, growable string (vs the borrowed `&str`). `run_id` owns its text so the `Session` doesn't borrow from anywhere. (Rust Book: Storing UTF-8 Encoded Text with Strings)
  - ⚠️ Invariant #9: "one run = one Session = … a unique working dir wiped on close." These two fields are the start of that isolation.

- [ ] Write a **failing** test for `Session::new`:
  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;

      #[test]
      fn new_creates_a_fresh_work_dir() {
          let s = Session::new("run-123").unwrap();
          assert!(s.work_dir().is_dir());   // the dir exists on disk
      }
  }
  ```
  - ✅ Done when: it fails to compile (no `Session::new` / `work_dir` yet).

- [ ] Implement `new` so it **creates and wipes** a unique working dir under the system temp directory:
  ```rust
  use std::fs;
  use crate::DropletError;

  impl Session {
      pub fn new(run_id: &str) -> Result<Self, DropletError> {
          // Unique per run so two sessions never collide (invariant #9 isolation).
          let work_dir = std::env::temp_dir().join(format!("droplet-{run_id}"));
          // Wipe any stale dir from a previous run, then recreate it empty.
          let _ = fs::remove_dir_all(&work_dir); // ignore "not found"
          fs::create_dir_all(&work_dir)?;        // io::Error -> DropletError via #[from]
          Ok(Self {
              run_id: run_id.to_string(),
              work_dir,
              handles: Registry::new(),
          })
      }

      pub fn work_dir(&self) -> &std::path::Path { &self.work_dir }
      pub fn run_id(&self) -> &str { &self.run_id }
  }
  ```
  - 🆕 Concept: `std::env::temp_dir()` returns the OS temp directory; `.join(...)` appends a path segment. `std::fs::create_dir_all` makes the directory (and parents); `remove_dir_all` deletes a directory tree. (Rust std docs: `std::fs`, `std::env`.)
  - 🆕 Concept: `fs::create_dir_all(&work_dir)?` — the `?` turns any `io::Error` into a `DropletError` via the `#[from] std::io::Error` variant from Chunk C. The error type *just folds in*. (Rust Book: Recoverable Errors with `Result`)
  - verify: for v1 a deterministic `droplet-{run_id}` dir is fine because `run_id` is unique per run. If you ever need collision-proof temp dirs without a meaningful `run_id`, consider the `tempfile` crate's `TempDir` (which also auto-removes on drop) — **not needed for M0**, just a note.
  - ✅ Done when: `new_creates_a_fresh_work_dir` passes.

- [ ] Add a **`Drop` impl** that removes the working dir when the session ends:
  ```rust
  impl Drop for Session {
      fn drop(&mut self) {
          // Best-effort cleanup; ignore errors during teardown.
          let _ = std::fs::remove_dir_all(&self.work_dir);
      }
  }
  ```
  - 🆕 Concept: the **`Drop` trait** runs code automatically when a value goes out of scope — Rust's deterministic cleanup (like a context-manager `__exit__`, but automatic and tied to ownership, no `with` needed). (Rust Book: Running Code on Cleanup with the `Drop` Trait)
  - 🆕 Concept: `Drop::drop` takes `&mut self` and can't return a `Result`, so cleanup is **best-effort** — you `let _ =` the result and never panic in a destructor.
  - ⚠️ Invariant #9: "a unique working dir wiped on close." `Drop` guarantees the wipe even if the run errors out.
  - 🔗 Maps to: this is the per-run isolation guarantee — credentials and tool paths get confined to this session dir in later milestones.
  - ✅ Done when: a test captures the `work_dir` path, drops the session (let it go out of scope), and asserts the path **no longer exists**; `cargo test -p droplet-core` is green.

- [ ] (Optional) Add an explicit `close(self) -> Result<(), DropletError>` that consumes the session and surfaces a teardown error, for callers who want it to be loud rather than best-effort.
  - 🆕 Concept: a method taking `self` (by value, not `&self`) **consumes** the receiver — after `close()` the session can't be used again, which models "the run is over." (Rust Book: Method Syntax / Ownership)
  - ✅ Done when: `close()` removes the dir and returns `Ok(())` on success; `cargo build -p droplet-core` is green. (`Drop` still runs as a backstop.)

- [ ] Keep `Session` minimal — leave a comment marking where engines plug in, so you don't over-build now:
  ```rust
  // Later milestones add (NOT in M0):
  //   duck: duckdb::Connection             // M1 — ephemeral per-session OLAP engine
  //   surreal: read-only Surreal<Mem>      // M6 — schema-derived field search (read-only)
  //   the four Box<dyn …> store backends   // Chunk F (wired in here as fields)
  ```
  - ✅ Done when: the comment is present and `cargo build -p droplet-core` is green.

---

### Chunk F — The FOUR store traits + trivial dev impls (the heart of step 1)

This is the central abstraction of the whole distributed design and the big **traits** lesson. Droplet's state plane has four pluggable seams (invariant #8): a **`Source`** (read bytes from S3 in prod), an **`ArtifactStore`** (content-addressed Parquet cache + intermediates), a **`SnapshotStore`** (REPL+manifest blobs), and a **`CoordinationStore`** (run registry / leases / cache index). You define each as a **trait**, then write the trivial **in-memory / local** dev impl, then a round-trip test — one store at a time.

> **Sync-vs-async decision (read first — PREFER THE SIMPLEST CORRECT CHOICE):** the real backends are async (S3, Redis, DynamoDB all `.await`). Native `async fn` in traits is stable on Rust 1.96, **but it is not dyn-compatible**, and Droplet holds these as `Box<dyn ArtifactStore>` so a `Session` can carry any backend. The digest's verdict: the clean, beginner-safe way to get async methods on a `dyn` trait is the **`async-trait`** crate (`0.1.89`). So: **define the traits as `#[async_trait]` async traits now.** The dev impls are trivially async (they just `Ok(...)` immediately — no real awaiting). This keeps the trait shape identical when the real S3/Redis/DynamoDB impls land in M2/M4/M7, so you never rewrite the seam.
>
> The alternative — keeping the traits **sync** in M0 and converting to async later — is *simpler to read* but forces a breaking trait-signature change when real backends arrive. Prefer `async-trait` now to avoid that churn. If you find async genuinely overwhelming at this point, it is acceptable to ship **sync** trait signatures in M0 and convert in M2 — just know you're trading a later rewrite for present simplicity.

- [ ] Add `async-trait` to the root `[workspace.dependencies]`:
  ```toml
  async-trait = "0.1.89"
  ```
  - 🆕 Concept: `#[async_trait]` is a proc-macro that rewrites async trait methods into something `Box<dyn Trait>` can hold (it boxes the returned future). You annotate **both** the trait and each `impl`. (No Rust Book chapter — see the `async-trait` crate docs.)
  - ✅ Done when: the line is present.

- [ ] Add `tokio` to the root `[workspace.dependencies]` with the M0 feature set:
  ```toml
  tokio = { version = "1.52.3", features = ["rt-multi-thread", "macros", "sync"] }
  ```
  - 🆕 Concept: **Tokio** is Rust's async runtime (it actually *runs* `async` functions). `#[tokio::test]` lets a test `.await`; `features = ["sync"]` brings in `tokio::sync::Mutex` for the in-memory stores. (Rust Book: there's no async chapter; the warm-up's async section + the Tokio docs cover this.)
  - Note: you'll add the `"fs"` feature later only if `LocalFsSource` (F.4) uses `tokio::fs`; for M0 simplicity you can keep `"fs"` out and use `std::fs::read` instead.
  - ✅ Done when: the line is present.

- [ ] In `crates/droplet-core/Cargo.toml` under `[dependencies]`, opt into both:
  ```toml
  async-trait.workspace = true
  tokio.workspace       = true
  ```
  - ✅ Done when: `cargo build -p droplet-core` is green with the new deps.

- [ ] Create `crates/droplet-core/src/stores.rs` and declare `pub mod stores;` in `lib.rs`. All four traits + dev impls live here for now.
  - ✅ Done when: `cargo build -p droplet-core` is green.

#### F.1 — `ArtifactStore` (immutable, content-addressed bytes)

- [ ] Define the trait. Two methods: store bytes under a key, fetch bytes by key.
  ```rust
  use async_trait::async_trait;
  use crate::DropletError;

  #[async_trait]
  pub trait ArtifactStore: Send + Sync {
      async fn put(&self, key: &str, bytes: Vec<u8>) -> Result<(), DropletError>;
      async fn get(&self, key: &str) -> Result<Vec<u8>, DropletError>;
  }
  ```
  - 🆕 Concept: a **trait** is a set of method signatures a type can implement — like a Python `Protocol`/ABC. `Box<dyn ArtifactStore>` then stores *any* implementor behind a pointer (dynamic dispatch). (Rust Book: Traits: Defining Shared Behavior; Using Trait Objects That Allow for Values of Different Types)
  - 🆕 Concept: the `: Send + Sync` supertrait bound means "safe to move/share across threads" — required because the store lives in a `Session` used from Tokio's multi-threaded runtime. (Rust Book: Extensible Concurrency with the `Send` and `Sync` Traits)
  - ⚠️ Invariant #8: "immutable data is content-addressed in the object store." In prod this is S3; the key is a content hash (computed in M2 with `blake3`). The trait shape is the seam.

- [ ] Add a `NotFound(String)` variant to `DropletError` so `get` on a missing key has somewhere to go:
  ```rust
  #[error("not found: {0}")]
  NotFound(String),
  ```
  - ✅ Done when: `cargo build -p droplet-core` is green.

- [ ] Write the trivial **in-memory** dev impl backed by a `HashMap`:
  ```rust
  use std::collections::HashMap;
  use tokio::sync::Mutex;

  #[derive(Default)]
  pub struct MemArtifactStore { inner: Mutex<HashMap<String, Vec<u8>>> }

  #[async_trait]
  impl ArtifactStore for MemArtifactStore {
      async fn put(&self, key: &str, bytes: Vec<u8>) -> Result<(), DropletError> {
          self.inner.lock().await.insert(key.to_string(), bytes);
          Ok(())
      }
      async fn get(&self, key: &str) -> Result<Vec<u8>, DropletError> {
          self.inner.lock().await.get(key).cloned()
              .ok_or_else(|| DropletError::NotFound(key.to_string()))
      }
  }
  ```
  - 🆕 Concept: `tokio::sync::Mutex` is an **async-aware lock** — `.lock().await` waits without blocking the runtime thread. Use it (not `std::sync::Mutex`) when the lock is held across an `.await` in async code. Interior mutability behind `&self` lets the trait methods take `&self` yet still mutate. (Rust Book: Shared-State Concurrency; the async variant is in the Tokio docs.)
  - 🆕 Concept: `#[derive(Default)]` gives you `MemArtifactStore::default()` for free (an empty map). `.cloned()` turns `Option<&Vec<u8>>` into `Option<Vec<u8>>` by copying the bytes out. (Rust Book: Derivable Traits, Appendix C)
  - ✅ Done when: it compiles.

- [ ] Round-trip test it under a Tokio test:
  ```rust
  #[tokio::test]
  async fn artifact_roundtrips() {
      let s = MemArtifactStore::default();
      s.put("k", b"hi".to_vec()).await.unwrap();
      assert_eq!(s.get("k").await.unwrap(), b"hi".to_vec());
      assert!(s.get("missing").await.is_err());
  }
  ```
  - 🆕 Concept: `#[tokio::test]` makes an `async fn` test runnable — it spins up a runtime just for the test. (Tokio docs.)
  - ✅ Done when: `cargo test -p droplet-core` is green.

#### F.2 — `SnapshotStore` (immutable, content-addressed run snapshots)

- [ ] Define the trait — same shape as `ArtifactStore` but semantically distinct (snapshots, not cache artifacts). Keep them **separate traits** even though the M0 dev impls look identical: in prod they're different buckets/policies (invariant #8), and separate traits document intent.
  ```rust
  #[async_trait]
  pub trait SnapshotStore: Send + Sync {
      async fn put(&self, key: &str, blob: Vec<u8>) -> Result<(), DropletError>;
      async fn get(&self, key: &str) -> Result<Vec<u8>, DropletError>;
  }
  ```
  - ⚠️ Invariant #8 + invariant #3: snapshots are "immutable, content-addressed … compressed" REPL-bytes-plus-manifest blobs. M0 only ships the seam; zstd/postcard/blake3 land in **M7**.

- [ ] Write a `MemSnapshotStore` (HashMap-backed, same pattern as F.1).
  - ✅ Done when: it compiles.

- [ ] Round-trip `#[tokio::test]` it (put then get, and a missing key is `Err`).
  - ✅ Done when: `cargo test -p droplet-core` is green.

#### F.3 — `CoordinationStore` (mutable, strongly consistent coordination)

- [ ] Define the trait with the three jobs the spec names: run registry, leases, cache index. Keep methods minimal and string-typed for M0.
  ```rust
  #[async_trait]
  pub trait CoordinationStore: Send + Sync {
      // Run registry: run_id -> snapshot pointer / status.
      async fn put_run(&self, run_id: &str, snapshot_key: &str) -> Result<(), DropletError>;
      async fn get_run(&self, run_id: &str) -> Result<Option<String>, DropletError>;
      // Cache index: cache_key -> artifact_key.
      async fn put_cache(&self, cache_key: &str, artifact_key: &str) -> Result<(), DropletError>;
      async fn get_cache(&self, cache_key: &str) -> Result<Option<String>, DropletError>;
      // Lease: acquire "one active worker per run"; true = we hold it.
      async fn try_acquire_lease(&self, run_id: &str, owner: &str) -> Result<bool, DropletError>;
  }
  ```
  - 🆕 Concept: returning `Result<Option<String>, _>` separates two failure modes: `Err` = the store itself broke; `Ok(None)` = the store is fine, the key just isn't there. Don't collapse them. (Rust Book: The `Option` Enum; Recoverable Errors with `Result`)
  - ⚠️ Invariant #8: "mutable coordination (registry, leases, cache index) is in the consistent store." In prod that's Redis or DynamoDB (M4); here it's an in-memory map. The lease method is the seam M4 makes correct with Redis `SET key val NX PX ms` (or DynamoDB `attribute_not_exists`).

- [ ] Write a `MemCoordinationStore` — fields only (three maps behind one or more `Mutex`es): a run-registry map, a cache-index map, and a leases map. Sketch:
  ```rust
  #[derive(Default)]
  pub struct MemCoordinationStore {
      runs:    Mutex<HashMap<String, String>>,
      cache:   Mutex<HashMap<String, String>>,
      leases:  Mutex<HashMap<String, String>>, // run_id -> owner
  }
  ```
  - ✅ Done when: it compiles.

- [ ] Implement `put_run`/`get_run` and `put_cache`/`get_cache` (plain HashMap insert/get behind `.lock().await`, cloning the value out for the getter).
  - ✅ Done when: a `#[tokio::test]` round-trips both the registry and the cache index; `cargo test -p droplet-core` is green.

- [ ] Implement the dev `try_acquire_lease`. Simplest correct rule: acquire succeeds only if no owner is recorded for that `run_id`.
  ```rust
  async fn try_acquire_lease(&self, run_id: &str, owner: &str) -> Result<bool, DropletError> {
      let mut g = self.leases.lock().await;
      if g.contains_key(run_id) { return Ok(false); }   // already held
      g.insert(run_id.to_string(), owner.to_string());
      Ok(true)
  }
  ```
  - 🔗 Maps to: this in-memory lease is enough to *write tests against the seam now*; M4 swaps in a TTL + atomicity (Redis `NX PX`, or DynamoDB conditional put). No affinity, reassignable later.
  - ✅ Done when: a `#[tokio::test]` proves a second `try_acquire_lease` for the same `run_id` returns `Ok(false)`; `cargo test -p droplet-core` is green.

#### F.4 — `Source` (read-only ingest seam)

- [ ] Define the `Source` trait — read bytes for a logical source name. In prod this fronts S3 (Parquet/CSV); the dev impl reads a local file (or an in-memory map).
  ```rust
  #[async_trait]
  pub trait Source: Send + Sync {
      async fn read(&self, name: &str) -> Result<Vec<u8>, DropletError>;
  }
  ```
  - ⚠️ Invariant #8 + invariant #9: sources are the *only* read seam to org data, scoped per session in prod. Keeping it a trait means the same code path serves a local dev file and S3.

- [ ] Write a **`LocalFsSource`** whose `read` resolves `name` under a base directory and returns the file bytes:
  ```rust
  use std::path::PathBuf;

  pub struct LocalFsSource { base: PathBuf }

  #[async_trait]
  impl Source for LocalFsSource {
      async fn read(&self, name: &str) -> Result<Vec<u8>, DropletError> {
          let path = self.base.join(name);
          // For M0 simplicity use std::fs::read (sync). If you prefer tokio::fs::read
          // (async, non-blocking), add the "fs" feature to tokio in workspace deps.
          Ok(std::fs::read(path)?) // io::Error folds into DropletError via #[from]
      }
  }
  ```
  - 🆕 Concept: even in an `async fn`, calling sync `std::fs::read` is fine for tiny dev reads. The async cousin `tokio::fs::read` only matters when you don't want to block the runtime thread on real IO (needs tokio feature `"fs"`). (Tokio docs.)
  - ✅ Done when: a `#[tokio::test]` writes a temp file, reads it back through `LocalFsSource`, asserts the bytes match; `cargo test -p droplet-core` is green.

#### F.5 — Hold them on the `Session` behind `Box<dyn …>`

- [ ] Add the four stores to `Session` as trait objects so any backend (dev now, S3/Redis/DynamoDB later) plugs in unchanged:
  ```rust
  pub struct Session {
      run_id: String,
      work_dir: PathBuf,
      handles: Registry<()>,
      artifacts: Box<dyn ArtifactStore>,
      snapshots: Box<dyn SnapshotStore>,
      coord:     Box<dyn CoordinationStore>,
      source:    Box<dyn Source>,
  }
  ```
  - 🆕 Concept: `Box<dyn Trait>` is a **trait object** — a pointer that erases the concrete type, so one field can hold an `S3ArtifactStore` *or* a `MemArtifactStore`. This is exactly why the methods are `&self`-only and the trait is `Send + Sync`. (Rust Book: Using Trait Objects That Allow for Values of Different Types)
  - ⚠️ Invariant #8: the four backends are *the* distributed seams. A `Session` carries them as plug-points, not concrete types.

- [ ] Update `Session::new` (or add `Session::new_with_dev_stores(run_id)`) to default to the `Mem*`/`LocalFs*` dev impls when building those four fields.
  - 🆕 Concept: `Box::new(MemArtifactStore::default())` coerces a concrete type into a `Box<dyn ArtifactStore>` automatically at the field assignment. (Rust Book: Using Trait Objects)
  - ✅ Done when: `cargo build -p droplet-core` is green.

- [ ] Prove a session carries a working store: a test builds a session and round-trips one artifact through `session.artifacts` (add a tiny `pub` accessor or test-only method if needed).
  - ✅ Done when: `cargo test -p droplet-core` is green with the session→artifact round-trip test passing.

---

### Chunk G — `droplet-py`: a PyO3 cdylib wheel (the pyo3 firewall)

Now add the **second** crate. `droplet-py` is the **only** place `pyo3` is allowed (invariant #1). It's a `cdylib` (a shared library Python imports) packaged into a wheel by **maturin**. This chunk proves the Python toolchain end-to-end with a trivial function — no Monty yet, no real core calls yet.

- [ ] Create the crate: `cargo new --lib crates/droplet-py`.
  - ✅ Done when: the folder `crates/droplet-py/` with `Cargo.toml` + `src/lib.rs` exists.

- [ ] Add it to the root `members`: `members = ["crates/droplet-core", "crates/droplet-py"]` (keep `"droplet-warmup"` only if you're still using it).
  - ✅ Done when: `cargo metadata --no-deps` lists `droplet-py`.

- [ ] Add `pyo3` to the root `[workspace.dependencies]` with the cdylib feature set:
  ```toml
  pyo3 = { version = "0.29", features = ["extension-module", "abi3-py310"] }
  ```
  - 🆕 Concept: PyO3's `extension-module` feature tells PyO3 **not** to link `libpython` directly (Python supplies the symbols at import). `abi3-py310` builds **one** stable-ABI wheel that runs on CPython ≥ 3.10 (instead of one wheel per Python version). (No Rust Book chapter — see https://pyo3.rs.)
  - ⚠️ Invariant #1: this dep belongs **only** to `droplet-py`. Do not add `pyo3` to `droplet-core`.

- [ ] Make `droplet-py` a `cdylib`. Edit `crates/droplet-py/Cargo.toml`:
  ```toml
  [package]
  name    = "droplet-py"
  edition.workspace    = true
  version.workspace    = true
  license.workspace    = true
  repository.workspace = true

  [lib]
  name       = "_droplet"        # compiled module name -> imported as droplet._droplet
  crate-type = ["cdylib"]

  [dependencies]
  pyo3.workspace = true
  ```
  - 🆕 Concept: a `cdylib` ("C dynamic library") compiles to a `.so`/`.pyd`/`.dylib` that CPython `dlopen`s as a native module — unlike a normal Rust `rlib` (only other Rust crates use that). (Cargo/PyO3 detail; Rust Book context: Packages and Crates.)
  - ⚠️ Gotcha: the `[lib] name`, the `#[pymodule]` function name, and the import name must all be `_droplet` (underscore-prefixed so a pure-Python `droplet` package can wrap it). A mismatch gives `ImportError: dynamic module does not define module export function`.
  - ⚠️ Invariant #1: "PyO3 lives only in droplet-py." This is the only crate with pyo3, now and forever. When `droplet-py` later calls `droplet-core`, only plain values/handles cross — no pyo3 types leak into core.
  - ✅ Done when: `cargo build -p droplet-py` resolves the manifest (it may not fully link until `src/lib.rs` has a module — next step).

- [ ] Write the smallest possible `crates/droplet-py/src/lib.rs`:
  ```rust
  use pyo3::prelude::*;

  #[pyfunction]
  fn add(a: u64, b: u64) -> u64 { a + b }

  // Function-style #[pymodule]: the param is &Bound<'_, PyModule> (current 0.29 API).
  #[pymodule]
  fn _droplet(m: &Bound<'_, PyModule>) -> PyResult<()> {
      m.add_function(wrap_pyfunction!(add, m)?)?;
      Ok(())
  }
  ```
  - 🆕 Concept: `#[pyfunction]`/`#[pymodule]` are PyO3 proc-macros generating the C glue so Python can call Rust; `wrap_pyfunction!` registers a function into the module. (https://pyo3.rs)
  - 🆕 Concept: `Bound<'py, T>` is PyO3 0.29's GIL-bound smart pointer to a Python object. The `#[pymodule]` fn takes `&Bound<'_, PyModule>` — older `&PyModule` "GIL Refs" snippets are pre-0.21 and won't compile.
  - ⚠️ Gotcha (the invariant-#6 rename): PyO3 0.26 renamed `allow_threads → detach`, `with_gil → attach`, `prepare_freethreaded_python → Python::initialize`, with **no** deprecated aliases. On 0.29 only the new names exist. You don't need them yet (the real GIL-release wrapping lands when DuckDB does in M1), but every stale tutorial uses `allow_threads` — use `detach`.
  - ✅ Done when: `cargo build -p droplet-py` is green (compiles the cdylib; doesn't install into Python yet).
  - Note (`extension-module` + `cargo test`): the `extension-module` feature makes a pure-Rust unit-test binary fail to link (no libpython). For M0 the smoke test is the Python-side import below, so this is fine. If you later add Rust unit tests here, gate `extension-module` behind an optional feature maturin turns on but `cargo test` leaves off. verify: confirm this is still the recommended pattern in the PyO3 0.29 "building & distribution" guide.

- [ ] Add `crates/droplet-py/pyproject.toml` so maturin can package the wheel:
  ```toml
  [build-system]
  requires      = ["maturin>=1.14,<2.0"]
  build-backend = "maturin"

  [project]
  name            = "droplet"
  requires-python = ">=3.10"

  [tool.maturin]
  module-name = "droplet._droplet"   # matches the [lib] name and #[pymodule] fn
  ```
  - 🆕 Concept: a *wheel* (`.whl`) is Python's binary install artifact. maturin compiles your cdylib and packages it, then can install it into the active virtualenv. (https://maturin.rs)
  - ⚠️ Gotcha: `requires-python = ">=3.10"` must agree with `abi3-py310` — both say "CPython 3.10+." A mismatch makes pip mis-resolve the wheel. (Current maturin is `1.14`; the `>=1.14,<2.0` bound covers it.)
  - ✅ Done when: the file exists with the three tables.

- [ ] Create and activate a Python virtualenv:
  ```bash
  python3 -m venv .venv
  source .venv/bin/activate
  ```
  - 🆕 Concept: a *virtualenv* is an isolated Python environment. `maturin develop` installs **into whatever venv is active** — with none active it errors or pollutes system Python. Always activate first.
  - ✅ Done when: `python --version` is ≥ 3.10 and your prompt shows `.venv`.

- [ ] Install maturin into the venv:
  ```bash
  pip install maturin
  ```
  - ✅ Done when: `which maturin` points inside `.venv`.

- [ ] Build + install the wheel into the venv for the dev loop:
  ```bash
  maturin develop --manifest-path crates/droplet-py/Cargo.toml
  ```
  - 🆕 Concept: `maturin develop` compiles **and** installs into the active venv (fast inner loop); `maturin build` just emits a distributable `.whl` in `target/wheels/` without installing. Use `develop` while iterating.
  - ✅ Done when: it prints success.

- [ ] Import it from Python and call the function:
  ```bash
  python -c "from droplet._droplet import add; print(add(2, 3))"
  ```
  - ✅ Done when: it prints `5`. **This is the first half of the M0 "Done when": `maturin develop` is green and Python can call into Rust.**

---

### Chunk H — Wire Monty into `droplet-core` and call ONE host function over shared `Session` state

This adds the **real** sandboxed interpreter (`monty`) to `droplet-core` and proves the suspend/resume seam: Python calls a host function, Monty *pauses*, your Rust host mutates shared `Session` state and resumes. **No pyo3 here** — `droplet-core` stays pure Rust (invariant #1). This is the M0 finish line for the Rust side.

> ⚠️ **MONTY DEPENDENCY TRAP:** crates.io `monty 0.0.0` is a placeholder ("Coming soon"), **not** the interpreter — `cargo add monty` will not compile against the real API. Depend on it via **git, pinned to tag `v0.0.18`**. verify: re-confirm `v0.0.18` is the latest tag before pinning (the digest verified it latest, released 2026-05-29, but the repo is pre-1.0). The real "docs" are the GitHub README and the source under `crates/monty/src/*.rs`; docs.rs shows nothing useful.
>
> The signatures below are the digest's **most-likely shape at `v0.0.18`**. The API is pre-1.0 and churns every few weeks. **`verify:` every name against the source** (`crates/monty/src/repl.rs`, `run_progress.rs`, `resource.rs`) before relying on it. Heads-up: this dep drags in Astral's `ty`/`ruff` (a custom Ruff fork) and `salsa` crates, so the **first build is long** — that's normal. **Commit `Cargo.lock`** afterward and avoid `cargo update` on this tree.

- [ ] Add the git dependency to the root `[workspace.dependencies]`:
  ```toml
  monty = { git = "https://github.com/pydantic/monty", tag = "v0.0.18" }
  ```
  - 🆕 Concept: a *git dependency* points Cargo at a repo (here pinned to tag `v0.0.18`) instead of crates.io. Pin a **tag**, never float `main` — the API changes fast. (Cargo reference: specifying dependencies.)
  - Note: the type-check-before-run loop lives in a **sibling** crate, `monty-type-checking` (same repo/tag), and is wired in **M5/M4**, not M0. Don't add it yet. (verify: exact `type_check(&SourceFile, Option<&SourceFile>)` API at the tag when you reach it — per the digest, but pre-1.0.)
  - ✅ Done when: the line is present.

- [ ] Opt into `monty` from `crates/droplet-core/Cargo.toml` under `[dependencies]`: `monty.workspace = true`. Then run `cargo build -p droplet-core`.
  - ⚠️ Invariant #1: do **not** add `monty-python` (that's the PyO3 binding crate, published as `pydantic_monty` on PyPI; it would pull pyo3 into core and break the firewall). Use the pure-Rust `monty` core crate only. No feature flags needed for M0.
  - ✅ Done when: `cargo build -p droplet-core` resolves and downloads `monty` from git (slow first fetch is normal). This step defeats the placeholder-crate trap.

- [ ] **Verify the core type/function names against the source before writing code.** Open `crates/monty/src/repl.rs`, `run_progress.rs`, `resource.rs` at tag `v0.0.18` and confirm the spellings of: `MontyRepl`, `MontyObject`, `ReplProgress`, `NoLimitTracker`, `LimitedTracker`, `ResourceLimits`, `PrintWriter`, `ExtFunctionResult`, `MontyException`, `NameLookupResult`.
  - verify: the digest's "most-likely" shapes — treat the snippets below as a sketch, the source as truth, and note any name that differs before continuing.
  - ✅ Done when: you've eyeballed the real signatures and written down any deviation.

- [ ] Create `crates/droplet-core/src/sandbox.rs`, declare `pub mod sandbox;` in `lib.rs`, and write a smoke test using the **persistent REPL** (`MontyRepl`) to run `1 + 2`:
  ```rust
  use monty::{MontyRepl, MontyObject, NoLimitTracker, PrintWriter};

  #[cfg(test)]
  mod tests {
      use super::*;

      #[test]
      fn repl_runs_trivial_expression() {
          // feed_run runs a chunk to a single value (no external-fn pauses).
          let mut repl = MontyRepl::new("session.py", NoLimitTracker);
          let v = repl.feed_run("1 + 2", vec![], PrintWriter::Stdout).unwrap();
          assert_eq!(v, MontyObject::Int(3));
      }
  }
  ```
  - 🆕 Concept: **`MontyRepl`** is the *persistent* session that runs successive code chunks and keeps variables alive between them — the model Droplet's per-`run_code`-step design needs (vs `MontyRun`, which runs one program). (Monty README.)
  - 🆕 Concept: `feed_run(code, inputs, print)` runs a chunk to a final value with **no** external-function pauses you handle; `feed_start` (next step) returns a `ReplProgress` you loop over to service host calls. Use `feed_run` for this pure smoke test.
  - 🆕 Concept: `MontyObject` is Monty's value type (an enum: `Int`, `Str`, …). Python values cross the boundary as `MontyObject`, never as native Rust types.
  - ⚠️ Invariant #7: Monty runs a *subset* of Python — no classes, no `match` statements, no third-party imports, limited stdlib (`sys`/`os`/`typing`/`asyncio`/`re`/`datetime`/`json` + `open()` at v0.0.18). Keep test scripts tiny and inside the subset. verify: re-read the README "limitations" at the exact tag — the supported-module list changes release to release.
  - verify: `MontyRepl::new(script_name, tracker)` arg order and that `feed_run` takes `&mut self` — both per the digest; confirm in `repl.rs`.
  - ✅ Done when: `cargo test -p droplet-core` passes the smoke test. The interpreter works end-to-end.

- [ ] Prove **state persists across chunks** (the per-step REPL model):
  ```rust
  #[test]
  fn repl_state_persists() {
      let mut r = MontyRepl::new("session.py", NoLimitTracker);
      r.feed_run("x = 10", vec![], PrintWriter::Stdout).unwrap();
      r.feed_run("y = 20", vec![], PrintWriter::Stdout).unwrap();
      let v = r.feed_run("x + y", vec![], PrintWriter::Stdout).unwrap();
      assert_eq!(v, MontyObject::Int(30));
  }
  ```
  - 🔗 Maps to: each `run_code(code)` step feeds the same REPL; variables defined in one step are visible in the next. This is why a session is *durable but ephemeral*.
  - ✅ Done when: the test passes.

- [ ] Switch to the **suspend/resume loop** with `feed_start`, handling a fake external function. Feed Python that calls an undefined function (e.g. `host_get(5)`); in the `ReplProgress::FunctionCall` arm, return a hardcoded `MontyObject`, and `resume`. Cover the other arms with safe defaults so nothing panics:
  ```rust
  use monty::ReplProgress;

  let mut progress = repl.feed_start("host_get(5)", vec![], PrintWriter::Stdout)?;
  let value = loop {
      match progress {
          ReplProgress::Complete { repl: r, value } => { repl = r; break value; }
          ReplProgress::FunctionCall(call) => {
              let reply: monty::ExtFunctionResult = match call.function_name.as_str() {
                  "host_get" => MontyObject::Int(123).into(),
                  other => unimplemented!("unknown extern fn: {other}"),
              };
              progress = call.resume(reply, PrintWriter::Stdout)?;
          }
          // Safe defaults for the other suspension kinds (fill in only if a test needs them):
          ReplProgress::OsCall(c)         => { progress = c.resume(MontyObject::None.into(), PrintWriter::Stdout)?; }
          ReplProgress::NameLookup(l)     => { progress = l.resume(monty::NameLookupResult::Undefined, PrintWriter::Stdout)?; }
          ReplProgress::ResolveFutures(f) => {
              let r: Vec<(u32, monty::ExtFunctionResult)> =
                  f.pending_call_ids().iter().map(|&id| (id, MontyObject::None.into())).collect();
              progress = f.resume(r, PrintWriter::Stdout)?;
          }
      }
  };
  ```
  - 🆕 Concept: external host functions are **not** registered closures (the Rust side has no register-functions API; the Python `external_functions=` dict is a convenience in `monty-python` only). Execution *pauses* and hands you a `ReplProgress` state machine; you `match` it in a `loop`. `Complete { repl, value }` ends the run *and hands the REPL back* (because `feed_start` consumes `self`); `FunctionCall(call)` asks you to compute, then `call.resume(reply, …)` continues. This is the seam every tool (`run_sql`, `search_fields`, `export`) plugs into in M4. (Rust Book: The `match` Control Flow Construct; Repetition with Loops)
  - 🆕 Concept: `MontyObject::Int(123).into()` builds an `ExtFunctionResult` (the value handed back to the sandbox) via a `From` impl. (verify the `From` impls and whether a tool can *raise* a sandbox exception — Droplet needs SQL errors to surface as catchable exceptions later.)
  - ⚠️ Invariant #4: the sandbox sees only the function name + `MontyObject` args; engine objects/handles stay entirely in your match arm, host-side. This dispatch *is* the boundary.
  - verify: the full `ReplProgress` variant set and that `feed_start` consumes `self` and returns the REPL inside `Complete` — both per the digest; confirm against `run_progress.rs`/`repl.rs` at the tag. The `NameLookup`/`ResolveFutures` resume shapes especially.
  - ✅ Done when: a test runs Python calling `host_get(5)` and gets `123` back through the loop; `cargo test -p droplet-core` is green.

- [ ] **Call one host function over shared `Session` state — the literal M0 goal.** Add a host counter to (or reachable from) the session, dispatch a `host_add(n)` external function that **mutates** it, and return the new total. The smallest version mutates a `&mut i64` you thread into the loop; the real version mutates state inside the `Session`:
  ```rust
  // In the FunctionCall arm, alongside "host_get":
  // "host_add" => {
  //     if let Some(MontyObject::Int(n)) = call.args.first() { *counter += *n; }
  //     MontyObject::Int(*counter).into()
  // }
  ```
  - 🆕 Concept: passing `&mut` host state into the loop lets the host function read and *mutate* state the sandbox can never touch directly — the same pattern the handle registry generalizes. The sandbox sends a name + args; the host mutates real state behind the seam. (Rust Book: References and Borrowing)
  - 🆕 Concept: `call.args` is a `Vec<MontyObject>`; `if let Some(MontyObject::Int(n)) = call.args.first()` reads the first arg only when it's an `Int`. (verify the field name `args`/`kwargs` shape in source.)
  - ⚠️ Invariant #4 + invariant #9: the shared state (a counter now; the DuckDB connection, registry, and stores later) lives host-side in the `Session`; the sandbox influences it only through the explicit named call.
  - ✅ Done when: a test calls `host_add(5)` then `host_add(7)` and asserts the returned total is `12` — proving "call one host function over shared session state." **This is the M0 "Done when" finish line for the Rust side.**

- [ ] Fold Monty's error into `DropletError` (invariant #10). Uncomment/add the `Monty(#[from] monty::MontyException)` variant, change your run helper to return `Result<MontyObject, DropletError>`, and replace `.unwrap()` with `?`.
  - ⚠️ Invariant #10: "all engine errors fold into DropletError." Monty is now folded in alongside `Io` and `NotFound`.
  - verify: the exact type name `MontyException` (vs any rename on your tag) before adding the `#[from]`.
  - ✅ Done when: the helper signature is `Result<MontyObject, DropletError>` and tests still pass.

- [ ] (Optional, easy plumbing check for M7) Confirm `dump`/`load` exist on the REPL at your tag: `let bytes = repl.dump()?;` then `let mut repl2 = MontyRepl::load(&bytes)?;`, feed a follow-up chunk on `repl2`, and assert it sees prior state.
  - 🆕 Concept: Monty serializes REPL state via `postcard` (compact binary). Full snapshot/resume — zstd + content-addressed blob + a version-tagged manifest — is **M7**; this step just confirms `dump`/`load` work on `v0.0.18` so M7 isn't a surprise.
  - verify: return types (`dump() -> Result<Vec<u8>, postcard::Error>` per the digest) and that the postcard format is **not** portable across monty versions — M7's manifest must record the monty tag and refuse cross-version loads (invariant #3).
  - ⚠️ Invariant #3: "Snapshot = REPL bytes + manifest only; never serialize engine heaps." This dump is the REPL-bytes half of that.
  - ✅ Done when: the round-trip test passes (or you've confirmed and noted the exact `dump`/`load` signatures if they differ).

---

### Chunk I — CI (fmt + clippy + build + test) and the xtask/anyhow split

Lock in quality with a minimal GitHub Actions workflow, and add the `xtask` binary that makes the "libraries use `thiserror`, binaries use `anyhow`" rule concrete (invariant #10).

- [ ] Add an `xtask` binary crate at the repo root: `cargo new --bin xtask`, then add `"xtask"` to the root `members`. (Per PRODUCT.md §9, `xtask/` sits at the repo root, not under `crates/`.)
  - 🆕 Concept: `--bin` makes an *executable* crate (`main.rs` with a `fn main`) — the opposite of the `--lib` crates you've built so far. (Rust Book: Packages and Crates)
  - ✅ Done when: `cargo metadata --no-deps` lists `xtask`.

- [ ] Add `anyhow` to the root `[workspace.dependencies]` (declared now, used **only** by binaries):
  ```toml
  anyhow = "1.0.102"
  ```
  - 🆕 Concept: `anyhow::Result` is type-erased error handling for **binaries** — no typed enum needed at the top of a program. Libraries use `thiserror`. (Rust Book: Error Handling)
  - ✅ Done when: the line is present.

- [ ] Make `xtask` depend on `anyhow` and give its `main` an `anyhow::Result<()>` return:
  ```toml
  [package]
  name    = "xtask"
  edition.workspace    = true
  version.workspace    = true
  repository.workspace = true

  [dependencies]
  anyhow.workspace = true
  ```
  ```rust
  fn main() -> anyhow::Result<()> {
      println!("xtask: nothing to do yet");
      Ok(())
  }
  ```
  - 🆕 Concept: `fn main() -> anyhow::Result<()>` lets a binary use `?` on any error and exit non-zero on failure. (Rust Book: Error Handling)
  - ⚠️ Invariant #10: "thiserror in libraries, anyhow at binaries." `xtask` is a binary → it gets `anyhow`; `droplet-core` is a library → it never does. This crate exists to make that boundary concrete.
  - ✅ Done when: `cargo build -p xtask` is green and `main` returns `anyhow::Result<()>`.

- [ ] Confirm `Cargo.lock` is committed. A workspace producing binaries (the `xtask` bin, the `droplet-py` cdylib) and pinning a **git** dependency (`monty`) must commit its lockfile for reproducible builds.
  - 🆕 Concept: `Cargo.lock` records the exact resolved version of every dependency (including the `monty` git rev). Commit it for apps/binaries. (Rust Book: Ensuring Reproducible Builds with the `Cargo.lock` File)
  - ⚠️ Reminder: avoid `cargo update` on the `monty`/`ruff`/`ty`/`salsa` tree — a bump can break the API and (later) the snapshot format. Pin one monty tag fleet-wide.
  - ✅ Done when: `git status` shows `Cargo.lock` is tracked, not ignored.

- [ ] Run the four checks locally and fix anything they flag:
  ```bash
  cargo fmt --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo build --workspace
  cargo test --workspace
  ```
  - 🆕 Concept: `cargo fmt --check` fails if code isn't formatted (run plain `cargo fmt` to fix); `clippy … -- -D warnings` turns lints into hard errors; `--workspace` runs across every member. (Rust Book: Appendix D; Cargo Workspaces)
  - ✅ Done when: all four are clean/green. Fix anything red before writing the workflow.

- [ ] Create `.github/workflows/ci.yml` running the same four steps on push/PR:
  ```yaml
  name: CI
  on: [push, pull_request]
  jobs:
    check:
      runs-on: ubuntu-latest
      steps:
        - uses: actions/checkout@v4
        - uses: dtolnay/rust-toolchain@stable
          with:
            components: rustfmt, clippy
        - run: cargo fmt --check
        - run: cargo clippy --workspace --all-targets -- -D warnings
        - run: cargo build --workspace
        - run: cargo test --workspace
  ```
  - 🆕 Concept: GitHub Actions runs these checks on every push/PR in a clean Linux VM — catching "works on my machine" issues. (GitHub Actions docs.)
  - Note: your `rust-toolchain.toml` pins `1.96.0`, so the runner uses it automatically when it reads the file. Expect the first CI run to be **slow** — it compiles `monty` + the bundled `ty`/`ruff`/`salsa` tree from scratch (cache the cargo registry/target later if it hurts). A separate `maturin build` job can come in M8.
  - ✅ Done when: you push a branch and the CI job goes green on GitHub.

---

## M0 acceptance checklist — "Done when"

Tick all of these to call M0 complete (this is the spec's BUILD ORDER step 1, expanded):

- [ ] `cargo build --workspace` is **green** (root virtual workspace; `droplet-core`, `droplet-py`, `xtask` members; `droplet-warmup` removed or noted for removal).
- [ ] `cargo test --workspace` is **green** — `DropletError`, the generic `Registry`, the `Session` (create + `Drop` wipes the work dir), and the four store dev impls (`MemArtifactStore`/`MemSnapshotStore`/`MemCoordinationStore`/`LocalFsSource` round-trips + the dev lease) all pass.
- [ ] `cargo fmt --check` and `cargo clippy --workspace --all-targets -- -D warnings` are **clean**.
- [ ] `maturin develop` installs the `_droplet` module, and `python -c "from droplet._droplet import add; print(add(2,3))"` prints **`5`** (invariant #1: pyo3 only in `droplet-py`).
- [ ] The **four store traits** (`Source`, `ArtifactStore`, `SnapshotStore`, `CoordinationStore`) exist with in-memory/local dev impls, each held by a `Session` behind `Box<dyn …>` (invariant #8 — the distributed seams).
- [ ] A trivial **Monty** run inside `droplet-core` calls **one host function over shared session state** (e.g. `host_add` mutating a counter) and returns the expected total — entirely pure Rust, **no pyo3 in `droplet-core`** (invariants #1, #4, #9).
- [ ] All engine/interpreter errors so far (`io::Error`, `MontyException`) fold into the single `DropletError` (invariant #10), with comment-stubs reserving the future `#[from]` variants (DuckDB/Surreal/S3/Redis/DynamoDB).
- [ ] `Cargo.lock` is committed (it pins the `monty` git rev), and CI runs fmt + clippy + build + test on push/PR and is green.

> When all boxes are ticked you have a working skeleton: a virtual workspace, a pure-Rust core with one boundary error type, a generic handle registry, a per-run `Session` with a wiped working dir, the four pluggable store seams with dev backends, a Python wheel, and a live Monty host-function seam over shared state. **Next:** [`M1-duckdb.md`](./M1-duckdb.md) — plug the DuckDB engine in (`run_sql`, S3 via httpfs, capped Arrow results, `spawn_blocking`, GIL release).
