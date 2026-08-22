//! Strict parsing for the six official APEX hook input schemas.

use chrono::DateTime;
use serde_json::{Map, Value};
use thiserror::Error;

use crate::event::HookEvent;

#[derive(Debug, Clone, PartialEq)]
pub struct HookInput {
    pub session_id: String,
    pub transcript_path: String,
    pub cwd: String,
    pub event: HookEvent,
    pub timestamp: String,
    pub payload: Value,
    pub(crate) original_bytes: usize,
}

#[derive(Debug, Error)]
pub enum SchemaError {
    #[error("hook input is not valid JSON")]
    InvalidJson(#[source] serde_json::Error),
    #[error("hook input must be a JSON object")]
    ExpectedObject,
    #[error("missing required hook field {0}")]
    MissingField(&'static str),
    #[error("hook field {0} has the wrong type")]
    InvalidField(&'static str),
    #[error("timestamp must be RFC3339")]
    InvalidTimestamp,
    #[error("hook_event_name is not an official hook event")]
    UnknownHookEvent,
}

impl HookInput {
    pub fn parse(raw: &[u8]) -> Result<Self, SchemaError> {
        let value: Value = serde_json::from_slice(raw).map_err(SchemaError::InvalidJson)?;
        let object = value.as_object().ok_or(SchemaError::ExpectedObject)?;
        let session_id = required_string(object, "session_id")?;
        let transcript_path = required_string(object, "transcript_path")?;
        let cwd = required_string(object, "cwd")?;
        let event_name = required_string(object, "hook_event_name")?;
        let timestamp = required_string(object, "timestamp")?;
        DateTime::parse_from_rfc3339(&timestamp).map_err(|_| SchemaError::InvalidTimestamp)?;

        let event = HookEvent::from_name(&event_name).ok_or(SchemaError::UnknownHookEvent)?;
        let payload = Value::Object(event_payload(object, event)?);
        Ok(Self {
            session_id,
            transcript_path,
            cwd,
            event,
            timestamp,
            payload,
            original_bytes: raw.len(),
        })
    }
}

fn event_payload(
    object: &Map<String, Value>,
    event: HookEvent,
) -> Result<Map<String, Value>, SchemaError> {
    let fields: &[(&str, FieldType)] = match event {
        HookEvent::SessionStart => &[("source", FieldType::String)],
        HookEvent::BeforeAgent => &[("prompt", FieldType::String)],
        HookEvent::AfterTool => &[
            ("tool_name", FieldType::String),
            ("tool_input", FieldType::Any),
            ("tool_response", FieldType::Any),
        ],
        HookEvent::AfterAgent => &[
            ("prompt", FieldType::String),
            ("prompt_response", FieldType::String),
            ("stop_hook_active", FieldType::Bool),
        ],
        HookEvent::PreCompress => &[("trigger", FieldType::String)],
        HookEvent::SessionEnd => &[("reason", FieldType::String)],
    };
    let mut payload = Map::new();
    for &(field, expected_type) in fields {
        let value = object
            .get(field)
            .ok_or_else(|| missing_event_field(field))?;
        let type_matches = match expected_type {
            FieldType::Any => true,
            FieldType::String => value.is_string(),
            FieldType::Bool => value.is_boolean(),
        };
        if !type_matches {
            return Err(invalid_event_field(field));
        }
        payload.insert(field.to_owned(), value.clone());
    }
    Ok(payload)
}

fn required_string(
    object: &Map<String, Value>,
    field: &'static str,
) -> Result<String, SchemaError> {
    let value = object.get(field).ok_or(SchemaError::MissingField(field))?;
    value
        .as_str()
        .map(str::to_owned)
        .ok_or(SchemaError::InvalidField(field))
}

fn missing_event_field(field: &str) -> SchemaError {
    match field {
        "source" => SchemaError::MissingField("source"),
        "prompt" => SchemaError::MissingField("prompt"),
        "tool_name" => SchemaError::MissingField("tool_name"),
        "tool_input" => SchemaError::MissingField("tool_input"),
        "tool_response" => SchemaError::MissingField("tool_response"),
        "prompt_response" => SchemaError::MissingField("prompt_response"),
        "stop_hook_active" => SchemaError::MissingField("stop_hook_active"),
        "trigger" => SchemaError::MissingField("trigger"),
        "reason" => SchemaError::MissingField("reason"),
        _ => SchemaError::ExpectedObject,
    }
}

fn invalid_event_field(field: &str) -> SchemaError {
    match field {
        "source" => SchemaError::InvalidField("source"),
        "prompt" => SchemaError::InvalidField("prompt"),
        "tool_name" => SchemaError::InvalidField("tool_name"),
        "tool_input" => SchemaError::InvalidField("tool_input"),
        "tool_response" => SchemaError::InvalidField("tool_response"),
        "prompt_response" => SchemaError::InvalidField("prompt_response"),
        "stop_hook_active" => SchemaError::InvalidField("stop_hook_active"),
        "trigger" => SchemaError::InvalidField("trigger"),
        "reason" => SchemaError::InvalidField("reason"),
        _ => SchemaError::ExpectedObject,
    }
}

#[derive(Clone, Copy)]
enum FieldType {
    Any,
    String,
    Bool,
}
