use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use serde::Serialize;

#[cfg(feature = "engine")]
use super::ReferenceEngineFactory;
use super::{
    CommitReceipt, CommitStatus, DeltaStore, PreparedDocument, ReferenceCommand, ReferenceConfig,
    ReferenceEngineIdentity, ReferenceError, ReferenceLayout, ReferenceLimits,
    ReferenceRememberArgs, Source, validate_published_generation,
};
use crate::cli::AgentError;
use crate::config::ProcessEnv;
use crate::detach::{DetachedProcess, ProcessSpawner, StdioPolicy, SystemProcessSpawner};

const READ_CHUNK_BYTES: usize = 64 * 1024;

pub trait PublishSpawner: Send + Sync {
    fn spawn(&self) -> io::Result<()>;
}

pub trait CognificationWaiter: Send + Sync {
    fn wait(
        &self,
        layout: &ReferenceLayout,
        receipt: &CommitReceipt,
        timeout: Duration,
    ) -> Result<bool, ReferenceError>;
}

#[derive(Debug, Clone, Serialize)]
pub struct RememberRecordReceipt {
    pub source_id: String,
    pub source_label: String,
    pub revision: u64,
    pub event_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RememberReceipt {
    pub status: CommitStatus,
    pub batch_id: Option<String>,
    pub first_sequence: Option<u64>,
    pub highest_committed_sequence: u64,
    pub records: Vec<RememberRecordReceipt>,
    pub publisher_started: bool,
    pub cognified: bool,
    pub wait_timed_out: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub status: &'static str,
    pub highest_committed_sequence: u64,
    pub committed_records: usize,
    pub orphan_records: usize,
    pub generation_id: Option<String>,
    pub included_through: u64,
    pub source_count: usize,
    pub generation_files: usize,
    pub publisher_locked: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecoveryReceipt {
    pub publish_lock_recovered: bool,
    pub delta_head: super::DeltaHead,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemPublishSpawner;

impl PublishSpawner for SystemPublishSpawner {
    fn spawn(&self) -> io::Result<()> {
        let executable = std::env::current_exe()?;
        SystemProcessSpawner.spawn(DetachedProcess {
            executable,
            args: vec!["reference".to_owned(), "publish".to_owned()],
            stdin: StdioPolicy::Null,
            stdout: StdioPolicy::Null,
            stderr: StdioPolicy::Null,
            new_session: true,
        })
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct FilesystemCognificationWaiter;

impl CognificationWaiter for FilesystemCognificationWaiter {
    fn wait(
        &self,
        layout: &ReferenceLayout,
        receipt: &CommitReceipt,
        timeout: Duration,
    ) -> Result<bool, ReferenceError> {
        let deadline = Instant::now() + timeout;
        loop {
            if receipt_is_cognified(layout, receipt)? {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

pub fn prepare_documents(
    arguments: &ReferenceRememberArgs,
    stdin: &mut dyn Read,
    limits: &ReferenceLimits,
) -> Result<Vec<PreparedDocument>, ReferenceError> {
    if arguments.files.len() > limits.max_batch_files {
        return Err(ReferenceError::TooManyFiles);
    }
    let documents = if arguments.files.is_empty() {
        let bytes = read_bounded(stdin, raw_input_limit(limits.max_input_bytes))?;
        vec![PreparedDocument::from_bytes(
            Source::Stdin,
            &bytes,
            arguments.source_id.as_deref(),
            arguments.label.as_deref(),
            limits,
        )?]
    } else {
        let mut documents = Vec::with_capacity(arguments.files.len());
        for path in &arguments.files {
            let metadata = fs::metadata(path).map_err(|_| ReferenceError::InvalidInput)?;
            if !metadata.is_file() {
                return Err(ReferenceError::InvalidInput);
            }
            let mut file = File::open(path).map_err(|_| ReferenceError::InvalidInput)?;
            let bytes = read_bounded(&mut file, raw_input_limit(limits.max_input_bytes))?;
            documents.push(PreparedDocument::from_bytes(
                Source::File(path.clone()),
                &bytes,
                None,
                None,
                limits,
            )?);
        }
        documents
    };
    let aggregate = documents.iter().try_fold(0_usize, |total, document| {
        total
            .checked_add(document.normalized_bytes)
            .ok_or(ReferenceError::BatchTooLarge)
    })?;
    if aggregate > limits.max_batch_bytes {
        return Err(ReferenceError::BatchTooLarge);
    }
    Ok(documents)
}

pub fn run_reference_remember_with(
    config: &ReferenceConfig,
    arguments: &ReferenceRememberArgs,
    stdin: &mut dyn Read,
    spawner: &dyn PublishSpawner,
    waiter: &dyn CognificationWaiter,
) -> Result<RememberReceipt, ReferenceError> {
    let documents = prepare_documents(arguments, stdin, &config.limits)?;
    let store = DeltaStore::new(config.layout.clone(), config.limits);
    let committed = store.commit_batch(&documents)?;
    let publisher_started = if committed.status == CommitStatus::Durable {
        spawner.spawn().is_ok()
    } else {
        false
    };
    let cognified = if arguments.wait_cognified {
        waiter.wait(
            &config.layout,
            &committed,
            Duration::from_secs(arguments.wait_seconds),
        )?
    } else {
        false
    };
    Ok(RememberReceipt {
        status: committed.status,
        batch_id: committed.batch_id.clone(),
        first_sequence: committed.first_sequence,
        highest_committed_sequence: committed.highest_committed_sequence,
        records: committed
            .records
            .iter()
            .map(|record| RememberRecordReceipt {
                source_id: record.source_id.clone(),
                source_label: record.source_label.clone(),
                revision: record.revision,
                event_id: record.event_id.clone(),
            })
            .collect(),
        publisher_started,
        cognified,
        wait_timed_out: arguments.wait_cognified && !cognified,
    })
}

pub fn run_reference_doctor(config: &ReferenceConfig) -> Result<DoctorReport, ReferenceError> {
    run_reference_doctor_with_identity(config, None)
}

pub fn run_reference_doctor_with_identity(
    config: &ReferenceConfig,
    expected_identity: Option<&ReferenceEngineIdentity>,
) -> Result<DoctorReport, ReferenceError> {
    config.layout.validate_reader_root()?;
    validate_reference_permissions(&config.layout)?;
    let store = DeltaStore::new(config.layout.clone(), config.limits);
    store.validate_schema()?;
    let snapshot = store.snapshot_after(0)?;
    store.validate_diagnostic_snapshot(&snapshot)?;
    let event_files = fs::read_dir(&config.layout.delta_events)?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".json"))
        .count();
    let published = if config.layout.current.exists() {
        Some(validate_published_generation(config, expected_identity)?)
    } else {
        None
    };
    let publisher_locked = super::publisher::publish_lock_present(&config.layout)?;
    Ok(DoctorReport {
        status: "ok",
        highest_committed_sequence: snapshot.head.highest_committed_sequence,
        committed_records: snapshot.records.len(),
        orphan_records: event_files.saturating_sub(snapshot.records.len()),
        generation_id: published
            .as_ref()
            .map(|generation| generation.generation_id.clone()),
        included_through: published
            .as_ref()
            .map_or(0, |generation| generation.included_through),
        source_count: published
            .as_ref()
            .map_or(0, |generation| generation.source_count),
        generation_files: published
            .as_ref()
            .map_or(0, |generation| generation.file_count),
        publisher_locked,
    })
}

#[cfg(feature = "engine")]
pub fn run_reference_publish_from_env(
    config: ReferenceConfig,
    env: &impl crate::config::EnvSource,
) -> Result<super::PublishRunReport, AgentError> {
    let agent =
        crate::config::AgentConfig::from_env(env).map_err(|_| ReferenceError::Unavailable)?;
    let factory = super::CogneeReferenceEngineFactory::new(agent)?;
    let publisher = super::ReferencePublisher::new(config, std::sync::Arc::new(factory))?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(ReferenceError::Io)?;
    runtime
        .block_on(publisher.publish_until_caught_up(Duration::from_secs(900)))
        .map_err(AgentError::from)
}

fn validate_reference_permissions(layout: &ReferenceLayout) -> Result<(), ReferenceError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        for directory in [
            &layout.root,
            &layout.delta,
            &layout.delta_events,
            &layout.generations,
        ] {
            validate_mode_and_kind(directory, 0o755, true)?;
        }
        for directory in [
            &layout.admin,
            layout
                .delta_lock
                .parent()
                .ok_or(ReferenceError::InvalidRoot)?,
            &layout.builder,
            &layout.staging,
            &layout.status,
        ] {
            validate_mode_and_kind(directory, 0o700, true)?;
        }
        for file in [&layout.schema, &layout.delta_head] {
            validate_mode_and_kind(file, 0o444, false)?;
        }
        if layout.current.exists() {
            validate_mode_and_kind(&layout.current, 0o444, false)?;
        }
        for entry in fs::read_dir(&layout.delta_events)? {
            let entry = entry?;
            if !entry.file_name().to_string_lossy().ends_with(".json") {
                return Err(ReferenceError::CorruptRecord);
            }
            validate_mode_and_kind(&entry.path(), 0o444, false)?;
        }

        fn validate_mode_and_kind(
            path: &Path,
            expected_mode: u32,
            directory: bool,
        ) -> Result<(), ReferenceError> {
            let metadata = fs::symlink_metadata(path)?;
            if metadata.file_type().is_symlink()
                || metadata.is_dir() != directory
                || metadata.permissions().mode() & 0o777 != expected_mode
            {
                return Err(ReferenceError::CorruptRecord);
            }
            Ok(())
        }
    }
    #[cfg(not(unix))]
    let _ = layout;
    Ok(())
}

pub fn run_reference_command(command: &ReferenceCommand) -> Result<(), AgentError> {
    let config = ReferenceConfig::from_env(&ProcessEnv)?.ok_or(ReferenceError::Unavailable)?;
    match command {
        ReferenceCommand::Remember(arguments) => {
            let receipt = run_reference_remember_with(
                &config,
                arguments,
                &mut io::stdin().lock(),
                &SystemPublishSpawner,
                &FilesystemCognificationWaiter,
            )?;
            write_json_line(io::stdout().lock(), &receipt)?;
            if receipt.wait_timed_out {
                return Err(AgentError::Timeout("reference cognification"));
            }
            Ok(())
        }
        ReferenceCommand::Publish => {
            #[cfg(feature = "engine")]
            {
                let report = run_reference_publish_from_env(config, &ProcessEnv)?;
                write_json_line(io::stdout().lock(), &report)?;
                return Ok(());
            }
            #[cfg(not(feature = "engine"))]
            Err(AgentError::Unavailable("reference publish"))
        }
        ReferenceCommand::Doctor { json } => {
            #[cfg(feature = "engine")]
            let report = {
                let agent = crate::config::AgentConfig::from_env(&ProcessEnv)
                    .map_err(|_| ReferenceError::Unavailable)?;
                let factory = super::CogneeReferenceEngineFactory::new(agent)?;
                run_reference_doctor_with_identity(&config, Some(&factory.identity()))?
            };
            #[cfg(not(feature = "engine"))]
            let report = run_reference_doctor(&config)?;
            if *json {
                write_json_line(io::stdout().lock(), &report)?;
            } else {
                writeln!(
                    io::stdout().lock(),
                    "reference: {} · head {} · committed {} · orphans {}",
                    report.status,
                    report.highest_committed_sequence,
                    report.committed_records,
                    report.orphan_records
                )
                .map_err(ReferenceError::Io)?;
            }
            Ok(())
        }
        ReferenceCommand::Recover { adopt_orphans } => {
            if !adopt_orphans {
                return Err(ReferenceError::InvalidInput.into());
            }
            let publish_lock_recovered = super::recover_publish_lock(
                &config.layout,
                &super::publisher::local_hostname()?,
                super::publisher::process_is_alive,
                std::sync::Arc::new(crate::atomic_fs::SystemSyncOps),
            )?;
            let head = DeltaStore::new(config.layout.clone(), config.limits).adopt_orphans()?;
            write_json_line(
                io::stdout().lock(),
                &RecoveryReceipt {
                    publish_lock_recovered,
                    delta_head: head,
                },
            )?;
            Ok(())
        }
    }
}

fn write_json_line(mut output: impl Write, value: &impl Serialize) -> Result<(), ReferenceError> {
    serde_json::to_writer(&mut output, value).map_err(|_| ReferenceError::CorruptRecord)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

fn read_bounded(reader: &mut dyn Read, maximum: usize) -> Result<Vec<u8>, ReferenceError> {
    let limit = u64::try_from(maximum).unwrap_or(u64::MAX).saturating_add(1);
    let mut bytes = Vec::with_capacity(maximum.min(READ_CHUNK_BYTES));
    reader.take(limit).read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(ReferenceError::InputTooLarge);
    }
    Ok(bytes)
}

fn raw_input_limit(normalized_limit: usize) -> usize {
    normalized_limit.saturating_mul(2).saturating_add(3)
}

fn receipt_is_cognified(
    layout: &ReferenceLayout,
    receipt: &CommitReceipt,
) -> Result<bool, ReferenceError> {
    let current = match read_json_value(&layout.current) {
        Ok(value) => value,
        Err(ReferenceError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(false);
        }
        Err(error) => return Err(error),
    };
    let included_through = current
        .get("included_through")
        .and_then(serde_json::Value::as_u64)
        .ok_or(ReferenceError::CorruptRecord)?;
    if included_through < receipt.highest_committed_sequence {
        return Ok(false);
    }
    let generation_id = current
        .get("generation_id")
        .and_then(serde_json::Value::as_str)
        .filter(|value| is_safe_component(value))
        .ok_or(ReferenceError::CorruptRecord)?;
    let manifest = read_json_value(&layout.generations.join(generation_id).join("manifest.json"))?;
    let Some(sources) = manifest
        .get("sources")
        .and_then(serde_json::Value::as_array)
    else {
        return Err(ReferenceError::CorruptRecord);
    };
    Ok(receipt.records.iter().all(|record| {
        sources.iter().any(|source| {
            source.get("source_id").and_then(serde_json::Value::as_str)
                == Some(record.source_id.as_str())
                && source.get("revision").and_then(serde_json::Value::as_u64)
                    == Some(record.revision)
                && source.get("event_id").and_then(serde_json::Value::as_str)
                    == Some(record.event_id.as_str())
        })
    }))
}

fn read_json_value(path: &Path) -> Result<serde_json::Value, ReferenceError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > 4 * 1024 * 1024
    {
        return Err(ReferenceError::CorruptRecord);
    }
    serde_json::from_slice(&fs::read(path)?).map_err(|_| ReferenceError::CorruptRecord)
}

fn is_safe_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}
