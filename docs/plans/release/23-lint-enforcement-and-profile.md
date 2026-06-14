# 23 — Lint enforcement & release profile

> Wave 5 · Priority P1 (should-fix) · Track A · Release-blocking: no · Effort: 0.5d ·
> Depends on: [01 — Decisions (D4)](01-decisions.md), [03 — FFI & Neon panic safety](03-ffi-neon-panic-safety.md), [04 — Rust code cleanup](04-rust-code-cleanup.md) ·
> Source: [release-readiness-plan.md](../release-readiness-plan.md) T7.1, T7.2, T6.6/T6.7

[← Back to index](00-INDEX.md)

## Goal

Turn three reviewer-discipline conventions into compiler/CI-enforced rules, plus shrink
release binaries:

1. **Lint enforcement** — add `[workspace.lints.clippy]` to the root
   [Cargo.toml](../../../Cargo.toml) that **denies** `unwrap_used` and `expect_used`, and
   make every workspace member opt in with `lints.workspace = true`. This converts the
   flagship project rule ("no `unwrap()` in non-test code", see `.claude/CLAUDE.md` →
   *Coding Conventions*) from convention to a `cargo clippy` failure. Test code and the
   sanctioned `Mutex/RwLock::lock().unwrap()` lock-poison pattern are explicitly exempted.
2. **Release profile** — set `[profile.release] debug` per **decision D4** in
   [01-decisions.md](01-decisions.md) (current value is `debug = true`, which bloats the
   edge/Android binary 2–5×).
3. **Public-API docs** — add `#![warn(missing_docs)]` + a crate-level `//!` summary to the
   nine primary public crates so `cargo doc` surfaces undocumented public items.

This task changes **no runtime behavior** and is **parity-neutral** (no `.rs` logic, no
schema, IDs, hashes, prompts, or collection names). It only adds manifest tables, lint
attributes, and doc comments.

## Background & why

### Why depend on 03 and 04

Denying `unwrap_used` / `expect_used` will turn **every** offending call site into a clippy
error. If you enable the deny before the codebase is clean, `cargo clippy -- -D warnings`
(the CI gate in `scripts/check_all.sh`) goes red across the board.

- **[03 — FFI & Neon panic safety](03-ffi-neon-panic-safety.md)** removes the `unwrap()`s in
  the FFI/Neon boundary code.
- **[04 — Rust code cleanup](04-rust-code-cleanup.md)** removes/justifies the remaining
  non-test `unwrap()`s (converting to `expect("why")` or `?`).

So land **03 and 04 first**, then flip the lint on here. If sites still remain when you
start, use the **staged rollout** below (warn → deny) instead of blocking the release.

### The exemptions you must preserve (read the convention)

From `.claude/CLAUDE.md` → *Coding Conventions* (verbatim policy this lint encodes):

- `expect("reason why this can never panic at runtime")` is the **sanctioned** alternative
  to `unwrap()`. But Clippy's `expect_used` lint flags `expect()` too. We still want it
  denied in non-test code by default **except** where an invariant genuinely guarantees the
  value — those few sites get a local `#[allow(clippy::expect_used, reason = "...")]`.
- **`Mutex::lock().unwrap()` and `RwLock::read()/write().unwrap()` are explicitly allowed**
  because lock poisoning only happens if a thread already panicked and there is no
  meaningful recovery. The convention asks for a `// lock poison is unrecoverable` comment.
  These call sites will trip `unwrap_used` and must be individually exempted with
  `#[allow(clippy::unwrap_used, reason = "lock poison is unrecoverable")]`.

> There is **no** narrower clippy lint that says "unwrap only on a lock guard". The lint is
> all-or-nothing per call site, so lock-guard unwraps need a local `#[allow]`. The cleanup
> in task 04 should already have annotated these; this task only makes the annotations
> load-bearing.

### Current manifest state (verified 2026-06-14)

- The root [Cargo.toml](../../../Cargo.toml) has **no** `[workspace.lints]` table and **no**
  member crate uses `lints.workspace = true` (verified: `grep -rln 'lints.workspace' crates/
  Cargo.toml` → no output).
- `[profile.release]` is exactly (Cargo.toml lines 149–150):
  ```toml
  [profile.release]
  debug = true
  ```
