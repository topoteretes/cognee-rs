//! Content-aware replay mock: serves recorded responses from an [`LlmCassette`].
//!
//! [`ReplayLlm`] is the Rust equivalent of Python's `_install_mocks()` LLM
//! substitution — it lets the whole pipeline run with no API calls. Where Python
//! matches by title substring and returns hand-authored graphs, this mock matches
//! by the T1 content hash ([`input_hash`]/[`vision_hash`]) and returns the
//! *recorded* response from a cassette. The same chunk in always yields the same
//! graph out, regardless of batching or call order.
//!
//! On a cache miss the [`MissPolicy`] decides what happens: the default
//! [`MissPolicy::EmptyGraph`] returns a schema-appropriate empty/stub value
//! (matching Python's `KnowledgeGraph(nodes=[], edges=[])` /
//! `SummarizedContent(summary="Mock summary.", description="")`), while
//! [`MissPolicy::Error`] surfaces the missing hash for debugging.

use std::path::Path;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::cassette::{LlmCassette, input_hash, vision_hash};
use crate::error::{LlmError, LlmResult};
use crate::llm_trait::Llm;
use crate::types::{GenerationOptions, GenerationResponse, Message};

/// What [`ReplayLlm`] does when an input hash is not present in the cassette.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MissPolicy {
    /// Return a schema-appropriate empty/stub response (Python parity). This is
    /// the default: an unrecorded chunk extracts to an empty graph rather than
    /// aborting the run.
    #[default]
    EmptyGraph,
    /// Return an [`LlmError`] describing the missing hash and input preview.
    Error,
}

/// A mock [`Llm`] that replays recorded responses from an [`LlmCassette`] by the
/// T1 content hash. See the module docs for the matching and miss-policy rules.
pub struct ReplayLlm {
    cassette: LlmCassette,
    miss: MissPolicy,
    model: String,
}

impl ReplayLlm {
    /// Load a cassette from `path` and build a replay mock over it.
    ///
    /// `model()` reports the cassette's recorded `model`, and the miss policy
    /// defaults to [`MissPolicy::EmptyGraph`] (Python parity).
    pub fn from_path(path: impl AsRef<Path>) -> LlmResult<Self> {
        let cassette = LlmCassette::load(path)?;
        let model = cassette.model.clone();
        Ok(Self {
            cassette,
            miss: MissPolicy::EmptyGraph,
            model,
        })
    }

    /// Build a replay mock directly from an in-memory [`LlmCassette`].
    ///
    /// Useful in tests that record into a cassette without going through disk.
    pub fn from_cassette(cassette: LlmCassette) -> Self {
        let model = cassette.model.clone();
        Self {
            cassette,
            miss: MissPolicy::EmptyGraph,
            model,
        }
    }

    /// Override the miss policy (builder style).
    pub fn with_miss_policy(mut self, p: MissPolicy) -> Self {
        self.miss = p;
        self
    }

    /// Apply the miss policy for a structured-output call, branching the empty
    /// value on the response `schema`.
    fn structured_miss(
        &self,
        schema: &Value,
        hash: &str,
        messages: &[Message],
    ) -> LlmResult<Value> {
        match self.miss {
            MissPolicy::EmptyGraph => Ok(empty_structured_response(schema)),
            MissPolicy::Error => Err(miss_error("structured output", hash, messages)),
        }
    }
}

/// Best-effort schema name from the schema's `title` field (the `JsonSchema`
/// derive emits the Rust type name there).
fn schema_title(schema: &Value) -> Option<&str> {
    schema.get("title").and_then(Value::as_str)
}

/// The empty/stub structured response for a cache miss, branched on the response
/// schema (the two callers — graph extraction and summarization — expect
/// incompatible shapes).
fn empty_structured_response(schema: &Value) -> Value {
    // Summarization's `SummarizedContent` has two required `String` fields, so an
    // empty graph object would fail to deserialize. Detect it by title and fall
    // back to a field probe; everything else defaults to the empty graph.
    let is_summary = match schema_title(schema) {
        Some(title) => title == "SummarizedContent",
        None => {
            schema_has_property(schema, "summary") && schema_has_property(schema, "description")
        }
    };
    if is_summary {
        summary_stub()
    } else {
        empty_graph()
    }
}

