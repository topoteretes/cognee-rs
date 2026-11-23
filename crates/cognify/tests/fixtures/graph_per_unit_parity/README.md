# Graph-per-unit parity fixture (SDK-1694, test 10)

`input.json` is a list of `(data_id, chunk_id, KnowledgeGraph)` triples in
pipeline order: three data items, the first with two chunks. It covers one
entity typed differently in every data item and in two chunks of one, an edge
to a node only an earlier chunk declares, an edge to a node only another data
item declares, a timestamp (and an edge drawn from one), two nodes sharing a
name in one chunk, a node with a blank type, relationship labels with spaces,
capitals and an apostrophe (one of them naming, in another spelling, an edge
an earlier chunk declared), two node ids that differ only in case, and an edge
whose endpoint spells a node id differently.

`expected.json` is what Python cognee's `construct_data_points_and_edges`
builds from it, called once per data item with fresh maps, as the pipeline
calls it when every data item fits in one `chunks_per_batch` batch:

- `entity_ids`: every `Entity`;
- `entity_types`: every `(entity id, EntityType id)` pair, unioned over the data
  items;
- `type_names`: EntityType id to name, for the types some entity points at
  (construction also builds one for a type that lost inside its batch, but
  nothing references it, so it is never stored);
- `timestamp_ids`: every `Timestamp`;
- `semantic_edges`: every `(source, target, relationship_name)`.

`tests/graph_per_unit_parity.rs` runs the same input through
`expand_extracted_graphs_by_unit` with `EndpointScope::Chunk` (Python's rule,
`cognify.cross_chunk_endpoints=false`) and asserts the entities, pairs, type
names, timestamps and semantic edges are equal. Under that scope Rust matches
endpoints by the verbatim node id only, as Python does. Both normalise
relationship labels (`generate_edge_name`, SDK-1694 stage O2).

## Regenerating

Generated from Python cognee `e27c7903` (`origin/dev`, 2026-11-20; regenerated
for SDK-1694 stage O2 from the same commit) with an environment that has
cognee's dependencies installed:

```bash
git -C /path/to/cognee checkout e27c7903
PYTHONPATH=/path/to/cognee python generate_expected.py
```

Review the diff of `expected.json` before committing it.