- Crate-doc spot-check of the nine primary public crates (verified by reading the top of
  each `src/lib.rs`):

  | Crate (dir) | crate-level `//!` doc | already has `missing_docs` attr |
  |---|---|---|
  | `crates/lib` (cognee-lib) | ✅ yes | ❌ no |
  | `crates/core` (cognee-core) | ✅ yes | ❌ no |
  | `crates/graph` (cognee-graph) | ✅ yes | ❌ no |
  | `crates/vector` (cognee-vector) | ✅ yes | ❌ no |
  | `crates/models` (cognee-models) | ❌ **no** | ❌ no |
  | `crates/cognify` (cognee-cognify) | ❌ **no** | ❌ no |
  | `crates/search` (cognee-search) | ❌ **no** | ❌ no |
  | `crates/embedding` (cognee-embedding) | ❌ **no** | ❌ no |
  | `crates/database` (cognee-database) | ❌ **no** | ❌ no |

  So four already have a `//!` summary (just add the `#![warn(missing_docs)]`); five need
  both a `//!` and the attribute.

## Prerequisites — read first

```bash
git checkout -b task/23-lint-enforcement-and-profile

# Re-confirm current state (positions are the 2026-06-14 snapshot — re-grep!):
sed -n '149,151p' Cargo.toml                       # [profile.release] debug = true
grep -n 'workspace.lints\|lints.workspace' Cargo.toml   # expect: no output
grep -rln 'lints.workspace' crates/                # expect: no output (no crate opts in yet)

# How many non-test unwrap()/expect() sites remain (gauges deny vs staged rollout):
grep -rn '\.unwrap()'  crates/*/src python/src capi/cognee-capi/src js/cognee-neon/src 2>/dev/null | wc -l
grep -rn '\.expect('   crates/*/src python/src capi/cognee-capi/src js/cognee-neon/src 2>/dev/null | wc -l

# Read the convention this lint encodes:
sed -n '/unwrap() is forbidden/,/Follow existing patterns/p' .claude/CLAUDE.md

# Confirm 03 + 04 are merged (otherwise use staged rollout, Step 1 variant B):
git log --oneline | grep -iE 'ffi.*panic|neon.*panic|rust code cleanup' | head
```

**Read [01-decisions.md](01-decisions.md) → D4 and copy the chosen value.** The
recommendation is `line-tables-only`; do not invent a value — use what D4 records.

## Files to change

| Path | Change |
|---|---|
| `Cargo.toml` | add `[workspace.lints.clippy]` table; change `[profile.release] debug` per D4 |
| `crates/*/Cargo.toml` (×27) | add a `[lints]` table with `workspace = true` |
| `python/Cargo.toml` | add `[lints] workspace = true` (it is a workspace member) |
| `crates/{models,cognify,search,embedding,database}/src/lib.rs` | add `//!` doc + `#![warn(missing_docs)]` |
| `crates/{lib,core,graph,vector}/src/lib.rs` | add `#![warn(missing_docs)]` (doc already present) |

> Out of scope here: `capi/cognee-capi` and `js/cognee-neon` are **separate workspaces**
> (their own `[workspace]`), so `lints.workspace = true` cannot reference the root table.
> The deny applies to the root workspace's members. If you also want the lint there, add a
> standalone `[lints.clippy]` block to each — but that is optional for this task and those
> crates are `publish = false`.

## Implementation steps

### Step 1 — Add the workspace clippy lints table (root Cargo.toml)

Open [Cargo.toml](../../../Cargo.toml). After the `[workspace.dependencies]` block and
before `[patch.crates-io]` (anywhere at top level is fine; group it with the other
`[workspace.*]` tables for readability), add:

**Variant A — full deny (use this once tasks 03 + 04 are merged and `cargo clippy` is clean):**

```toml
# Project rule made enforceable: "no unwrap()/expect() in non-test code"
# (.claude/CLAUDE.md → Coding Conventions). Test modules re-allow these
# (see per-crate guidance). Sanctioned lock-guard unwraps carry a local
# #[allow(clippy::unwrap_used, reason = "lock poison is unrecoverable")].
[workspace.lints.clippy]
unwrap_used = "deny"
expect_used = "deny"
```

**Variant B — staged rollout (use this if many non-test sites still remain):**

```toml
[workspace.lints.clippy]
unwrap_used = "warn"   # TODO(task-23): bump to "deny" once all sites are cleaned
expect_used = "warn"   # TODO(task-23): bump to "deny" once all sites are cleaned
```

