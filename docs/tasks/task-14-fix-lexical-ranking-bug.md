# Task 14: Fix Lexical retriever ranking bug -- Scores must always be computed internally regardless of `with_scores` flag

## Summary

The Rust `LexicalRetriever::get_context()` conditionally sets `SearchItem.score` to `None` when `with_scores` is `false`. The subsequent sort uses `unwrap_or_default()` (which yields `0.0`) for the `None` scores, making all items compare as equal. This means ranking is lost when `with_scores` is false -- the top-k items are effectively random rather than the highest-scoring ones. The Python implementation always computes and sorts by score internally; the `with_scores` flag only controls whether scores appear in the **output**.

## Current Rust Behavior

**File:** `crates/search/src/retrievers/lexical_retriever.rs`, lines 188-214

```rust
let mut items_with_score = chunks
    .into_iter()
    .filter_map(|(id, payload, text)| {
        let tokens = self.tokenize(&text);
        if tokens.is_empty() {
            return None;
        }

        let score = self.score(&query_tokens, &tokens);
        Some(SearchItem {
            id,
            score: if self.with_scores { Some(score) } else { None },  // BUG: score is None when with_scores=false
            payload,
        })
    })
    .collect::<Vec<_>>();

items_with_score.sort_by(|left, right| {
    right
        .score
        .unwrap_or_default()                                            // BUG: yields 0.0 when score is None
        .partial_cmp(&left.score.unwrap_or_default())                   // all items compare as 0.0 == 0.0
        .unwrap_or(std::cmp::Ordering::Equal)
});
items_with_score.truncate(self.top_k);
```

**The problem in detail:**

1. On line 199, when `self.with_scores` is `false`, the score is set to `None`.
2. On lines 206-210, `sort_by` uses `unwrap_or_default()` which gives `0.0` for all `None` scores.
3. Since all scores compare as `0.0`, the sort is a no-op. The `truncate(self.top_k)` then takes whichever items happened to be first in the original graph traversal order -- not the best matches.

## Required Behavior (Python Reference)

**File:** `/tmp/cognee-python/cognee/modules/retrieval/lexical_retriever.py`, lines 94-117

```python
results = []
for chunk_id, chunk_tokens in self.chunks.items():
    try:
        score = self.scorer(query_tokens, chunk_tokens)
        if not isinstance(score, (int, float)):
            logger.warning("Non-numeric score for chunk %s -> treated as 0.0", chunk_id)
            score = 0.0
    except Exception as e:
        logger.error("Scorer failed for chunk %s: %s", chunk_id, str(e))
        score = 0.0
    results.append((chunk_id, score))

top_results = nlargest(self.top_k, results, key=lambda x: x[1])    # always sorts by score

if self.with_scores:
    return [(self.payloads[chunk_id], score) for chunk_id, score in top_results]
else:
    return [self.payloads[chunk_id] for chunk_id, _ in top_results]  # score omitted from output only
```

**Key difference:** Python always computes and ranks by score. The `with_scores` flag only affects whether the score is included in the return value (tuple vs bare payload). Ranking is never compromised.

## Step-by-Step Code Changes

### Change 1: Always store the score internally, strip it from output only at the end

**File:** `crates/search/src/retrievers/lexical_retriever.rs`

Replace lines 188-214 (the entire `get_context` body after `let chunks = ...`):

```rust
        let chunks = self.load_document_chunks().await?;
        if chunks.is_empty() {
            return Ok(vec![]);
        }
```

The section starting from `let mut items_with_score = chunks` through `Ok(items_with_score)`:

**Old code (lines 188-214):**
```rust
        let mut items_with_score = chunks
            .into_iter()
            .filter_map(|(id, payload, text)| {
                let tokens = self.tokenize(&text);
                if tokens.is_empty() {
                    return None;
                }

                let score = self.score(&query_tokens, &tokens);
                Some(SearchItem {
                    id,
                    score: if self.with_scores { Some(score) } else { None },
                    payload,
                })
            })
            .collect::<Vec<_>>();

        items_with_score.sort_by(|left, right| {
            right
                .score
                .unwrap_or_default()
                .partial_cmp(&left.score.unwrap_or_default())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        items_with_score.truncate(self.top_k);

        Ok(items_with_score)
```

