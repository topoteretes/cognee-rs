//! The CODE_REPO cognify route: one enola run over a whole project.
//!
//! Port of Python's `extract_code_repo_graph`
//! (`cognee/tasks/code_graph/code_repo.py`, parity pin `a59433e6b`). A
//! directory `add` stores one repo-level manifest item for a code project
//! (`cognee_ingestion::code_repo`); cognify routes it here, and this reads
//! the manifest for the repository path and loads the code graph from the
//! **original** directory (enola writes its `.enola/` snapshot there). One
//! repository node, cross-file edges, one graph read. No LLM, no embeddings.

use std::path::PathBuf;

use cognee_graph::GraphDBTrait;

use crate::config::CodeGraphConfig;
use crate::error::{CodeGraphError, CodeGraphResult};
use crate::pipeline::{CodeGraphLoad, load_code_graph};
use crate::vectors::CodeGraphVectors;
use crate::writer::CodeGraphLedger;

pub use cognee_models::code_repo::CODE_REPO_MANIFEST_KIND;

/// The repository path a stored repo manifest points at (its `repo_path`).
///
/// # Errors
///
/// [`CodeGraphError::InvalidRepoItem`] when the text is not a JSON object
/// with a string `repo_path` (Python raises from `json.loads` or the key
/// lookup).
pub fn manifest_repo_path(manifest_text: &str) -> CodeGraphResult<PathBuf> {
    let manifest: serde_json::Value = serde_json::from_str(manifest_text).map_err(|error| {
        CodeGraphError::InvalidRepoItem(format!("the code repo manifest is not JSON: {error}"))
    })?;
    manifest
        .get("repo_path")
        .and_then(serde_json::Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| {
            CodeGraphError::InvalidRepoItem("the code repo manifest has no 'repo_path'".to_string())
        })
}

/// Load the code graph of the repository a stored repo manifest names.
/// Python's `extract_code_repo_graph` for one data item.
///
/// `node_set` is the manifest row's call-level node set (read off its
/// `external_metadata`, as documents read theirs); it tags every code node,
/// the repository included. `ledger` records what is written under the
/// manifest's `Data` row.
///
/// One repository, on its own enola run. A dataset of several repositories
/// is loaded as one enola cluster instead ([`crate::snapshot_dataset_code_repos`],
/// [`crate::load_cluster_member`]), which cognify's route decides.
///
/// # Errors
///
/// [`CodeGraphError::InvalidRepoItem`] for an unreadable manifest or a
/// repository directory that no longer exists, and what [`load_code_graph`]
/// raises.
pub async fn extract_code_repo_graph(
    graph: &dyn GraphDBTrait,
    ledger: Option<&CodeGraphLedger<'_>>,
    config: &CodeGraphConfig,
    manifest_text: &str,
    node_set: Option<&[String]>,
    vectors: Option<&CodeGraphVectors>,
) -> CodeGraphResult<CodeGraphLoad> {
    let repo_path = manifest_repo_path(manifest_text)?;
    if !repo_path.is_dir() {
        return Err(CodeGraphError::InvalidRepoItem(format!(
            "Code repository '{}' no longer exists; the repo item reads the original directory \
             at cognify time. Re-add the repository from its current location.",
            repo_path.display()
        )));
    }
    let load = load_code_graph(graph, ledger, config, &repo_path, node_set, vectors).await?;
    tracing::info!(
        "Code repo graph extracted for {}{}.",
        repo_path.display(),
        ledger.map_or(String::new(), |ledger| format!(
            " ({})",
            ledger.owner.data_id
        ))
    );
    Ok(load)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use cognee_graph::MockGraphDB;

    use super::*;

    #[test]
    fn the_repo_path_is_read_from_the_manifest() {
        let path = manifest_repo_path(
            "{\n  \"kind\": \"code_repo_manifest\",\n  \"repo_path\": \"/tmp/r\\u00e9po\",\n  \"file_count\": 0,\n  \"files\": [],\n  \"content_hash\": \"x\"\n}",
        )
        .unwrap();
        assert_eq!(path, PathBuf::from("/tmp/r\u{e9}po"));
        assert!(matches!(
            manifest_repo_path("not json"),
            Err(CodeGraphError::InvalidRepoItem(_))
        ));
        assert!(matches!(
            manifest_repo_path("{\"kind\": \"code_repo_manifest\"}"),
            Err(CodeGraphError::InvalidRepoItem(_))
        ));
    }

    #[tokio::test]
    async fn a_vanished_repository_is_an_error() {
        let graph = MockGraphDB::new();
        let error = extract_code_repo_graph(
            &graph,
            None,
            &CodeGraphConfig::default(),
            "{\"repo_path\": \"/definitely/not/here\"}",
            None,
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, CodeGraphError::InvalidRepoItem(_)));
        assert!(error.to_string().contains("no longer exists"), "{error}");
    }
}
