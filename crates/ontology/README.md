# cognee-ontology

RDF/OWL ontology integration for the cognee knowledge-graph pipeline — fuzzy entity matching, subgraph extraction, and multi-format parsing (Turtle, RDF/XML, N-Triples, JSON-LD).

## Enumerating an ontology's terms

`OntologyResolver::terms()` returns the ontology's `owl:Class` and
`owl:ObjectProperty` subjects as an `OntologyTerms`: two URI-sorted, URI-deduplicated
pools of `OntologyTerm { uri, label, comment }` carrying the **raw** `rdfs:label` and
`rdfs:comment` lexical forms, with `None` meaning "predicate absent".

It is the raw material for deriving a closed-set extraction schema (a fixed list of
entity and relation types) from an ontology, which `build_lookup` cannot supply —
that collects classes and individuals only, and never properties. Normalisation is
deliberately left to the caller, because callers normalise differently.

`terms()` is a **default** method returning an empty `OntologyTerms`, so existing
implementors keep compiling; `RdfLibOntologyResolver` overrides it by walking its
graph.

Part of [cognee-rs](https://github.com/topoteretes/cognee-rs) — see the [project README](../../README.md) for an architecture overview and how the pieces fit together.

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or [Apache-2.0](../../LICENSE-APACHE), at your option.
