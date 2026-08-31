//! COGX archive export.
//!
//! Wraps `cognee::migration` so every binding can produce the one interchange
//! format Python cognee re-imports, without each of them repeating the
//! export-then-pack dance.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use cognee::migration::{ARCHIVE_SUFFIX, ExportOptions, ExportSummary, export_graph, pack_archive};

use crate::{HandleState, SdkError};

/// Keys of the JSON object [`export_cogx`] returns. The Java/TS/Python result
/// types are deserialized by name and tolerate unknown fields, so a rename on
/// either side degrades silently to a default value instead of failing — these
/// constants exist so a test can pin the contract.
#[cfg(test)]
const RESULT_KEYS: &[&str] = &[
    "archive",
    "directory",
    "numNodes",
    "numEdges",
    "numEntities",
    "numDocuments",
    "numFacts",
    "numRawNodes",
];

/// Export the graph store as a COGX archive directory, then pack it into a
/// `.cogx.tar.gz` tarball beside it.
///
/// `destination` is the archive *directory* to write, and it must be **absent
/// or empty** — see [`check_destination`]. The tarball is
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
/// Returns JSON with the two paths plus the export counts (see [`RESULT_KEYS`]).
pub async fn export_cogx(
    state: &HandleState,
    destination: &str,
    opts: &serde_json::Value,
) -> Result<serde_json::Value, SdkError> {
    let destination = PathBuf::from(destination.trim());
    check_destination(&destination)?;
    // Resolved before any work, so a destination that cannot name a tarball
    // fails before the graph is read rather than after.
    let archive = tarball_path(&destination)?;

    let options = ExportOptions {
        embedding_model: opt_string(opts, "embeddingModel"),
        dataset_name: opt_string(opts, "datasetName"),
    };

    let svc = state.services().await?;
    let graph_db = Arc::clone(&svc.graph_db);

    let summary = export_graph(&*graph_db, &destination, &options)
        .await
        .map_err(|e| SdkError::Runtime(format!("COGX export failed: {e}")))?;

    if let Err(error) = pack_archive(&destination, &archive) {
        // `pack_archive` creates the file before it can fail, and never reaches
        // `GzEncoder::finish`, so what is left behind is a truncated gzip stream
        // sitting at exactly the path a successful call would have returned.
        // Anything that keys off the file existing — a retry, a resumed upload —
        // would ship a stream Python's importer rejects with an opaque error.
        let _ = std::fs::remove_file(&archive);
        return Err(SdkError::Runtime(format!(
            "COGX archive packing failed: {error}"
        )));
    }

    Ok(result_json(&archive, &summary))
}

/// The op's return value. Split out so a test can assert the exact key set the
/// bindings deserialize, rather than a copy of it.
fn result_json(archive: &Path, summary: &ExportSummary) -> serde_json::Value {
    serde_json::json!({
        "archive": archive.to_string_lossy(),
        "directory": summary.destination.to_string_lossy(),
        "numNodes": summary.num_nodes,
        "numEdges": summary.num_edges,
        "numEntities": summary.num_entities,
        "numDocuments": summary.num_documents,
        "numFacts": summary.num_facts,
        "numRawNodes": summary.num_raw_nodes,
    })
}

/// Require the destination to be absent or empty.
///
/// `pack_archive` tars **every** top-level file of the directory it is given,
/// and the archive writer only removes the files it owns, so a caller who reads
/// "the archive directory to write" as "somewhere I already keep things" would
/// ship those things inside a tarball whose whole purpose is to be uploaded.
/// `export.rs` calls a mislabelled archive "a disclosure hazard, not just a
/// stale label"; the same bar applies to its contents.
///
/// This fails closed rather than filtering: enumerating the archive's own file
/// names here would silently drift from the writer's list, and the failure mode
/// of drift is leaking a file, so the check must not depend on that list.
fn check_destination(destination: &Path) -> Result<(), SdkError> {
    if destination.as_os_str().is_empty() {
        return Err(SdkError::Validation(
            "destination must be a non-empty path".to_string(),
        ));
    }
    let Ok(mut entries) = std::fs::read_dir(destination) else {
        // Absent (or unreadable — the export itself will report that better).
        return Ok(());
    };
    if entries.next().is_some() {
        return Err(SdkError::Validation(format!(
            "destination '{}' is not empty; COGX export packs everything in the \
             directory into the archive, so it must be given a fresh or empty one",
            destination.display()
        )));
    }
    Ok(())
}

