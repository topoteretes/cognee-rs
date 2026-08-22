//! Private per-engineer state paths and file creation helpers.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateLayout {
    pub root: PathBuf,
    pub system: PathBuf,
    pub data: PathBuf,
    pub cache: PathBuf,
    pub spool_pending: PathBuf,
    pub spool_processing: PathBuf,
    pub spool_failed: PathBuf,
    pub locks: PathBuf,
    pub context: PathBuf,
    pub status: PathBuf,
    pub ledger: PathBuf,
}

#[derive(Debug, Error)]
pub enum LayoutError {
    #[error("private state I/O failed")]
    Io(#[source] std::io::Error),
    #[error("private file must remain below the state root")]
    OutsideRoot,
}

impl From<std::io::Error> for LayoutError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl StateLayout {
    pub fn under(root: PathBuf) -> Self {
        Self {
            system: root.join("system"),
            data: root.join("data"),
            cache: root.join("cache"),
            spool_pending: root.join("spool/pending"),
            spool_processing: root.join("spool/processing"),
            spool_failed: root.join("spool/failed"),
            locks: root.join("locks"),
            context: root.join("context"),
            status: root.join("status"),
            ledger: root.join("ledger"),
            root,
        }
    }

    pub fn ensure_private(&self) -> Result<(), LayoutError> {
        #[cfg(unix)]
        {
            unix_fs::ensure_private(self)
        }
        #[cfg(not(unix))]
        {
            portable_fs::ensure_private(self)
        }
    }

    pub fn write_private_file(&self, path: &Path, contents: &[u8]) -> Result<(), LayoutError> {
        let relative = private_relative_path(&self.root, path)?;
        #[cfg(unix)]
        {
            unix_fs::write_private_file(&self.root, relative, contents)
        }
        #[cfg(not(unix))]
        {
            portable_fs::write_private_file(&self.root, relative, contents)
        }
    }
}

fn private_relative_path<'a>(root: &Path, path: &'a Path) -> Result<&'a Path, LayoutError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| LayoutError::OutsideRoot)?;
    if relative.as_os_str().is_empty()
        || !relative
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
    {
        return Err(LayoutError::OutsideRoot);
    }
    Ok(relative)
}

#[cfg(unix)]
mod unix_fs {
    use std::ffi::OsString;
    use std::fs::{self, File};
    use std::io::Write;
    use std::path::{Component, Path};

    use rustix::fd::OwnedFd;
    use rustix::fs::{Mode, OFlags, fchmod, mkdirat, open, openat};
    use rustix::io::Errno;
    use rustix::process::umask;

    use super::{LayoutError, StateLayout};

    const DIRECTORY_MODE: Mode = Mode::RUSR.union(Mode::WUSR).union(Mode::XUSR);
    const FILE_MODE: Mode = Mode::RUSR.union(Mode::WUSR);
    const PRIVATE_UMASK: Mode = Mode::RWXG.union(Mode::RWXO);

    pub(super) fn ensure_private(layout: &StateLayout) -> Result<(), LayoutError> {
        establish_private_umask();
        let root = open_or_create_root(&layout.root)?;
        drop(root);
        for relative in [
            "system",
            "data",
            "cache",
            "spool",
            "spool/pending",
            "spool/processing",
            "spool/failed",
            "locks",
            "context",
            "status",
            "status/logs",
            "ledger",
        ] {
            let root = open_or_create_root(&layout.root)?;
            drop(ensure_relative_directory(root, Path::new(relative))?);
        }
        Ok(())
    }

    pub(super) fn write_private_file(
        root: &Path,
        relative: &Path,
        contents: &[u8],
    ) -> Result<(), LayoutError> {
        establish_private_umask();
        let components = normal_components(relative)?;
        let (file_name, directories) = components.split_last().ok_or(LayoutError::OutsideRoot)?;
        let root = open_or_create_root(root)?;
        let parent = ensure_component_directories(root, directories)?;
        // O_NOFOLLOW rejects a final-component symlink. Every directory was
        // opened one component at a time with the same flag.
        let file_fd = openat(
            &parent,
            file_name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            FILE_MODE,
        )
        .map_err(io_error)?;
        fchmod(&file_fd, FILE_MODE).map_err(io_error)?;
        let mut file = File::from(file_fd);
        file.set_len(0)?;
        file.write_all(contents)?;
        file.sync_all()?;
        Ok(())
    }

