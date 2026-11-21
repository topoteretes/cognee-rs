//! Code-project detection and repo-level ingestion for directory adds.
//!
//! Port of Python's `cognee/tasks/code_graph/code_repo.py` (detection,
//! partition, manifest) and of the directory half of
//! `cognee/tasks/ingestion/resolve_data_directories.py` (parity pin
//! `a59433e6b`).
//!
//! When `add` receives a directory that **is** a code project (its root
//! carries a project marker such as `pyproject.toml` or `go.mod`), flattening
//! it file by file is the wrong granularity. [`resolve_data_directories`]
//! partitions the tree instead:
//!
//! - **one** repo-level manifest item covers the source files and project
//!   manifests. Its `system_metadata` stamp (`{"source": "code_repo", ...}`)
//!   sends it down cognify's CODE_REPO route, which runs one enola pass over
//!   the original directory;
//! - document-like files (`README.md`, docs, PDFs, ...) are emitted
//!   individually and go through their normal loaders and cognify route;
//! - everything else (VCS internals, caches, lockfiles, binaries, dotfiles,
//!   which may hold secrets, and Git LFS pointers) is skipped and counted.
//!
//! The manifest item's raw data is a small JSON document (repo path, covered
//! files, combined content hash), so the stored record changes exactly when
//! the code does. Its data id is pinned to `(user, dataset, repo path)`
//! ([`code_repo_data_id`], applied when the item is stored), so a re-add of
//! the repository refreshes that one record, and a changed one is cognified
//! again. The repository itself is read from its original location at cognify
//! time.
//!
//! A directory that is not a code project is flattened into its files, as
//! Python does.

use std::cmp::Ordering;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

use cognee_models::DataInput;
use sha2::{Digest, Sha256};

/// A directory is a code project when any of these exist at its root.
/// Python's `PROJECT_MARKERS`: enola's own per-language project detection plus
/// `.git`.
pub const PROJECT_MARKERS: &[&str] = &[
    ".git",
    "pyproject.toml",
    "setup.py",
    "requirements.txt",
    "go.mod",
    "package.json",
    "Cargo.toml",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    "Package.swift",
    "Gemfile",
    "composer.json",
    "pubspec.yaml",
    "CMakeLists.txt",
];

/// Project manifests and build files the repo item covers (enola reads them
/// for detection and dependency facts). Python's `PROJECT_MANIFEST_FILES`:
/// the markers without `.git`, plus `go.sum`, `tsconfig.json` and the
/// makefiles.
pub const PROJECT_MANIFEST_FILES: &[&str] = &[
    "pyproject.toml",
    "setup.py",
    "requirements.txt",
    "go.mod",
    "package.json",
    "Cargo.toml",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    "Package.swift",
    "Gemfile",
    "composer.json",
    "pubspec.yaml",
    "CMakeLists.txt",
    "go.sum",
    "tsconfig.json",
    "Makefile",
    "makefile",
];

/// Directories never worth ingesting from a working tree. A path with a
/// component in this set, or with any hidden (dot-prefixed) component, is
/// skipped. Python's `SKIP_DIRS`.
pub const SKIP_DIRS: &[&str] = &[
    "__pycache__",
    "node_modules",
    "venv",
    "dist",
    "build",
    "target",
    "coverage",
];

/// Audio and video inside a code project are demo material, not knowledge,
/// and skipped. Python's `REPO_SKIPPED_MEDIA_EXTENSIONS`.
pub const REPO_SKIPPED_MEDIA_EXTENSIONS: &[&str] = &[
    "aac", "aiff", "amr", "avi", "flac", "m4a", "m4v", "mid", "mkv", "mov", "mp3", "mp4", "ogg",
    "wav", "webm",
];

/// Source-file extensions of the languages enola extracts. Python's
/// `SUPPORTED_CODE_EXTENSIONS` (`cognee/infrastructure/loaders/core/code_loader.py`).
pub const SUPPORTED_CODE_EXTENSIONS: &[&str] = &[
    "c", "cc", "cpp", "cs", "cxx", "dart", "fs", "go", "h", "hcl", "hh", "hpp", "java", "js",
    "jsx", "kt", "kts", "php", "proto", "py", "rake", "rb", "rs", "scala", "svelte", "swift", "tf",
    "ts", "tsx", "vb", "vue",
];

/// Every extension a non-code loader claims by name. Python's
/// `_document_extensions()` evaluated over its default loaders (text, PDF,
/// image, audio, video, CSV, unstructured, BeautifulSoup) minus
/// [`SUPPORTED_CODE_EXTENSIONS`]; the same set Rust's add-time loaders name
/// explicitly ([`crate::get_loader_name`]). Keyed on extension only: a file
/// whose extension no loader names is junk in a repository, not a document.
#[rustfmt::skip]
pub const DOCUMENT_EXTENSIONS: &[&str] = &[
    // text_loader
    "txt", "md", "json", "xml", "yaml", "yml", "log",
    // pypdf_loader
    "pdf",
    // image_loader
    "png", "dwg", "xcf", "jpg", "jpe", "jpeg", "jpx", "apng", "gif", "webp", "cr2", "tif", "tiff",
    "bmp", "jxr", "psd", "ico", "heic", "avif",
    // audio_loader
    "aac", "mid", "mp3", "m4a", "ogg", "flac", "wav", "amr", "aiff",
    // video_loader
    "mp4", "m4v", "mov", "webm", "mkv", "avi",
    // csv_loader
    "csv",
    // unstructured_loader
    "docx", "doc", "odt", "xlsx", "xls", "ods", "pptx", "ppt", "odp", "rtf", "html", "htm", "eml",
    "msg", "epub",
];

