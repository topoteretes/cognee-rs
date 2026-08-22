//! Normalized event envelopes and deterministic identifiers.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::config::lowercase_hex;
use crate::hook_input::HookInput;
use crate::redact::{redact_json, truncate_utf8};

pub const EVENT_SCHEMA_VERSION: u32 = 1;
pub const PROMPT_LIMIT_BYTES: usize = 32 * 1024;
pub const RESPONSE_LIMIT_BYTES: usize = 32 * 1024;
pub const TOOL_INPUT_LIMIT_BYTES: usize = 16 * 1024;
pub const TOOL_RESPONSE_LIMIT_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HookEvent {
    SessionStart,
    BeforeAgent,
    AfterTool,
    AfterAgent,
    PreCompress,
    SessionEnd,
}

impl HookEvent {
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        match name {
            "SessionStart" => Some(Self::SessionStart),
            "BeforeAgent" => Some(Self::BeforeAgent),
            "AfterTool" => Some(Self::AfterTool),
            "AfterAgent" => Some(Self::AfterAgent),
            "PreCompress" => Some(Self::PreCompress),
            "SessionEnd" => Some(Self::SessionEnd),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SessionStart => "SessionStart",
            Self::BeforeAgent => "BeforeAgent",
            Self::AfterTool => "AfterTool",
            Self::AfterAgent => "AfterAgent",
            Self::PreCompress => "PreCompress",
            Self::SessionEnd => "SessionEnd",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventKind {
    SessionStart,
    BeforeAgent,
    AfterTool,
    AfterAgent,
    PreCompress,
    SessionEnd,
    McpRemember,
}

impl EventKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SessionStart => "SessionStart",
            Self::BeforeAgent => "BeforeAgent",
            Self::AfterTool => "AfterTool",
            Self::AfterAgent => "AfterAgent",
            Self::PreCompress => "PreCompress",
            Self::SessionEnd => "SessionEnd",
            Self::McpRemember => "McpRemember",
        }
    }
}

impl From<HookEvent> for EventKind {
    fn from(event: HookEvent) -> Self {
        match event {
            HookEvent::SessionStart => Self::SessionStart,
            HookEvent::BeforeAgent => Self::BeforeAgent,
            HookEvent::AfterTool => Self::AfterTool,
            HookEvent::AfterAgent => Self::AfterAgent,
            HookEvent::PreCompress => Self::PreCompress,
            HookEvent::SessionEnd => Self::SessionEnd,
        }
    }
}

pub type EventPayload = Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureMetadata {
    pub original_bytes: usize,
    pub retained_bytes: usize,
    pub redaction_count: usize,
    pub truncation_count: usize,
    pub prompt_truncated: bool,
    pub response_truncated: bool,
    pub tool_input_truncated: bool,
    pub tool_response_truncated: bool,
    pub capture_degraded: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub schema_version: u32,
    pub event_id: String,
    pub engineer: String,
    pub host: String,
    pub session_id: String,
    pub event: EventKind,
    pub timestamp: String,
    pub cwd: String,
    pub dataset: String,
    pub dataset_generation: u64,
    pub payload_hash: String,
    pub payload: EventPayload,
    pub capture: CaptureMetadata,
}