/// Python parity: `KnowledgeGraph(nodes=[], edges=[])`.
fn empty_graph() -> Value {
    json!({"nodes": [], "edges": []})
}

/// Python parity: `SummarizedContent(summary="Mock summary.", description="")`.
fn summary_stub() -> Value {
    json!({"summary": "Mock summary.", "description": ""})
}

/// Whether the schema declares property `name` under `properties` (fallback when
/// there is no `title` to match on).
fn schema_has_property(schema: &Value, name: &str) -> bool {
    schema
        .get("properties")
        .and_then(Value::as_object)
        .is_some_and(|props| props.contains_key(name))
}

/// Build a descriptive [`LlmError`] for a [`MissPolicy::Error`] miss.
fn miss_error(method: &str, hash: &str, messages: &[Message]) -> LlmError {
    LlmError::InvalidResponse(format!(
        "ReplayLlm: no recorded {method} response for hash {hash} (input: {})",
        input_preview(messages)
    ))
}

/// A short preview of the last user message for miss diagnostics.
fn input_preview(messages: &[Message]) -> String {
    let last_user = messages
        .iter()
        .rev()
        .find(|m| m.role == crate::types::MessageRole::User)
        .map(|m| m.content.as_str())
        .unwrap_or("");
    last_user.chars().take(120).collect()
}

