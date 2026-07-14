//! Recording decorator that wraps a real [`Llm`] and captures every response into
//! an [`LlmCassette`].
//!
//! Wrap any `Llm` with [`RecordingLlm`], run the pipeline once, and the decorator
//! passes each call through to the inner LLM unchanged while recording the parsed
//! response keyed by its T1 content hash. The resulting cassette can be replayed
//! (T3) to reproduce the run bit-for-bit, with no API access.
//!
//! Recording happens at the trait's single chokepoint
//! ([`Llm::create_structured_output_with_messages_raw`]) plus [`Llm::generate`] and
//! [`Llm::transcribe_image`], so the recorded `Value` is exactly what the pipeline
//! consumed. The default [`Llm::create_structured_output_raw`] is intentionally not
//! overridden — it funnels into `*_with_messages_raw`, which is already
//! intercepted, so overriding it would record twice.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::Value;

use super::cassette::{CassetteEntry, CassetteMethod, LlmCassette, input_hash, vision_hash};
use crate::error::LlmResult;
use crate::llm_trait::{Llm, StructuredOutputValidator};
use crate::types::{GenerationOptions, GenerationResponse, Message, MessageRole};

/// Maximum length (in characters) of the recorded user-input preview.
const PREVIEW_MAX_CHARS: usize = 120;

/// A decorator that wraps an inner [`Llm`], delegates every call unchanged, and
/// records the response into a cassette keyed by the T1 content hash.
///
/// Recorded entries accumulate in memory under a [`Mutex`] and are written to disk
/// by [`flush`](RecordingLlm::flush) (also called best-effort on [`Drop`]), so a
/// long run is not slowed by per-call IO and a crash mid-run still persists what
/// was recorded so far. Identical inputs collapse to one entry (idempotent by
/// hash), which makes concurrent recording from cognify's parallel extraction safe.
pub struct RecordingLlm {
    inner: Arc<dyn Llm>,
    entries: Mutex<BTreeMap<String, CassetteEntry>>,
    path: PathBuf,
}

impl RecordingLlm {
    /// Wrap `inner` and record responses to `path`.
    ///
    /// If `path` already exists and parses as a cassette, its entries seed the
    /// in-memory map so re-recording merges into (rather than clobbers) the
    /// existing file. A missing or unparseable file is ignored — recording starts
    /// from an empty map.
    pub fn new(inner: Arc<dyn Llm>, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let entries = match LlmCassette::load(&path) {
            Ok(cassette) => cassette.entries,
            Err(_) => BTreeMap::new(),
        };
        Self {
            inner,
            entries: Mutex::new(entries),
            path,
        }
    }

    /// Insert a recorded entry, keyed by `hash`. Idempotent: re-recording the same
    /// hash overwrites with the (identical) latest response.
    #[allow(clippy::unwrap_used, reason = "lock poison is unrecoverable")]
    fn record(&self, hash: String, entry: CassetteEntry) {
        // lock poison is unrecoverable
        self.entries.lock().unwrap().insert(hash, entry);
    }

    /// Snapshot the recorded entries and write them to the configured path as a
    /// cassette (`model = inner.model()`, `version = 1`).
    #[allow(clippy::unwrap_used, reason = "lock poison is unrecoverable")]
    pub fn flush(&self) -> LlmResult<()> {
        // lock poison is unrecoverable
        let entries = self.entries.lock().unwrap().clone();
        let cassette = LlmCassette {
            version: 1,
            model: self.inner.model().to_string(),
            entries,
        };
        cassette.save(&self.path)
    }
}

/// Best-effort preview of the last user message, truncated to [`PREVIEW_MAX_CHARS`]
/// characters (on a char boundary).
fn user_input_preview(messages: &[Message]) -> String {
    let last_user = messages
        .iter()
        .rev()
        .find(|m| m.role == MessageRole::User)
        .map(|m| m.content.as_str())
        .unwrap_or("");
    last_user.chars().take(PREVIEW_MAX_CHARS).collect()
}

/// Best-effort schema name from the schema's `title` field, if present.
fn schema_name(schema: &Value) -> Option<String> {
    schema
        .get("title")
        .and_then(Value::as_str)
        .map(str::to_string)
}

#[async_trait]
impl Llm for RecordingLlm {
    async fn generate(
        &self,
        messages: Vec<Message>,
        options: Option<GenerationOptions>,
    ) -> LlmResult<GenerationResponse> {
        let hash = input_hash(&messages, None);
        let preview = user_input_preview(&messages);
        let response = self.inner.generate(messages, options).await?;
        self.record(
            hash,
            CassetteEntry {
                method: CassetteMethod::Generate,
                user_input_preview: preview,
                schema_name: None,
                response: Value::String(response.content.clone()),
            },
        );
        Ok(response)
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        messages: Vec<Message>,
        json_schema: &Value,
        options: Option<GenerationOptions>,
    ) -> LlmResult<Value> {
        let hash = input_hash(&messages, Some(json_schema));
        let preview = user_input_preview(&messages);
        let schema_name = schema_name(json_schema);
        let response = self
            .inner
            .create_structured_output_with_messages_raw(messages, json_schema, options)
            .await?;
        self.record(
            hash,
            CassetteEntry {
                method: CassetteMethod::StructuredOutput,
                user_input_preview: preview,
                schema_name,
                response: response.clone(),
            },
        );
        Ok(response)
    }

