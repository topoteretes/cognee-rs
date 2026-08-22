use std::path::{Component, Path, PathBuf};

use super::{ReferenceError, ReferenceLayout};
use crate::config::EnvSource;

pub const REFERENCE_DATASET: &str = "fleet_reference";
const REFERENCE_ROOT_ENV: &str = "APEX_COGNEE_REFERENCE_ROOT";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceConfig {
    pub layout: ReferenceLayout,
    pub dataset: &'static str,
    pub limits: ReferenceLimits,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReferenceLimits {
    pub max_input_bytes: usize,
    pub max_batch_bytes: usize,
    pub max_batch_files: usize,
    pub max_pending_events: u64,
    pub max_pending_bytes: u64,
    pub max_item_bytes: usize,
    pub max_payload_bytes: usize,
}

impl Default for ReferenceLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 2 * 1024 * 1024,
            max_batch_bytes: 8 * 1024 * 1024,
            max_batch_files: 32,
            max_pending_events: 128,
            max_pending_bytes: 64 * 1024 * 1024,
            max_item_bytes: 2 * 1024,
            max_payload_bytes: 8 * 1024,
        }
    }
}

impl ReferenceConfig {
    pub fn from_env(env: &impl EnvSource) -> Result<Option<Self>, ReferenceError> {
        let Some(configured) = env
            .get(REFERENCE_ROOT_ENV)
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
        else {
            return Ok(None);
        };
        let configured = PathBuf::from(configured);
        validate_configured_root(&configured)?;
        let root = canonicalize_with_missing_tail(&configured)?;
        validate_configured_root(&root)?;
        Ok(Some(Self {
            layout: ReferenceLayout::under(root),
            dataset: REFERENCE_DATASET,
            limits: ReferenceLimits::default(),
        }))
    }
}

fn canonicalize_with_missing_tail(root: &Path) -> Result<PathBuf, ReferenceError> {
    let mut existing = root.to_path_buf();
    let mut missing = Vec::new();
    loop {
        match std::fs::symlink_metadata(&existing) {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = existing
                    .file_name()
                    .ok_or(ReferenceError::InvalidRoot)?
                    .to_owned();
                missing.push(name);
                existing = existing
                    .parent()
                    .ok_or(ReferenceError::InvalidRoot)?
                    .to_path_buf();
            }
            Err(_) => return Err(ReferenceError::InvalidRoot),
        }
    }
    let mut canonical = existing
        .canonicalize()
        .map_err(|_| ReferenceError::InvalidRoot)?;
    for component in missing.into_iter().rev() {
        canonical.push(component);
    }
    Ok(canonical)
}

pub(crate) fn validate_configured_root(root: &Path) -> Result<(), ReferenceError> {
    if !root.is_absolute() || root.parent().is_none() {
        return Err(ReferenceError::InvalidRoot);
    }
    let mut normal_components = 0_usize;
    for component in root.components() {
        match component {
            Component::RootDir | Component::Prefix(_) => {}
            Component::Normal(_) => normal_components += 1,
            Component::CurDir | Component::ParentDir => return Err(ReferenceError::InvalidRoot),
        }
    }
    if normal_components == 0 {
        return Err(ReferenceError::InvalidRoot);
    }
    Ok(())
}
