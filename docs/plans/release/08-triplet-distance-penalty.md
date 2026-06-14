# 08 — Fix triplet distance penalty default

> Wave 2 · Priority P0 · Track A · Release-blocking: yes · Effort: 0.25d ·
> Depends on: — · Source: [cleanup-and-parity-audit.md](../cleanup-and-parity-audit.md) B3.1, [release-readiness-plan.md](../release-readiness-plan.md) T8.1

[← back to index](00-INDEX.md)

## Goal

Change Rust's `DEFAULT_TRIPLET_DISTANCE_PENALTY` from `3.5` to `6.5` so it matches
the Python cognee default, and fix the two false doc comments that claim the Rust
value of `3.5` "matches Python." Add a unit test that pins the constant to `6.5` so
the value can never silently drift again. This penalty is applied to every graph
element that has **no vector match** during a default graph-completion search, so the
wrong value re-ranks triplets on *every* default search and degrades answer parity.

## Background & why

`brute_force_triplet_search` scores graph triplets by combining the cosine distances
of the source node, the edge, and the target node. When a node or edge has no vector
hit for the current query, it is assigned a fixed "penalty" distance instead. A
**larger** penalty pushes unmatched elements further down the ranking. Rust currently
uses `3.5`; Python uses `6.5`. With the smaller penalty, Rust keeps poorly-matched
triplets higher in the ranked context than Python would — divergent LLM input,
divergent answers.

The Rust constant and its `GraphRetrievalConfig::triplet_distance_penalty` default
are both `3.5`, and **both** carry a comment asserting parity with Python's `3.5`,
which is simply wrong (Python is `6.5`). This is a one-value correctness fix plus
comment cleanup.

### Python vs Rust

| | Value | Source |
|---|---|---|
| **Python** | `6.5` | `modules/retrieval/utils/brute_force_triplet_search.py:56,227` (and every graph retriever) |
| **Rust (now)** | `3.5` | `crates/search/src/graph_retrieval/brute_force_triplet_search.rs:16` |
| **Rust (target)** | `6.5` | same file |

## Prerequisites

```bash
git checkout main && git pull
git checkout -b task/08-triplet-distance-penalty
```

Read first:
- Rust: [crates/search/src/graph_retrieval/brute_force_triplet_search.rs](../../../crates/search/src/graph_retrieval/brute_force_triplet_search.rs) lines 13–60.
- Python: `/tmp/cognee-python/cognee/modules/retrieval/utils/brute_force_triplet_search.py` lines 56 and 227.

If `/tmp/cognee-python` is missing:
```bash
git clone --depth 1 https://github.com/topoteretes/cognee.git /tmp/cognee-python
```

## Files to change

| Path | Change |
|---|---|
| `crates/search/src/graph_retrieval/brute_force_triplet_search.rs` | Constant `3.5` → `6.5`; fix both false "matches Python's 3.5" comments; add a unit test pinning the constant. |

## Python reference

`/tmp/cognee-python/cognee/modules/retrieval/utils/brute_force_triplet_search.py`

```python
# line 56 — the public brute_force_triplet_search signature default
    triplet_distance_penalty: Optional[float] = 6.5,
...
# line 227 — the brute_force_search wrapper default
    triplet_distance_penalty: Optional[float] = 6.5,
```

Every Python graph retriever inherits this `6.5` default and threads it straight into
`brute_force_triplet_search` (re-verify with the grep in the Verification section):

| Retriever | line |
|---|---|
| `graph_completion_retriever.py` | `52` |
| `graph_completion_cot_retriever.py` | `66` |
| `graph_completion_context_extension_retriever.py` | `30` |
| `graph_summary_completion_retriever.py` | `30` |
| `graph_completion_decomposition_retriever.py` | `42` |
| `temporal_retriever.py` | `45` |

**Behavior to match:** the default distance assigned to a graph element with no vector
match must be `6.5`, identical to Python, so triplet ranking on a default
graph-completion search is byte-for-byte comparable across SDKs.

## Implementation steps

1. **Re-confirm the current Rust value and line numbers** (the audit cites 2026-06-14):
   ```bash
   grep -n "DEFAULT_TRIPLET_DISTANCE_PENALTY\|3.5\|matches Python" \
     crates/search/src/graph_retrieval/brute_force_triplet_search.rs
   ```
   Expect the constant declaration near line 16 and a doc comment referencing `3.5`.