impl EventEnvelope {
    pub fn from_hook(
        hook: HookInput,
        engineer: impl Into<String>,
        host: impl Into<String>,
        dataset: impl Into<String>,
        dataset_generation: u64,
    ) -> Self {
        let redacted = redact_json(&hook.payload);
        let (payload, truncation) = bound_payload(redacted.value, hook.event);
        let canonical_payload = canonical_json(&payload);
        let payload_hash = sha256_hex(canonical_payload.as_bytes());
        let event = EventKind::from(hook.event);
        let event_id = deterministic_event_id(
            EVENT_SCHEMA_VERSION,
            &hook.session_id,
            event.as_str(),
            &hook.timestamp,
            &hook.cwd,
            dataset_generation,
            &payload_hash,
        );
        Self {
            schema_version: EVENT_SCHEMA_VERSION,
            event_id,
            engineer: engineer.into(),
            host: host.into(),
            session_id: hook.session_id,
            event,
            timestamp: hook.timestamp,
            cwd: hook.cwd,
            dataset: dataset.into(),
            dataset_generation,
            payload_hash,
            payload,
            capture: CaptureMetadata {
                original_bytes: hook.original_bytes,
                retained_bytes: canonical_payload.len(),
                redaction_count: redacted.redaction_count,
                truncation_count: truncation.count(),
                prompt_truncated: truncation.prompt,
                response_truncated: truncation.response,
                tool_input_truncated: truncation.tool_input,
                tool_response_truncated: truncation.tool_response,
                capture_degraded: false,
            },
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn from_mcp_remember(
        data: &str,
        session_id: Option<&str>,
        self_improvement: bool,
        engineer: impl Into<String>,
        host: impl Into<String>,
        timestamp: impl Into<String>,
        cwd: impl Into<String>,
        dataset: impl Into<String>,
        dataset_generation: u64,
    ) -> Self {
        let timestamp = timestamp.into();
        let cwd = cwd.into();
        let dataset = dataset.into();
        let envelope_session_id = session_id.unwrap_or_default().to_owned();
        let mut payload = Map::from_iter([
            ("data".to_owned(), Value::String(data.to_owned())),
            ("self_improvement".to_owned(), Value::Bool(self_improvement)),
        ]);
        if let Some(session_id) = session_id {
            payload.insert(
                "session_id".to_owned(),
                Value::String(session_id.to_owned()),
            );
        }
        let payload = Value::Object(payload);
        let original_bytes = canonical_json(&payload).len();
        let redacted = redact_json(&payload);
        let mut payload = redacted.value;
        let data = payload
            .get("data")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let (data, truncated) = truncate_utf8(data, RESPONSE_LIMIT_BYTES);
        payload["data"] = json!(data);
        let canonical_payload = canonical_json(&payload);
        let payload_hash = sha256_hex(canonical_payload.as_bytes());
        let event_id = deterministic_event_id(
            EVENT_SCHEMA_VERSION,
            &envelope_session_id,
            EventKind::McpRemember.as_str(),
            &timestamp,
            &cwd,
            dataset_generation,
            &payload_hash,
        );
        Self {
            schema_version: EVENT_SCHEMA_VERSION,
            event_id,
            engineer: engineer.into(),
            host: host.into(),
            session_id: envelope_session_id,
            event: EventKind::McpRemember,
            timestamp,
            cwd,
            dataset,
            dataset_generation,
            payload_hash,
            payload,
            capture: CaptureMetadata {
                original_bytes,
                retained_bytes: canonical_payload.len(),
                redaction_count: redacted.redaction_count,
                truncation_count: usize::from(truncated),
                prompt_truncated: false,
                response_truncated: truncated,
                tool_input_truncated: false,
                tool_response_truncated: false,
                capture_degraded: false,
            },
        }
    }

    /// Return the idempotency key used at the Cognee storage boundary.
    ///
    /// APEX may deliver the terminal callback more than once with a new source
    /// timestamp or cwd spelling each time. Keep those callbacks as distinct
    /// audit events while collapsing their identical session-end effect in
    /// Cognee. The payload hash still distinguishes materially different
    /// terminal reasons.
    pub fn external_event_id(&self) -> String {
        match self.event {
            EventKind::SessionEnd => deterministic_event_id(
                self.schema_version,
                &self.session_id,
                self.event.as_str(),
                "",
                "",
                self.dataset_generation,
                &self.payload_hash,
            ),
            _ => self.event_id.clone(),
        }
    }
}

pub fn canonical_json(value: &Value) -> String {
    let mut output = String::new();
    write_canonical(value, &mut output);
    output
}

fn write_canonical(value: &Value, output: &mut String) {
    match value {
        Value::Null => output.push_str("null"),
        Value::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
        Value::Number(value) => output.push_str(&value.to_string()),
        Value::String(value) => {
            let quoted = serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_owned());
            output.push_str(&quoted);
        }
        Value::Array(values) => {
            output.push('[');
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    output.push(',');
                }
                write_canonical(value, output);
            }
            output.push(']');
        }
        Value::Object(object) => {
            output.push('{');
            let mut keys: Vec<&String> = object.keys().collect();
            keys.sort_unstable();
            for (index, key) in keys.into_iter().enumerate() {
                if index != 0 {
                    output.push(',');
                }
                let quoted = serde_json::to_string(key).unwrap_or_else(|_| "\"\"".to_owned());
                output.push_str(&quoted);
                output.push(':');
                write_canonical(&object[key], output);
            }
            output.push('}');
        }
    }
}

