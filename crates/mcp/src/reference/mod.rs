//! Curated, fleet-readable reference-memory contracts.
//!
//! Reference memory is deliberately independent from the private user state
//! used by hooks and the existing memory tools.  This module contains only the
//! stable configuration, layout, and administrator command surface; storage
//! and recall are layered on in the sibling modules.

#[cfg(feature = "runtime")]
mod admin;
mod config;
mod delta;
mod engine;
mod layout;
mod publisher;
mod reader;
mod record;

use std::path::PathBuf;

use clap::{Args, Subcommand};

#[cfg(feature = "engine")]
pub use admin::run_reference_publish_from_env;
#[cfg(feature = "runtime")]
pub use admin::{
    CognificationWaiter, DoctorReport, FilesystemCognificationWaiter, PublishSpawner,
    RecoveryReceipt, RememberReceipt, RememberRecordReceipt, SystemPublishSpawner,
    prepare_documents, run_reference_command, run_reference_doctor,
    run_reference_doctor_with_identity, run_reference_remember_with,
};
pub use config::{REFERENCE_DATASET, ReferenceConfig, ReferenceLimits};
pub use delta::{CommitReceipt, CommitStatus, DeltaHead, DeltaSnapshot, DeltaStore};
#[cfg(feature = "engine")]
pub use engine::CogneeReferenceEngineFactory;
pub use engine::{
    ReferenceEngineFactory, ReferenceEngineIdentity, ReferenceEngineInput, ReferenceEngineOpen,
    ReferenceProviderFingerprint, ReferenceReadEngine, ReferenceRecallProbe, ReferenceWriteEngine,
};
pub use layout::ReferenceLayout;
pub use publisher::{
    CurrentPointer, FileManifestEntry, GenerationManifest, PublishFaultPoint, PublishHooks,
    PublishReceipt, PublishRunReport, PublishedGenerationStatus, PublisherLock, ReferencePublisher,
    SourceManifestEntry, recover_publish_lock, validate_published_generation,
};
pub use reader::{
    ReferenceReadHooks, ReferenceReader, ReferenceRecallItem, ReferenceRecallMetadata,
    ReferenceRecallRequest, ReferenceRecallResponse,
};
pub use record::{PreparedDocument, ReferenceOperation, ReferenceRecord, Source, SourceKind};

pub use crate::error::ReferenceError;

pub const DEFAULT_WAIT_SECONDS: u64 = 1_800;
pub const MAX_WAIT_SECONDS: u64 = 7_200;

#[derive(Debug, Subcommand)]
pub enum ReferenceCommand {
    Remember(ReferenceRememberArgs),
    Publish,
    Doctor {
        #[arg(long)]
        json: bool,
    },
    Recover {
        #[arg(long)]
        adopt_orphans: bool,
    },
}

#[derive(Debug, Args)]
pub struct ReferenceRememberArgs {
    #[arg(short = 'f', long = "file", action = clap::ArgAction::Append)]
    pub files: Vec<PathBuf>,
    #[arg(long, conflicts_with = "files")]
    pub source_id: Option<String>,
    #[arg(long, conflicts_with = "files")]
    pub label: Option<String>,
    #[arg(long)]
    pub wait_cognified: bool,
    #[arg(
        long,
        default_value_t = DEFAULT_WAIT_SECONDS,
        value_parser = clap::value_parser!(u64).range(1..=MAX_WAIT_SECONDS)
    )]
    pub wait_seconds: u64,
}

impl ReferenceRememberArgs {
    pub fn uses_stdin(&self) -> bool {
        self.files.is_empty()
    }
}