2. **Fix the constant and its doc comment.** In
   `crates/search/src/graph_retrieval/brute_force_triplet_search.rs`:

   **Before** (lines ~13–16):
   ```rust
   /// Default cosine distance assigned to graph elements (nodes or edges) that have no
   /// vector match for the current query. Matches Python's `triplet_distance_penalty` default
   /// of 3.5 in `brute_force_triplet_search.py`.
   pub const DEFAULT_TRIPLET_DISTANCE_PENALTY: f32 = 3.5;
   ```

   **After:**
   ```rust
   /// Default cosine distance assigned to graph elements (nodes or edges) that have no
   /// vector match for the current query. Matches Python's `triplet_distance_penalty`
   /// default of 6.5 in
   /// `cognee/modules/retrieval/utils/brute_force_triplet_search.py:56,227`.
   pub const DEFAULT_TRIPLET_DISTANCE_PENALTY: f32 = 6.5;
   ```

3. **Fix the second false comment** on the `GraphRetrievalConfig` field (the field uses
   the constant, so no value change is needed — only the comment).

   **Before** (lines ~36–38):
   ```rust
       /// Default cosine distance used for nodes/edges not found in vector search.
       /// Matches Python's `triplet_distance_penalty` semantics (default 3.5).
       pub triplet_distance_penalty: f32,
   ```

   **After:**
   ```rust
       /// Default cosine distance used for nodes/edges not found in vector search.
       /// Matches Python's `triplet_distance_penalty` semantics (default 6.5).
       pub triplet_distance_penalty: f32,
   ```

4. **Add a regression test** that pins the constant and the config default. Append to
   the existing `#[cfg(test)] mod tests` block in the same file (or create one at the
   bottom if none exists):

   ```rust
   #[cfg(test)]
   mod penalty_default_tests {
       use super::*;

       #[test]
       fn default_triplet_distance_penalty_matches_python() {
           // Python: cognee/modules/retrieval/utils/brute_force_triplet_search.py:56,227
           assert_eq!(DEFAULT_TRIPLET_DISTANCE_PENALTY, 6.5);
       }

       #[test]
       fn graph_retrieval_config_default_uses_python_penalty() {
           let cfg = GraphRetrievalConfig::default();
           assert_eq!(cfg.triplet_distance_penalty, 6.5);
       }
   }
   ```

   > Note: if a `mod tests` already exists in this file, add the two `#[test]` fns into
   > it instead of a second module to avoid a duplicate-name collision.

## Verification

```bash
# 1. Re-confirm Python is 6.5 on both lines (sanity check the source of truth).
grep -n "triplet_distance_penalty: Optional\[float\] = " \
  /tmp/cognee-python/cognee/modules/retrieval/utils/brute_force_triplet_search.py
# Expected: lines 56 and 227 both show `= 6.5,`

# 2. No stray 3.5 / false comment remains in the Rust file.
grep -n "3.5\|matches Python's .*3.5" \
  crates/search/src/graph_retrieval/brute_force_triplet_search.rs
# Expected: no matches.

# 3. Build + run the new tests.
cargo test -p cognee-search default_triplet_distance_penalty_matches_python
cargo test -p cognee-search graph_retrieval_config_default_uses_python_penalty
# Expected: both pass.

# 4. Full crate test + gate.
cargo test -p cognee-search
scripts/check_all.sh
```

Expected outcome: the constant is `6.5`, both comments reference `6.5` with the exact
Python file:line, and the two new tests pass.

## Acceptance criteria

- [ ] `DEFAULT_TRIPLET_DISTANCE_PENALTY == 6.5` in
      `crates/search/src/graph_retrieval/brute_force_triplet_search.rs`.
- [ ] Both doc comments (the constant and the `GraphRetrievalConfig` field) say `6.5`
      and cite the Python file:line; no `3.5` remains in the file.
- [ ] `GraphRetrievalConfig::default().triplet_distance_penalty == 6.5` (it already
      derives from the constant — verify, do not duplicate the literal).
- [ ] Two new unit tests pass and assert the value against the documented Python source.
- [ ] `cargo test -p cognee-search` and `scripts/check_all.sh` pass.

## Gotchas / do-not

- **Single source of truth.** `GraphRetrievalConfig::default()` already assigns
  `triplet_distance_penalty: DEFAULT_TRIPLET_DISTANCE_PENALTY`. Do **not** hardcode
  `6.5` in the default impl — change only the constant so there is one place to edit.
- **Cross-SDK determinism.** This value directly changes ranked-context ordering on
  default graph-completion searches. It is a *parity correction*, not a tuning change —
  do not "split the difference" or pick a different number; match Python exactly.
- **Do not** touch the `SEARCH_COLLECTIONS` list in this task — the missing
  `Triplet_text` collection (audit B3.2) is handled separately in the Tier-2 backlog
  (task index entry for B3.2/B4.1), not here.
- `f32` literal: `6.5` is exactly representable, so there is no float-comparison
  flakiness in the `assert_eq!`.

## Rollback

Single-file change. Revert with:
```bash
git checkout main -- crates/search/src/graph_retrieval/brute_force_triplet_search.rs
```
or drop the branch entirely (`git branch -D task/08-triplet-distance-penalty`).
