//! Request / response transforms for `POST {endpoint}/model/{modelId}/converse`
//! (plan §1.4.2 / §1.4.3).
//!
//! Everything in here is a **pure** function over `serde_json::Value`, so the
//! wire shape is testable without a server (`crates/llm/tests/bedrock_*.rs`).
//! The adapter in [`super`] owns the HTTP, retry and repair loops.
//!
//! Body shape (`converse_transformation.py::_transform_request_helper`):
//!
//! ```json
//! { "messages": [...], "system": [...], "inferenceConfig": {...},
//!   "additionalModelRequestFields": {...}, "toolConfig": {...},
//!   "outputConfig": {...} }
//! ```

use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::caps::ModelCaps;
use crate::error::LlmError;
use crate::types::{GenerationOptions, Message, MessageRole, TokenUsage};

/// litellm's `RESPONSE_FORMAT_TOOL_NAME` (`constants.py`) — the name of the
/// synthetic tool used by the §1.4.3 fallback branch. The exact spelling is
/// wire-visible, so it must not drift.
pub const RESPONSE_FORMAT_TOOL_NAME: &str = "json_tool_call";

/// Prompt used by the vision path, matching the other adapters.
pub const IMAGE_DESCRIPTION_PROMPT: &str = "What's in this image?";

// ---------------------------------------------------------------------------
// URL
// ---------------------------------------------------------------------------

