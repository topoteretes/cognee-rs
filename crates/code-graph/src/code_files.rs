//! The per-file CODE cognify route: one enola run per stored code file.
//!
//! Port of Python's `cognee/tasks/code_graph/code_files.py` (parity pin
//! `a59433e6b`). A source file added on its own is claimed by `code_loader`
//! (`cognee_ingestion::loaders::code`), which stores its text and stamps the
//! row `system_metadata = {"source": "code"}`; cognify routes the row here.
//! enola only accepts directories, so the stored text is staged under the
//! file's original name in a temporary per-file directory and the standard
//! code graph load ([`load_code_graph`]) runs on it. Graph-only, untagged:
//! no LLM, no embeddings.

use std::path::{Path, PathBuf};

use cognee_graph::GraphDBTrait;
use cognee_models::Data;
use cognee_models::code_repo::{SOURCE_CODE, system_metadata_source};

use crate::config::CodeGraphConfig;
use crate::error::{CodeGraphError, CodeGraphResult};
use crate::pipeline::{CodeGraphLoad, load_code_graph};
use crate::writer::CodeGraphLedger;

/// The prefix of the temporary directory a code file is staged in (Python's
/// `tempfile.TemporaryDirectory(prefix="cognee_code_")`).
pub const CODE_FILE_STAGING_PREFIX: &str = "cognee_code_";

/// The `tsconfig.json` enola needs next to plain JavaScript (Python's
/// `_TSCONFIG_JSON`).
const TSCONFIG_JSON: &str = "{\"compilerOptions\": {}}\n";

/// The files staged next to a code file of `extension` (lowercase, no dot)
/// so enola detects its language (Python's `_DETECTION_MARKERS`).
///
/// enola detects projects, not lone files, but since enola 0.4.5 a bare
/// source file of nearly every supported language produces facts on its own.
/// Plain JavaScript is the exception: `.js` / `.jsx` are only claimed once a
/// `tsconfig.json` sits next to them, so exactly that marker is fabricated.
/// Manifests (`package.json`, `go.mod`, ...) are never fabricated: enola
/// turns a manifest's declared dependencies into facts the user's file never
/// had.
pub fn detection_markers(extension: &str) -> &'static [(&'static str, &'static str)] {
    match extension {
        "js" | "jsx" => &[("tsconfig.json", TSCONFIG_JSON)],
        _ => &[],
    }
}

/// Whether `data` is stamped as a code file (`system_metadata.source ==
/// "code"`). Python's `is_code_sourced`.
pub fn is_code_sourced(data: &Data) -> bool {
    system_metadata_source(data.system_metadata.as_deref()).as_deref() == Some(SOURCE_CODE)
}

/// The original file's basename, e.g. `payments.py` (Python's
/// `_original_file_name`: `os.path.basename` of `original_data_location`
/// with trailing `/` stripped).
///
/// enola detects languages by file extension, and only the original location
/// keeps the real one (the stored row's `extension` is content-sniffed), so
/// the staged copy reuses this name.
pub fn original_file_name(data: &Data) -> String {
    let location = data.original_data_location.trim_end_matches('/');
    location.rsplit('/').next().unwrap_or(location).to_string()
}

/// The staged repository's name: `<original file name>_<data id>` (Python's
/// `_staged_repo_name`).
///
/// Code node ids, the stale sweep and the unchanged-snapshot skip are keyed
/// on the repository name, so it must be stable across re-ingestions of the
/// same file and unique per `Data` row: two files named `utils.py` must not
/// share (and sweep) one repository. The full row id gives both.
pub fn staged_repo_name(data: &Data) -> String {
    format!("{}_{}", original_file_name(data), data.id)
}

/// The lowercase extension of `file_name`, without its dot, as Python's
/// `os.path.splitext(name)[1].lstrip(".").lower()` reads it: leading dots do
/// not start an extension.
fn file_extension(file_name: &str) -> String {
    let stem = file_name.trim_start_matches('.');
    stem.rfind('.')
        .map(|dot| stem[dot + 1..].to_lowercase())
        .unwrap_or_default()
}