    fn establish_private_umask() {
        // umask is process-global; every state-creating path sets the same
        // strictly-more-restrictive value and deliberately retains it.
        umask(PRIVATE_UMASK);
    }

    fn open_or_create_root(path: &Path) -> Result<OwnedFd, LayoutError> {
        let component = path
            .file_name()
            .ok_or(LayoutError::OutsideRoot)?
            .to_os_string();
        let parent_path = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        // The configured root's parent is outside the private-state boundary.
        // Establish the restrictive umask before creating a missing parent;
        // the root itself and every descendant are then opened relative to a
        // directory descriptor with O_NOFOLLOW.
        fs::create_dir_all(parent_path)?;
        let parent = open_directory_path(parent_path)?;
        let (root, _) = open_or_create_directory_at(&parent, &component)?;
        fchmod(&root, DIRECTORY_MODE).map_err(io_error)?;
        Ok(root)
    }

    fn ensure_relative_directory(root: OwnedFd, relative: &Path) -> Result<OwnedFd, LayoutError> {
        let components = normal_components(relative)?;
        ensure_component_directories(root, &components)
    }

    fn ensure_component_directories(
        mut current: OwnedFd,
        components: &[OsString],
    ) -> Result<OwnedFd, LayoutError> {
        for component in components {
            let (child, _) = open_or_create_directory_at(&current, component)?;
            fchmod(&child, DIRECTORY_MODE).map_err(io_error)?;
            current = child;
        }
        Ok(current)
    }

    fn open_or_create_directory_at(
        parent: &OwnedFd,
        component: &OsString,
    ) -> Result<(OwnedFd, bool), LayoutError> {
        let created = match mkdirat(parent, component, DIRECTORY_MODE) {
            Ok(()) => true,
            Err(Errno::EXIST) => false,
            Err(error) => return Err(io_error(error)),
        };
        let child = openat(
            parent,
            component,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(io_error)?;
        Ok((child, created))
    }

    fn open_directory_path(path: &Path) -> Result<OwnedFd, LayoutError> {
        open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(io_error)
    }

    fn normal_components(path: &Path) -> Result<Vec<OsString>, LayoutError> {
        let mut result = Vec::new();
        for component in path.components() {
            match component {
                Component::RootDir | Component::CurDir => {}
                Component::Normal(value) => result.push(value.to_os_string()),
                Component::ParentDir | Component::Prefix(_) => {
                    return Err(LayoutError::OutsideRoot);
                }
            }
        }
        Ok(result)
    }

    fn io_error(error: Errno) -> LayoutError {
        LayoutError::Io(error.into())
    }
}

#[cfg(not(unix))]
mod portable_fs {
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::path::Path;

    use super::{LayoutError, StateLayout};

    pub(super) fn ensure_private(layout: &StateLayout) -> Result<(), LayoutError> {
        for directory in [
            layout.root.clone(),
            layout.system.clone(),
            layout.data.clone(),
            layout.cache.clone(),
            layout.root.join("spool"),
            layout.spool_pending.clone(),
            layout.spool_processing.clone(),
            layout.spool_failed.clone(),
            layout.locks.clone(),
            layout.context.clone(),
            layout.status.clone(),
            layout.status.join("logs"),
            layout.ledger.clone(),
        ] {
            fs::create_dir_all(directory)?;
        }
        Ok(())
    }

    pub(super) fn write_private_file(
        root: &Path,
        relative: &Path,
        contents: &[u8],
    ) -> Result<(), LayoutError> {
        let path = root.join(relative);
        let parent = path.parent().ok_or(LayoutError::OutsideRoot)?;
        fs::create_dir_all(parent)?;
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)?;
        file.write_all(contents)?;
        file.sync_all()?;
        Ok(())
    }
}
