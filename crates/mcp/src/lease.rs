//! Cross-process and cross-host engine ownership with nonce fencing.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::atomic_fs::{
    AtomicFsError, AtomicWriteOutcome, ReplaceMode, SystemSyncOps, ensure_private_directory,
    remove_durable, write_atomic,
};
use crate::layout::{LayoutError, StateLayout};

const OWNER_MAX_BYTES: u64 = 64 * 1024;
static NONCE_COUNTER: AtomicU64 = AtomicU64::new(1);

pub trait LeaseRuntime: Send + Sync {
    fn hostname(&self) -> String;
    fn process_id(&self) -> u32;
    fn now(&self) -> DateTime<Utc>;
    fn next_nonce(&self) -> String;
    fn process_is_alive(&self, pid: u32) -> bool;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemLeaseRuntime;

impl LeaseRuntime for SystemLeaseRuntime {
    fn hostname(&self) -> String {
        system_hostname()
    }

    fn process_id(&self) -> u32 {
        std::process::id()
    }

    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }

    fn next_nonce(&self) -> String {
        let counter = NONCE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut hash = Sha256::new();
        hash.update(system_hostname());
        hash.update(std::process::id().to_be_bytes());
        hash.update(
            Utc::now()
                .timestamp_nanos_opt()
                .unwrap_or_default()
                .to_be_bytes(),
        );
        hash.update(counter.to_be_bytes());
        format!("{:x}", hash.finalize())
    }