fn deterministic_event_id(
    schema_version: u32,
    session_id: &str,
    event_name: &str,
    timestamp: &str,
    cwd: &str,
    dataset_generation: u64,
    payload_hash: &str,
) -> String {
    let schema_version = schema_version.to_string();
    let dataset_generation = dataset_generation.to_string();
    let fields = [
        schema_version.as_bytes(),
        session_id.as_bytes(),
        event_name.as_bytes(),
        timestamp.as_bytes(),
        cwd.as_bytes(),
        dataset_generation.as_bytes(),
        payload_hash.as_bytes(),
    ];
    let mut hasher = Sha256::new();
    for field in fields {
        hasher.update((field.len() as u64).to_be_bytes());
        hasher.update(field);
    }
    lowercase_hex(&hasher.finalize())
}

fn sha256_hex(bytes: &[u8]) -> String {
    lowercase_hex(&Sha256::digest(bytes))
}

#[derive(Default)]
struct Truncation {
    prompt: bool,
    response: bool,
    tool_input: bool,
    tool_response: bool,
}

impl Truncation {
    fn count(&self) -> usize {
        [
            self.prompt,
            self.response,
            self.tool_input,
            self.tool_response,
        ]
        .into_iter()
        .filter(|truncated| *truncated)
        .count()
    }
}

fn bound_payload(mut payload: Value, event: HookEvent) -> (Value, Truncation) {
    let mut truncation = Truncation::default();
    match event {
        HookEvent::BeforeAgent => {
            truncation.prompt = truncate_field(&mut payload, "prompt", PROMPT_LIMIT_BYTES);
        }
        HookEvent::AfterTool => {
            truncation.tool_input =
                truncate_field(&mut payload, "tool_input", TOOL_INPUT_LIMIT_BYTES);
            truncation.tool_response =
                truncate_field(&mut payload, "tool_response", TOOL_RESPONSE_LIMIT_BYTES);
        }
        HookEvent::AfterAgent => {
            truncation.prompt = truncate_field(&mut payload, "prompt", PROMPT_LIMIT_BYTES);
            truncation.response =
                truncate_field(&mut payload, "prompt_response", RESPONSE_LIMIT_BYTES);
        }
        HookEvent::SessionStart | HookEvent::PreCompress | HookEvent::SessionEnd => {}
    }
    (payload, truncation)
}

fn truncate_field(payload: &mut Value, field: &str, limit: usize) -> bool {
    let Some(value) = payload.get_mut(field) else {
        return false;
    };
    if let Some(text) = value.as_str() {
        let (bounded, truncated) = truncate_utf8(text, limit);
        if truncated {
            *value = Value::String(bounded);
        }
        return truncated;
    }
    let canonical = canonical_json(value);
    if canonical.len() <= limit {
        return false;
    }
    while canonical_json(value).len() > limit {
        let excess = canonical_json(value).len() - limit;
        if !shrink_one_string(value, excess) {
            *value = Value::String("[TRUNCATED]".to_owned());
            break;
        }
    }
    true
}

fn shrink_one_string(value: &mut Value, excess: usize) -> bool {
    match value {
        Value::String(text) if !text.is_empty() => {
            let target = text.len().saturating_sub(excess.max(1));
            let (bounded, _) = truncate_utf8(text, target);
            *text = bounded;
            true
        }
        Value::Array(values) => values
            .iter_mut()
            .any(|value| shrink_one_string(value, excess)),
        Value::Object(object) => {
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort_unstable();
            keys.into_iter().any(|key| {
                object
                    .get_mut(&key)
                    .is_some_and(|value| shrink_one_string(value, excess))
            })
        }
        _ => false,
    }
}
