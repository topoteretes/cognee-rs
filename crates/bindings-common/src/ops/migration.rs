//! COGX archive export.
//!
//! Wraps `cognee::migration` so every binding can produce the one interchange
//! format Python cognee re-imports, without each of them repeating the
//! export-then-pack dance.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use cognee::migration::{ARCHIVE_SUFFIX, ExportOptions, export_graph, pack_archive};

use crate::{HandleState, SdkError};

/// Export the graph store as a COGX archive directory, then pack it into a
/// `.cogx.tar.gz` tarball beside it.
///
/// `destination` is the archive *directory* to write; the tarball is
/// `{destination}{ARCHIVE_SUFFIX}`. Packing happens here rather than in the
/// caller because the two halves have to agree on the suffix — Python's
/// importer only accepts `manifest.json` at the tarball root or in a single
/// subdirectory, which is what `pack_archive` guarantees.
///
/// `opts` keys, camelCase like every other binding op:
/// - `embeddingModel` — recorded in the manifest (advisory; the archive carries
///   no vectors, so the importing instance re-embeds).
/// - `datasetName` — a manifest *label* only. It does not scope the export:
///   cognee-rs has no per-dataset graph partition, so the archive contains the
///   whole graph store. Callers that surface this to a user must say so.
///
/// Returns JSON with the two paths plus the export counts:
/// `{"archive":…,"directory":…,"numNodes":…,"numEdges":…,"numEntities":…,
///   "numDocuments":…,"numFacts":…,"numRawNodes":…}`
pub async fn export_cogx(
    state: &HandleState,
    destination: &str,
    opts: &serde_json::Value,
) -> Result<serde_json::Value, SdkError> {
    if destination.trim().is_empty() {
        return Err(SdkError::Validation(
            "destination must be a non-empty path".to_string(),
        ));
    }
    let destination = PathBuf::from(destination);

    let options = ExportOptions {
        embedding_model: opt_string(opts, "embeddingModel"),
        dataset_name: opt_string(opts, "datasetName"),
    };

    let svc = state.services().await?;
    let graph_db = Arc::clone(&svc.graph_db);

    let summary = export_graph(&*graph_db, &destination, &options)
        .await
        .map_err(|e| SdkError::Runtime(format!("COGX export failed: {e}")))?;

    let archive = tarball_path(&destination);
    pack_archive(&destination, &archive)
        .map_err(|e| SdkError::Runtime(format!("COGX archive packing failed: {e}")))?;

    Ok(serde_json::json!({
        "archive": archive.to_string_lossy(),
        "directory": summary.destination.to_string_lossy(),
        "numNodes": summary.num_nodes,
        "numEdges": summary.num_edges,
        "numEntities": summary.num_entities,
        "numDocuments": summary.num_documents,
        "numFacts": summary.num_facts,
        "numRawNodes": summary.num_raw_nodes,
    }))
}

/// `dir` -> `dir.cogx.tar.gz`, appended to the file name rather than replacing
/// an extension: the suffix is multi-part, so `set_extension` would mangle it.
fn tarball_path(destination: &Path) -> PathBuf {
    let mut name = destination
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "cognee".to_string());
    name.push_str(ARCHIVE_SUFFIX);
    match destination.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(name),
        _ => PathBuf::from(name),
    }
}

fn opt_string(opts: &serde_json::Value, key: &str) -> Option<String> {
    opts.get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tarball_sits_beside_the_directory() {
        assert_eq!(
            tarball_path(Path::new("/tmp/exports/berlin_cogx")),
            PathBuf::from("/tmp/exports/berlin_cogx.cogx.tar.gz")
        );
    }

    #[test]
    fn multi_part_suffix_is_appended_not_substituted() {
        // set_extension would turn "trip.v2" into "trip.cogx.tar.gz",
        // silently dropping the ".v2" the caller chose.
        assert_eq!(
            tarball_path(Path::new("trip.v2")),
            PathBuf::from("trip.v2.cogx.tar.gz")
        );
    }

    #[test]
    fn opts_treat_blank_strings_as_absent() {
        let opts = serde_json::json!({ "embeddingModel": "  ", "datasetName": "berlin" });
        assert_eq!(opt_string(&opts, "embeddingModel"), None);
        assert_eq!(opt_string(&opts, "datasetName"), Some("berlin".to_string()));
    }
}