Run the count commands from Prerequisites. **Decision rule:** if both counts are 0
(excluding test modules and annotated lock unwraps), use **Variant A**. If non-trivial
counts remain and you cannot clean them in this PR, ship **Variant B** and open a
follow-up issue to flip to `deny`. Do **not** ship a `deny` that makes `check_all.sh` red.

> `priority` note: levels are strings (`"deny"`/`"warn"`/`"allow"`). If you later add a
> broad group lint (e.g. `[workspace.lints.clippy.all]`) alongside a specific override, use
> the table form with `level`/`priority`. For just these two specific lints, the simple
> `name = "level"` form above is correct and needs no `priority`.

### Step 2 — Opt every workspace member into the shared lints

Cargo does **not** apply `[workspace.lints]` automatically — each member must declare
`lints.workspace = true`. For **each** of the 27 `crates/*/Cargo.toml` **and**
`python/Cargo.toml`, add a `[lints]` table. Place it after `[package]` (top of file is
conventional). Example for `crates/models/Cargo.toml`:

Before:
```toml
[package]
name = "cognee-models"
version.workspace = true
edition.workspace = true

[dependencies]
...
```

After:
```toml
[package]
name = "cognee-models"
version.workspace = true
edition.workspace = true

[lints]
workspace = true

[dependencies]
...
```

Apply the identical `[lints]\nworkspace = true` block to all 27 crates and `python/`.

Verify every member opted in:
```bash
ls crates/*/Cargo.toml | wc -l                       # expect 27
grep -L 'lints.workspace = true' crates/*/Cargo.toml python/Cargo.toml
# expect: no output (every manifest matched). If a file is listed, it is missing the block.
```

> `test-utils`, `bench`, `cli`, `cloud` are also members — add the block to them too. The
> lint is harmless for `publish = false` crates and keeps the rule uniform. `cli`/`bench`
> are **binaries**, not libraries; the deny still applies to their non-test `src`. If a
> binary legitimately needs an `unwrap()` in `main` startup, annotate that site with
> `#[allow(clippy::unwrap_used, reason = "...")]` rather than dropping the crate from the
> lint set.

### Step 3 — Exempt test code

`unwrap()`/`expect()` are fine in tests. There are two scopes to handle:

1. **Inline `#[cfg(test)] mod tests`** in a `src/*.rs` file — add an allow at the top of the
   test module:
   ```rust
   #[cfg(test)]
   #[allow(clippy::unwrap_used, clippy::expect_used)]
   mod tests {
       // ... unwrap()/expect() freely ...
   }
   ```
2. **Integration tests** under `crates/*/tests/*.rs` and any `#[cfg(test)]`-only helper —
   add a crate-/file-level inner attribute at the **top of the test file**:
   ```rust
   #![allow(clippy::unwrap_used, clippy::expect_used)]
   ```

> Clippy lints `--all-targets` including `--tests`, so without these allows the deny would
> fire inside test code. Tasks 03/04 may already have added some of these; grep for them and
> only add where missing. Use `reason = "test code"` form if you prefer
> (`#[allow(clippy::unwrap_used, reason = "test code")]`) — both compile.

To find inline test modules that lack the allow:
```bash
grep -rln '#\[cfg(test)\]' crates/*/src | while read -r f; do
  grep -q 'allow(clippy::unwrap_used' "$f" || echo "needs allow: $f"
done
```

### Step 4 — Annotate the sanctioned lock-poison unwraps

These are the **only** non-test `unwrap()`s that stay. Per the convention, each
`Mutex::lock().unwrap()` / `RwLock::read().unwrap()` / `RwLock::write().unwrap()` gets a
local allow with a reason. Task 04 should have done this; verify and backfill:

```bash
# Lock-guard unwraps that are NOT yet annotated:
grep -rnE '\.(lock|read|write)\(\)\.unwrap\(\)' crates/*/src | grep -v 'allow(clippy::unwrap_used'
```

For each remaining hit, change e.g.:

Before:
```rust
let guard = self.inner.lock().unwrap();
```
After:
```rust
// lock poison is unrecoverable
#[allow(clippy::unwrap_used, reason = "lock poison is unrecoverable")]
let guard = self.inner.lock().unwrap();
```

> The `#[allow]` attribute on a `let` statement is valid Rust. If the unwrap is mid-
> expression (not a `let`), refactor to a `let` binding first, or put the allow on the
> enclosing function with a precise reason. Keep the existing `// lock poison is
> unrecoverable` comment the convention asks for.