pub use cognee_models::code_repo::{CODE_REPO_MANIFEST_KIND, SOURCE_CODE_REPO};

/// Git LFS leaves a file it did not download as a small pointer (under 1024
/// bytes: a version line, optional ext lines, then oid and size). Python's
/// `_LFS_POINTER`, matched in full.
#[allow(
    clippy::expect_used,
    reason = "the pattern is a compile-time constant, compiled by the unit tests"
)]
static LFS_POINTER: LazyLock<regex::bytes::Regex> = LazyLock::new(|| {
    regex::bytes::Regex::new(
        r"(?-u)\Aversion https://git-lfs\.github\.com/spec/v1\n(?:ext-\d+-\w+ sha256:[0-9a-f]{64}\n)*oid sha256:[0-9a-f]{64}\nsize \d+\n?\z",
    )
    .expect("the LFS pointer pattern is a valid regex")
});
const LFS_POINTER_MAX_BYTES: u64 = 1024;

/// Whether a directory's root carries a code-project marker. Python's
/// `detect_code_project` (`Path.exists`, which follows symlinks).
pub fn detect_code_project(directory: &Path) -> bool {
    PROJECT_MARKERS
        .iter()
        .any(|marker| directory.join(marker).exists())
}

/// Whether one path component names a skipped directory or a hidden entry.
fn is_skipped_name(part: &std::ffi::OsStr) -> bool {
    let part = part.to_string_lossy();
    SKIP_DIRS.contains(&part.as_ref()) || part.starts_with('.')
}

fn is_skipped_path(relative: &Path) -> bool {
    relative.components().any(|component| match component {
        Component::Normal(part) => is_skipped_name(part),
        _ => false,
    })
}

/// Python's `PurePath.suffix` without its dot, lowercased: the text after the
/// last dot of the file name, when that dot is neither the first nor the last
/// character.
fn extension_of(path: &Path) -> String {
    let Some(name) = path.file_name() else {
        return String::new();
    };
    let name = name.to_string_lossy();
    match name.rfind('.') {
        Some(index) if index > 0 && index < name.len() - 1 => name[index + 1..].to_lowercase(),
        _ => String::new(),
    }
}

/// Whether `file_path` is a Git LFS pointer rather than the file it stands
/// for. Python's `_is_lfs_pointer`.
fn is_lfs_pointer(file_path: &Path, size: u64) -> io::Result<bool> {
    if size >= LFS_POINTER_MAX_BYTES {
        return Ok(false);
    }
    Ok(LFS_POINTER.is_match(&fs::read(file_path)?))
}

/// Order paths as Python orders `Path` objects: component by component, not
/// as strings (so `a/b` sorts before `a.txt`).
fn python_path_order(left: &Path, right: &Path) -> Ordering {
    left.components().cmp(right.components())
}

/// A directory walk's output.
#[derive(Debug, Default)]
struct Walk {
    /// Every entry listed, as Python's `directory.rglob("*")` lists it:
    /// files, directories and symlinks, without descending into symlinked
    /// directories.
    entries: Vec<PathBuf>,
    /// Directories not descended into because their name is skipped
    /// ([`SKIP_DIRS`] or hidden), when the walk prunes.
    pruned: Vec<PathBuf>,
}

/// Walk `directory`. With `prune`, a directory whose name is skipped is
/// recorded in [`Walk::pruned`] and not descended into: everything under it
/// would be skipped anyway, and a `node_modules` or `.git` can be huge.
///
/// Like Python's `rglob`, a subdirectory that cannot be listed, an entry that
/// cannot be read and an entry that vanished mid-walk are skipped (logged)
/// rather than failing the walk.
///
/// # Errors
///
/// `directory` itself cannot be listed.
fn walk(directory: &Path, prune: bool) -> io::Result<Walk> {
    let mut walk = Walk::default();
    walk_entries(fs::read_dir(directory)?, prune, &mut walk);
    Ok(walk)
}

fn walk_entries(entries: fs::ReadDir, prune: bool, walk: &mut Walk) {
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                tracing::warn!("Skipping an unreadable directory entry: {error}");
                continue;
            }
        };
        let path = entry.path();
        // Does not follow symlinks: a symlinked directory is not descended.
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                tracing::warn!("Skipping '{}': {error}", path.display());
                continue;
            }
        };
        if file_type.is_dir() && prune && is_skipped_name(&entry.file_name()) {
            walk.pruned.push(path);
            continue;
        }
        walk.entries.push(path.clone());
        if file_type.is_dir() {
            match fs::read_dir(&path) {
                Ok(children) => walk_entries(children, prune, walk),
                Err(error) => {
                    tracing::warn!(
                        "Skipping unreadable directory '{}': {error}",
                        path.display()
                    );
                }
            }
        }
    }
}

