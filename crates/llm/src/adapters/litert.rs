//! LiteRT-LM adapter for Android local inference.
//!
//! This adapter is feature-gated behind `android-litert` and compiled only on Android.
//! Structured output is achieved by embedding a compact JSON schema directly into the prompt.
//!
//! **Limitations:** Vision (`transcribe_image`) and audio transcription are not
//! supported. LiteRT is a text-only inference engine; image understanding would
//! require a separate multimodal model and integration.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use cognee_litert_lm::{
    Backend, ConstraintType, Conversation, ConversationConfig, Engine, EngineSettings,
    OptionalArgs, SamplerParams, SamplerType, SessionConfig,
};
use serde_json::{Value, json};
use tracing::{debug, warn};

use crate::error::{LlmError, LlmResult};
use crate::llm_trait::Llm;
use crate::types::{GenerationOptions, GenerationResponse, Message, MessageRole};

#[derive(Clone)]
pub struct LiteRtAdapter {
    model: String,
    backend: String,
    engine: Arc<Engine>,
    conversation_lock: Arc<Mutex<()>>,
}

impl LiteRtAdapter {
    pub fn new(model_path: impl Into<String>, backend: Option<String>) -> LlmResult<Self> {
        let model = model_path.into();
        let backend = backend.unwrap_or_else(|| "cpu".to_string());

        // Keep LiteRT logs quiet in normal operation.
        cognee_litert_lm::set_min_log_level(3);

        let engine_backend = match backend.to_lowercase().as_str() {
            "cpu" => Backend::Cpu,
            "gpu" => Backend::Gpu,
            other => Backend::Custom(other.to_string()),
        };

        let settings = EngineSettings::new(&model, engine_backend, None, None).map_err(|e| {
            LlmError::LocalModelError(format!("Failed to create engine settings: {e}"))
        })?;
        let engine = settings
            .build()
            .map_err(|e| LlmError::LocalModelError(format!("Failed to create engine: {e}")))?;

        Ok(Self {
            model,
            backend,
            engine: Arc::new(engine),
            conversation_lock: Arc::new(Mutex::new(())),
        })
    }

    fn build_prompt(messages: &[Message]) -> String {
        let mut prompt = String::new();

        for message in messages {
            let role = match message.role {
                MessageRole::System => "System",
                MessageRole::User => "User",
                MessageRole::Assistant => "Assistant",
            };
            prompt.push_str(role);
            prompt.push_str(":\n");
            prompt.push_str(&message.content);
            prompt.push_str("\n\n");
        }

        if !messages
            .last()
            .map(|m| matches!(m.role, MessageRole::Assistant))
            .unwrap_or(false)
        {
            prompt.push_str("Assistant:\n");
        }

        prompt
    }

    fn extract_text_content(response_json: &str) -> String {
        let parsed = serde_json::from_str::<Value>(response_json);
        if let Ok(value) = parsed
            && let Some(content_items) = value.get("content").and_then(Value::as_array)
        {
            let mut text = String::new();
            for item in content_items {
                if let Some(chunk) = item.get("text").and_then(Value::as_str) {
                    text.push_str(chunk);
                }
            }
            if !text.trim().is_empty() {
                return text;
            }
        }

        response_json.to_string()
    }

    fn compact_schema(json_schema: &Value) -> LlmResult<String> {
        serde_json::to_string(json_schema)
            .map_err(|e| LlmError::SerializationError(format!("Failed to compact schema: {e}")))
    }

    fn extract_first_json_object(text: &str) -> Option<&str> {
        let mut depth: i32 = 0;
        let mut start: Option<usize> = None;
        let mut in_string = false;
        let mut escaped = false;

        for (index, ch) in text.char_indices() {
            if in_string {
                if escaped {
                    escaped = false;
                    continue;
                }
                if ch == '\\' {
                    escaped = true;
                    continue;
                }
                if ch == '"' {
                    in_string = false;
                }
                continue;
            }

            match ch {
                '"' => in_string = true,
                '{' => {
                    if depth == 0 {
                        start = Some(index);
                    }
                    depth += 1;
                }
                '}' => {
                    if depth > 0 {
                        depth -= 1;
                        if depth == 0
                            && let Some(start_index) = start
                        {
                            return Some(&text[start_index..=index]);
                        }
                    }
                }
                _ => {}
            }
        }

        None
    }

