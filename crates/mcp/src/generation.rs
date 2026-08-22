//! Dataset generation fencing for destructive forget and recovery operations.

use std::collections::BTreeMap;
use std::fs;
#[cfg(feature = "runtime")]
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::atomic_fs::AtomicFsError;
#[cfg(feature = "runtime")]
use crate::atomic_fs::{ReplaceMode, SyncOps, SystemSyncOps, write_atomic};
use crate::layout::{LayoutError, StateLayout};
#[cfg(feature = "runtime")]
use crate::spool::{Spool, SpoolError};

const GENERATION_STATE_MAX_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct GenerationState {
    #[serde(default)]
    global: u64,
    #[serde(default)]
    datasets: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenerationAdvanceReport {
    pub previous: u64,
    pub current: u64,
    pub quarantined: usize,
}

#[derive(Debug, Error)]
pub enum GenerationError {
    #[error("dataset generation state I/O failed")]
    Io(#[source] std::io::Error),
    #[error("dataset generation atomic write failed")]
    Atomic(#[source] AtomicFsError),
    #[error("private state layout failed")]
    Layout(#[source] LayoutError),
    #[error("dataset generation state is invalid")]
    Json(#[source] serde_json::Error),
    #[error("dataset generation state is larger than its cap")]
    StateTooLarge,
    #[error("dataset generation overflowed")]
    Overflow,
    #[cfg(feature = "runtime")]
    #[error("dataset generation quarantine failed")]
    Spool(#[source] SpoolError),
}

impl From<std::io::Error> for GenerationError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<AtomicFsError> for GenerationError {
    fn from(error: AtomicFsError) -> Self {
        Self::Atomic(error)
    }
}

impl From<LayoutError> for GenerationError {
    fn from(error: LayoutError) -> Self {
        Self::Layout(error)
    }
}

impl From<serde_json::Error> for GenerationError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

#[cfg(feature = "runtime")]
impl From<SpoolError> for GenerationError {
    fn from(error: SpoolError) -> Self {
        Self::Spool(error)
    }
}

#[derive(Clone)]
pub struct GenerationStore {
    layout: StateLayout,
    #[cfg(feature = "runtime")]
    sync: Arc<dyn SyncOps>,
}

impl GenerationStore {
    pub fn new(layout: StateLayout) -> Self {
        #[cfg(feature = "runtime")]
        {
            Self::with_sync(layout, Arc::new(SystemSyncOps))
        }
        #[cfg(not(feature = "runtime"))]
        {
            Self { layout }
        }
    }

    #[cfg(feature = "runtime")]
    pub fn with_sync(layout: StateLayout, sync: Arc<dyn SyncOps>) -> Self {
        Self { layout, sync }
    }

    pub fn current(&self, dataset: &str) -> Result<u64, GenerationError> {
        let state = self.load()?;
        Ok(state
            .datasets
            .get(dataset)
            .copied()
            .unwrap_or_default()
            .max(state.global))
    }

    #[cfg(feature = "runtime")]
    pub fn advance_and_quarantine(
        &self,
        dataset: &str,
        spool: &Spool,
    ) -> Result<GenerationAdvanceReport, GenerationError> {
        let (previous, current) = self.advance(dataset)?;
        let quarantined = spool.quarantine_superseded(dataset, previous)?;
        Ok(GenerationAdvanceReport {
            previous,
            current,
            quarantined,
        })
    }

    #[cfg(feature = "runtime")]
    pub(crate) fn advance(&self, dataset: &str) -> Result<(u64, u64), GenerationError> {
        let mut state = self.load()?;
        let previous = state
            .datasets
            .get(dataset)
            .copied()
            .unwrap_or_default()
            .max(state.global);
        let current = previous.checked_add(1).ok_or(GenerationError::Overflow)?;
        state.datasets.insert(dataset.to_owned(), current);
        let bytes = serde_json::to_vec(&state)?;
        if bytes.len() as u64 > GENERATION_STATE_MAX_BYTES {
            return Err(GenerationError::StateTooLarge);
        }
        self.layout.ensure_private()?;
        write_atomic(
            &self.state_path(),
            &bytes,
            ReplaceMode::Replace,
            self.sync.as_ref(),
        )?;
        Ok((previous, current))
    }

    #[cfg(feature = "runtime")]
    pub(crate) fn advance_global(&self) -> Result<u64, GenerationError> {
        let mut state = self.load()?;
        let maximum = state
            .datasets
            .values()
            .copied()
            .fold(state.global, u64::max);
        let current = maximum.checked_add(1).ok_or(GenerationError::Overflow)?;
        state.global = current;
        let bytes = serde_json::to_vec(&state)?;
        if bytes.len() as u64 > GENERATION_STATE_MAX_BYTES {
            return Err(GenerationError::StateTooLarge);
        }
        self.layout.ensure_private()?;
        write_atomic(
            &self.state_path(),
            &bytes,
            ReplaceMode::Replace,
            self.sync.as_ref(),
        )?;
        Ok(current)
    }

    fn load(&self) -> Result<GenerationState, GenerationError> {
        self.layout.ensure_private()?;
        let path = self.state_path();
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(GenerationState::default());
            }
            Err(error) => return Err(GenerationError::Io(error)),
        };
        if metadata.len() > GENERATION_STATE_MAX_BYTES {
            return Err(GenerationError::StateTooLarge);
        }
        let bytes = fs::read(path)?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    fn state_path(&self) -> std::path::PathBuf {
        self.layout.status.join("dataset-generations.json")
    }
}
