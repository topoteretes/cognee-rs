# Task 37: FeelingLucky self-referencing guard

**Priority:** P3 (low)
**Status:** Already implemented in Rust

## Summary

The FeelingLucky search type uses an LLM to select the best retriever for a given query. A self-referencing guard prevents it from selecting itself (infinite recursion). This is already correctly implemented in Rust.

## Current Rust Implementation

In `crates/search/src/retrievers/lucky_feedback_rules_retrievers.rs`, the `FeelingLuckyRetriever::select_retriever` method:

1. **Excludes FeelingLucky from allowed types:** When building the list of allowed search types for the LLM prompt, it filters out `SearchType::FeelingLucky`:

```rust
let allowed_types = self
    .retrievers
    .keys()
    .copied()
    .filter(|search_type| *search_type != SearchType::FeelingLucky)
    .map(|search_type| format!("{:?}", search_type).to_ascii_uppercase())
    .collect::<Vec<_>>()
    .join(", ");
```

2. **Rejects FeelingLucky if selected anyway:** After parsing the LLM response, it filters out FeelingLucky as a selected type:

```rust
let selected_type = response
    .ok()
    .and_then(|completion| Self::parse_search_type(completion.content.as_str()))
    .filter(|search_type| *search_type != SearchType::FeelingLucky);
```

3. **Falls back to a safe default:** If the LLM returns an invalid or self-referencing type, the fallback retriever (default: `RagCompletion`) is used:

```rust
match selected_type.and_then(|search_type| self.retrievers.get(&search_type).cloned()) {
    Some(retriever) => Ok(retriever),
    None => self.fallback_retriever(),
}
```

## Test Coverage

The existing test `feeling_lucky_falls_back_on_invalid_selection` in the same file verifies that when the LLM returns an invalid type string (`"NOT_A_REAL_TYPE"`), the retriever correctly falls back to `RagCompletion`.

## No Changes Required

This task is complete. The guard has two layers of protection:
1. FeelingLucky is excluded from the prompt's allowed types list.
2. FeelingLucky is explicitly filtered from the parsed LLM response.

Both ensure the retriever can never select itself, preventing infinite recursion.