    fn extract_json_code_fence(text: &str) -> Option<&str> {
        let rest = text.trim();
        let start = rest.find("```")?;
        let rest = &rest[start + 3..];
        let rest = rest
            .strip_prefix("json")
            .or_else(|| rest.strip_prefix("JSON"))
            .unwrap_or(rest);
        let rest = rest.strip_prefix('\n').unwrap_or(rest);
        let end = rest.find("```")?;
        Some(rest[..end].trim())
    }

    fn extract_json_code_fence_loose(text: &str) -> Option<&str> {
        let start = text.find("```")?;
        let rest = &text[start + 3..];
        let rest = rest
            .strip_prefix("json")
            .or_else(|| rest.strip_prefix("JSON"))
            .unwrap_or(rest);
        let rest = rest.strip_prefix('\n').unwrap_or(rest);

        // If the closing fence is missing, still return the unfenced body so
        // repair logic can attempt to close truncated JSON brackets.
        let body = if let Some(end) = rest.find("```") {
            &rest[..end]
        } else {
            rest
        };

        Some(body.trim())
    }

    fn extract_json_objects(text: &str) -> Vec<&str> {
        let mut out = Vec::new();
        let mut depth: i32 = 0;
        let mut start: Option<usize> = None;
        let mut in_string = false;
        let mut escaped = false;

        for (index, ch) in text.char_indices() {
            if in_string {
                if escaped {
                    escaped = false;
                    continue;
                }
                if ch == '\\' {
                    escaped = true;
                    continue;
                }
                if ch == '"' {
                    in_string = false;
                }
                continue;
            }

            match ch {
                '"' => in_string = true,
                '{' => {
                    if depth == 0 {
                        start = Some(index);
                    }
                    depth += 1;
                }
                '}' => {
                    if depth > 0 {
                        depth -= 1;
                        if depth == 0
                            && let Some(s) = start
                        {
                            out.push(&text[s..=index]);
                            start = None;
                        }
                    }
                }
                _ => {}
            }
        }

        out
    }

    fn is_schema_echo_object(value: &Value) -> bool {
        let Some(obj) = value.as_object() else {
            return false;
        };

        obj.contains_key("$schema")
            || (obj.contains_key("definitions") && obj.contains_key("properties"))
    }

    fn matches_expected_shape(value: &Value, json_schema: &Value) -> bool {
        let Some(value_obj) = value.as_object() else {
            return false;
        };

        let schema_properties = json_schema
            .get("properties")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();

        if schema_properties.is_empty() {
            return true;
        }

        let has_any_expected_property = schema_properties
            .keys()
            .any(|key| value_obj.contains_key(key));

        if !has_any_expected_property {
            return false;
        }

        if let Some(required) = json_schema.get("required").and_then(Value::as_array) {
            return required
                .iter()
                .filter_map(Value::as_str)
                .all(|key| value_obj.contains_key(key));
        }

        true
    }

