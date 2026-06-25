# Scoping Memify with Node Filtering

## What it does

`memify` enriches an existing knowledge graph by building and indexing triplet
embeddings from its edges. By default it runs over the whole graph; node
filtering scopes it to a specific **node set** — a node *type* plus one or more
node *names*. This is the "Node Sets" concept (see
[concepts.md](../concepts.md)): a named subgraph you can enrich or query
independently. Mirrors Python's `memify(node_type=…, node_name=…)`.

## When to use it

- Re-enrich only part of a large graph (e.g. just `Entity` nodes named "Acme").
- Build focused triplet indexes for a subset of your memory.

## CLI

```bash
# Enrich the whole graph
cognee-cli memify -d my_dataset

# Scope to a node type + names (OR logic across names)
cognee-cli memify -d my_dataset --node-type Entity --node-name Acme --node-name Globex
```

Flags ([`crates/cli/src/cli.rs`](../../crates/cli/src/cli.rs), `MemifyArgs`):

- `--node-type <T>` — filter to one node type (e.g. `Entity`).
- `--node-name <N>` — repeatable; matches any of the given names (OR).
- `--batch-size <N>` — triplet extraction/embedding batch size (default 100).

Filtering takes effect only when **both** a node type and at least one node name
are supplied — the pipeline then pulls the matching node-set subgraph
([`crates/cognify/src/memify/extract_triplets.rs`](../../crates/cognify/src/memify/extract_triplets.rs)).

## Programmatic

```rust
use cognee_cognify::memify::MemifyConfig;

let config = MemifyConfig::default()
    .with_node_type_filter("Entity".to_string())
    .with_node_name_filter(vec!["Acme".to_string(), "Globex".to_string()])
    .with_node_name_filter_operator("OR".to_string()); // "OR" (default) or "AND"
```

See [`MemifyConfig`](../../crates/cognify/src/memify/config.rs) —
`node_type_filter`, `node_name_filter`, `node_name_filter_operator` (validated to
be `"OR"` or `"AND"`).

## Pointers

- [`MemifyConfig`](../../crates/cognify/src/memify/config.rs) — filter fields and builders.
- [`crates/cognify/src/memify/extract_triplets.rs`](../../crates/cognify/src/memify/extract_triplets.rs) — node-set subgraph selection.
- [concepts.md](../concepts.md) — Node Sets.
- [operations.md](../operations.md) — the memify stage.