### Step 5 — Set the release profile per D4

Open [Cargo.toml](../../../Cargo.toml) lines 149–150. Current:

```toml
[profile.release]
debug = true
```

Replace `debug = true` with the value recorded in **D4**. The three D4 options map to:

| D4 choice | Edit |
|---|---|
| `line-tables-only` (recommended) | `debug = "line-tables-only"` |
| `false` (smallest binary, no line info) | `debug = false` |
| keep `true` | leave as-is (no edit; note D4 in the PR) |

Recommended (D4 default) — after:

```toml
[profile.release]
# D4: line-tables-only keeps backtraces useful while avoiding the 2–5× binary
# bloat of full debuginfo on the edge/Android target. See docs/plans/release/01-decisions.md.
debug = "line-tables-only"
```

> Optional companion (only if D4 explicitly asks for it): `split-debuginfo = "packed"` to
> emit a separate symbol file. Do **not** add it unless D4 records it — it interacts with
> platform linkers and is out of scope by default. `debug = "line-tables-only"` alone is the
> recommended, low-risk change.

### Step 6 — Add `#![warn(missing_docs)]` + crate docs to the nine public crates

For the **four crates that already have a `//!` summary** (`lib`, `core`, `graph`,
`vector`), just add the warn attribute as the **first line** of `src/lib.rs`, above the
existing `//!` block:

```rust
#![warn(missing_docs)]
//! Unified public API for Cognee-Rust.
//! ...
```

For the **five crates missing a summary** (`models`, `cognify`, `search`, `embedding`,
`database`), add both the attribute and a one-paragraph `//!`:

```rust
#![warn(missing_docs)]
//! Core data types shared across the cognee-rust crates: `Data`, `Dataset`,
//! `DataInput`, `Document`, `DocumentChunk`, `Entity`, `KnowledgeGraph`, etc.
//! Pure data structures with no trait abstractions.
```

Suggested first-line summaries (lift from `.claude/CLAUDE.md` → *Crate Details*, keep them
factual):

| Crate | `//!` summary (first line) |
|---|---|
| models | Core data types shared across the cognee-rust crates (Data, Dataset, Document, …). |
| cognify | Knowledge-graph extraction pipeline (classify → chunk → extract → summarize → index) and the memify enrichment pipeline. |
| search | Unified search orchestration across 15 retrieval strategies (GraphCompletion, RagCompletion, Chunks, …). |
| embedding | Multi-provider text-embedding engine (ONNX, OpenAI-compatible, Ollama, Mock). |
| database | Relational metadata persistence (SeaORM/SQLite) for ingestion, search history, and deletion. |

> `#![warn(missing_docs)]` (not `deny`) is intentional: it surfaces undocumented public
> items in `cargo doc` / clippy output without breaking the build while docs are filled in.
> CI already runs `cargo doc --no-deps` (see `lib-tests` notes in `.claude/CLAUDE.md`), so
> the warnings become visible there. Do **not** use `deny(missing_docs)` for 0.1.0 — many
> public items are still undocumented and that would block the build.

> Place `#![warn(missing_docs)]` strictly **before** any `//!` doc and before any other
> inner attribute (`#![...]` must precede items). If a crate already has other inner
> attributes (e.g. `#![allow(...)]`), group it with them at the top.

## Verification

```bash
# 1. Manifest parses and the lints table is present.
grep -n 'workspace.lints.clippy' Cargo.toml
sed -n '/\[workspace.lints.clippy\]/,/^\[/p' Cargo.toml   # shows unwrap_used / expect_used

# 2. Every workspace member opted in.
grep -L 'lints.workspace = true' crates/*/Cargo.toml python/Cargo.toml   # expect: no output

# 3. Release profile updated per D4.
sed -n '/\[profile.release\]/,/^\[/p' Cargo.toml          # shows the D4 value

# 4. The lint actually fires / is clean. This is the real gate:
cargo clippy --all-targets -- -D warnings
#   Variant A (deny): expect SUCCESS (clean) — if it errors on unwrap_used/expect_used,
#     either 03/04 are not fully merged or a lock/test site is unannotated. Fix per Steps 3–4.
#   Variant B (warn): expect SUCCESS with `unwrap_used`/`expect_used` WARNINGS listed.

# 5. Build still works and docs warn (not error) on missing docs.
cargo check --all-targets
cargo doc --no-deps 2>&1 | grep -i 'missing documentation' | head   # warnings, build still succeeds

# 6. Full project gate (the canonical pre-push check).
scripts/check_all.sh        # fmt → check → clippy -D warnings → C/Python/JS binding checks

# 7. Confirm a deliberate violation is caught (smoke test, then revert):
#    add `let _ = Some(1).unwrap();` to a non-test fn in crates/models/src/lib.rs
cargo clippy -p cognee-models -- -D warnings   # Variant A: ERROR on unwrap_used. Then revert.
```