/// Percent-encode a model id for use as a single URL path segment.
///
/// Port of `base_aws_llm.py::encode_model_id`, which is
/// `urllib.parse.quote(model_id, safe="")` — i.e. **everything** outside
/// Python's unreserved set (`A-Za-z0-9_.-~`) is escaped, `/` and `:` included.
/// This is what makes an inference-profile ARN usable as one path segment, and
/// what turns `…-v1:0` into `…-v1%3A0`.
pub fn encode_model_id(model_id: &str) -> String {
    let mut out = String::with_capacity(model_id.len());
    for byte in model_id.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => {
                out.push(*byte as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Build the Converse URL for `model`.
///
/// **`model` is the ORIGINAL, un-normalised id** — the cross-region prefix, ARN
/// wrapper and any suffix all stay (plan §1.4.2). Normalisation feeds routing
/// and the capability lookup only, never this path.
///
/// The one exception is litellm's own routing tokens — `bedrock/`, `converse/`,
/// `invoke/` — which are configuration syntax rather than part of the Bedrock
/// identifier. litellm strips them before building the URL and so does
/// [`super::model_id::wire_model_id`]; leaving them on produces a 400 against
/// `/model/bedrock%2F…/converse`.
pub fn converse_url(endpoint: &str, model: &str) -> String {
    format!(
        "{}/model/{}/converse",
        endpoint.trim_end_matches('/'),
        encode_model_id(super::model_id::wire_model_id(model))
    )
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/// Split messages into Converse's top-level `system` blocks and its
/// `user`/`assistant` turns.
///
/// The Converse API has **no system role inside `messages`**, so every system
/// message is hoisted into the top-level `system` array as its own
/// `{"text": …}` block. Non-system content becomes
/// `{"role": …, "content": [{"text": …}]}`.
pub fn split_messages(messages: &[Message]) -> (Vec<Value>, Vec<Value>) {
    let mut system_blocks: Vec<Value> = Vec::new();
    let mut turns: Vec<Value> = Vec::new();
    for message in messages {
        let role = match message.role {
            MessageRole::System => {
                system_blocks.push(json!({ "text": message.content }));
                continue;
            }
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
        };
        turns.push(json!({
            "role": role,
            "content": [{ "text": message.content }],
        }));
    }
    (system_blocks, turns)
}

/// The corrective re-ask text for the branch that produced the bad answer.
///
/// The failure preamble is shared with the other adapters
/// ([`crate::schema::corrective_reason_detail`]); only the directive differs.
/// The native branch must **not** be told to call a tool — there is none when
/// the schema travels in `outputConfig`, and both Anthropic models cognee ships
/// take that branch.
pub fn corrective_instruction(reason: Option<&str>, native: bool) -> String {
    let detail = crate::schema::corrective_reason_detail(reason);
    if native {
        format!(
            "{detail}Reply with ONE complete JSON object that fills in every required field, \
             strictly matching the schema. No extra text."
        )
    } else {
        crate::schema::corrective_instruction(reason, RESPONSE_FORMAT_TOOL_NAME, "tool")
    }
}

/// Append a corrective user turn to a Converse body.
///
/// The Converse analogue of [`crate::schema::append_corrective_instruction`]:
/// the failure wording is shared but the append semantics are not, because
/// Converse content is a **block array** rather than a string — the shared
/// helper's string-content append would corrupt the body. Extends the last user
/// turn with an extra `text` block when there is one, otherwise pushes a new
/// user turn so the correction is never silently dropped.
///
/// `native` selects the directive; pass
/// [`ModelCaps::supports_native_structured_output`].
pub fn append_corrective_instruction(body: &mut Value, reason: Option<&str>, native: bool) {
    let instruction = corrective_instruction(reason, native);
    let Some(messages) = body["messages"].as_array_mut() else {
        return;
    };
    match messages.last_mut() {
        Some(last) if last["role"] == "user" => {
            if let Some(content) = last["content"].as_array_mut() {
                content.push(json!({ "text": instruction }));
            } else {
                last["content"] = json!([{ "text": instruction }]);
            }
        }
        _ => messages.push(json!({
            "role": "user",
            "content": [{ "text": instruction }],
        })),
    }
}

// ---------------------------------------------------------------------------
// Inference config / additional model request fields
// ---------------------------------------------------------------------------

/// Build `inferenceConfig` from `opts` with an already-clamped `max_tokens`.
///
/// `maxTokens` is `min(llm_max_completion_tokens, model cap)` (§1.0) and is
/// computed by the adapter; `temperature`, `topP` and `stopSequences` come
/// straight from [`GenerationOptions`]. Keys the caller did not set are
/// omitted rather than sent as `null`, which Converse rejects.
///
/// `frequency_penalty` / `presence_penalty` have **no `inferenceConfig`
/// analogue** — see [`penalty_model_fields`].
pub fn inference_config(opts: &GenerationOptions, max_tokens: u32) -> Value {
    let mut config = json!({ "maxTokens": max_tokens });
    if let Some(temperature) = opts.temperature {
        config["temperature"] = json!(temperature);
    }
    if let Some(top_p) = opts.top_p {
        config["topP"] = json!(top_p);
    }
    if let Some(stop) = opts.stop.as_ref().filter(|stop| !stop.is_empty()) {
        config["stopSequences"] = json!(stop);
    }
    config
}

/// The frequency/presence penalties, shaped for `additionalModelRequestFields`.
///
/// **Decision (plan §4 R3 step 6 leaves the choice open):** they are *routed
/// through* `additionalModelRequestFields` rather than dropped. Converse's
/// `inferenceConfig` has no analogue, and litellm's own transform sweeps every
/// inference param it does not recognise into `additionalModelRequestFields`
/// (`converse_transformation.py`: `additional_request_params = {k: v for k, v in
/// inference_params.items() if k not in total_supported_params}`), so this is
/// the parity-faithful destination. Both fields are `None` on every cognee call
/// path today (`GenerationOptions::default()` leaves them unset and no caller in
/// the workspace sets them), so a model that rejects the extra fields is only
/// reachable by a caller that opted in.
pub fn penalty_model_fields(opts: &GenerationOptions) -> Map<String, Value> {
    let mut fields = Map::new();
    if let Some(frequency_penalty) = opts.frequency_penalty {
        fields.insert("frequency_penalty".to_string(), json!(frequency_penalty));
    }
    if let Some(presence_penalty) = opts.presence_penalty {
        fields.insert("presence_penalty".to_string(), json!(presence_penalty));
    }
    fields
}

/// Merge `LLM_ARGS` and the adapter's explicit fields into
/// `additionalModelRequestFields`, litellm-style `{**llm_args, **explicit}`.
///
/// `llm_args` fills gaps; every key in `explicit` wins. The key is omitted
/// entirely when both are empty, because Converse rejects an empty object on
/// some models.
pub fn merge_additional_model_request_fields(
    body: &mut Value,
    llm_args: &Map<String, Value>,
    explicit: &Map<String, Value>,
) {
    if llm_args.is_empty() && explicit.is_empty() {
        return;
    }
    let mut merged = llm_args.clone();
    for (key, value) in explicit {
        merged.insert(key.clone(), value.clone());
    }
    body["additionalModelRequestFields"] = Value::Object(merged);
}

// ---------------------------------------------------------------------------
// Structured output — the two §1.4.3 branches
// ---------------------------------------------------------------------------

/// Recursively force `"additionalProperties": false` onto **every** object node
/// of a JSON schema.
///
/// Bedrock's native structured-output API returns a validation error unless the
/// field is explicitly set on each object node
/// (`_add_additional_properties_to_schema`). Recurses through `properties`,
/// `items`, `$defs` / `definitions` and `anyOf` / `allOf` / `oneOf`.
pub fn force_additional_properties_false(schema: &Value) -> Value {
    let Some(object) = schema.as_object() else {
        return schema.clone();
    };
    let mut out = object.clone();

    if out.get("type").and_then(Value::as_str) == Some("object")
        && !out.contains_key("additionalProperties")
    {
        out.insert("additionalProperties".to_string(), json!(false));
    }

    let recurse_map = |map: &Value| -> Value {
        match map.as_object() {
            Some(entries) => Value::Object(
                entries
                    .iter()
                    .map(|(key, value)| (key.clone(), force_additional_properties_false(value)))
                    .collect(),
            ),
            None => map.clone(),
        }
    };

    if let Some(properties) = out.get("properties") {
        let rewritten = recurse_map(properties);
        out.insert("properties".to_string(), rewritten);
    }
    if let Some(items) = out.get("items").filter(|items| items.is_object()) {
        let rewritten = force_additional_properties_false(items);
        out.insert("items".to_string(), rewritten);
    }
    for defs_key in ["$defs", "definitions"] {
        if let Some(defs) = out.get(defs_key) {
            let rewritten = recurse_map(defs);
            out.insert(defs_key.to_string(), rewritten);
        }
    }
    for combinator in ["anyOf", "allOf", "oneOf"] {
        if let Some(Value::Array(branches)) = out.get(combinator) {
            let rewritten: Vec<Value> = branches
                .iter()
                .map(force_additional_properties_false)
                .collect();
            out.insert(combinator.to_string(), Value::Array(rewritten));
        }
    }

    Value::Object(out)
}

/// Strip the `$schema` meta key, which Bedrock does not expect on a tool input
/// schema or inside `outputConfig`.
fn sanitize_schema(schema: &Value) -> Value {
    let mut out = schema.clone();
    if let Some(object) = out.as_object_mut() {
        object.remove("$schema");
    }
    out
}

/// Build the **native** branch: `outputConfig.textFormat` carrying the schema.
///
/// Port of `_create_output_config_for_response_format`. The schema is embedded
/// as a JSON **string**, and `additionalProperties: false` is forced onto every
/// object node first.
pub fn output_config_block(json_schema: &Value) -> Value {
    let schema = force_additional_properties_false(&sanitize_schema(json_schema));
    let schema_string = serde_json::to_string(&schema).unwrap_or_else(|_| "{}".to_string());
    json!({
        "textFormat": {
            "type": "json_schema",
            "structure": {
                "jsonSchema": { "schema": schema_string }
            }
        }
    })
}

/// Build the **fallback** branch: a `toolConfig` holding the synthetic
/// [`RESPONSE_FORMAT_TOOL_NAME`] tool whose input schema *is* the response
/// schema.
///
/// `toolChoice` is forced **only when** `force_tool_choice` — plan §1.4.3:
/// litellm applies it only if the model advertises `supports_tool_choice`, and
/// `amazon.nova-lite-v1:0` (one of cognee's three shipped models) does not.
pub fn json_tool_config(json_schema: &Value, force_tool_choice: bool) -> Value {
    let mut config = json!({
        "tools": [{
            "toolSpec": {
                "name": RESPONSE_FORMAT_TOOL_NAME,
                "description": "Return the extracted data in the required schema.",
                "inputSchema": { "json": sanitize_schema(json_schema) },
            }
        }]
    });
    if force_tool_choice {
        config["toolChoice"] = json!({ "tool": { "name": RESPONSE_FORMAT_TOOL_NAME } });
    }
    config
}

/// Whether the native `outputConfig` branch will be used for `caps`.
///
/// One place decides, so the request builder and the response unwrapper can
/// never disagree about which shape they are dealing with.
pub fn uses_native_structured_output(caps: &ModelCaps) -> bool {
    caps.supports_native_structured_output
}

/// Apply §1.4.3's **capability-gated** structured-output shape to `body`.
///
/// Native models get `outputConfig`; everything else gets the `json_tool_call`
/// tool, with `toolChoice` forced only when the model advertises
/// `supports_tool_choice`. The branch is read from `caps` — never hard-coded.
pub fn apply_structured_output(body: &mut Value, json_schema: &Value, caps: &ModelCaps) {
    if uses_native_structured_output(caps) {
        body["outputConfig"] = output_config_block(json_schema);
    } else {
        body["toolConfig"] = json_tool_config(json_schema, caps.supports_tool_choice);
    }
}

// ---------------------------------------------------------------------------
// Vision
// ---------------------------------------------------------------------------

/// Map an `image/*` MIME type onto a Converse image `format` token.
///
/// Converse accepts exactly `png`, `jpeg`, `gif` and `webp`.
pub fn image_format_for_mime(mime_type: &str) -> Option<&'static str> {
    match mime_type.trim().to_ascii_lowercase().as_str() {
        "image/png" => Some("png"),
        "image/jpeg" | "image/jpg" => Some("jpeg"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        _ => None,
    }
}

/// A single user turn holding the description prompt and a base64 image block.
///
/// Converse carries images as `{"image": {"format": …, "source": {"bytes": …}}}`
/// — over the REST-JSON wire the blob is base64. Plan §6.5: this **exceeds**
/// Python parity, whose `BedrockAdapter.transcribe_image` raises
/// `NotImplementedError`.
pub fn image_message(format: &str, base64_image: &str) -> Value {
    json!({
        "role": "user",
        "content": [
            { "text": IMAGE_DESCRIPTION_PROMPT },
            { "image": { "format": format, "source": { "bytes": base64_image } } },
        ],
    })
}

// ---------------------------------------------------------------------------
// Response
// ---------------------------------------------------------------------------

/// A parsed Converse response.
///
/// `content` blocks stay raw `Value`s so `text`, `toolUse` and
/// `reasoningContent` are all handled without an exhaustive enum, the same way
/// the Anthropic adapter treats its content blocks.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConverseResponse {
    /// `output.message.content[]`.
    #[serde(default)]
    pub output: ConverseOutput,
    /// `end_turn` | `tool_use` | `max_tokens` | `stop_sequence` | …
    #[serde(default)]
    pub stop_reason: Option<String>,
    /// Token counts.
    #[serde(default)]
    pub usage: Option<ConverseUsage>,
}

/// The `output` envelope.
#[derive(Debug, Default, Deserialize)]
pub struct ConverseOutput {
    /// The assistant message.
    #[serde(default)]
    pub message: ConverseMessage,
}

/// The assistant message inside `output`.
#[derive(Debug, Default, Deserialize)]
pub struct ConverseMessage {
    /// Raw content blocks.
    #[serde(default)]
    pub content: Vec<Value>,
}

/// Converse `usage`.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConverseUsage {
    /// Prompt tokens.
    #[serde(default)]
    pub input_tokens: u32,
    /// Completion tokens.
    #[serde(default)]
    pub output_tokens: u32,
    /// Total, when Bedrock reports one.
    #[serde(default)]
    pub total_tokens: u32,
}

impl From<ConverseUsage> for TokenUsage {
    fn from(usage: ConverseUsage) -> Self {
        let total = if usage.total_tokens > 0 {
            usage.total_tokens
        } else {
            usage.input_tokens.saturating_add(usage.output_tokens)
        };
        TokenUsage {
            prompt_tokens: usage.input_tokens,
            completion_tokens: usage.output_tokens,
            total_tokens: total,
        }
    }
}

impl ConverseResponse {
    /// Concatenate every `text` content block.
    pub fn text(&self) -> String {
        self.output
            .message
            .content
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("")
    }

    /// The `input` of the first `toolUse` block named `tool_name`.
    pub fn tool_input(&self, tool_name: &str) -> Option<Value> {
        self.output.message.content.iter().find_map(|block| {
            let tool_use = block.get("toolUse")?;
            (tool_use.get("name").and_then(Value::as_str) == Some(tool_name))
                .then(|| tool_use.get("input").cloned())
                .flatten()
        })
    }

    /// Whether generation stopped because the output budget ran out — i.e. the
    /// object present in the response is incomplete.
    pub fn is_truncated(&self) -> bool {
        self.stop_reason.as_deref() == Some("max_tokens")
    }

    /// The structured payload for the branch `caps` selected: the parsed
    /// `toolUse.input` for the fallback branch, or the text content parsed as
    /// JSON for the native branch.
    ///
    /// `None` means "nothing usable came back", which the repair loop turns
    /// into a corrective re-ask.
    pub fn structured_payload(&self, caps: &ModelCaps) -> Option<Value> {
        if uses_native_structured_output(caps) {
            let text = self.text();
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return None;
            }
            serde_json::from_str::<Value>(trimmed).ok()
        } else {
            self.tool_input(RESPONSE_FORMAT_TOOL_NAME)
        }
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Bedrock exception names that mean "retry later" (§4 R3 step 7).
const THROTTLING_EXCEPTIONS: [&str; 2] = ["ThrottlingException", "ModelNotReadyException"];

/// Bedrock exception names that mean "the caller is not allowed to do this".
const AUTH_EXCEPTIONS: [&str; 4] = [
    "AccessDeniedException",
    "UnrecognizedClientException",
    "ExpiredTokenException",
    "IncompleteSignatureException",
];

/// Map a Bedrock HTTP status + error body onto the [`LlmError`] taxonomy.
///
/// The one non-obvious rule, and the reason this is not the 429-only mapping
/// the other adapters use: **Bedrock signals throttling as HTTP 400
/// `ThrottlingException` as well as 429** (plan §4 R3 step 7). Mapping the 400
/// to a generic bad-request error strands the retry layer and turns a
/// recoverable throttle into a hard pipeline failure.
///
/// The exception name is read from the body — `__type` / `code` when the
/// AWS-JSON envelope is present, otherwise a substring scan of the raw body.
/// The authoritative `x-amzn-errortype` **header** is not available here: the
/// transport seam hands back status + body only, by design (mapping errors is
/// the adapter's job, not the transport's). Every Bedrock runtime error body
/// observed in practice names its own exception, and the status-code fallback
/// below covers the rest.
pub fn map_error(status: u16, body: &str) -> LlmError {
    let named = |candidates: &[&str]| candidates.iter().any(|name| mentions(body, name));

    if named(&THROTTLING_EXCEPTIONS) || status == 429 {
        return LlmError::RateLimitExceeded(format!("HTTP {status}: {body}"));
    }
    if named(&AUTH_EXCEPTIONS) || matches!(status, 401 | 403) {
        return LlmError::AuthenticationError(format!("HTTP {status}: {body}"));
    }
    if mentions(body, "ResourceNotFoundException") || status == 404 {
        return LlmError::ModelNotFound(format!("HTTP {status}: {body}"));
    }
    if mentions(body, "ModelTimeoutException") || status == 408 {
        return LlmError::Timeout(format!("HTTP {status}: {body}"));
    }
    if mentions(body, "ServiceUnavailableException")
        || mentions(body, "InternalServerException")
        || status >= 500
    {
        return LlmError::ApiError(format!("HTTP {status}: {body}"));
    }
    if mentions(body, "ValidationException") || status == 400 {
        return LlmError::InvalidResponse(format!("Bad request (HTTP {status}): {body}"));
    }
    LlmError::ApiError(format!("HTTP {status}: {body}"))
}

/// Whether the error body names `exception`.
///
/// Checks the AWS-JSON `__type` / `code` fields first (where the value is
/// often namespaced, e.g. `com.amazon.coral.service#ThrottlingException`) and
/// falls back to a raw substring scan.
fn mentions(body: &str, exception: &str) -> bool {
    if let Ok(Value::Object(parsed)) = serde_json::from_str::<Value>(body) {
        for key in ["__type", "code", "Code", "errorType", "type"] {
            if let Some(value) = parsed.get(key).and_then(Value::as_str)
                && value.contains(exception)
            {
                return true;
            }
        }
    }
    body.contains(exception)
}

/// Whether `error` is worth another transport attempt.
///
/// Terminal: auth, unknown model, and a `ValidationException` (re-POSTing an
/// identical rejected body cannot start working). The structured-output repair
/// loop still re-asks on `InvalidResponse` — with *changed* content — which is
/// the "model rejected the schema" repair case.
pub fn is_retryable(error: &LlmError) -> bool {
    matches!(
        error,
        LlmError::RateLimitExceeded(_)
            | LlmError::ApiError(_)
            | LlmError::NetworkError(_)
            | LlmError::Timeout(_)
    )
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    #[test]
    fn model_id_is_percent_encoded_as_one_path_segment() {
        assert_eq!(
            encode_model_id("eu.anthropic.claude-sonnet-4-5-20250929-v1:0"),
            "eu.anthropic.claude-sonnet-4-5-20250929-v1%3A0"
        );
        assert_eq!(
            encode_model_id("arn:aws:bedrock:eu-west-1:1:application-inference-profile/abc"),
            "arn%3Aaws%3Abedrock%3Aeu-west-1%3A1%3Aapplication-inference-profile%2Fabc"
        );
    }

    #[test]
    fn usage_total_falls_back_to_the_sum() {
        let usage = TokenUsage::from(ConverseUsage {
            input_tokens: 4,
            output_tokens: 6,
            total_tokens: 0,
        });
        assert_eq!(usage.total_tokens, 10);
    }

    #[test]
    fn corrective_instruction_extends_the_last_user_turn_as_a_block() {
        let mut body = json!({
            "messages": [{ "role": "user", "content": [{ "text": "hi" }] }]
        });
        append_corrective_instruction(&mut body, Some("missing required field `x`"), false);
        let content = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2, "the correction is a second text block");
        assert!(
            content[1]["text"]
                .as_str()
                .unwrap()
                .contains("missing required field `x`")
        );
    }

    #[test]
    fn corrective_instruction_pushes_a_user_turn_after_an_assistant_turn() {
        let mut body = json!({
            "messages": [{ "role": "assistant", "content": [{ "text": "prior" }] }]
        });
        append_corrective_instruction(&mut body, None, false);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1]["role"], "user");
    }
}