    fn parse_structured_json(raw_text: &str, json_schema: &Value) -> LlmResult<Value> {
        let trimmed = raw_text.trim();

        match serde_json::from_str::<Value>(trimmed) {
            Ok(value) => {
                if !Self::is_schema_echo_object(&value)
                    && Self::matches_expected_shape(&value, json_schema)
                {
                    debug!(
                        "LiteRT parse attempt raw_trimmed: accepted (len={})",
                        trimmed.len()
                    );
                    return Ok(value);
                }

                debug!(
                    "LiteRT parse attempt raw_trimmed: parsed but rejected by shape/schema_echo (len={})",
                    trimmed.len()
                );
            }
            Err(err) => {
                debug!("LiteRT parse attempt raw_trimmed failed: {err}; raw_text=\n{trimmed}");
            }
        }

        let mut candidates: Vec<(&str, &str)> = Vec::new();

        if let Some(fenced) = Self::extract_json_code_fence(trimmed) {
            candidates.push(("fenced", fenced));
        }

        // Also try a loose unfenced body so truncated fenced output (missing
        // closing fence/brackets) can still be repaired and parsed.
        if let Some(fenced_loose) = Self::extract_json_code_fence_loose(trimmed) {
            candidates.push(("fenced_loose", fenced_loose));
        }

        for (index, candidate) in Self::extract_json_objects(trimmed).into_iter().enumerate() {
            let label = if index == 0 {
                "object_0"
            } else if index == 1 {
                "object_1"
            } else if index == 2 {
                "object_2"
            } else {
                "object_n"
            };
            candidates.push((label, candidate));
        }

        if candidates.is_empty()
            && let Some(candidate) = Self::extract_first_json_object(trimmed)
        {
            candidates.push(("first_object", candidate));
        }

        for (source, candidate) in candidates {
            let Some(value) = Self::parse_or_repair_json(source, candidate) else {
                continue;
            };

            if Self::is_schema_echo_object(&value) {
                debug!(
                    "LiteRT parse attempt {source}: parsed but detected schema echo; candidate=\n{candidate}"
                );
                continue;
            }

            if Self::matches_expected_shape(&value, json_schema) {
                debug!("LiteRT parse attempt {source}: accepted structured output");
                return Ok(value);
            }

            debug!(
                "LiteRT parse attempt {source}: parsed but rejected by schema shape; candidate=\n{candidate}"
            );
        }

        Err(LlmError::DeserializationError(format!(
            "No valid JSON object found in model response: {raw_text}"
        )))
    }

    fn parse_or_repair_json(source: &str, candidate: &str) -> Option<Value> {
        match serde_json::from_str::<Value>(candidate) {
            Ok(value) => {
                debug!("LiteRT parse attempt {source}: JSON parse succeeded");
                return Some(value);
            }
            Err(parse_err) => {
                debug!(
                    "LiteRT parse attempt {source}: JSON parse failed: {parse_err}; candidate=\n{candidate}"
                );
            }
        }

        let Some(repaired) = Self::repair_truncated_json(candidate) else {
            debug!("LiteRT parse attempt {source}: repair skipped/not possible");
            return None;
        };

        match serde_json::from_str::<Value>(&repaired) {
            Ok(value) => {
                debug!(
                    "LiteRT parse attempt {source}: parse succeeded after repair; repaired=\n{repaired}"
                );
                Some(value)
            }
            Err(repair_err) => {
                debug!(
                    "LiteRT parse attempt {source}: parse failed after repair: {repair_err}; repaired=\n{repaired}"
                );
                None
            }
        }
    }

    fn repair_truncated_json(candidate: &str) -> Option<String> {
        let mut stack: Vec<char> = Vec::new();
        let mut in_string = false;
        let mut escaped = false;

        for ch in candidate.chars() {
            if in_string {
                if escaped {
                    escaped = false;
                    continue;
                }
                if ch == '\\' {
                    escaped = true;
                    continue;
                }
                if ch == '"' {
                    in_string = false;
                }
                continue;
            }

            match ch {
                '"' => in_string = true,
                '{' => stack.push('}'),
                '[' => stack.push(']'),
                '}' | ']' => {
                    if let Some(expected) = stack.pop() {
                        if ch != expected {
                            return None;
                        }
                    } else {
                        return None;
                    }
                }
                _ => {}
            }
        }

        if in_string {
            return None;
        }

        if stack.is_empty() {
            return None;
        }

        let mut repaired = candidate.trim_end().to_string();
        while let Some(closer) = stack.pop() {
            repaired.push(closer);
        }

        Some(repaired)
    }

    fn looks_like_schema_echo(raw_text: &str) -> bool {
        raw_text.contains("\"$schema\"")
            || (raw_text.contains("\"definitions\"") && raw_text.contains("\"properties\""))
    }

