# Guides

Focused, task-oriented how-tos for cognee-rust. Each guide states what the
feature does, when to use it, a runnable example, and a pointer into the code.
For the end-to-end story start with [getting-started.md](../getting-started.md)
and [operations.md](../operations.md).

| Guide | What it covers |
|---|---|
| [external-providers.md](external-providers.md) | Plug in a vector / graph / LLM backend from a crate outside this repository — the factory traits, `ComponentRegistry`, and what `BackendBuildContext` hands you (including resolved Postgres URLs). |
| [custom-prompts.md](custom-prompts.md) | Override the LLM prompt used for entity/relationship extraction in cognify. |
| [custom-graph-schema.md](custom-graph-schema.md) | Supply custom summarization and runtime graph-extraction schemas. |
| [ontology.md](ontology.md) | Ground extraction with an RDF/OWL/JSON-LD ontology. |
| [temporal-cognify.md](temporal-cognify.md) | Extract events/timestamps and query them with temporal recall. |
| [memify-node-filtering.md](memify-node-filtering.md) | Scope memify enrichment to specific node types / names (node sets). |
