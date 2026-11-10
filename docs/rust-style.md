# Rust style

These are the repo's Rust rules. Each rule has a number, so a gate refusal or a review finding can cite it. The rules come from four rust-skills files: `m05-type-driven`, `m06-error-handling`, `m15-anti-pattern` and `coding-guidelines`. This page carries the distilled rules in full. You do not need the plugin to read them.

Much of the Rust here began as a port of Python. The ports kept JSON shapes for byte parity, so rows travel as `serde_json::Value` and readers pick fields by string key. A misspelled key compiles and reads `None`. Most of these rules exist to end that shape. A shrink-only gate holds the count of the worst two patterns, so new code cannot add to them.

## Types

### R1. Parse at the boundary

JSON from a socket, a file or a subprocess deserializes once into a `#[derive(Deserialize)]` struct. No code past that boundary reads `.get("field")` on a `serde_json::Value`. Why: the compiler checks a field name, and it never checks a string key.

```rust
// before: a typo here compiles and reads None
let parent = row.get("parnet").and_then(Value::as_str);
// after: crates/fno/src/node_row.rs
let parent = row.parent();
```

Some boundaries must accept every row, a legacy one too. Such a boundary still gets a struct. Make each field an `Option`, and keep the keys the struct does not name in a map, as `NodeRow::extra` does. The typed read stays total, and the row round-trips. A pass that must also write the row in place reads it through a borrowed view instead. `crates/fno-agents/src/backlog/row_facts.rs` is the specimen: the graph store's derivations parse each row once into `RowFacts` and write their derived keys back on the row.

### R2. A closed vocabulary is an enum

A status, a column or a kind is an `enum`, not a `&str` or a `String`. If a reader must stay total over words it does not know, give the enum an `Other(String)` arm. Why: a `match` on an enum with a missing case fails to compile, and a string compare never fails.

```rust
// before
if card.column == "Done" { ... }
// after
if card.column == KanbanColumn::Done { ... }
```

### R3. A newtype guards a real mix-up

Two values can share a primitive type and still never be interchangeable. A node id and a session id are both strings. If mixing two such values is a real bug, wrap each in a newtype with a checking constructor. Why: the type system then refuses the swap.

### R4. No boolean flags or Option stand-ins for states

A value with three or more states gets an enum. A flag that changes what the other fields mean gets an enum too. Why: `(bool, bool)` admits a combination that has no meaning.

### R5. A field with an invariant is private

If a field must satisfy a rule, keep it private behind a constructor that checks the rule. Make a field public only for a type where every value is valid. Why: a public field lets any caller break the rule.

## Errors

### R6. An expected failure returns a typed error

A public seam that can fail returns `Result<T, E>`, where `E` is an enum of the ways it fails. Use `thiserror` where the crate already depends on it. Do not add a new public `Result<_, String>`. A normal absence is an `Option`, not an error. A bug is a `panic!`, an `assert!` or an `unreachable!` with a message. Why: a caller can match an enum variant, and a caller can only guess from a string.

```rust
// before
pub fn rows(graph: &Path) -> Result<Vec<NodeRow>, String>
// after: crates/fno/src/store_client.rs
pub fn rows(graph: &Path) -> Result<Vec<NodeRow>, StoreClientError>
```

### R7. Propagate with ? and carry context in fields

Put the context a reader needs in the error variant's fields, not in a formatted string. Why: a field survives to the caller, and a string only survives to a log.

### R8. No unwrap in product code

Use `.expect("why this holds")` for a true invariant. Tests can unwrap. Why: an unwrap panics with no reason, and the reason is the thing the next reader needs.

### R9. Never drop an error in silence

A `let _ =` on a `Result` carries a comment. So does a `.ok()` that discards an error. The comment names why the failure is safe to ignore. Why: a dropped error is the bug report nobody gets.

## Anti-patterns

### R10. No clone to escape the borrow checker

Borrow, or change who owns the data. Why: a clone that exists only to compile hides a design problem and costs an allocation.

### R11. Take &str for text you only read

A function that only reads text takes `&str`. A function that sometimes builds new text takes `Cow<str>`. Why: the caller then chooses the allocation.

### R12. Iterators over index loops

Chain iterator adapters. Do not `collect` into a `Vec` only to iterate it again. Why: the chain has no bounds checks and no throwaway allocation.

### R13. Use the standard library's lazy statics

Use `std::sync::OnceLock` and `LazyLock`. Do not add `lazy_static!` or `once_cell`. Why: the standard library now does this, with no dependency.

### R14. No unsafe without a SAFETY comment

Every `unsafe` block carries a `// SAFETY:` comment. The comment names the invariant that makes the block sound. Why: a reviewer must check the invariant, and the comment is where they find it.

### R15. Split a long function by its questions

A function past about 50 lines gets split. So does a `match` arm that grows into a function of its own. Name each part by the question it answers. Why: one question per function is one thing to test.

## Naming and tools

### R16. Follow the standard names

Do not prefix a getter with `get_`. Use `as_` for a cheap borrow, `to_` for a costly conversion, and `into_` for one that consumes. Iterators are `iter`, `iter_mut` and `into_iter`. Why: a Rust reader knows what each prefix costs before reading the body.

### R17. cargo fmt and cargo clippy are the tools of record

Run `cargo fmt` and `cargo clippy -- -D warnings`. Why: a formatter ends style debates, and a lint that warns is a lint nobody reads.

## Dropped from the sources

The source skills ask for three more things that this repo does not adopt. They are named here, so nobody adds them back by accident.

- A `G_` prefix for statics. The repo uses `SCREAMING_SNAKE_CASE` alone.
- A blanket swap from `std::sync::Mutex` to `parking_lot::Mutex`. That is a performance call, and no measurement here asks for it.
- A blanket swap from `std::sync::mpsc` to `crossbeam::channel`. The same reason applies.

## Enforced by

| Rule | Enforced by |
|---|---|
| R1 | `scripts/ci/check-value-reads.sh`: per crate, `.get("field")` reads are shrink-only against the merge base |
| R6 | `scripts/ci/check-value-reads.sh`: per crate, `Result<_, String>` is shrink-only against the merge base |
| R17 | CI `cargo fmt --check` and `cargo clippy` |
| the rest | review |

A gate refusal names the crate, the pattern, the two counts and the rule. The remedy is in the rule above. The gate has no number to raise.