    fn resolve_local_ref<'a>(schema_root: &'a Value, ref_path: &str) -> Option<&'a Value> {
        let path = ref_path.strip_prefix("#/")?;
        let mut current = schema_root;
        for part in path.split('/') {
            current = current.get(part)?;
        }
        Some(current)
    }

    fn minimal_value_from_schema(schema_root: &Value, schema: &Value) -> Value {
        if let Some(ref_path) = schema.get("$ref").and_then(Value::as_str)
            && let Some(resolved) = Self::resolve_local_ref(schema_root, ref_path)
        {
            return Self::minimal_value_from_schema(schema_root, resolved);
        }

        if let Some(default) = schema.get("default") {
            return default.clone();
        }

        match schema.get("type").and_then(Value::as_str) {
            Some("object") => {
                let mut object = serde_json::Map::new();
                if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
                    for (key, property_schema) in properties {
                        object.insert(
                            key.clone(),
                            Self::minimal_value_from_schema(schema_root, property_schema),
                        );
                    }
                }

                if let Some(required) = schema.get("required").and_then(Value::as_array) {
                    for key in required.iter().filter_map(Value::as_str) {
                        object.entry(key.to_string()).or_insert(Value::Null);
                    }
                }

                Value::Object(object)
            }
            Some("array") => Value::Array(Vec::new()),
            Some("string") => Value::String(String::new()),
            Some("integer") | Some("number") => Value::from(0),
            Some("boolean") => Value::Bool(false),
            _ => Value::Null,
        }
    }

    fn fallback_structured_object(json_schema: &Value) -> Value {
        if json_schema.get("type").and_then(Value::as_str) == Some("object") {
            return Self::minimal_value_from_schema(json_schema, json_schema);
        }

        if let Some(ref_path) = json_schema.get("$ref").and_then(Value::as_str)
            && let Some(resolved) = Self::resolve_local_ref(json_schema, ref_path)
        {
            return Self::minimal_value_from_schema(json_schema, resolved);
        }

        // If schema is unconventional, still return a stable object payload.
        json!({})
    }

    fn configure_session(options: &GenerationOptions) -> LlmResult<SessionConfig> {
        let mut session_config = SessionConfig::new().map_err(|e| {
            LlmError::LocalModelError(format!("Failed to create LiteRT session config: {e}"))
        })?;

        if let Some(max_tokens) = options.max_tokens {
            let max_tokens = i32::try_from(max_tokens).map_err(|_| {
                LlmError::ConfigError(format!("max_tokens is too large for LiteRT: {max_tokens}"))
            })?;
            session_config.set_max_output_tokens(max_tokens);
        }

        let mut sampler = SamplerParams::default();
        if let Some(temperature) = options.temperature {
            sampler.temperature = temperature.max(0.0);
        }
        if let Some(top_p) = options.top_p {
            sampler.sampler_type = SamplerType::TopP;
            sampler.top_p = top_p.clamp(0.0, 1.0);
        }
        session_config.set_sampler_params(&sampler);

        Ok(session_config)
    }

    fn run_prompt(
        engine: Arc<Engine>,
        prompt: String,
        options: GenerationOptions,
    ) -> LlmResult<String> {
        let session_config = Self::configure_session(&options)?;

        let message_json = json!({
            "role": "user",
            "content": [{"type": "text", "text": prompt}],
        })
        .to_string();

        let mut last_error = None;
        for attempt in 0..3 {
            let conversation_config = ConversationConfig::new(
                engine.as_ref(),
                Some(&session_config),
                None,
                None,
                None,
                false,
            )
            .map_err(|e| {
                LlmError::LocalModelError(format!("Failed to create conversation config: {e}"))
            })?;

            match Conversation::new(engine.as_ref(), Some(conversation_config)) {
                Ok(conversation) => {
                    let response = conversation.send_message(&message_json).map_err(|e| {
                        LlmError::LocalModelError(format!("LiteRT message send failed: {e}"))
                    })?;

                    let raw_response = response.as_str().ok_or_else(|| {
                        LlmError::InvalidResponse(
                            "LiteRT returned an empty response body".to_string(),
                        )
                    })?;

                    return Ok(Self::extract_text_content(raw_response));
                }
                Err(err) => {
                    last_error = Some(err.to_string());
                    if attempt < 2 {
                        std::thread::sleep(Duration::from_millis(50 * (attempt + 1) as u64));
                    }
                }
            }
        }

        Err(LlmError::LocalModelError(format!(
            "Failed to create conversation: {}",
            last_error.unwrap_or_else(|| "unknown error".to_string())
        )))
    }

    fn run_structured_prompt(
        engine: Arc<Engine>,
        prompt: String,
        json_schema: String,
        options: GenerationOptions,
    ) -> LlmResult<String> {
        let session_config = Self::configure_session(&options)?;
        let conversation_config = ConversationConfig::new(
            engine.as_ref(),
            Some(&session_config),
            None,
            None,
            None,
            true,
        )
        .map_err(|e| {
            LlmError::LocalModelError(format!("Failed to create conversation config: {e}"))
        });

        let conversation_config = match conversation_config {
            Ok(cfg) => cfg,
            Err(err) => {
                warn!(
                    "LiteRT constrained conversation config failed, falling back to unconstrained generation: {err}"
                );
                return Self::run_prompt(engine, prompt, options);
            }
        };

        let conversation = Conversation::new(engine.as_ref(), Some(conversation_config));
        let conversation = match conversation {
            Ok(c) => c,
            Err(err) => {
                warn!(
                    "LiteRT constrained conversation creation failed, falling back to unconstrained generation: {err}"
                );
                return Self::run_prompt(engine, prompt, options);
            }
        };

        let mut optional_args = OptionalArgs::new()
            .map_err(|e| LlmError::LocalModelError(format!("Failed to create optional args: {e}")));
        let Some(optional_args) = optional_args.as_mut().ok() else {
            warn!("LiteRT optional args creation failed, falling back to unconstrained generation");
            return Self::run_prompt(engine, prompt, options);
        };

        if optional_args
            .set_constraint(ConstraintType::JsonSchema, &json_schema)
            .is_err()
        {
            warn!(
                "LiteRT JSON schema constraint unsupported/failed, falling back to unconstrained generation"
            );
            return Self::run_prompt(engine, prompt, options);
        }

        let message_json = json!({
            "role": "user",
            "content": [{"type": "text", "text": prompt}],
        })
        .to_string();

        let response = conversation
            .send_message_with_args(&message_json, Some(optional_args))
            .map_err(|e| LlmError::LocalModelError(format!("LiteRT message send failed: {e}")));

        let response = match response {
            Ok(r) => r,
            Err(err) => {
                warn!(
                    "LiteRT constrained send failed, falling back to unconstrained generation: {err}"
                );
                return Self::run_prompt(engine, prompt, options);
            }
        };

        let raw_response = response.as_str().ok_or_else(|| {
            LlmError::InvalidResponse("LiteRT returned an empty response body".to_string())
        })?;

        Ok(Self::extract_text_content(raw_response))
    }
}