/// A project tree split by [`partition_repo_files`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoPartition {
    /// Source files and project manifests: represented by the one repo item
    /// and consumed by enola.
    pub covered: Vec<PathBuf>,
    /// Files whose extension a non-code loader claims: added individually.
    pub documents: Vec<PathBuf>,
    /// Hidden files, build junk, unclaimed extensions, skipped media,
    /// symlinks, Git LFS pointers and unreadable files: never ingested. A
    /// hidden or [`SKIP_DIRS`] directory is listed once, as the directory,
    /// since it is not walked.
    pub skipped: Vec<PathBuf>,
}

/// Partition a project tree into covered, document and skipped files.
/// Python's `partition_repo_files`.
///
/// Symlinks are skipped rather than followed: a repository containing
/// `creds.py -> ~/.aws/credentials` must not pull the target into the
/// manifest. Hidden and [`SKIP_DIRS`] directories are not walked. An
/// unreadable subdirectory or file, or one that vanishes mid-walk, is
/// skipped with a warning (Python's `rglob` skips what it cannot list).
///
/// # Errors
///
/// `directory` itself cannot be listed.
pub fn partition_repo_files(directory: &Path) -> io::Result<RepoPartition> {
    let Walk {
        mut entries,
        pruned,
    } = walk(directory, true)?;
    entries.sort_by(|left, right| python_path_order(left, right));

    let mut partition = RepoPartition::default();
    for file_path in entries {
        let metadata = match fs::symlink_metadata(&file_path) {
            Ok(metadata) => metadata,
            Err(error) => {
                tracing::warn!("Skipping '{}': {error}", file_path.display());
                continue;
            }
        };
        if metadata.file_type().is_symlink() {
            partition.skipped.push(file_path);
            continue;
        }
        if !metadata.is_file() {
            continue;
        }
        let relative = file_path.strip_prefix(directory).unwrap_or(&file_path);
        if is_skipped_path(relative) {
            partition.skipped.push(file_path);
            continue;
        }
        let extension = extension_of(&file_path);
        let file_name = file_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        if SUPPORTED_CODE_EXTENSIONS.contains(&extension.as_str())
            || PROJECT_MANIFEST_FILES.contains(&file_name.as_str())
        {
            partition.covered.push(file_path);
        } else if REPO_SKIPPED_MEDIA_EXTENSIONS.contains(&extension.as_str()) {
            partition.skipped.push(file_path);
        } else if DOCUMENT_EXTENSIONS.contains(&extension.as_str()) {
            match is_lfs_pointer(&file_path, metadata.len()) {
                Ok(false) => partition.documents.push(file_path),
                Ok(true) => partition.skipped.push(file_path),
                Err(error) => {
                    tracing::warn!("Skipping unreadable '{}': {error}", file_path.display());
                    partition.skipped.push(file_path);
                }
            }
        } else {
            partition.skipped.push(file_path);
        }
    }
    partition.skipped.extend(pruned);
    partition
        .skipped
        .sort_by(|left, right| python_path_order(left, right));
    Ok(partition)
}

