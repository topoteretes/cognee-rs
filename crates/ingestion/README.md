# cognee-ingestion

Data ingestion pipeline for cognee (the `add` stage) — streams input, computes content hashes, deduplicates, and persists data plus metadata with deterministic UUID5 IDs and multi-tenant isolation.

Part of [cognee-rs](https://github.com/topoteretes/cognee-rs) — see the [project README](../../README.md) for an architecture overview and how the pieces fit together.

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or [Apache-2.0](../../LICENSE-APACHE), at your option.
