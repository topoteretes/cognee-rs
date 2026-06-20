//! Data ingestion pipeline for Cognee (the `add` stage).
//!
//! Streams input content, computes content hashes, deduplicates, and persists
//! data plus metadata. Mirrors the Python `cognee.add()` behaviour:
//! MD5/SHA-256 content hashing, deterministic UUID5 IDs, multi-tenant
//! isolation, a loader-engine registry, and (behind the `html-loader` feature)
//! URL crawling.
//!
//! Main entry point: [`pipeline::AddPipeline`]. No trait abstraction — it builds
//! on `StorageTrait` and `IngestDb` from the sibling crates.

mod content_hasher;
mod id_generation;
mod loader_registry;
pub mod loaders;
pub mod pipeline;
// URL crawling + HTML extraction. The `extract_html` extractor is shared with
// the HTML document loader, so both live behind the `html-loader` feature.
#[cfg(feature = "html-loader")]
pub mod url_crawler;
pub mod url_resolver;

pub use content_hasher::{ContentHasher, HashAlgorithm};
pub use id_generation::{generate_data_id, generate_dataset_id};
pub use loader_registry::get_loader_name;
pub use loaders::{DocumentLoader, LoaderError, LoaderOutput, LoaderRegistry};
pub use pipeline::{
    AddParams, AddPipeline, IngestionError, ProcessedInput, build_add_pipeline,
    build_add_pipeline_with_acl, make_persist_data_task, make_persist_data_task_with_acl,
    make_process_input_task, persist_data, persist_data_with_acl, process_input,
};
// `UrlMetadata`/`ResolvedUrlInput` are plain data types and stay always-on so
// `pipeline.rs` signatures compile without the feature; `resolve_url_input`
// (which drives the URL crawler) is gated behind `html-loader`.
#[cfg(feature = "html-loader")]
pub use url_resolver::resolve_url_input;
pub use url_resolver::{ResolvedUrlInput, UrlMetadata};