/// `dir` -> `dir.cogx.tar.gz`, appended to the file name rather than replacing
/// an extension: the suffix is multi-part, so `set_extension` would mangle it.
///
/// A destination with no final component (`.`, `..`, `/`) is rejected. Such a
/// path cannot name a tarball beside itself, and the previous fallback put one
/// called `cognee.cogx.tar.gz` in the *current working directory* — which for
/// `.` is inside the directory being packed, so `pack_archive` (which creates
/// the file before it enumerates) appended the half-written tarball to itself.
/// The result is a stream Python's `tarfile` reads as a single junk member with
/// no `manifest.json`: a silently useless export.
fn tarball_path(destination: &Path) -> Result<PathBuf, SdkError> {
    let Some(name) = destination.file_name() else {
        return Err(SdkError::Validation(format!(
            "destination '{}' has no final path component, so the archive cannot \
             be named after it; pass a path ending in a directory name",
            destination.display()
        )));
    };
    let mut name = name.to_string_lossy().to_string();
    name.push_str(ARCHIVE_SUFFIX);
    Ok(match destination.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(name),
        _ => PathBuf::from(name),
    })
}

fn opt_string(opts: &serde_json::Value, key: &str) -> Option<String> {
    opts.get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
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
    fn tarball_sits_beside_the_directory() {
        assert_eq!(
            tarball_path(Path::new("/tmp/exports/berlin_cogx")).expect("named"),
            PathBuf::from("/tmp/exports/berlin_cogx.cogx.tar.gz")
        );
    }

    #[test]
    fn multi_part_suffix_is_appended_not_substituted() {
        // set_extension would turn "trip.v2" into "trip.cogx.tar.gz",
        // silently dropping the ".v2" the caller chose.
        assert_eq!(
            tarball_path(Path::new("trip.v2")).expect("named"),
            PathBuf::from("trip.v2.cogx.tar.gz")
        );
    }

    #[test]
    fn a_relative_name_packs_beside_itself_in_the_working_directory() {
        assert_eq!(
            tarball_path(Path::new("berlin_cogx")).expect("named"),
            PathBuf::from("berlin_cogx.cogx.tar.gz")
        );
    }

    #[test]
    fn destinations_with_no_final_component_are_rejected() {
        // Each of these used to yield "cognee.cogx.tar.gz" in the CWD. For "."
        // that is inside the directory being packed, and the tarball ended up
        // inside itself.
        for path in [".", "..", "/", "a/.."] {
            let error = tarball_path(Path::new(path)).expect_err(path);
            assert!(
                matches!(error, SdkError::Validation(_)),
                "{path} should be rejected as invalid, got {error:?}"
            );
        }
    }

    #[test]
    fn opts_treat_blank_strings_as_absent() {
        let opts = serde_json::json!({ "embeddingModel": "  ", "datasetName": "berlin" });
        assert_eq!(opt_string(&opts, "embeddingModel"), None);
        assert_eq!(opt_string(&opts, "datasetName"), Some("berlin".to_string()));
    }

    #[test]
    fn an_empty_destination_is_accepted_and_a_populated_one_is_not() {
        let dir = std::env::temp_dir().join(format!("cogx-guard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        // Absent.
        check_destination(&dir).expect("absent destination is fine");

        // Empty.
        std::fs::create_dir_all(&dir).expect("create");
        check_destination(&dir).expect("empty destination is fine");

        // Holding something that is not ours: this is the disclosure case.
        std::fs::write(dir.join("private-notes.txt"), b"secret").expect("write");
        let error = check_destination(&dir).expect_err("populated destination is rejected");
        assert!(
            matches!(error, SdkError::Validation(_)),
            "expected a validation error, got {error:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_result_carries_exactly_the_documented_keys() {
        // The bindings deserialize this object by name and ignore unknown
        // fields, so a rename here does not fail — it silently yields 0 or null
        // on the other side (ExportResult's counts are ints). Pin the real
        // output of the function the op returns, not a copy of it.
        let value = result_json(Path::new("/tmp/b.cogx.tar.gz"), &sample_summary());
        let object = value.as_object().expect("object");

        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        let mut expected: Vec<&str> = RESULT_KEYS.to_vec();
        expected.sort_unstable();
        assert_eq!(keys, expected, "export_cogx result keys drifted");
    }

    #[test]
    fn the_result_reports_the_summary_it_was_given() {
        // Guards against the counts being wired to the wrong summary field —
        // which no key check would catch, since the names would still match.
        let value = result_json(Path::new("/tmp/b.cogx.tar.gz"), &sample_summary());
        assert_eq!(value["archive"], "/tmp/b.cogx.tar.gz");
        assert_eq!(value["directory"], "/tmp/b");
        assert_eq!(value["numNodes"], 1);
        assert_eq!(value["numEdges"], 2);
        assert_eq!(value["numEntities"], 3);
        assert_eq!(value["numDocuments"], 4);
        assert_eq!(value["numFacts"], 5);
        assert_eq!(value["numRawNodes"], 6);
    }

    fn sample_summary() -> ExportSummary {
        ExportSummary {
            destination: PathBuf::from("/tmp/b"),
            num_nodes: 1,
            num_edges: 2,
            num_entities: 3,
            num_documents: 4,
            num_facts: 5,
            num_raw_nodes: 6,
        }
    }
}
