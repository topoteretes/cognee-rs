# Temporal Search

## What it does

Answers "what happened when?" questions over a dataset built by the ordinary
cognify. Default cognify writes every date it extracts as a shared `Timestamp`
node, linked to the chunks and entities that name it; the `TEMPORAL` search
type runs hybrid retrieval and reranks the candidates by how well their
`Timestamp`s fit the time window the question asks about. This is Python's
behaviour since cognee 1.6.3.

## There is no temporal cognify

Rust used to have a separate event-graph pipeline (`--temporal-cognify`,
`CognifyConfig::with_temporal_cognify`, the bindings' `temporalCognify` option).
Python deprecated `temporal_cognify` and ignores it ("cognee 1.6.2 was the last
release in which temporal_cognify=True built the event graph"), and Rust removed
it outright (SDK-1039):

- the CLI rejects `--temporal-cognify` as an unknown argument;
- the bindings warn about `temporalCognify` as an unknown option and ignore it,
  as Python ignores the keyword with a warning;
- `CognifyConfig` has no `temporal_cognify` field.

Just run cognify. A dataset an older build cognified with the event pipeline
holds `Event` nodes nothing reads any more; TEMPORAL answers on it with plain
hybrid results. To get the rerank, rebuild it: forget the dataset's memory and
cognify again.

## Querying

```bash
cognee-cli cognify -d my_dataset
cognee-cli search "what happened in 1945?" -t TEMPORAL -d my_dataset
# or let recall auto-route to a temporal strategy:
cognee-cli recall "timeline of the project"
```

This uses `SearchType::Temporal`, served by `TemporalHybridRetriever`
([`crates/search/src/retrievers/temporal_hybrid/`](../../crates/search/src/retrievers/temporal_hybrid/)):
hybrid retrieval reranked by the `Timestamp` nodes inside the question's time
window (see [operations.md](../operations.md#search-retrieval)). When the
question names no time, or nothing in the graph is dated inside its window, the
answer is the plain hybrid one. The recall router can auto-select the temporal
strategy from the query
([`crates/search/src/query_router.rs`](../../crates/search/src/query_router.rs)).

## Pointers

- [`crates/cognify/src/temporal_hints.rs`](../../crates/cognify/src/temporal_hints.rs)
  — the date hints default cognify gives the graph prompt.
- [`crates/cognify/src/graph_integration/expansion.rs`](../../crates/cognify/src/graph_integration/expansion.rs)
  — how extracted `Timestamp` nodes are built and linked.
- [`crates/search/tests/temporal_on_default_cognify.rs`](../../crates/search/tests/temporal_on_default_cognify.rs)
  — default cognify followed by a TEMPORAL rerank, end to end.
- [operations.md](../operations.md) — cognify and search stages.
