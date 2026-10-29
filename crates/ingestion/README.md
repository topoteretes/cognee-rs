# cognee-ingestion

Data ingestion pipeline for cognee (the `add` stage) — streams input, computes content hashes, and persists data plus metadata with multi-tenant isolation.

`data` rows are dataset-scoped, as in Python (`ingest_data.py`): a new row gets a random `uuid4` id and its dataset in `data.dataset_id`; re-adding content reuses the row that holds the same `content_hash` in the same dataset for the same owner and tenant (Python's `identify_many`). The same content in another dataset is another row. Dataset ids are deterministic (`generate_dataset_id`: UUID5 of name, owner and tenant).

Loaders run at `add`, as in Python (`ingest_data.py`, `loaders/store_derived_text.py`): the stored file is the extracted text `text_<content_hash>.txt` (`extension` `txt`, `mime_type` `text/plain`, `raw_content_hash` its MD5), and `original_data_location` keeps the source. `AddPipeline` uses `LoaderRegistry::default_registry()` (text, HTML, PDF, CSV, unstructured) unless `with_loader_registry` gives it another. The image and audio loaders need an LLM or a transcriber, so they are registered with `LoaderRegistry::register_image_loader` / `register_audio_loader`; the CLI and the bindings pass `ComponentManager::add_loader_registry()`, which has both whenever the configured LLM provides them. An image is then stored as its vision-LLM description and an audio file as its Whisper transcript. Without them an image or audio input fails with `UnsupportedDocumentType`.

Part of [cognee-rs](https://github.com/topoteretes/cognee-rs) — see the [project README](../../README.md) for an architecture overview and how the pieces fit together.

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or [Apache-2.0](../../LICENSE-APACHE), at your option.