/// A string as Python's `json.dumps` writes it (`ensure_ascii=True`):
/// non-ASCII characters escaped as `\uXXXX`, astral ones as surrogate pairs.
fn py_json_string(text: &str) -> String {
    let encoded = serde_json::Value::String(text.to_owned()).to_string();
    let mut out = String::with_capacity(encoded.len());
    for character in encoded.chars() {
        if character.is_ascii() {
            out.push(character);
        } else {
            let mut units = [0u16; 2];
            for unit in character.encode_utf16(&mut units) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    out
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// The repo item's raw data: the covered files and a combined content hash,
/// byte for byte as Python's `build_repo_manifest` writes it
/// (`json.dumps(..., indent=2)`).
///
/// The hash covers every covered file's content, so the manifest text, and
/// therefore the record's content hash, changes exactly when the code does.
///
/// # Errors
///
/// A covered file that cannot be read.
pub fn build_repo_manifest(directory: &Path, covered: &[PathBuf]) -> io::Result<String> {
    let mut combined = Sha256::new();
    let mut files = Vec::with_capacity(covered.len());
    for file_path in covered {
        let relative = path_text(file_path.strip_prefix(directory).unwrap_or(file_path));
        let digest = hex::encode(Sha256::digest(fs::read(file_path)?));
        // Python's `len(relative)` counts characters, not bytes.
        combined.update(format!("{}:{relative}:{digest}", relative.chars().count()).as_bytes());
        files.push(relative);
    }

    let files_json = if files.is_empty() {
        "[]".to_string()
    } else {
        let lines: Vec<String> = files
            .iter()
            .map(|file| format!("    {}", py_json_string(file)))
            .collect();
        format!("[\n{}\n  ]", lines.join(",\n"))
    };
    Ok(format!(
        "{{\n  \"kind\": \"{CODE_REPO_MANIFEST_KIND}\",\n  \"repo_path\": {},\n  \"file_count\": {},\n  \"files\": {},\n  \"content_hash\": \"{}\"\n}}",
        py_json_string(&path_text(directory)),
        files.len(),
        files_json,
        hex::encode(combined.finalize()),
    ))
}

/// The `system_metadata` stamp of a repo item, as Python's `json.dumps`
/// writes the dict: `{"source": "code_repo", "repo_path": ..., "file_count": ...}`,
/// plus `"repo_url"` (credential-free) for a cloned remote repository.
pub fn code_repo_system_metadata(
    directory: &Path,
    file_count: usize,
    source_url: Option<&str>,
) -> String {
    let repo_url = source_url
        .map(|url| format!(", \"repo_url\": {}", py_json_string(url)))
        .unwrap_or_default();
    format!(
        "{{\"source\": \"{SOURCE_CODE_REPO}\", \"repo_path\": {}, \"file_count\": {file_count}{repo_url}}}",
        py_json_string(&path_text(directory))
    )
}

/// The pinned data id of a code repository's manifest: Python's
/// `get_unique_data_id(f"code_repo:{directory}", user, dataset_id)`
/// (`resolve_code_repository`). Re-adding the same repository to the same
/// dataset as the same user then updates one record instead of adding one
/// per version of the code.
pub fn code_repo_data_id(
    repo_path: &str,
    user_id: uuid::Uuid,
    tenant_id: Option<uuid::Uuid>,
    dataset_id: uuid::Uuid,
) -> uuid::Uuid {
    crate::id_generation::generate_unique_data_id(
        &format!("code_repo:{repo_path}"),
        user_id,
        tenant_id,
        Some(dataset_id),
    )
}

/// The `repo_path` of a code-repository route stamp (`system_metadata` with
/// `source == "code_repo"`), `None` for any other stamp.
pub fn code_repo_stamp_path(system_metadata: Option<&str>) -> Option<String> {
    let stamp: serde_json::Value = serde_json::from_str(system_metadata?).ok()?;
    if stamp.get("source").and_then(serde_json::Value::as_str) != Some(SOURCE_CODE_REPO) {
        return None;
    }
    stamp
        .get("repo_path")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

/// The identity a code-repository route stamp pins its data id to
/// ([`code_repo_data_id`]): its `repo_path`, except for a cloned remote
/// repository (a stamp with `repo_url`), which is keyed on the default
/// branch's clone directory, as Python passes `identity=clone_dir(source_url)`
/// (`resolve_code_repository_url`). Rows of one URL keep one id whichever
/// branch was cloned, since their code graphs share the repository's node
/// ids. `None` for any other stamp.
pub fn code_repo_stamp_identity(system_metadata: Option<&str>) -> Option<String> {
    let repo_path = code_repo_stamp_path(system_metadata)?;
    let stamp: serde_json::Value = serde_json::from_str(system_metadata?).ok()?;
    let Some(repo_url) = stamp.get("repo_url").and_then(serde_json::Value::as_str) else {
        return Some(repo_path);
    };
    let clones_dir = Path::new(&repo_path).parent()?;
    Some(path_text(&crate::resolve_repo::clone_dir(
        repo_url, None, clones_dir,
    )))
}

/// What [`resolve_code_repository`] makes of a project directory.
#[derive(Debug, Clone)]
pub struct ResolvedCodeRepository {
    /// The one repo-level manifest item, stamped for the CODE_REPO route.
    pub manifest_item: DataInput,
    /// The project's document files, to be added individually.
    pub document_paths: Vec<PathBuf>,
    /// How many files were skipped (documents excluded for want of an LLM
    /// included).
    pub skipped: usize,
}

/// Build the repo-level item (and the document file list) for a project
/// directory. Python's `resolve_code_repository`.
///
/// Without a usable LLM (`llm_available == false`) the project's documents
/// are excluded and logged instead of emitted: their routes need an LLM
/// (images are described at add time, text is extracted at cognify), so they
/// would only fail later. The code graph itself never needs one.
/// `include_documents == false` returns no document paths at all, for callers
/// that index the code graph only.
///
/// The manifest's data id is pinned to `(user, dataset, repo path)` when it
/// is stored ([`code_repo_data_id`]), so a re-add of the same repository
/// updates one record. `source_url` (credential-free) records where a cloned
/// repository came from as `system_metadata["repo_url"]`.
///
/// # Errors
///
/// What [`partition_repo_files`] and [`build_repo_manifest`] raise.
pub fn resolve_code_repository(
    directory: &Path,
    include_documents: bool,
    llm_available: bool,
    source_url: Option<&str>,
) -> io::Result<ResolvedCodeRepository> {
    let RepoPartition {
        covered,
        mut documents,
        mut skipped,
    } = partition_repo_files(directory)?;

    if !include_documents {
        documents.clear();
    } else if !documents.is_empty() && !llm_available {
        tracing::warn!(
            "No LLM API key configured (LLM_API_KEY): excluding {} document file(s) of the code \
             project from processing — their pipelines need an LLM (image transcription at add \
             time, text extraction at cognify). The code graph is unaffected (enola is \
             LLM-free). Set LLM_API_KEY and re-add the repository to ingest its documents.",
            documents.len()
        );
        skipped.append(&mut documents);
    }

    let manifest_text = build_repo_manifest(directory, &covered)?;
    let directory_name = directory
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let manifest_item = DataInput::DataItem {
        data: Box::new(DataInput::Text(manifest_text)),
        label: Some(format!("code_repo:{directory_name}")),
        external_metadata: None,
        system_metadata: Some(code_repo_system_metadata(
            directory,
            covered.len(),
            source_url,
        )),
    };

    tracing::info!(
        "Code project detected: 1 repo item covering {} file(s), {} document(s), {} skipped.",
        covered.len(),
        documents.len(),
        skipped.len()
    );
    Ok(ResolvedCodeRepository {
        manifest_item,
        document_paths: documents,
        skipped: skipped.len(),
    })
}

/// Every regular file under `directory`, recursively (Python's
/// `rglob("*")` + `is_file()`), in Python's path order. Hidden directories
/// are walked (only a code project skips them); unreadable ones are skipped.
fn flatten_directory(directory: &Path) -> io::Result<Vec<PathBuf>> {
    let mut entries = walk(directory, false)?.entries;
    entries.sort_by(|left, right| python_path_order(left, right));
    // `is_file` follows symlinks, as Python's does.
    Ok(entries.into_iter().filter(|path| path.is_file()).collect())
}

/// A directory input that could not be resolved.
#[derive(Debug, thiserror::Error)]
#[error("could not read directory '{path}': {source}")]
pub struct ResolveDirectoryError {
    /// The directory given to `add`.
    pub path: String,
    /// The I/O failure.
    #[source]
    pub source: io::Error,
}

/// Replace every directory among `inputs` with what it holds. Python's
/// `resolve_data_directories` for local paths (recursive, the default
/// `include_subdirectories=True`).
///
/// A directory that is a code project ([`detect_code_project`]) resolves to
/// one repo manifest item plus its documents ([`resolve_code_repository`]);
/// any other directory is flattened into its files. Only a top-level
/// [`DataInput::FilePath`] naming an existing directory is touched; everything
/// else passes through unchanged.
///
/// Blocking: walks and hashes files. Call it off the async executor.
///
/// # Errors
///
/// [`ResolveDirectoryError`] for a directory that cannot be read.
pub fn resolve_data_directories(
    inputs: Vec<DataInput>,
    llm_available: bool,
) -> Result<Vec<DataInput>, ResolveDirectoryError> {
    let mut resolved = Vec::with_capacity(inputs.len());
    for input in inputs {
        let DataInput::FilePath(path) = &input else {
            resolved.push(input);
            continue;
        };
        let local = Path::new(path.strip_prefix("file://").unwrap_or(path));
        if !local.is_dir() {
            resolved.push(input);
            continue;
        }
        let error = |source: io::Error| ResolveDirectoryError {
            path: path.clone(),
            source,
        };
        // Python resolves the path (`resolve_local_path`) before it is
        // recorded as the manifest's `repo_path`.
        let directory = fs::canonicalize(local).map_err(error)?;
        if detect_code_project(&directory) {
            let repository =
                resolve_code_repository(&directory, true, llm_available, None).map_err(error)?;
            resolved.push(repository.manifest_item);
            resolved.extend(
                repository
                    .document_paths
                    .iter()
                    .map(|document| DataInput::FilePath(path_text(document))),
            );
        } else {
            resolved.extend(
                flatten_directory(&directory)
                    .map_err(error)?
                    .iter()
                    .map(|file| DataInput::FilePath(path_text(file))),
            );
        }
    }
    Ok(resolved)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    fn write(root: &Path, relative: &str, content: &[u8]) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn names(root: &Path, paths: &[PathBuf]) -> Vec<String> {
        paths
            .iter()
            .map(|path| path_text(path.strip_prefix(root).unwrap()))
            .collect()
    }

    // Python: test_project_marker_detection / test_git_directory_is_a_project_marker
    #[test]
    fn project_markers_are_detected_at_the_root_only() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!detect_code_project(dir.path()));
        write(dir.path(), "sub/pyproject.toml", b"");
        assert!(!detect_code_project(dir.path()));
        write(dir.path(), "go.mod", b"module x\n");
        assert!(detect_code_project(dir.path()));

        let git = tempfile::tempdir().unwrap();
        fs::create_dir(git.path().join(".git")).unwrap();
        assert!(detect_code_project(git.path()));
    }

    // Python: test_partition_buckets
    #[test]
    fn partition_buckets() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "pyproject.toml", b"[project]\n");
        write(root, "pkg/mod.py", b"def f(): pass\n");
        write(root, "pkg/sub/x.ts", b"export const x = 1;\n");
        write(root, "Makefile", b"all:\n");
        write(root, "README.md", b"# readme\n");
        write(root, "docs/guide.pdf", b"%PDF-1.4");
        write(root, "logo.png", b"\x89PNG");
        write(root, "promo.mp4", b"\x00\x00");
        write(root, ".env", b"SECRET=1\n");
        write(root, ".git/config", b"[core]\n");
        write(root, "node_modules/lib/index.js", b"x\n");
        write(root, "pkg/__pycache__/mod.cpython-312.pyc", b"\x00");
        write(root, "uv.lock", b"lock\n");
        write(root, "bin/tool.exe", b"\x00");

        let partition = partition_repo_files(root).unwrap();
        assert_eq!(
            names(root, &partition.covered),
            vec!["Makefile", "pkg/mod.py", "pkg/sub/x.ts", "pyproject.toml"]
        );
        assert_eq!(
            names(root, &partition.documents),
            vec!["README.md", "docs/guide.pdf", "logo.png"]
        );
        // Python lists every file under a skipped directory; the directory
        // is not walked here, so it is listed once instead.
        let mut skipped = names(root, &partition.skipped);
        skipped.sort();
        assert_eq!(
            skipped,
            vec![
                ".env",
                ".git",
                "bin/tool.exe",
                "node_modules",
                "pkg/__pycache__",
                "promo.mp4",
                "uv.lock",
            ]
        );
    }

    #[test]
    fn skipped_directories_are_not_walked() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "go.mod", b"module x\n");
        write(root, "node_modules/lib/deep/index.js", b"x\n");
        write(root, ".git/objects/ab/cdef", b"\x00");
        write(root, "src/.cache/x.go", b"package x\n");
        write(root, "src/main.go", b"package main\n");

        let walked = walk(root, true).unwrap();
        let entries = names(root, &walked.entries);
        assert!(
            !entries
                .iter()
                .any(|entry| entry.starts_with("node_modules"))
        );
        assert!(!entries.iter().any(|entry| entry.starts_with(".git")));
        assert!(!entries.iter().any(|entry| entry.contains(".cache")));
        let mut pruned = names(root, &walked.pruned);
        pruned.sort();
        assert_eq!(pruned, vec![".git", "node_modules", "src/.cache"]);

        // A plain directory flattens everything, hidden directories included.
        let flat = flatten_directory(root).unwrap();
        assert!(
            flat.iter()
                .any(|path| path.ends_with("node_modules/lib/deep/index.js"))
        );
        assert!(
            flat.iter()
                .any(|path| path.ends_with(".git/objects/ab/cdef"))
        );
    }

    // Python's rglob skips a directory it cannot list.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_directory_is_skipped() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "go.mod", b"module x\n");
        write(root, "src/main.go", b"package main\n");
        write(root, "locked/secret.go", b"package locked\n");
        write(root, "README.md", b"# r\n");
        let locked = root.join("locked");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read_dir(&locked).is_ok() {
            // Running as root: permissions are not enforced.
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
            return;
        }

        let partition = partition_repo_files(root);
        let flattened = flatten_directory(root);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();

        let partition = partition.unwrap();
        assert_eq!(
            names(root, &partition.covered),
            vec!["go.mod", "src/main.go"]
        );
        assert_eq!(names(root, &partition.documents), vec!["README.md"]);
        let flattened = flattened.unwrap();
        assert_eq!(flattened.len(), 3, "{flattened:?}");
    }

    #[test]
    fn an_unlistable_root_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(partition_repo_files(&dir.path().join("missing")).is_err());
    }

    // Python: test_lfs_pointers_of_documents_are_skipped
    #[test]
    fn lfs_pointers_of_documents_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "go.mod", b"module x\n");
        let oid = "a".repeat(64);
        write(
            root,
            "docs/manual.pdf",
            format!("version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize 12345\n")
                .as_bytes(),
        );
        write(
            root,
            "docs/ext.pdf",
            format!(
                "version https://git-lfs.github.com/spec/v1\next-0-foo sha256:{oid}\noid sha256:{oid}\nsize 1"
            )
            .as_bytes(),
        );
        write(
            root,
            "docs/real.md",
            b"version https://git-lfs.github.com/spec/v1\nbut then prose\n",
        );
        let partition = partition_repo_files(root).unwrap();
        assert_eq!(names(root, &partition.documents), vec!["docs/real.md"]);
        assert!(names(root, &partition.skipped).contains(&"docs/manual.pdf".to_string()));
        assert!(names(root, &partition.skipped).contains(&"docs/ext.pdf".to_string()));
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_not_followed_into_the_manifest() {
        let outside = tempfile::tempdir().unwrap();
        write(outside.path(), "credentials", b"secret\n");
        write(outside.path(), "lib/inner.py", b"x = 1\n");
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "go.mod", b"module x\n");
        std::os::unix::fs::symlink(outside.path().join("credentials"), root.join("creds.py"))
            .unwrap();
        std::os::unix::fs::symlink(outside.path().join("lib"), root.join("lib")).unwrap();
        let partition = partition_repo_files(root).unwrap();
        assert_eq!(names(root, &partition.covered), vec!["go.mod"]);
        let manifest = build_repo_manifest(root, &partition.covered).unwrap();
        assert!(!manifest.contains("creds.py"));
        assert!(!manifest.contains("inner.py"));
    }

    #[test]
    fn paths_sort_by_component_like_python() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "a.py", b"");
        write(root, "a/b.py", b"");
        write(root, "a-b.py", b"");
        let partition = partition_repo_files(root).unwrap();
        assert_eq!(
            names(root, &partition.covered),
            vec!["a/b.py", "a-b.py", "a.py"]
        );
    }

    #[test]
    fn suffix_follows_python() {
        assert_eq!(extension_of(Path::new("x/a.PY")), "py");
        assert_eq!(extension_of(Path::new("x/.bashrc")), "");
        assert_eq!(extension_of(Path::new("x/a.")), "");
        assert_eq!(extension_of(Path::new("x/a.tar.gz")), "gz");
        assert_eq!(extension_of(Path::new("Makefile")), "");
    }

    /// The manifest text Python writes for the same tree, byte for byte.
    /// Expected value computed with Python's `build_repo_manifest` at the pin
    /// for this layout (`json.dumps(..., indent=2)` and the combined hash).
    #[test]
    fn manifest_matches_python_format() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "go.mod", b"module x\n");
        write(root, "pkg/m\u{e9}.go", b"package pkg\n");
        let partition = partition_repo_files(root).unwrap();
        let manifest = build_repo_manifest(root, &partition.covered).unwrap();

        let mut combined = Sha256::new();
        for (relative, content) in [
            ("go.mod", &b"module x\n"[..]),
            ("pkg/m\u{e9}.go", &b"package pkg\n"[..]),
        ] {
            let digest = hex::encode(Sha256::digest(content));
            combined.update(format!("{}:{relative}:{digest}", relative.chars().count()));
        }
        let expected = format!(
            "{{\n  \"kind\": \"code_repo_manifest\",\n  \"repo_path\": {},\n  \"file_count\": 2,\n  \"files\": [\n    \"go.mod\",\n    \"pkg/m\\u00e9.go\"\n  ],\n  \"content_hash\": \"{}\"\n}}",
            serde_json::Value::String(path_text(root)),
            hex::encode(combined.finalize())
        );
        assert_eq!(manifest, expected);
        // A valid JSON document carrying the decoded path.
        let parsed: serde_json::Value = serde_json::from_str(&manifest).unwrap();
        assert_eq!(parsed["files"][1], "pkg/m\u{e9}.go");
        assert_eq!(
            build_repo_manifest(root, &[])
                .unwrap()
                .matches("[]")
                .count(),
            1
        );
    }

    /// Python's `build_repo_manifest` run over the same tree wrote this file
    /// list and `content_hash` (the hash does not depend on the repo path).
    #[test]
    fn manifest_hash_matches_python_output() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "go.mod", b"module x\n");
        write(root, "pkg/m\u{e9}.go", b"package pkg\n");
        write(root, "a.py", b"");
        write(root, "a/b.py", b"");
        write(root, "a-b.py", b"");
        write(root, "README.md", b"# r\n");
        let partition = partition_repo_files(root).unwrap();
        let manifest = build_repo_manifest(root, &partition.covered).unwrap();
        let tail = "  \"file_count\": 5,\n  \"files\": [\n    \"a/b.py\",\n    \"a-b.py\",\n    \
                    \"a.py\",\n    \"go.mod\",\n    \"pkg/m\\u00e9.go\"\n  ],\n  \"content_hash\": \
                    \"2eb886bd4d526b98c5814b2e911614255964953402da7d2b6447c8f9b2bd1742\"\n}";
        assert!(manifest.ends_with(tail), "{manifest}");
    }

    // Python: test_manifest_hash_tracks_code_content
    #[test]
    fn manifest_hash_tracks_code_content() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "go.mod", b"module x\n");
        write(root, "main.go", b"package main\n");
        let covered = partition_repo_files(root).unwrap().covered;
        let first = build_repo_manifest(root, &covered).unwrap();
        assert_eq!(first, build_repo_manifest(root, &covered).unwrap());
        write(root, "main.go", b"package main\nfunc main() {}\n");
        assert_ne!(first, build_repo_manifest(root, &covered).unwrap());
    }

    #[test]
    fn the_manifest_pin_reads_the_code_repo_stamp() {
        let stamp = code_repo_system_metadata(Path::new("/src/shop"), 2, None);
        assert_eq!(
            code_repo_stamp_path(Some(&stamp)).as_deref(),
            Some("/src/shop")
        );
        assert_eq!(code_repo_stamp_path(Some(r#"{"source": "code"}"#)), None);
        assert_eq!(
            code_repo_stamp_path(Some(r#"{"source": "code_repo"}"#)),
            None
        );
        assert_eq!(code_repo_stamp_path(Some("not json")), None);
        assert_eq!(code_repo_stamp_path(None), None);

        let (user, dataset) = (uuid::Uuid::from_u128(1), uuid::Uuid::from_u128(2));
        assert_eq!(
            code_repo_data_id("/src/shop", user, None, dataset),
            crate::generate_unique_data_id("code_repo:/src/shop", user, None, Some(dataset))
        );
        assert_ne!(
            code_repo_data_id("/src/shop", user, None, dataset),
            code_repo_data_id("/src/shop", user, None, uuid::Uuid::from_u128(3))
        );
    }

    #[test]
    fn system_metadata_is_python_json() {
        assert_eq!(
            code_repo_system_metadata(Path::new("/r\u{e9}po"), 3, None),
            "{\"source\": \"code_repo\", \"repo_path\": \"/r\\u00e9po\", \"file_count\": 3}"
        );
        // Python appends `repo_url` last for a cloned repository.
        assert_eq!(
            code_repo_system_metadata(
                Path::new("/c/github.com-o-r"),
                1,
                Some("https://github.com/o/r")
            ),
            "{\"source\": \"code_repo\", \"repo_path\": \"/c/github.com-o-r\", \"file_count\": 1, \"repo_url\": \"https://github.com/o/r\"}"
        );
    }

    // Python: resolve_code_repository_url keys every branch of a URL on the
    // default branch's clone (`identity=clone_dir(source_url)`).
    #[test]
    fn a_cloned_repository_is_pinned_to_its_default_clone() {
        let url = "https://github.com/o/r";
        let branch_clone = crate::resolve_repo::clone_dir(url, Some("dev"), Path::new("/c"));
        let stamp = code_repo_system_metadata(&branch_clone, 1, Some(url));
        assert_eq!(
            code_repo_stamp_identity(Some(&stamp)).as_deref(),
            Some("/c/github.com-o-r")
        );
        let local = code_repo_system_metadata(Path::new("/src/shop"), 1, None);
        assert_eq!(
            code_repo_stamp_identity(Some(&local)).as_deref(),
            Some("/src/shop")
        );
        assert_eq!(
            code_repo_stamp_identity(Some(r#"{"source": "code"}"#)),
            None
        );
    }

    // Python: test_directory_with_project_resolves_to_repo_item_plus_documents
    #[test]
    fn a_project_directory_resolves_to_one_repo_item_plus_documents() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        write(&root, "pyproject.toml", b"[project]\n");
        write(&root, "app.py", b"print(1)\n");
        write(&root, "README.md", b"# hi\n");
        let resolved = resolve_data_directories(
            vec![
                DataInput::FilePath(path_text(&root)),
                DataInput::Text("plain".into()),
            ],
            true,
        )
        .unwrap();
        assert_eq!(resolved.len(), 3);
        let DataInput::DataItem {
            data,
            label,
            system_metadata,
            external_metadata,
        } = &resolved[0]
        else {
            panic!("expected the manifest item first, got {:?}", resolved[0]);
        };
        let root_name = root.file_name().unwrap().to_string_lossy();
        assert_eq!(
            label.as_deref(),
            Some(format!("code_repo:{root_name}").as_str())
        );
        assert!(external_metadata.is_none());
        let stamp: serde_json::Value =
            serde_json::from_str(system_metadata.as_deref().unwrap()).unwrap();
        assert_eq!(stamp["source"], "code_repo");
        assert_eq!(stamp["repo_path"], path_text(&root));
        assert_eq!(stamp["file_count"], 2);
        let DataInput::Text(manifest) = data.as_ref() else {
            panic!("manifest is text");
        };
        let manifest: serde_json::Value = serde_json::from_str(manifest).unwrap();
        assert_eq!(
            manifest["files"],
            serde_json::json!(["app.py", "pyproject.toml"])
        );
        assert!(matches!(&resolved[1], DataInput::FilePath(path) if path.ends_with("README.md")));
        assert!(matches!(&resolved[2], DataInput::Text(text) if text == "plain"));
    }

    // Python: test_documents_excluded_without_llm_api_key
    #[test]
    fn documents_are_excluded_without_an_llm() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "go.mod", b"module x\n");
        write(root, "README.md", b"# hi\n");
        let resolved = resolve_code_repository(root, true, false, None).unwrap();
        assert!(resolved.document_paths.is_empty());
        assert_eq!(resolved.skipped, 1);
        let with_llm = resolve_code_repository(root, true, true, None).unwrap();
        assert_eq!(with_llm.document_paths.len(), 1);
        // Python: test_documents_omitted_for_a_code_graph_only_caller
        let code_only = resolve_code_repository(root, false, true, None).unwrap();
        assert!(code_only.document_paths.is_empty());
    }

    // Python: test_directory_without_project_still_flattens
    #[test]
    fn a_plain_directory_still_flattens() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        write(&root, "notes/a.txt", b"a\n");
        write(&root, "b.md", b"b\n");
        let resolved =
            resolve_data_directories(vec![DataInput::FilePath(path_text(&root))], true).unwrap();
        let paths: Vec<String> = resolved
            .iter()
            .map(|input| match input {
                DataInput::FilePath(path) => path.clone(),
                other => panic!("expected file paths, got {other:?}"),
            })
            .collect();
        assert_eq!(
            paths,
            vec![
                path_text(&root.join("b.md")),
                path_text(&root.join("notes/a.txt"))
            ]
        );
    }

    #[test]
    fn files_and_missing_paths_pass_through() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.txt", b"a");
        let file = path_text(&dir.path().join("a.txt"));
        let missing = path_text(&dir.path().join("missing"));
        let resolved = resolve_data_directories(
            vec![
                DataInput::FilePath(file.clone()),
                DataInput::FilePath(missing.clone()),
            ],
            true,
        )
        .unwrap();
        assert!(matches!(&resolved[0], DataInput::FilePath(path) if *path == file));
        assert!(matches!(&resolved[1], DataInput::FilePath(path) if *path == missing));
    }
}