/// Stage one code file, with its detection markers, for an enola run:
/// `<staging_root>/<staged repo name>/<original file name>` holding
/// `content`. Python's `_stage_code_file`.
///
/// A marker never overwrites the staged file itself, should the file share
/// its name (a user's own `tsconfig.json`).
///
/// # Errors
///
/// [`CodeGraphError::Staging`] when the directory or a file cannot be
/// written (the directory must not exist yet, as Python's `mkdir()`
/// requires).
pub fn stage_code_file(
    data: &Data,
    staging_root: &Path,
    content: &str,
) -> CodeGraphResult<PathBuf> {
    let staging_error = |path: &Path, error: std::io::Error| {
        CodeGraphError::Staging(format!(
            "staging the code file of data item {} at '{}' failed: {error}",
            data.id,
            path.display()
        ))
    };
    let repo_dir = staging_root.join(staged_repo_name(data));
    std::fs::create_dir(&repo_dir).map_err(|error| staging_error(&repo_dir, error))?;
    let file_name = original_file_name(data);
    let file_path = repo_dir.join(&file_name);
    std::fs::write(&file_path, content).map_err(|error| staging_error(&file_path, error))?;
    for (marker_name, marker_content) in detection_markers(&file_extension(&file_name)) {
        if *marker_name != file_name {
            let marker_path = repo_dir.join(marker_name);
            std::fs::write(&marker_path, marker_content)
                .map_err(|error| staging_error(&marker_path, error))?;
        }
    }
    Ok(repo_dir)
}