#[async_trait]
impl Llm for ReplayLlm {
    async fn generate(
        &self,
        messages: Vec<Message>,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<GenerationResponse> {
        let hash = input_hash(&messages, None);
        match self.cassette.entries.get(&hash) {
            Some(entry) => {
                let content = match &entry.response {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                Ok(GenerationResponse {
                    content,
                    model: self.model.clone(),
                    usage: None,
                    finish_reason: Some("stop".to_string()),
                })
            }
            None => match self.miss {
                MissPolicy::EmptyGraph => Ok(GenerationResponse {
                    content: String::new(),
                    model: self.model.clone(),
                    usage: None,
                    finish_reason: Some("stop".to_string()),
                }),
                MissPolicy::Error => Err(miss_error("generate", &hash, &messages)),
            },
        }
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        messages: Vec<Message>,
        json_schema: &Value,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<Value> {
        let hash = input_hash(&messages, Some(json_schema));
        match self.cassette.entries.get(&hash) {
            Some(entry) => Ok(entry.response.clone()),
            None => self.structured_miss(json_schema, &hash, &messages),
        }
    }

    async fn transcribe_image(
        &self,
        image_bytes: &[u8],
        mime_type: &str,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<String> {
        let hash = vision_hash(image_bytes, mime_type);
        match self.cassette.entries.get(&hash) {
            Some(entry) => Ok(match &entry.response {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            }),
            None => match self.miss {
                MissPolicy::EmptyGraph => Ok(String::new()),
                MissPolicy::Error => Err(LlmError::InvalidResponse(format!(
                    "ReplayLlm: no recorded transcription for hash {hash} ([{mime_type}])"
                ))),
            },
        }
    }

    fn model(&self) -> &str {
        &self.model
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "test code — panics are acceptable"
    )]
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::super::cassette::{CassetteEntry, CassetteMethod};
    use super::super::recording::RecordingLlm;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    // A minimal in-test stub LLM. We cannot reuse `cognee_test_utils::MockLlm`
    // here: `cognee-test-utils` depends on `cognee-llm` *without* the `mock`
    // feature, so enabling `mock` on this crate's own test target builds two
    // distinct copies of `cognee-llm` (and thus two distinct `Llm` traits),
    // which fail to unify. A local stub sidesteps that dev-dependency cycle (see
    // the note in `recording.rs`).
    struct StubLlm {
        responses: Mutex<VecDeque<String>>,
    }

    impl StubLlm {
        fn new(responses: Vec<String>) -> Self {
            Self {
                responses: Mutex::new(VecDeque::from(responses)),
            }
        }

        fn pop(&self) -> String {
            // lock poison is unrecoverable
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| r#"{"nodes":[],"edges":[]}"#.to_string())
        }
    }

    #[async_trait]
    impl Llm for StubLlm {
        async fn generate(
            &self,
            _messages: Vec<Message>,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<GenerationResponse> {
            Ok(GenerationResponse {
                content: self.pop(),
                model: "stub-llm".to_string(),
                usage: None,
                finish_reason: Some("stop".to_string()),
            })
        }

        async fn create_structured_output_with_messages_raw(
            &self,
            _messages: Vec<Message>,
            _json_schema: &Value,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<Value> {
            let raw = self.pop();
            serde_json::from_str(&raw)
                .map_err(|e| LlmError::DeserializationError(format!("StubLlm: invalid JSON: {e}")))
        }

        fn model(&self) -> &str {
            "stub-llm"
        }
    }

    fn graph_msgs() -> Vec<Message> {
        vec![
            Message::system("Extract a knowledge graph."),
            Message::user("Alice met Bob."),
        ]
    }

    fn graph_schema() -> Value {
        json!({"title": "KnowledgeGraph", "type": "object"})
    }

    fn summary_schema() -> Value {
        json!({
            "title": "SummarizedContent",
            "type": "object",
            "properties": {
                "summary": {"type": "string"},
                "description": {"type": "string"}
            }
        })
    }

    fn cassette_with(entries: Vec<(String, CassetteEntry)>) -> LlmCassette {
        LlmCassette {
            version: 1,
            model: "test-model".to_string(),
            entries: entries.into_iter().collect::<BTreeMap<_, _>>(),
        }
    }

    #[tokio::test]
    async fn record_then_replay_round_trip() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let path = dir.path().join("cassette.json");

        let graph = json!({"nodes": [{"name": "Alice"}], "edges": []});
        let schema = graph_schema();

        // Record a known graph through RecordingLlm over a local stub.
        {
            let stub: Arc<dyn Llm> = Arc::new(StubLlm::new(vec![graph.to_string()]));
            let recorder = RecordingLlm::new(stub, &path);
            let value = recorder
                .create_structured_output_with_messages_raw(graph_msgs(), &schema, None)
                .await
                .expect("record structured output");
            assert_eq!(value, graph);
            recorder.flush().expect("flush");
        }

        // Replay the recorded value back.
        let replay = ReplayLlm::from_path(&path).expect("load replay");
        let replayed = replay
            .create_structured_output_with_messages_raw(graph_msgs(), &schema, None)
            .await
            .expect("replay structured output");
        assert_eq!(replayed, graph, "replayed value must equal recorded");
    }

    #[tokio::test]
    async fn hit_returns_recorded_value() {
        let graph = json!({"nodes": [{"name": "Bob"}], "edges": []});
        let hash = input_hash(&graph_msgs(), Some(&graph_schema()));
        let cassette = cassette_with(vec![(
            hash,
            CassetteEntry {
                method: CassetteMethod::StructuredOutput,
                user_input_preview: "Alice met Bob.".to_string(),
                schema_name: Some("KnowledgeGraph".to_string()),
                response: graph.clone(),
            },
        )]);
        let replay = ReplayLlm::from_cassette(cassette);
        let value = replay
            .create_structured_output_with_messages_raw(graph_msgs(), &graph_schema(), None)
            .await
            .expect("hit");
        assert_eq!(value, graph);
        assert_eq!(replay.model(), "test-model");
    }

    #[tokio::test]
    async fn miss_empty_graph_returns_empty_graph() {
        let replay = ReplayLlm::from_cassette(cassette_with(vec![]));
        let value = replay
            .create_structured_output_with_messages_raw(graph_msgs(), &graph_schema(), None)
            .await
            .expect("empty-graph miss");
        assert_eq!(value, json!({"nodes": [], "edges": []}));
    }

    #[tokio::test]
    async fn miss_empty_graph_returns_summary_stub_for_summary_schema() {
        let replay = ReplayLlm::from_cassette(cassette_with(vec![]));
        let value = replay
            .create_structured_output_with_messages_raw(graph_msgs(), &summary_schema(), None)
            .await
            .expect("summary stub miss");
        assert_eq!(
            value,
            json!({"summary": "Mock summary.", "description": ""})
        );
        // The stub must deserialize into SummarizedContent's required fields.
        assert!(value.get("summary").and_then(Value::as_str).is_some());
        assert!(value.get("description").and_then(Value::as_str).is_some());
    }

    #[tokio::test]
    async fn miss_empty_graph_probes_fields_without_title() {
        // No title: fall back to probing for summary/description properties.
        let schema = json!({
            "type": "object",
            "properties": {
                "summary": {"type": "string"},
                "description": {"type": "string"}
            }
        });
        let replay = ReplayLlm::from_cassette(cassette_with(vec![]));
        let value = replay
            .create_structured_output_with_messages_raw(graph_msgs(), &schema, None)
            .await
            .expect("probe miss");
        assert_eq!(
            value,
            json!({"summary": "Mock summary.", "description": ""})
        );
    }

    #[tokio::test]
    async fn miss_error_returns_err() {
        let replay =
            ReplayLlm::from_cassette(cassette_with(vec![])).with_miss_policy(MissPolicy::Error);
        let result = replay
            .create_structured_output_with_messages_raw(graph_msgs(), &graph_schema(), None)
            .await;
        assert!(matches!(result, Err(LlmError::InvalidResponse(_))));
    }

    #[tokio::test]
    async fn generate_hit_and_miss() {
        let msgs = graph_msgs();
        let hash = input_hash(&msgs, None);
        let cassette = cassette_with(vec![(
            hash,
            CassetteEntry {
                method: CassetteMethod::Generate,
                user_input_preview: "Alice met Bob.".to_string(),
                schema_name: None,
                response: Value::String("recorded text".to_string()),
            },
        )]);
        let replay = ReplayLlm::from_cassette(cassette);

        let hit = replay.generate(msgs, None).await.expect("generate hit");
        assert_eq!(hit.content, "recorded text");
        assert_eq!(hit.model, "test-model");
        assert_eq!(hit.finish_reason.as_deref(), Some("stop"));

        // Miss with default EmptyGraph policy → empty content.
        let other = vec![Message::user("never recorded")];
        let miss = replay
            .generate(other.clone(), None)
            .await
            .expect("generate miss");
        assert_eq!(miss.content, "");

        // Miss with Error policy → Err.
        let replay_err =
            ReplayLlm::from_cassette(cassette_with(vec![])).with_miss_policy(MissPolicy::Error);
        assert!(replay_err.generate(other, None).await.is_err());
    }

    #[tokio::test]
    async fn transcribe_image_hit_and_miss() {
        let bytes = b"\x89PNG\r\n";
        let mime = "image/png";
        let hash = vision_hash(bytes, mime);
        let cassette = cassette_with(vec![(
            hash,
            CassetteEntry {
                method: CassetteMethod::TranscribeImage,
                user_input_preview: "[image/png]".to_string(),
                schema_name: None,
                response: Value::String("a cat".to_string()),
            },
        )]);
        let replay = ReplayLlm::from_cassette(cassette);

        let hit = replay
            .transcribe_image(bytes, mime, None)
            .await
            .expect("transcribe hit");
        assert_eq!(hit, "a cat");

        // Miss with EmptyGraph → empty string.
        let miss = replay
            .transcribe_image(b"other", mime, None)
            .await
            .expect("transcribe miss");
        assert_eq!(miss, "");

        // Miss with Error policy → Err.
        let replay_err =
            ReplayLlm::from_cassette(cassette_with(vec![])).with_miss_policy(MissPolicy::Error);
        assert!(replay_err.transcribe_image(b"x", mime, None).await.is_err());
    }

    #[test]
    fn empty_response_helpers_are_schema_aware() {
        assert_eq!(empty_graph(), json!({"nodes": [], "edges": []}));
        assert_eq!(
            summary_stub(),
            json!({"summary": "Mock summary.", "description": ""})
        );
        assert_eq!(empty_structured_response(&graph_schema()), empty_graph());
        assert_eq!(empty_structured_response(&summary_schema()), summary_stub());
    }

    #[test]
    fn preview_picks_last_user_message() {
        let msgs = vec![Message::system("sys"), Message::user("hello")];
        assert_eq!(input_preview(&msgs), "hello");
    }
}
