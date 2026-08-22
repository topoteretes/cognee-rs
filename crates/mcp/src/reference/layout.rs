use std::fs;
use std::path::{Path, PathBuf};

use super::ReferenceError;
use super::config::validate_configured_root;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceLayout {
    pub root: PathBuf,
    pub schema: PathBuf,
    pub current: PathBuf,
    pub delta: PathBuf,
    pub delta_head: PathBuf,
    pub delta_events: PathBuf,
    pub generations: PathBuf,
    pub admin: PathBuf,
    pub delta_lock: PathBuf,
    pub publish_lock: PathBuf,
    pub builder: PathBuf,
    pub staging: PathBuf,
    pub status: PathBuf,
}

impl ReferenceLayout {
    pub fn under(root: PathBuf) -> Self {
        Self {
            schema: root.join("schema.json"),
            current: root.join("current.json"),
            delta: root.join("delta"),
            delta_head: root.join("delta/head.json"),
            delta_events: root.join("delta/events"),
            generations: root.join("generations"),
            admin: root.join("admin"),
            delta_lock: root.join("admin/lock/delta.lock"),
            publish_lock: root.join("admin/lock/publish.lock"),
            builder: root.join("admin/builder"),
            staging: root.join("admin/staging"),
            status: root.join("admin/status"),
            root,
        }
    }

    pub fn validate_reader_root(&self) -> Result<(), ReferenceError> {
        validate_configured_root(&self.root)?;
        require_directory(&self.root)?;
        require_regular_file(&self.schema)?;
        require_directory(&self.delta)?;
        require_regular_file(&self.delta_head)?;
        require_directory(&self.delta_events)?;
        require_directory(&self.generations)?;
        if self.current.exists() {
            require_regular_file(&self.current)?;
        }
        Ok(())
    }

    pub fn ensure_admin_tree(&self) -> Result<(), ReferenceError> {
        validate_configured_root(&self.root)?;
        ensure_directory(&self.root, 0o755)?;
        for directory in [&self.delta, &self.delta_events, &self.generations] {
            ensure_directory(directory, 0o755)?;
        }
        let lock_directory = self
            .delta_lock
            .parent()
            .ok_or(ReferenceError::InvalidRoot)?;
        for directory in [
            &self.admin,
            lock_directory,
            &self.builder,
            &self.staging,
            &self.status,
        ] {
            ensure_directory(directory, 0o700)?;
        }
        Ok(())
    }
}

fn require_directory(path: &Path) -> Result<(), ReferenceError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| ReferenceError::Unavailable)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ReferenceError::Unavailable);
    }
    Ok(())
}

fn require_regular_file(path: &Path) -> Result<(), ReferenceError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| ReferenceError::Unavailable)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ReferenceError::Unavailable);
    }
    Ok(())
}

fn ensure_directory(path: &Path, mode: u32) -> Result<(), ReferenceError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(ReferenceError::InvalidRoot);
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path).map_err(ReferenceError::Io)?;
        }
        Err(error) => return Err(ReferenceError::Io(error)),
    }
    set_directory_mode(path, mode)
}

#[cfg(unix)]
fn set_directory_mode(path: &Path, mode: u32) -> Result<(), ReferenceError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(ReferenceError::Io)
}

#[cfg(not(unix))]
fn set_directory_mode(_path: &Path, _mode: u32) -> Result<(), ReferenceError> {
    Ok(())
}