/// Load the code graph of one stored code file. Python's
/// `extract_code_files_graph` for one data item.
///
/// `content` is the item's stored text, read as Python's text mode reads it
/// (strict UTF-8, universal newlines). It is staged into a temporary
/// directory under its original file name ([`stage_code_file`]), and the
/// standard code graph load ([`load_code_graph`]) runs on that directory:
/// enola, the node write, the edges, sweep and stamp, with the
/// unchanged-snapshot skip. Graph-only and untagged, as in Python: no
/// `node_set` reaches the nodes. The staging directory is removed afterwards.
/// `ledger` records what is written under the item's `Data` row.
///
/// # Errors
///
/// [`CodeGraphError::InvalidCodeItem`] when `data` is not stamped as a code
/// file (it must not reach this route), [`CodeGraphError::Staging`] when the
/// file cannot be staged or its staging directory removed, and what
/// [`load_code_graph`] raises.
pub async fn extract_code_file_graph(
    graph: &dyn GraphDBTrait,
    ledger: Option<&CodeGraphLedger<'_>>,
    config: &CodeGraphConfig,
    data: &Data,
    content: &str,
) -> CodeGraphResult<CodeGraphLoad> {
    if !is_code_sourced(data) {
        return Err(CodeGraphError::InvalidCodeItem(format!(
            "Data item {} is not code-sourced (system_metadata.source != 'code'); it must not \
             reach the CODE route.",
            data.id
        )));
    }

    let staged_data = data.clone();
    let staged_content = content.to_string();
    let (staging, repo_dir) = tokio::task::spawn_blocking(move || {
        let staging = tempfile::Builder::new()
            .prefix(CODE_FILE_STAGING_PREFIX)
            .tempdir()
            .map_err(|error| {
                CodeGraphError::Staging(format!(
                    "creating the staging directory for data item {} failed: {error}",
                    staged_data.id
                ))
            })?;
        let repo_dir = stage_code_file(&staged_data, staging.path(), &staged_content)?;
        Ok::<_, CodeGraphError>((staging, repo_dir))
    })
    .await
    .map_err(|error| CodeGraphError::Staging(format!("staging the code file failed: {error}")))??;

    let load = load_code_graph(graph, ledger, config, &repo_dir, None).await;
    // The staging directory goes either way; a failed load's error wins.
    let removed = tokio::task::spawn_blocking(move || staging.close()).await;
    let load = load?;
    match removed {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            return Err(CodeGraphError::Staging(format!(
                "removing the staging directory of data item {} failed: {error}",
                data.id
            )));
        }
        Err(error) => {
            return Err(CodeGraphError::Staging(format!(
                "removing the staging directory of data item {} failed: {error}",
                data.id
            )));
        }
    }
    tracing::info!(
        "Code graph extracted for {} ({}).",
        original_file_name(data),
        data.id
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
    use uuid::Uuid;

    use super::*;

    fn code_item(original: &str, system_metadata: Option<&str>) -> Data {
        let mut builder = Data::builder(
            Uuid::new_v4(),
            "payments",
            "file:///data/code_abc.py",
            original,
            "txt",
            "text/plain",
            "abc",
            Uuid::new_v4(),
        );
        if let Some(stamp) = system_metadata {
            builder = builder.system_metadata(stamp);
        }
        builder.build()
    }

    #[test]
    fn the_original_file_name_is_the_basename_of_the_original_location() {
        let cases = [
            ("/home/me/shop/payments.py", "payments.py"),
            ("file:///home/me/shop/payments.py", "payments.py"),
            ("/home/me/shop/pkg/", "pkg"),
            ("/home/me/shop/pkg//", "pkg"),
            ("payments.py", "payments.py"),
            ("", ""),
        ];
        for (original, expected) in cases {
            assert_eq!(
                original_file_name(&code_item(original, None)),
                expected,
                "{original}"
            );
        }
    }

    #[test]
    fn the_staged_repo_name_is_the_file_name_and_the_full_row_id() {
        let item = code_item("/src/utils.py", Some(r#"{"source": "code"}"#));
        assert_eq!(staged_repo_name(&item), format!("utils.py_{}", item.id));
        let other = code_item("/elsewhere/utils.py", Some(r#"{"source": "code"}"#));
        assert_ne!(staged_repo_name(&item), staged_repo_name(&other));
    }

    #[test]
    fn extensions_follow_python_splitext() {
        let cases = [
            ("app.JS", "js"),
            ("a.tar.gz", "gz"),
            ("Makefile", ""),
            (".bashrc", ""),
            ("..js", ""),
            (".hidden.jsx", "jsx"),
            ("file.", ""),
        ];
        for (name, expected) in cases {
            assert_eq!(file_extension(name), expected, "{name}");
        }
    }

    #[test]
    fn code_sourced_reads_the_system_metadata_stamp() {
        assert!(is_code_sourced(&code_item(
            "/a.py",
            Some(r#"{"source": "code"}"#)
        )));
        assert!(!is_code_sourced(&code_item("/a.py", None)));
        assert!(!is_code_sourced(&code_item(
            "/a.py",
            Some(r#"{"source": "code_repo"}"#)
        )));
        assert!(!is_code_sourced(&code_item("/a.py", Some("null"))));
    }

    #[test]
    fn javascript_is_staged_with_a_tsconfig_marker() {
        for name in ["app.js", "App.JSX"] {
            let root = tempfile::tempdir().unwrap();
            let item = code_item(&format!("/src/{name}"), None);
            let repo = stage_code_file(&item, root.path(), "export const a = 1;\r\n").unwrap();
            assert_eq!(repo, root.path().join(staged_repo_name(&item)));
            assert_eq!(
                std::fs::read_to_string(repo.join(name)).unwrap(),
                "export const a = 1;\r\n",
                "the content is written as given"
            );
            assert_eq!(
                std::fs::read_to_string(repo.join("tsconfig.json")).unwrap(),
                "{\"compilerOptions\": {}}\n"
            );
            assert_eq!(std::fs::read_dir(&repo).unwrap().count(), 2);
        }
    }

    #[test]
    fn other_languages_are_staged_alone() {
        for name in ["payments.py", "main.go", "lib.rs", "index.ts", "Main.kt"] {
            let root = tempfile::tempdir().unwrap();
            let item = code_item(&format!("/src/{name}"), None);
            let repo = stage_code_file(&item, root.path(), "x\n").unwrap();
            let staged: Vec<String> = std::fs::read_dir(&repo)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            assert_eq!(staged, vec![name.to_string()], "{name}");
        }
    }

    #[test]
    fn staging_never_reuses_an_existing_directory() {
        // Python's `mkdir()`: a second staging of one row into the same root
        // fails rather than overwriting what is there.
        let root = tempfile::tempdir().unwrap();
        let item = code_item("/src/app.js", None);
        stage_code_file(&item, root.path(), "a").unwrap();
        assert!(matches!(
            stage_code_file(&item, root.path(), "b"),
            Err(CodeGraphError::Staging(_))
        ));
        assert_eq!(
            std::fs::read_to_string(root.path().join(staged_repo_name(&item)).join("app.js"))
                .unwrap(),
            "a"
        );
    }

    #[tokio::test]
    async fn an_item_not_stamped_as_code_is_refused() {
        let graph = MockGraphDB::new();
        let item = code_item("/src/app.py", Some(r#"{"source": "code_repo"}"#));
        let error =
            extract_code_file_graph(&graph, None, &CodeGraphConfig::default(), &item, "x = 1\n")
                .await
                .unwrap_err();
        assert!(matches!(error, CodeGraphError::InvalidCodeItem(_)));
        assert_eq!(error.name(), "ValueError");
        assert!(error.to_string().contains("is not code-sourced"), "{error}");
    }
}
