//! The global context index: retrieval-ready summaries above a dataset.
//!
//! Port of Python's `cognee/memify_pipelines/global_context_index.py` and
//! `cognee/tasks/memify/global_context_index/`. Two methods build it:
//!
//! - [`GlobalContextIndexMethod::FromEntities`] (the default, no LLM): one
//!   bucket per document naming the entities it is distinctively about
//!   (tf·idf over the dataset's chunks) and the relations among them, plus a
//!   dataset root of three name-dense lines. Every run computes the full
//!   index, writes only the nodes whose text changed and deletes the index
//!   nodes it did not compute ([`document_index`]).
//! - [`GlobalContextIndexMethod::Llm`] (opt-in): LLM bucket and root
//!   summaries above the dataset's `TextSummary` nodes, placed by vector
//!   similarity level by level ([`llm`]).
//!
//! Both write [`GlobalContextSummary`] nodes, embedded into the
//! `GlobalContextSummary_text` collection, and `summarized_in` edges from a
//! bucket to its parent.

pub mod content;
pub mod document_index;
pub mod llm;
pub mod persist;

use std::sync::Arc;

use cognee_llm::Llm;
pub use cognee_models::GlobalContextSummary;
use uuid::Uuid;

use crate::error::CognifyError;

pub use content::{DatasetContent, DatasetEntity, load_dataset_content};
pub use document_index::{build_context_summaries, build_document_index, compute_idf_from_counts};
pub use llm::{LlmIndexOptions, update_global_context_index};
pub use persist::IndexStore;

/// Edge from a bucket to its parent bucket or the root.
pub const SUMMARIZED_IN: &str = "summarized_in";
/// Edge from a `from_entities` bucket to the document it summarizes.
pub const SUMMARIZES: &str = "summarizes";
/// The `TextSummary` collection the LLM method places level 0 by.
pub const TEXT_SUMMARY_COLLECTION: &str = "TextSummary_text";
/// Node types the LLM method reads.
pub const SUMMARY_GRAPH_NODE_TYPES: &[&str] =
    &["TextSummary", "DocumentChunk", "GlobalContextSummary"];
/// Node types the `from_entities` method reads.
pub const DOCUMENT_INDEX_NODE_TYPES: &[&str] = &[
    "DocumentChunk",
    "Entity",
    "EntityType",
    "GlobalContextSummary",
];
/// The type of an entity with no `is_a` edge.
pub const DEFAULT_ENTITY_TYPE_NAME: &str = "Entity";
/// The longest entity or type name bucket and root text state; a longer one is cut.
pub const ENTITY_NAME_MAX_CHARS: usize = 80;
/// Entities named per document bucket.
pub const DOCUMENT_BUCKET_MAX_ENTITIES: usize = 20;
/// Relation lines per document bucket.
pub const DOCUMENT_BUCKET_MAX_RELATIONS: usize = 40;
/// Names on the root's `Entities:` line.
pub const ROOT_MAX_ENTITIES: usize = 40;
/// Types on the root's `Topics:` line.
pub const ROOT_MAX_TYPES: usize = 20;
/// Names on the root's `Documents:` line.
pub const ROOT_MAX_DOCUMENTS: usize = 40;

/// Id of an LLM-built bucket: `uuid5(NAMESPACE_OID,
/// "GlobalContextSummary:{dataset}:{level}:{sorted child ids}")`.
pub fn create_bucket_id<S: AsRef<str>>(dataset_id: &str, level: i64, child_ids: &[S]) -> Uuid {
    let mut children: Vec<&str> = child_ids.iter().map(AsRef::as_ref).collect();
    children.sort_unstable();
    let key = children.join(",");
    Uuid::new_v5(
        &Uuid::NAMESPACE_OID,
        format!("GlobalContextSummary:{dataset_id}:{level}:{key}").as_bytes(),
    )
}

/// Id of a dataset's root: `uuid5(NAMESPACE_OID, "GlobalContextSummary:{dataset}:root")`.
pub fn create_root_summary_id(dataset_id: &str) -> Uuid {
    Uuid::new_v5(
        &Uuid::NAMESPACE_OID,
        format!("GlobalContextSummary:{dataset_id}:root").as_bytes(),
    )
}

/// Id of a document's bucket:
/// `uuid5(NAMESPACE_OID, "GlobalContextSummary:{dataset}:doc:{document}")`.
pub fn create_document_bucket_id(dataset_id: &str, document_id: &str) -> Uuid {
    Uuid::new_v5(
        &Uuid::NAMESPACE_OID,
        format!("GlobalContextSummary:{dataset_id}:doc:{document_id}").as_bytes(),
    )
}

/// The data item the index's ownership rows name, so deleting the dataset
/// removes the index with it: `uuid5(NAMESPACE_URL,
/// "cognee:global-context-index:{dataset}")` (Python's
/// `ensure_global_context_storage_context`).
pub fn index_data_item_id(dataset_id: Uuid) -> Uuid {
    Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("cognee:global-context-index:{dataset_id}").as_bytes(),
    )
}

/// How the index is built.
#[derive(Debug, Clone, PartialEq)]
pub enum GlobalContextIndexMethod {
    /// One bucket per document plus a root, by counting entities; no LLM.
    FromEntities {
        /// Write the `summarizes` edge from each bucket to its document.
        link_documents: bool,
    },
    /// LLM summaries over vector-placed buckets of `TextSummary` nodes.
    Llm(LlmIndexOptions),
}

impl Default for GlobalContextIndexMethod {
    /// Python's `method="from_entities", link_documents=True`.
    fn default() -> Self {
        Self::FromEntities {
            link_documents: true,
        }
    }
}

/// Build or update a dataset's global context index (Python's
/// `global_context_index_pipeline`, minus the pipeline-run bookkeeping).
///
/// Returns the index nodes it wrote. The caller enters the dataset's storage
/// scope. `llm` is only called by [`GlobalContextIndexMethod::Llm`].
pub async fn build_global_context_index(
    store: &IndexStore<'_>,
    llm: &Arc<dyn Llm>,
    method: &GlobalContextIndexMethod,
) -> Result<Vec<GlobalContextSummary>, CognifyError> {
    match method {
        GlobalContextIndexMethod::FromEntities { link_documents } => {
            let content = load_dataset_content(store).await?;
            build_document_index(&content, store, *link_documents).await
        }
        GlobalContextIndexMethod::Llm(options) => {
            update_global_context_index(store, llm, options).await
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    #[test]
    fn ids_match_python_uuid5() {
        // uuid5(NAMESPACE_OID, ...) / uuid5(NAMESPACE_URL, ...), computed in Python.
        assert_eq!(
            create_root_summary_id("ds").to_string(),
            "9e5b970d-fb7f-5280-bbdf-3f12ed0869ee"
        );
        assert_eq!(
            create_document_bucket_id("ds", "d1").to_string(),
            "cf9863ef-ea84-5693-b5ad-3eed99b404f6"
        );
        assert_eq!(
            create_bucket_id("ds", 0, &["b", "a"]).to_string(),
            "890b0a75-6f16-5f5e-8f80-dd8e1431533d"
        );
        assert_eq!(
            index_data_item_id(Uuid::nil()).to_string(),
            "2a605947-031a-52e8-9a6c-de0eeea6ffb2"
        );
    }
}