    /// Must be overridden (not left to the trait default) so the caller's typed
    /// `validator` reaches the wrapped real adapter's repair loop. The default
    /// impl drops the validator and delegates to
    /// `create_structured_output_with_messages_raw`, which would bypass
    /// validation-retry (a well-formed response omitting a required field would
    /// fail the pipeline instead of being re-asked). We forward the validator to
    /// the inner adapter and record the (repaired) response.
    async fn create_structured_output_with_messages_raw_validated(
        &self,
        messages: Vec<Message>,
        json_schema: &Value,
        options: Option<GenerationOptions>,
        validator: StructuredOutputValidator<'_>,
    ) -> LlmResult<Value> {
        let hash = input_hash(&messages, Some(json_schema));
        let preview = user_input_preview(&messages);
        let schema_name = schema_name(json_schema);
        let response = self
            .inner
            .create_structured_output_with_messages_raw_validated(
                messages,
                json_schema,
                options,
                validator,
            )
            .await?;
        self.record(
            hash,
            CassetteEntry {
                method: CassetteMethod::StructuredOutput,
                user_input_preview: preview,
                schema_name,
                response: response.clone(),
            },
        );
        Ok(response)
    }

    async fn transcribe_image(
        &self,
        image_bytes: &[u8],
        mime_type: &str,
        options: Option<GenerationOptions>,
    ) -> LlmResult<String> {
        let hash = vision_hash(image_bytes, mime_type);
        let response = self
            .inner
            .transcribe_image(image_bytes, mime_type, options)
            .await?;
        self.record(
            hash,
            CassetteEntry {
                method: CassetteMethod::TranscribeImage,
                user_input_preview: format!("[{mime_type}]"),
                schema_name: None,
                response: Value::String(response.clone()),
            },
        );
        Ok(response)
    }

    fn model(&self) -> &str {
        self.inner.model()
    }

    fn supports_streaming(&self) -> bool {
        self.inner.supports_streaming()
    }

    fn supports_function_calling(&self) -> bool {
        self.inner.supports_function_calling()
    }

    fn max_context_length(&self) -> u32 {
        self.inner.max_context_length()
    }

    fn supports_vision(&self) -> bool {
        self.inner.supports_vision()
    }
}

impl Drop for RecordingLlm {
    fn drop(&mut self) {
        // Best-effort: persist whatever was recorded even on an abnormal exit.
        // Never panic in `drop`; log and move on.
        if let Err(e) = self.flush() {
            tracing::warn!(
                error = %e,
                path = %self.path.display(),
                "RecordingLlm: failed to flush cassette on drop"
            );
        }
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
    use serde_json::json;
    use std::collections::VecDeque;

    use crate::error::LlmError;

    // A minimal in-test stub LLM. We cannot reuse `cognee_test_utils::MockLlm`
    // here: `cognee-test-utils` depends on `cognee-llm` *without* the `mock`
    // feature, so enabling `mock` on this crate's own test target builds two
    // distinct copies of `cognee-llm` (and thus two distinct `Llm` traits),
    // which fail to unify. A local stub sidesteps that dev-dependency cycle.
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
                .unwrap_or_else(|| r#"{"nodes":[],"relationships":[]}"#.to_string())
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

    #[tokio::test]
    async fn records_structured_output_entry() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let path = dir.path().join("cassette.json");

        let graph = json!({"nodes": [{"name": "Alice"}], "relationships": []});
        let mock: Arc<dyn Llm> = Arc::new(StubLlm::new(vec![graph.to_string()]));
        let recorder = RecordingLlm::new(mock, &path);

        let schema = json!({"title": "KnowledgeGraph", "type": "object"});
        let value = recorder
            .create_structured_output_with_messages_raw(graph_msgs(), &schema, None)
            .await
            .expect("structured output");
        assert_eq!(value, graph);

        recorder.flush().expect("flush");

        let cassette = LlmCassette::load(&path).expect("load cassette");
        assert_eq!(cassette.entries.len(), 1);
        let key = input_hash(&graph_msgs(), Some(&schema));
        let entry = cassette.entries.get(&key).expect("entry present");
        assert_eq!(entry.method, CassetteMethod::StructuredOutput);
        assert_eq!(entry.schema_name.as_deref(), Some("KnowledgeGraph"));
        assert_eq!(entry.response, graph);
        assert_eq!(entry.user_input_preview, "Alice met Bob.");
    }

    #[tokio::test]
    async fn identical_inputs_dedup_to_one_entry() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let path = dir.path().join("cassette.json");

        let graph = json!({"nodes": [], "relationships": []});
        // Two identical responses queued so both calls succeed.
        let mock: Arc<dyn Llm> = Arc::new(StubLlm::new(vec![graph.to_string(), graph.to_string()]));
        let recorder = RecordingLlm::new(mock, &path);

        let schema = json!({"title": "KnowledgeGraph", "type": "object"});
        recorder
            .create_structured_output_with_messages_raw(graph_msgs(), &schema, None)
            .await
            .expect("first call");
        recorder
            .create_structured_output_with_messages_raw(graph_msgs(), &schema, None)
            .await
            .expect("second call");

        recorder.flush().expect("flush");

        let cassette = LlmCassette::load(&path).expect("load cassette");
        assert_eq!(cassette.entries.len(), 1, "identical inputs must dedup");
    }

