//! Fast, fail-open implementation of the six official APEX hooks.

use std::io::{self, Read, Write};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::atomic_fs::{ReplaceMode, SystemSyncOps, ensure_private_directory, write_atomic};
use crate::config::{AgentConfig, ProcessEnv};
use crate::context::ContextCache;
use crate::detach::{DrainSpawner, SystemDrainSpawner};
use crate::event::{EventEnvelope, HookEvent};
use crate::generation::GenerationStore;
use crate::hook_input::{HookInput, SchemaError};
use crate::spool::{Priority, Spool};

pub const MAX_HOOK_INPUT_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookResponse {
    #[serde(rename = "suppressOutput")]
    pub suppress_output: bool,
    #[serde(rename = "hookSpecificOutput", skip_serializing_if = "Option::is_none")]
    pub hook_specific_output: Option<HookSpecificOutput>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookSpecificOutput {
    #[serde(rename = "hookEventName")]
    pub hook_event_name: String,
    #[serde(rename = "additionalContext")]
    pub additional_context: String,
}

impl HookResponse {
    pub const fn capture() -> Self {
        Self {
            suppress_output: true,
            hook_specific_output: None,
        }
    }

    fn with_context(event: HookEvent, additional_context: String) -> Self {
        Self {
            suppress_output: true,
            hook_specific_output: Some(HookSpecificOutput {
                hook_event_name: event.as_str().to_owned(),
                additional_context,
            }),
        }
    }
}

pub struct HookServices {
    config: AgentConfig,
    spawner: Arc<dyn DrainSpawner>,
    engineer: String,
    host: String,
    dataset_generation: Result<u64, ()>,
}

impl HookServices {
    pub fn new(config: AgentConfig, spawner: Arc<dyn DrainSpawner>) -> Self {
        let dataset_generation = GenerationStore::new(config.layout.clone())
            .current(&config.dataset)
            .map_err(|_| ());
        Self {
            config,
            spawner,
            engineer: process_engineer(),
            host: system_hostname(),
            dataset_generation,
        }
    }

    pub fn with_identity(mut self, engineer: impl Into<String>, host: impl Into<String>) -> Self {
        self.engineer = engineer.into();
        self.host = host.into();
        self
    }

    pub fn config(&self) -> &AgentConfig {
        &self.config
    }
}

pub fn run_hook<R: Read, W: Write>(input: R, mut output: W) -> io::Result<()> {
    let mut diagnostics = io::stderr().lock();
    let config = match AgentConfig::from_env(&ProcessEnv) {
        Ok(config) => config,
        Err(_) => {
            let _ = writeln!(diagnostics, "cognee hook: configuration");
            return write_response(&mut output, &HookResponse::capture());
        }
    };
    let services = HookServices::new(config, Arc::new(SystemDrainSpawner));
    run_hook_with(input, output, &mut diagnostics, &services)
}

pub fn run_hook_with<R: Read, W: Write, E: Write>(
    mut input: R,
    mut output: W,
    mut diagnostics: E,
    services: &HookServices,
) -> io::Result<()> {
    let mut raw = Vec::new();
    let read_result = input
        .by_ref()
        .take((MAX_HOOK_INPUT_BYTES + 1) as u64)
        .read_to_end(&mut raw);
    if read_result.is_err() {
        report_failure(services, &mut diagnostics, "input_io");
        return write_response(&mut output, &HookResponse::capture());
    }
    if raw.len() > MAX_HOOK_INPUT_BYTES {
        report_failure(services, &mut diagnostics, "input_too_large");
        return write_response(&mut output, &HookResponse::capture());
    }

    let hook = match HookInput::parse(&raw) {
        Ok(hook) => hook,
        Err(error) => {
            report_failure(services, &mut diagnostics, schema_error_class(&error));
            return write_response(&mut output, &HookResponse::capture());
        }
    };
    let event = hook.event;
    let session_id = hook.session_id.clone();
    let generation = match services.dataset_generation {
        Ok(generation) => generation,
        Err(()) => {
            report_failure(services, &mut diagnostics, "generation");
            return write_response(&mut output, &HookResponse::capture());
        }
    };
    let envelope = EventEnvelope::from_hook(
        hook,
        &services.engineer,
        &services.host,
        &services.config.dataset,
        generation,
    );
    let spool = Spool::new(
        services.config.layout.clone(),
        services.config.limits.clone(),
    );
    let durable = match spool.enqueue(&envelope, priority(event)) {
        Ok(_) => true,
        Err(_) => {
            report_failure(services, &mut diagnostics, "spool");
            false
        }
    };

    let response = if matches!(event, HookEvent::SessionStart) {
        let cache = ContextCache::new(services.config.layout.clone());
        let cached = cache.read(&session_id).and_then(|context| match context {
            Some(context) => Ok(Some(context)),
            None => cache.read_bootstrap(&services.config.dataset),
        });
        match cached {
            Ok(Some(context)) => HookResponse::with_context(event, context),
            Ok(None) => HookResponse::capture(),
            Err(_) => {
                report_failure(services, &mut diagnostics, "context_cache");
                HookResponse::capture()
            }
        }
    } else {
        HookResponse::capture()
    };
    write_response(&mut output, &response)?;

    if durable
        && should_spawn(event, &spool, services.config.limits.max_events_per_drain)
        && services.spawner.spawn().is_err()
    {
        report_failure(services, &mut diagnostics, "drain_spawn");
    }
    Ok(())
}

fn write_response(output: &mut impl Write, response: &HookResponse) -> io::Result<()> {
    serde_json::to_writer(&mut *output, response).map_err(io::Error::other)?;
    output.write_all(b"\n")?;
    output.flush()
}

fn should_spawn(event: HookEvent, spool: &Spool, after_tool_threshold: u32) -> bool {
    match event {
        HookEvent::SessionStart
        | HookEvent::AfterAgent
        | HookEvent::PreCompress
        | HookEvent::SessionEnd => true,
        HookEvent::AfterTool => spool.depths().is_ok_and(|depths| {
            depths.pending >= usize::try_from(after_tool_threshold).unwrap_or(usize::MAX)
        }),
        HookEvent::BeforeAgent => false,
    }
}

const fn priority(event: HookEvent) -> Priority {
    if matches!(event, HookEvent::PreCompress) {
        Priority::High
    } else {
        Priority::Normal
    }
}

fn schema_error_class(error: &SchemaError) -> &'static str {
    match error {
        SchemaError::InvalidJson(_) => "invalid_json",
        SchemaError::InvalidTimestamp => "invalid_timestamp",
        SchemaError::ExpectedObject
        | SchemaError::MissingField(_)
        | SchemaError::InvalidField(_)
        | SchemaError::UnknownHookEvent => "invalid_schema",
    }
}

fn report_failure(services: &HookServices, diagnostics: &mut impl Write, class: &'static str) {
    let _ = writeln!(diagnostics, "cognee hook: {class}");
    let status_directory = &services.config.layout.status;
    if ensure_private_directory(status_directory).is_err() {
        return;
    }
    let Ok(bytes) = serde_json::to_vec(&json!({
        "error_class": class,
        "timestamp": chrono::Utc::now().to_rfc3339()
    })) else {
        return;
    };
    let _ = write_atomic(
        &status_directory.join("hook-last-error.json"),
        &bytes,
        ReplaceMode::Replace,
        &SystemSyncOps,
    );
}

fn process_engineer() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown-engineer".to_owned())
}

fn system_hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .or_else(|_| std::fs::read_to_string("/etc/hostname"))
        .map(|value| value.trim().to_owned())
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown-host".to_owned())
}
