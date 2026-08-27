# cognee-delete

Cascading deletion of data and datasets across all cognee backends (relational DB → graph DB → vector DB → file storage), with dry-run previews and permission-checked variants.

It also owns the **run sweep** (`RunSweeper`): removing the graph nodes, vector points and ownership rows a single pipeline run created in a dataset — optionally narrowed to a set of data items — and clearing those items' cognify completion markers. Same artifact-deletion path as dataset delete, different selection.

Part of [cognee-rs](https://github.com/topoteretes/cognee-rs) — see the [project README](../../README.md) for an architecture overview and how the pieces fit together.

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or [Apache-2.0](../../LICENSE-APACHE), at your option.