    fn process_is_alive(&self, pid: u32) -> bool {
        process_is_alive(pid)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseOwner {
    pub nonce: String,
    pub host: String,
    pub pid: u32,
    pub started_at: String,
    pub heartbeat_at: String,
    pub operation: String,
}

#[derive(Debug, Error)]
pub enum LeaseError {
    #[error("engine lease I/O failed")]
    Io(#[source] io::Error),
    #[error("engine lease atomic operation failed")]
    Atomic(#[source] AtomicFsError),
    #[error("private state layout failed")]
    Layout(#[source] LayoutError),
    #[error("engine lease owner metadata is invalid")]
    Json(#[source] serde_json::Error),
    #[error("engine lease operation is invalid")]
    InvalidOperation,
    #[error("engine lease nonce is invalid")]
    InvalidNonce,
    #[error("engine lease ownership was lost")]
    LeaseLost,
}

impl From<io::Error> for LeaseError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<AtomicFsError> for LeaseError {
    fn from(error: AtomicFsError) -> Self {
        Self::Atomic(error)
    }
}

impl From<LayoutError> for LeaseError {
    fn from(error: LayoutError) -> Self {
        Self::Layout(error)
    }
}

impl From<serde_json::Error> for LeaseError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

#[derive(Clone)]
pub struct EngineLease {
    layout: StateLayout,
    stale_after: Duration,
    runtime: Arc<dyn LeaseRuntime>,
}

impl EngineLease {
    pub fn new(layout: StateLayout, stale_after: Duration) -> Self {
        Self::with_runtime(layout, stale_after, Arc::new(SystemLeaseRuntime))
    }

    pub fn with_runtime(
        layout: StateLayout,
        stale_after: Duration,
        runtime: Arc<dyn LeaseRuntime>,
    ) -> Self {
        Self {
            layout,
            stale_after,
            runtime,
        }
    }

    pub fn try_acquire(&self, operation: &str) -> Result<Option<LeaseGuard>, LeaseError> {
        if !is_safe_token(operation, 64) {
            return Err(LeaseError::InvalidOperation);
        }
        self.layout.ensure_private()?;
        let nonce = self.runtime.next_nonce();
        if !is_safe_token(&nonce, 128) {
            return Err(LeaseError::InvalidNonce);
        }
        let now = self.runtime.now();
        let owner = LeaseOwner {
            nonce,
            host: self.runtime.hostname(),
            pid: self.runtime.process_id(),
            started_at: now.to_rfc3339(),
            heartbeat_at: now.to_rfc3339(),
            operation: operation.to_owned(),
        };

        match self.create_owned_directory(&owner) {
            Ok(guard) => return Ok(Some(guard)),
            Err(LeaseError::Io(error)) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }

        if !self.reclaim_stale_same_host()? {
            return Ok(None);
        }
        match self.create_owned_directory(&owner) {
            Ok(guard) => Ok(Some(guard)),
            Err(LeaseError::Io(error)) if error.kind() == io::ErrorKind::AlreadyExists => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn create_owned_directory(&self, owner: &LeaseOwner) -> Result<LeaseGuard, LeaseError> {
        let lease_path = self.lease_path();
        fs::create_dir(&lease_path)?;
        let result = (|| {
            ensure_private_directory(&lease_path)?;
            let bytes = serde_json::to_vec(owner)?;
            let outcome = write_atomic(
                &lease_path.join("owner.json"),
                &bytes,
                ReplaceMode::NoReplace,
                &SystemSyncOps,
            )?;
            if outcome != AtomicWriteOutcome::Written {
                return Err(LeaseError::LeaseLost);
            }
            sync_directory(&self.layout.locks)?;
            Ok(LeaseGuard {
                lease_path: lease_path.clone(),
                locks_path: self.layout.locks.clone(),
                owner: owner.clone(),
                runtime: self.runtime.clone(),
            })
        })();
        if result.is_err() {
            let _ = fs::remove_file(lease_path.join("owner.json"));
            let _ = fs::remove_dir(&lease_path);
            let _ = sync_directory(&self.layout.locks);
        }
        result
    }

    fn reclaim_stale_same_host(&self) -> Result<bool, LeaseError> {
        let lease_path = self.lease_path();
        let Some(first) = read_owner_conservative(&lease_path.join("owner.json"))? else {
            return Ok(false);
        };
        if first.host != self.runtime.hostname() || self.runtime.process_is_alive(first.pid) {
            return Ok(false);
        }
        let Ok(heartbeat) = DateTime::parse_from_rfc3339(&first.heartbeat_at) else {
            return Ok(false);
        };
        let age = self
            .runtime
            .now()
            .signed_duration_since(heartbeat.with_timezone(&Utc));
        let Ok(stale_after) = chrono::Duration::from_std(self.stale_after) else {
            return Ok(false);
        };
        if age <= stale_after {
            return Ok(false);
        }

        let Some(second) = read_owner_conservative(&lease_path.join("owner.json"))? else {
            return Ok(false);
        };
        if first != second {
            return Ok(false);
        }
        let quarantine_nonce = self.runtime.next_nonce();
        if !is_safe_token(&quarantine_nonce, 128) {
            return Err(LeaseError::InvalidNonce);
        }
        let quarantine = self
            .layout
            .locks
            .join(format!(".engine-stale-{quarantine_nonce}"));
        match fs::rename(&lease_path, &quarantine) {
            Ok(()) => {
                sync_directory(&self.layout.locks)?;
                fs::remove_dir_all(&quarantine)?;
                sync_directory(&self.layout.locks)?;
                Ok(true)
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::AlreadyExists
                ) =>
            {
                Ok(false)
            }
            Err(error) => Err(LeaseError::Io(error)),
        }
    }

    fn lease_path(&self) -> PathBuf {
        self.layout.locks.join("engine")
    }
}

pub struct LeaseGuard {
    lease_path: PathBuf,
    locks_path: PathBuf,
    owner: LeaseOwner,
    runtime: Arc<dyn LeaseRuntime>,
}

impl LeaseGuard {
    pub fn nonce(&self) -> &str {
        &self.owner.nonce
    }

    pub fn verify(&self) -> Result<(), LeaseError> {
        let current = read_owner(&self.lease_path.join("owner.json"))?;
        if current.nonce != self.owner.nonce {
            return Err(LeaseError::LeaseLost);
        }
        Ok(())
    }

    pub fn heartbeat(&mut self) -> Result<(), LeaseError> {
        self.verify()?;
        self.owner.heartbeat_at = self.runtime.now().to_rfc3339();
        let bytes = serde_json::to_vec(&self.owner)?;
        write_atomic(
            &self.lease_path.join("owner.json"),
            &bytes,
            ReplaceMode::Replace,
            &SystemSyncOps,
        )?;
        self.verify()
    }

    pub fn release(self) -> Result<(), LeaseError> {
        self.verify()?;
        remove_durable(&self.lease_path.join("owner.json"), &SystemSyncOps)?;
        fs::remove_dir(&self.lease_path)?;
        sync_directory(&self.locks_path)?;
        Ok(())
    }
}

fn read_owner(path: &Path) -> Result<LeaseOwner, LeaseError> {
    let metadata = fs::metadata(path)?;
    if metadata.len() > OWNER_MAX_BYTES {
        return Err(LeaseError::LeaseLost);
    }
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn read_owner_conservative(path: &Path) -> Result<Option<LeaseOwner>, LeaseError> {
    match read_owner(path) {
        Ok(owner) => Ok(Some(owner)),
        Err(LeaseError::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(LeaseError::Json(_)) | Err(LeaseError::LeaseLost) => Ok(None),
        Err(error) => Err(error),
    }
}

fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

fn is_safe_token(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn system_hostname() -> String {
    fs::read_to_string("/proc/sys/kernel/hostname")
        .or_else(|_| fs::read_to_string("/etc/hostname"))
        .map(|value| value.trim().to_owned())
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("unknown-host-{}", std::process::id()))
}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    // SAFETY: kill with signal 0 does not deliver a signal and only probes the PID.
    let result = unsafe { libc::kill(pid, 0) };
    if result == 0 {
        return true;
    }
    io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn process_is_alive(_pid: u32) -> bool {
    false
}