    #[tokio::test]
    async fn drop_flushes_without_explicit_flush() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let path = dir.path().join("cassette.json");

        {
            let mock: Arc<dyn Llm> = Arc::new(StubLlm::new(vec!["\"hello\"".to_string()]));
            let recorder = RecordingLlm::new(mock, &path);
            recorder
                .generate(graph_msgs(), None)
                .await
                .expect("generate");
            // No explicit flush; rely on Drop.
        }

        assert!(path.exists(), "cassette should exist after drop");
        let cassette = LlmCassette::load(&path).expect("cassette parses after drop");
        assert_eq!(cassette.entries.len(), 1);
        let entry = cassette
            .entries
            .values()
            .next()
            .expect("one recorded entry");
        assert_eq!(entry.method, CassetteMethod::Generate);
        assert_eq!(entry.response, Value::String("\"hello\"".to_string()));
    }

    // Distinguishes which inner trait method a wrapper delegates to: `_raw`
    // returns `{"marker":"raw"}`, `_validated` returns `{"marker":"validated"}`.
    struct MarkerLlm;

    #[async_trait]
    impl Llm for MarkerLlm {
        async fn generate(
            &self,
            _messages: Vec<Message>,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<GenerationResponse> {
            unimplemented!()
        }

        async fn create_structured_output_with_messages_raw(
            &self,
            _messages: Vec<Message>,
            _json_schema: &Value,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<Value> {
            Ok(json!({"marker": "raw"}))
        }

        async fn create_structured_output_with_messages_raw_validated(
            &self,
            _messages: Vec<Message>,
            _json_schema: &Value,
            _options: Option<GenerationOptions>,
            _validator: crate::llm_trait::StructuredOutputValidator<'_>,
        ) -> LlmResult<Value> {
            Ok(json!({"marker": "validated"}))
        }

        fn model(&self) -> &str {
            "marker"
        }
    }

    #[tokio::test]
    async fn validated_path_delegates_to_inner_validated() {
        // #2: RecordingLlm must forward the typed validator to the inner
        // adapter's `_validated` method. If it fell back to the trait default it
        // would call `_with_messages_raw` (returning the "raw" marker) and bypass
        // validation-retry.
        let dir = tempfile::tempdir().expect("create tempdir");
        let path = dir.path().join("cassette.json");
        let recorder = RecordingLlm::new(Arc::new(MarkerLlm), &path);

        let schema = json!({"title": "KnowledgeGraph", "type": "object"});
        let validate = |_: &Value| Ok(());
        let value = recorder
            .create_structured_output_with_messages_raw_validated(
                graph_msgs(),
                &schema,
                None,
                &validate,
            )
            .await
            .expect("validated delegation");

        assert_eq!(
            value,
            json!({"marker": "validated"}),
            "must delegate to inner `_validated`, not `_with_messages_raw`"
        );
        // The delegated response is still recorded.
        recorder.flush().expect("flush");
        let cassette = LlmCassette::load(&path).expect("load");
        assert_eq!(cassette.entries.len(), 1);
    }

    #[tokio::test]
    async fn re_recording_merges_existing_entries() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let path = dir.path().join("cassette.json");

        let schema = json!({"title": "KnowledgeGraph", "type": "object"});

        // First recording session.
        {
            let graph = json!({"nodes": [{"name": "Alice"}], "relationships": []});
            let mock: Arc<dyn Llm> = Arc::new(StubLlm::new(vec![graph.to_string()]));
            let recorder = RecordingLlm::new(mock, &path);
            recorder
                .create_structured_output_with_messages_raw(graph_msgs(), &schema, None)
                .await
                .expect("first session");
            recorder.flush().expect("flush first");
        }

        // Second session with a different input must keep the first entry.
        {
            let other_msgs = vec![
                Message::system("Extract a knowledge graph."),
                Message::user("Carol knows Dave."),
            ];
            let graph = json!({"nodes": [{"name": "Carol"}], "relationships": []});
            let mock: Arc<dyn Llm> = Arc::new(StubLlm::new(vec![graph.to_string()]));
            let recorder = RecordingLlm::new(mock, &path);
            recorder
                .create_structured_output_with_messages_raw(other_msgs, &schema, None)
                .await
                .expect("second session");
            recorder.flush().expect("flush second");
        }

        let cassette = LlmCassette::load(&path).expect("load merged cassette");
        assert_eq!(cassette.entries.len(), 2, "re-recording must merge");
    }
}
