//! Atomic private-file operations with explicit durability boundaries.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use thiserror::Error;

static TEMPORARY_NONCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplaceMode {
    NoReplace,
    Replace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtomicWriteOutcome {
    Written,
    Existing,
}

#[derive(Debug, Error)]
pub enum AtomicFsError {
    #[error("atomic private-file I/O failed")]
    Io(#[source] io::Error),
    #[error("atomic private-file path is invalid")]
    InvalidPath,
}

impl From<io::Error> for AtomicFsError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub trait SyncOps: Send + Sync {
    fn sync_file(&self, file: &File) -> io::Result<()>;

    fn before_rename(&self, _temporary: &Path, _destination: &Path) -> io::Result<()> {
        Ok(())
    }

    fn sync_directory(&self, directory: &Path) -> io::Result<()>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemSyncOps;

impl SyncOps for SystemSyncOps {
    fn sync_file(&self, file: &File) -> io::Result<()> {
        file.sync_all()
    }

    fn sync_directory(&self, directory: &Path) -> io::Result<()> {
        File::open(directory)?.sync_all()
    }
}

pub fn write_atomic(
    destination: &Path,
    contents: &[u8],
    replace: ReplaceMode,
    sync: &dyn SyncOps,
) -> Result<AtomicWriteOutcome, AtomicFsError> {
    write_atomic_with_permissions(destination, contents, replace, 0o700, 0o600, sync)
}

/// Atomically install a file with explicit parent-directory and file modes.
///
/// Private-state callers should continue to use [`write_atomic`]. The shared
/// reference plane uses this variant for immutable, reader-visible files.
pub fn write_atomic_with_permissions(
    destination: &Path,
    contents: &[u8],
    replace: ReplaceMode,
    directory_mode: u32,
    file_mode: u32,
    sync: &dyn SyncOps,
) -> Result<AtomicWriteOutcome, AtomicFsError> {
    let parent = destination.parent().ok_or(AtomicFsError::InvalidPath)?;
    let file_name = destination.file_name().ok_or(AtomicFsError::InvalidPath)?;
    if replace == ReplaceMode::NoReplace && destination.exists() {
        return Ok(AtomicWriteOutcome::Existing);
    }
    ensure_directory_with_mode(parent, directory_mode)?;

    let temporary = temporary_path(parent, file_name);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(file_mode);
    }
    let mut file = options.open(&temporary)?;
    let result = (|| {
        set_file_mode(&temporary, file_mode)?;
        file.write_all(contents)?;
        sync.sync_file(&file)?;
        drop(file);

        sync.before_rename(&temporary, destination)?;
        match replace {
            ReplaceMode::NoReplace => match fs::hard_link(&temporary, destination) {
                Ok(()) => {
                    fs::remove_file(&temporary)?;
                    sync.sync_directory(parent)?;
                    Ok(AtomicWriteOutcome::Written)
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    fs::remove_file(&temporary)?;
                    sync.sync_directory(parent)?;
                    Ok(AtomicWriteOutcome::Existing)
                }
                Err(error) => Err(error),
            },
            ReplaceMode::Replace => {
                fs::rename(&temporary, destination)?;
                sync.sync_directory(parent)?;
                Ok(AtomicWriteOutcome::Written)
            }
        }
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(AtomicFsError::Io)
}

pub fn rename_durable(
    source: &Path,
    destination: &Path,
    sync: &dyn SyncOps,
) -> Result<(), AtomicFsError> {
    let source_parent = source.parent().ok_or(AtomicFsError::InvalidPath)?;
    let destination_parent = destination.parent().ok_or(AtomicFsError::InvalidPath)?;
    ensure_private_directory(destination_parent)?;
    sync.before_rename(source, destination)?;
    fs::rename(source, destination)?;
    sync.sync_directory(destination_parent)?;
    if source_parent != destination_parent {
        sync.sync_directory(source_parent)?;
    }
    Ok(())
}

pub fn rename_durable_no_replace(
    source: &Path,
    destination: &Path,
    sync: &dyn SyncOps,
) -> Result<AtomicWriteOutcome, AtomicFsError> {
    let source_parent = source.parent().ok_or(AtomicFsError::InvalidPath)?;
    let destination_parent = destination.parent().ok_or(AtomicFsError::InvalidPath)?;
    ensure_private_directory(destination_parent)?;
    sync.before_rename(source, destination)?;
    match fs::hard_link(source, destination) {
        Ok(()) => {
            fs::remove_file(source)?;
            sync.sync_directory(destination_parent)?;
            if source_parent != destination_parent {
                sync.sync_directory(source_parent)?;
            }
            Ok(AtomicWriteOutcome::Written)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            Ok(AtomicWriteOutcome::Existing)
        }
        Err(error) => Err(AtomicFsError::Io(error)),
    }
}

pub fn remove_durable(path: &Path, sync: &dyn SyncOps) -> Result<(), AtomicFsError> {
    let parent = path.parent().ok_or(AtomicFsError::InvalidPath)?;
    fs::remove_file(path)?;
    sync.sync_directory(parent)?;
    Ok(())
}

pub fn ensure_private_directory(path: &Path) -> Result<(), AtomicFsError> {
    ensure_directory_with_mode(path, 0o700)
}

fn ensure_directory_with_mode(path: &Path, mode: u32) -> Result<(), AtomicFsError> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

fn temporary_path(parent: &Path, file_name: &std::ffi::OsStr) -> PathBuf {
    let nonce = TEMPORARY_NONCE.fetch_add(1, Ordering::Relaxed);
    let mut name = format!(".tmp-{}-{nonce}-", std::process::id());
    name.push_str(&file_name.to_string_lossy());
    parent.join(name)
}

fn set_file_mode(path: &Path, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}
