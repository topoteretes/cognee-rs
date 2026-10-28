# cognee-ingestion

Data ingestion pipeline for cognee (the `add` stage) — streams input, computes content hashes, and persists data plus metadata with multi-tenant isolation.

`data` rows are dataset-scoped, as in Python (`ingest_data.py`): a new row gets a random `uuid4` id and its dataset in `data.dataset_id`; re-adding content reuses the row that holds the same `content_hash` in the same dataset for the same owner and tenant (Python's `identify_many`). The same content in another dataset is another row. Dataset ids are deterministic (`generate_dataset_id`: UUID5 of name, owner and tenant).

Part of [cognee-rs](https://github.com/topoteretes/cognee-rs) — see the [project README](../../README.md) for an architecture overview and how the pieces fit together.

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or [Apache-2.0](../../LICENSE-APACHE), at your option.
