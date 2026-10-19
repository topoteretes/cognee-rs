# Temporal Cognify

## What it does

Runs the temporal variant of the cognify pipeline: instead of (or in addition
to) standard entity/relationship extraction, it extracts **events and
timestamps** so the knowledge graph supports temporal reasoning. Mirrors
Python's `temporal_cognify=True`.

## When to use it

- Your corpus is event-driven (logs, meeting notes, news, changelogs) and you
  want to ask "what happened when?" / "what came before X?".

You do not need it to ask about dates: default cognify already writes the dates
it extracts as `Timestamp` nodes, and those are what `TEMPORAL` search reads
(see below).

## CLI

```bash
cognee-cli cognify -d my_dataset --temporal-cognify
```

The flag is `--temporal-cognify`
([`crates/cli/src/cli.rs`](../../crates/cli/src/cli.rs), `CognifyArgs`). It maps
to `CognifyConfig.temporal_cognify`.

## Programmatic

```rust
use cognee_cognify::CognifyConfig;

let config = CognifyConfig::default()
    .with_temporal_cognify(true);
// also: .with_data_per_batch(n) tunes the temporal batch size (default 20)
```

See [`CognifyConfig`](../../crates/cognify/src/config.rs) (`temporal_cognify`,
`with_temporal_cognify`, `data_per_batch`).

## Querying temporal memory

Retrieve with the `TEMPORAL` search type:

```bash
cognee-cli search "what happened before the launch?" -t TEMPORAL -d my_dataset
# or let recall auto-route to a temporal strategy:
cognee-cli recall "timeline of the project"
```

This uses `SearchType::Temporal`, served by `TemporalHybridRetriever`
([`crates/search/src/retrievers/temporal_hybrid/`](../../crates/search/src/retrievers/temporal_hybrid/)):
hybrid retrieval reranked by the `Timestamp` nodes inside the question's time
window (see [operations.md](../operations.md#search-retrieval)). As in Python, it
reads the `Timestamp` nodes default cognify writes, not the `Event` nodes this
pipeline adds, so on a graph built only by temporal cognify it answers with plain
hybrid results. The recall router can auto-select the temporal strategy from the
query
([`crates/search/src/query_router.rs`](../../crates/search/src/query_router.rs)).

## Pointers

- [`CognifyConfig`](../../crates/cognify/src/config.rs) — `temporal_cognify`.
- [`crates/cli/src/cli.rs`](../../crates/cli/src/cli.rs) — `--temporal-cognify`, `-t TEMPORAL`.
- [operations.md](../operations.md) — cognify and search stages.