#[async_trait]
impl Llm for LiteRtAdapter {
    async fn generate(
        &self,
        messages: Vec<Message>,
        options: Option<GenerationOptions>,
    ) -> LlmResult<GenerationResponse> {
        let engine = Arc::clone(&self.engine);
        let conversation_lock = Arc::clone(&self.conversation_lock);
        let prompt = Self::build_prompt(&messages);
        let options = options.unwrap_or_default();
        let model = self.model.clone();

        let content = tokio::task::spawn_blocking(move || {
            let _guard = conversation_lock.lock().map_err(|_| {
                LlmError::LocalModelError("LiteRT conversation lock poisoned".to_string())
            })?;
            Self::run_prompt(engine, prompt, options)
        })
        .await
        .map_err(|e| {
            LlmError::LocalModelError(format!("LiteRT generation task failed to join: {e}"))
        })??;

        Ok(GenerationResponse {
            content,
            model,
            usage: None,
            finish_reason: Some("stop".to_string()),
        })
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        mut messages: Vec<Message>,
        json_schema: &Value,
        options: Option<GenerationOptions>,
    ) -> LlmResult<Value> {
        let compact_schema = Self::compact_schema(json_schema)?;
        let schema_instructions = format!(
            "\n\nReturn ONLY a valid JSON object that matches this schema (compact JSON):\n{}\nNo markdown, no explanation, and no surrounding text.",
            compact_schema
        );

        if let Some(last_user) = messages
            .iter_mut()
            .rev()
            .find(|message| matches!(message.role, MessageRole::User))
        {
            last_user.content.push_str(&schema_instructions);
        } else {
            messages.push(Message::user(schema_instructions));
        }

        let engine = Arc::clone(&self.engine);
        let conversation_lock = Arc::clone(&self.conversation_lock);
        let prompt = Self::build_prompt(&messages);
        let options = options.unwrap_or_default();
        let json_schema_str = serde_json::to_string(json_schema).map_err(|e| {
            LlmError::SerializationError(format!("Failed to serialize schema for constraint: {e}"))
        })?;

        let response_text = tokio::task::spawn_blocking(move || {
            let _guard = conversation_lock.lock().map_err(|_| {
                LlmError::LocalModelError("LiteRT conversation lock poisoned".to_string())
            })?;
            Self::run_structured_prompt(engine, prompt, json_schema_str, options)
        })
        .await
        .map_err(|e| {
            LlmError::LocalModelError(format!(
                "LiteRT structured generation task failed to join: {e}"
            ))
        })??;

        match Self::parse_structured_json(&response_text, json_schema) {
            Ok(value) => Ok(value),
            Err(first_err) => {
                if !Self::looks_like_schema_echo(&response_text) {
                    warn!(
                        "LiteRT structured parse failed without schema echo; attempting strict retry: {first_err}"
                    );
                }

                // Retry once with a stricter recovery instruction if the model echoes schema text.
                if let Some(last_user) = messages
                    .iter_mut()
                    .rev()
                    .find(|message| matches!(message.role, MessageRole::User))
                {
                    last_user.content.push_str(
                        "\n\nYour previous answer repeated schema/instructions. Retry now and output ONLY one JSON object with keys `nodes` and `edges`, matching the schema. No markdown and no explanation.",
                    );
                } else {
                    messages.push(Message::user(
                        "Output ONLY one JSON object with keys `nodes` and `edges`, matching the provided schema. No markdown and no explanation.",
                    ));
                }

                let retry = self
                    .generate(messages, Some(GenerationOptions::default()))
                    .await?;
                match Self::parse_structured_json(&retry.content, json_schema) {
                    Ok(value) => Ok(value),
                    Err(retry_err) => {
                        warn!(
                            "LiteRT structured parse failed after retry; using schema-shaped fallback object: {retry_err}"
                        );
                        Ok(Self::fallback_structured_object(json_schema))
                    }
                }
            }
        }
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn supports_streaming(&self) -> bool {
        false
    }

    fn supports_function_calling(&self) -> bool {
        false
    }

    fn max_context_length(&self) -> u32 {
        8192
    }
}

impl LiteRtAdapter {
    pub fn backend(&self) -> &str {
        &self.backend
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_embedded_json_object() {
        let raw = "text before {\"a\":1,\"b\":[2,3]} text after";
        let extracted = LiteRtAdapter::extract_first_json_object(raw);
        assert_eq!(extracted, Some("{\"a\":1,\"b\":[2,3]}"));
    }

    #[test]
    fn compacts_schema_to_single_line() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"}
            }
        });

        let compact = LiteRtAdapter::compact_schema(&schema).expect("compact schema");
        assert!(!compact.contains('\n'));
        assert!(compact.contains("\"properties\""));
    }
}
