//! COGX export — portable knowledge-graph archives that Python cognee imports.
//!
//! COGX is Python cognee's hub format for memory migration, and the only one
//! of its five export formats that has a reader on the other side: `json`,
//! `graphml` and `cypher` are one-way egress, and `pydantic` is a Python
//! in-process object graph. So a Rust archive written here is re-importable by
//! the Python SDK two ways:
//!
//! ```text
//! # locally, from a directory
//! cognee.remember(COGXArchiveSource("/path/to/archive"))
//!
//! # over the wire, from a tarball (requires the `archive` feature)
//! POST /api/v1/remember  content_type=cogx-archive  data=@dataset.cogx.tar.gz
//! ```
//!
//! Both land in the same loader, in `preserve` mode by default: entities,
//! facts and raw nodes go straight into the graph with no LLM completions.
//!
//! # Fidelity
//!
//! What survives a round trip: node ids, node types and all node properties,
//! entity names and descriptions, every edge with its relationship name, and
//! edge temporal validity (`valid_at`/`invalid_at`).
//!
//! What does not:
//!
//! * **Arbitrary edge properties.** A [`CogxFact`] carries only `fact_text`,
//!   `valid_at`, `invalid_at` and `confidence`; any other edge property is
//!   dropped. This is a limitation of COGX 0.1 itself, not of this writer.
//! * **Vector embeddings.** The archive holds no vectors — only the source
//!   embedding model's *name*, in the manifest. The importing instance
//!   re-embeds with whatever it has configured.
//!
//! # Scope
//!
//! [`export_graph`] exports the whole graph store, which is what a
//! single-database deployment means by "the dataset's graph" — the same thing
//! `visualize` reads. Python additionally scopes by switching database context
//! per dataset when backend access control is on; cognee-rs has no equivalent
//! per-dataset graph partition today.

#![warn(missing_docs)]

#[cfg(feature = "archive")]
pub mod archive;
pub mod cogx;
pub mod error;
pub mod export;

#[cfg(feature = "archive")]
pub use archive::{ARCHIVE_SUFFIX, pack_archive};
pub use cogx::{
    COGX_VERSION, CogxArchiveWriter, CogxDocument, CogxEntity, CogxFact, CogxManifest, CogxRecord,
    CogxScope, MANIFEST_FILE, RAW_NODES_FILE, SOURCE_SYSTEM, parse_timestamp,
};
pub use error::{MigrationError, MigrationResult};
pub use export::{ExportOptions, ExportSummary, export_graph, write_cogx};
