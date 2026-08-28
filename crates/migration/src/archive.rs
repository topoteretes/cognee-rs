//! Pack a COGX archive directory into `.cogx.tar.gz` for transport.
//!
//! Python's `POST /api/v1/remember` with `content_type=cogx-archive` takes a
//! single tarball, not a directory, so this is the encoding a Rust `push`
//! needs. It is the sending half of Python's `archive.py`; the receiving half
//! (`unpack_archive`) lives there.

use std::fs::File;
use std::path::{Path, PathBuf};

use flate2::Compression;
use flate2::write::GzEncoder;

use crate::error::{MigrationError, MigrationResult};

/// Suffix Python's `pack_archive` uses. Kept identical so an uploaded file
/// name looks the same coming from either SDK.
pub const ARCHIVE_SUFFIX: &str = ".cogx.tar.gz";

/// Tar and gzip an archive directory so its files sit at the tarball root.
///
/// Root placement matters: Python's `find_archive_root` accepts `manifest.json`
/// at the tarball root or inside a *single* subdirectory, and nothing else.
pub fn pack_archive(
    archive_dir: impl AsRef<Path>,
    tar_path: impl Into<PathBuf>,
) -> MigrationResult<PathBuf> {
    let archive_dir = archive_dir.as_ref();
    let tar_path = tar_path.into();

    let file = File::create(&tar_path).map_err(|error| MigrationError::io(&tar_path, error))?;
    let encoder = GzEncoder::new(file, Compression::default());
    let mut builder = tar::Builder::new(encoder);

    // Sorted, so a re-pack of unchanged content produces a stable member order.
    let mut entries: Vec<PathBuf> = std::fs::read_dir(archive_dir)
        .map_err(|error| MigrationError::io(archive_dir, error))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect();
    entries.sort();

    for path in entries {
        let name = path
            .file_name()
            .ok_or_else(|| {
                MigrationError::io(
                    &path,
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "archive entry has no file name",
                    ),
                )
            })?
            .to_owned();
        builder
            .append_path_with_name(&path, &name)
            .map_err(|error| MigrationError::io(&path, error))?;
    }

    builder
        .into_inner()
        .map_err(|error| MigrationError::io(&tar_path, error))?
        .finish()
        .map_err(|error| MigrationError::io(&tar_path, error))?;

    Ok(tar_path)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use crate::cogx::MANIFEST_FILE;
    use flate2::read::GzDecoder;

    #[test]
    fn manifest_lands_at_the_tarball_root() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("archive");
        std::fs::create_dir_all(&archive).unwrap();
        std::fs::write(archive.join(MANIFEST_FILE), "{}").unwrap();
        std::fs::write(archive.join("entities.jsonl"), "{}\n").unwrap();

        let tar_path = pack_archive(&archive, dir.path().join("out.cogx.tar.gz")).unwrap();

        let names: Vec<String> = tar::Archive::new(GzDecoder::new(File::open(&tar_path).unwrap()))
            .entries()
            .unwrap()
            .map(|entry| entry.unwrap().path().unwrap().display().to_string())
            .collect();

        assert!(
            names.contains(&MANIFEST_FILE.to_string()),
            "manifest is not at the tarball root: {names:?}"
        );
        assert!(names.contains(&"entities.jsonl".to_string()));
    }
}