Expected outcomes:
- Variant A: `cargo clippy --all-targets -- -D warnings` is **green**; the smoke-test
  violation (#7) **fails** clippy. That proves enforcement is live.
- Variant B: clippy is green at `-D warnings` only because the two lints are `warn` (a
  warning is not denied unless its name is in `-D`); you will see the warnings in output.
  Track the flip-to-deny as a follow-up.
- `cargo doc --no-deps` succeeds; missing-doc messages appear as warnings.

### New checks added

- The `[workspace.lints.clippy]` deny is the new compiler-enforced guard for the no-unwrap
  rule (replaces reviewer discipline).
- `#![warn(missing_docs)]` on nine crates surfaces public-API doc gaps in `cargo doc`.

## Acceptance criteria

- [ ] Root `Cargo.toml` has `[workspace.lints.clippy]` with `unwrap_used` and `expect_used`
      (`deny` if clean, else `warn` with a TODO to flip).
- [ ] All 27 `crates/*/Cargo.toml` **and** `python/Cargo.toml` declare `[lints] workspace = true`.
- [ ] Inline `#[cfg(test)]` modules and `tests/*.rs` files carry
      `#[allow(clippy::unwrap_used, clippy::expect_used)]` (or file-level `#![allow(...)]`).
- [ ] Every sanctioned `Mutex/RwLock::lock().unwrap()` has a local
      `#[allow(clippy::unwrap_used, reason = "lock poison is unrecoverable")]` + comment.
- [ ] `[profile.release] debug` matches D4 (recommended `"line-tables-only"`).
- [ ] The nine primary crates (`lib, models, core, cognify, search, embedding, graph,
      vector, database`) each begin with `#![warn(missing_docs)]` and have a crate-level `//!`.
- [ ] `cargo clippy --all-targets -- -D warnings` passes; `cargo doc --no-deps` succeeds;
      `scripts/check_all.sh` is green.

## Gotchas / do-not

- **Do NOT enable `deny` before 03 + 04 land.** The deny turns every offending site into a
  build error and reddens `check_all.sh`. Confirm both are merged or use Variant B.
- **`[workspace.lints]` is not inherited automatically** — each member needs
  `lints.workspace = true`. Forgetting one silently leaves that crate unguarded (the
  `grep -L` check in Verification #2 catches it).
- **Clippy lints test targets too.** Without the test-module/file allows (Step 3), the deny
  fires inside tests. This is the most common cause of a "suddenly red" build.
- **Lock-guard unwraps are sanctioned but still tripped by the lint** — they need the local
  `#[allow]`. There is no narrower lint; do not try to globally re-allow `unwrap_used` (that
  defeats the purpose).
- **`missing_docs` must be `warn`, not `deny`, for 0.1.0** — many public items are
  undocumented; `deny` would block the build and `cargo doc`.
- **`#![warn(missing_docs)]` must be the first inner attribute**, before any `//!` and
  before any item. Misplacement is a compile error (`inner attribute not permitted here`).
- **capi / neon are separate workspaces** — `lints.workspace = true` referencing the root
  table will fail there. Leave them out (they are `publish = false`) or give them a
  standalone `[lints.clippy]` block; do not point them at the root workspace.
- **Parity-neutral:** no `.rs` logic, schema, IDs, hashes, prompts, or collection names
  change. Only attributes, doc comments, and manifest tables. Keep it that way.
- **`split-debuginfo` is not in scope** unless D4 records it — `debug = "line-tables-only"`
  alone is the recommended change; the split flag is platform-linker-sensitive.

## Rollback

All changes are additive (lints table, `[lints]` blocks, `#[allow]`/`//!`/`#![warn]`
attributes) plus a one-token profile edit. Revert with `git checkout -- Cargo.toml
crates/ python/ && git checkout -- <the nine lib.rs files>`. To partially back out only the
deny while keeping the metadata, switch the two lint levels to `"allow"`. No data, schema,
or behavior impact.