**New code:**
```rust
        let mut items_with_score = chunks
            .into_iter()
            .filter_map(|(id, payload, text)| {
                let tokens = self.tokenize(&text);
                if tokens.is_empty() {
                    return None;
                }

                let score = self.score(&query_tokens, &tokens);
                Some(SearchItem {
                    id,
                    score: Some(score),
                    payload,
                })
            })
            .collect::<Vec<_>>();

        items_with_score.sort_by(|left, right| {
            right
                .score
                .unwrap_or_default()
                .partial_cmp(&left.score.unwrap_or_default())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        items_with_score.truncate(self.top_k);

        if !self.with_scores {
            for item in &mut items_with_score {
                item.score = None;
            }
        }

        Ok(items_with_score)
```

The changes are:
1. Line 199: Changed `score: if self.with_scores { Some(score) } else { None }` to `score: Some(score)` -- always store the score during ranking.
2. After `truncate`: Added a post-ranking pass that strips scores from the output when `self.with_scores` is `false`.

This ensures the sort always operates on real scores, and the `with_scores` flag only controls the final output, matching Python behavior.

## Test Verification

### Existing test that masks the bug

**File:** `crates/search/src/retrievers/lexical_retriever.rs`, lines 354-374

The test `get_completion_returns_items_output` uses `with_scores: false` but only has **one chunk**, so there is nothing to mis-rank:

```rust
#[tokio::test]
async fn get_completion_returns_items_output() {
    let mock_graph_db = Arc::new(MockGraphDB::new());
    add_chunk(&mock_graph_db, "exact term matching with jaccard").await;
    // only 1 chunk -- ranking bug is invisible
    let graph_db: Arc<dyn GraphDBTrait> = mock_graph_db;

    let retriever =
        JaccardChunksRetriever::new(Arc::clone(&graph_db), Some(5), false, None, false);
    // ...
    assert!(items[0].score.is_none());
}
```

### New test to add

Add the following test that verifies ranking is correct even when `with_scores` is `false`. Place it inside the existing `mod tests` block in `crates/search/src/retrievers/lexical_retriever.rs`:

```rust
#[tokio::test]
async fn ranks_correctly_when_with_scores_is_false() {
    let mock_graph_db = Arc::new(MockGraphDB::new());
    // The query will be "ownership safety". The first chunk matches both tokens,
    // the second matches neither, the third matches "ownership" only.
    add_chunk(&mock_graph_db, "ownership and safety are core rust features").await;
    add_chunk(&mock_graph_db, "python async search orchestration").await;
    add_chunk(&mock_graph_db, "ownership model in rust").await;
    let graph_db: Arc<dyn GraphDBTrait> = mock_graph_db;

    let retriever = JaccardChunksRetriever::new(
        Arc::clone(&graph_db),
        Some(2),
        false,  // scores NOT included in output
        Some(vec!["and".to_string(), "are".to_string(), "in".to_string()]),
        false,
    );

    let context = retriever.get_context("ownership safety").await.unwrap();

    assert_eq!(context.len(), 2);

    // Scores should be None (with_scores=false)
    assert!(context[0].score.is_none());
    assert!(context[1].score.is_none());

    // But ranking must still be correct: the chunk with both "ownership" and
    // "safety" should come first.
    let first_text = context[0]
        .payload
        .get("text")
        .and_then(|v| v.as_str())
        .expect("first item should have text");
    assert!(
        first_text.contains("ownership") && first_text.contains("safety"),
        "highest-ranked chunk should contain both query terms, got: {first_text}"
    );
}
```

### How to verify

```bash
cargo test -p cognee-search -- lexical_retriever::tests
```

All four tests should pass. The new test `ranks_correctly_when_with_scores_is_false` will fail on the old code (random ranking) and pass after the fix.
