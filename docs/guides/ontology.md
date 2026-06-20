# Grounding Extraction with an Ontology

## What it does

Supplies an ontology (RDF/OWL, Turtle, or JSON-LD) to cognify so extracted
entities are matched and grounded against a known vocabulary. The
[`OntologyResolver`](../../crates/ontology/src/) loads the file and matches
extracted entity names/types to ontology terms during knowledge-graph
extraction.

## When to use it

- You have a controlled vocabulary / taxonomy and want extraction to align with
  it (consistent entity types, canonical names).
- You want to constrain or normalize the messy free-form output of an LLM.

## CLI

`cognee-cli cognify` exposes the ontology file directly:

```bash
cognee-cli cognify -d my_dataset --ontology-file ./ontologies/domain.owl
```

The flag is `--ontology-file <path>`
([`crates/cli/src/cli.rs`](../../crates/cli/src/cli.rs), `CognifyArgs`).

## Configuration (env / config keys)

You can also set the ontology globally instead of per-invocation:

| Env var | Config key | Default |
|---|---|---|
| `ONTOLOGY_FILE_PATH` | `ontology_file_path` | _(empty)_ |
| `ONTOLOGY_RESOLVER` | `ontology_resolver` | `rdflib` |
| `ONTOLOGY_MATCHING_STRATEGY` | `ontology_matching_strategy` | `fuzzy` |

```bash
export ONTOLOGY_FILE_PATH=./ontologies/domain.owl
cognee-cli cognify -d my_dataset
```

Note: `ontology_resolver` and `ontology_matching_strategy` are stored for
parity but the active path currently uses the RDF/JSON-LD/Turtle resolver with a
fuzzy matching strategy whenever an ontology file is set. See
[configuration.md](../configuration.md) for the full surface and
[`crates/lib/src/config.rs`](../../crates/lib/src/config.rs) for the field
definitions and `set_ontology_*` setters.

## Supported formats

RDF/OWL, Turtle, and JSON-LD, parsed via the `sophia` family of crates. See
[`crates/ontology/`](../../crates/ontology/) for the resolver implementations and
the test fixtures under `crates/ontology/tests/fixtures/`.

## Pointers

- [`crates/ontology/`](../../crates/ontology/) — `OntologyResolver`, `RdfLibOntologyResolver`, `NoOpOntologyResolver`.
- [`crates/cli/src/cli.rs`](../../crates/cli/src/cli.rs) — `--ontology-file`.
- [configuration.md](../configuration.md) — ontology env vars.
- [operations.md](../operations.md) — cognify stage.
