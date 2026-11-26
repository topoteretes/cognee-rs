# cognee-search

Unified search orchestration across 18 retrieval strategies (GraphCompletion, RagCompletion, Chunks, Code, and more) for the cognee `search` stage.

`SearchType::Code` (`"CODE"`) runs one deterministic operation over the
dataset's code graph (`CodeRetriever`, engine in
[`cognee-code-graph`](../code-graph/README.md)): no LLM, embedding or vector
search. The request's `code_query` (`{"operation": "neighbors", "name":
"create_order"}`) is merged over `retriever_specific_config`; a `code_query`
with another search type is refused with Python's message. The result is
`SearchOutput::Structured`, and an `only_context` search returns Python's
context text, the result as compact JSON with sorted keys. Errors are
`SearchError::Code`, carrying Python's `error_code` and `error_param`. Under
per-dataset storage a search of several datasets reports a seed one dataset
cannot resolve as that dataset's result (`{"seed_not_found": true, "error",
"code", "param"}`, Python's `_report_code_seed_miss`) instead of failing,
and `search` merges the datasets' results into one array.

Part of [cognee-rs](https://github.com/topoteretes/cognee-rs) — see the [project README](../../README.md) for an architecture overview and how the pieces fit together.

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or [Apache-2.0](../../LICENSE-APACHE), at your option.
