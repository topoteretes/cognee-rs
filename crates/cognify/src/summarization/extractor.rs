//! Summary extractor using LLM for text summarization.
//!
//! Port of Python's:
//! - cognee/infrastructure/llm/extraction/extract_summary.py
//! - cognee/tasks/summarization/summarize_text.py

use std::sync::Arc;

use cognee_llm::{GenerationOptions, Llm, LlmExt};
use cognee_models::DocumentChunk;

use super::models::{SummarizedContent, TextSummary};
use crate::error::CognifyError;

/// Default summarization options shared by both the typed and dynamic paths.
fn default_summary_options() -> GenerationOptions {
    // Python parity: `acreate_structured_output` passes no output cap on the
    // summarization call (the ≤200-token limit is enforced via the prompt, not
    // an API max_tokens). A hard cap here can truncate the structured JSON
    // response mid-object. Leave max_tokens as None to match Python.
    GenerationOptions {
        temperature: Some(0.3),
        max_tokens: None,
        ..Default::default()
    }
}

/// Default system prompt for text summarization.
///
/// Vendored byte-for-byte from Python's
/// `cognee/infrastructure/llm/prompts/summarize_content.txt` (structured
/// categories + ordered facts, ≤200 tokens). Kept in sync via the prompt-parity
/// drift guard.
const DEFAULT_SUMMARY_PROMPT: &str = include_str!("prompts/summarize_content.txt");

/// Summarize a single chunk of text. Shared by [`SummaryExtractor::extract_summary`]
/// and the bounded [`SummaryExtractor::summarize_chunks`] pipeline so neither has
/// to fabricate a throwaway extractor per call.
async fn summarize_one(
    llm: &Arc<dyn Llm>,
    summary_schema: &Option<serde_json::Value>,
    text: &str,
    custom_prompt: Option<&str>,
) -> Result<SummarizedContent, CognifyError> {
    let system_prompt = custom_prompt.unwrap_or(DEFAULT_SUMMARY_PROMPT);
    let options = Some(default_summary_options());

    match summary_schema {
        None => llm
            .create_structured_output(text, system_prompt, options)
            .await
            .map_err(|e| CognifyError::LlmError(e.to_string())),
        Some(schema) => {
            let raw: serde_json::Value = llm
                .create_structured_output_raw(text, system_prompt, schema, options)
                .await
                .map_err(|e| CognifyError::LlmError(e.to_string()))?;
            let summary = raw.get("summary").and_then(|v| v.as_str()).ok_or_else(|| {
                CognifyError::LlmError(
                    "summary_schema output missing string `summary` field".to_string(),
                )
            })?;
            Ok(SummarizedContent {
                summary: summary.to_string(),
                description: String::new(),
            })
        }
    }
}

/// What one [`SummaryExtractor::summarize_chunks`] pass produced.
///
/// Both halves are complete and in input order: the successes are every chunk
/// that summarized, the failures every chunk that did not. Deliberately not a
/// `Result` — a per-chunk failure is data the caller records against a policy,
/// not a reason to throw away the summaries that succeeded.
#[derive(Debug, Default)]
pub struct SummarizeOutcome {
    /// Summaries for the chunks that succeeded, in input order.
    pub summaries: Vec<TextSummary>,

    /// `(chunk_id, error)` for the chunks that failed, in input order.
    pub failures: Vec<(uuid::Uuid, CognifyError)>,
}

impl SummarizeOutcome {
    /// `true` when every chunk summarized.
    pub fn is_complete(&self) -> bool {
        self.failures.is_empty()
    }
}

/// Summary extractor for text chunks.
///
/// Uses an LLM (via the Llm trait) to generate hierarchical summaries from text chunks.
/// Produces TextSummary objects linked to source chunks via deterministic UUIDs.
///
/// # Example
/// ```ignore
/// use cognee_cognify::SummaryExtractor;
/// use cognee_llm::OpenAIAdapter;
/// use std::sync::Arc;
///
/// let llm = Arc::new(OpenAIAdapter::new("gpt-4", "sk-...", None)?);
/// let extractor = SummaryExtractor::new(llm);
///
/// let text = "Long article text here...";
/// let summary = extractor.extract_summary(text, None).await?;
///
/// println!("Summary: {}", summary.summary);
/// ```
#[derive(Clone)]
pub struct SummaryExtractor {
    llm: Arc<dyn Llm>,
    /// When `Some`, the dynamic schema path is taken instead of the typed
    /// `SummarizedContent` path (Python `summarization_model` parity).
    summary_schema: Option<serde_json::Value>,
    /// Maximum number of concurrent summarization LLM calls. Bounds in-flight
    /// requests on large documents (see issue #19).
    max_parallel: usize,
}

impl SummaryExtractor {
    /// Default cap on concurrent summarization LLM calls. Matches the graph
    /// extractor's default `max_parallel_extractions` so both stages share the
    /// same in-flight ceiling. Python's summarization stage
    /// (`cognee/tasks/summarization/summarize_text.py:58`) gathers the whole
    /// batch unbounded, so this is deliberately non-binding at the default batch
    /// size while remaining a real, lowerable ceiling.
    pub const DEFAULT_MAX_PARALLEL: usize = crate::config::DEFAULT_MAX_PARALLEL_EXTRACTIONS;

    /// Create a new summary extractor using the built-in `SummarizedContent` schema.
    pub fn new(llm: Arc<dyn Llm>) -> Self {
        Self {
            llm,
            summary_schema: None,
            max_parallel: Self::DEFAULT_MAX_PARALLEL,
        }
    }

    /// Create a new summary extractor with an optional custom output schema.
    ///
    /// When `schema` is `Some`, the LLM is called via the dynamic raw path and
    /// the `summary` string field is extracted from the response. When `None`,
    /// the built-in typed `SummarizedContent` path is used.
    pub fn new_with_schema(llm: Arc<dyn Llm>, schema: Option<serde_json::Value>) -> Self {
        Self {
            llm,
            summary_schema: schema,
            max_parallel: Self::DEFAULT_MAX_PARALLEL,
        }
    }

    /// Set the maximum number of concurrent summarization LLM calls. Values
    /// below 1 are coerced to 1.
    pub fn with_max_parallel(mut self, max_parallel: usize) -> Self {
        self.max_parallel = max_parallel.max(1);
        self
    }

    /// Extract a summary from text.
    ///
    /// When `summary_schema` is `None`, uses the typed `SummarizedContent` path.
    /// When `Some`, calls the LLM with the custom schema and extracts the
    /// `summary` string field from the raw response (Python parity).
    pub async fn extract_summary(
        &self,
        text: &str,
        custom_prompt: Option<&str>,
    ) -> Result<SummarizedContent, CognifyError> {
        summarize_one(&self.llm, &self.summary_schema, text, custom_prompt).await
    }

    /// Summarize multiple text chunks in parallel.
    ///
    /// # Arguments
    /// * `chunks` - Slice of DocumentChunks to summarize
    /// * `custom_prompt` - Optional custom system prompt
    ///
    /// # Returns
    /// A [`SummarizeOutcome`] holding the summaries that succeeded and the
    /// chunks that failed, both in input order.
    ///
    /// The stream is *always* drained to completion. It used to `try_collect`,
    /// which drops the stream on the first `Err` — discarding summaries that
    /// had already been paid for and cancelling in-flight requests, so the
    /// reported failure list could never be complete. The return type has no
    /// `Err` channel at all, which is what keeps that from coming back: there
    /// is no failure mode of this function that is not per-chunk, and whether
    /// a per-chunk failure ends the run is the caller's policy decision, not
    /// this function's.
    pub async fn summarize_chunks(
        &self,
        chunks: &[DocumentChunk],
        custom_prompt: Option<String>,
    ) -> SummarizeOutcome {
        if chunks.is_empty() {
            return SummarizeOutcome::default();
        }

        use futures::stream::{self, StreamExt};

        let model_name = self.llm.model().to_string();

        // Pre-extract owned per-chunk inputs (index, text, chunk id) so the
        // stream yields owned items. Mapping a stream over borrowed `&chunk`
        // references trips a higher-ranked-lifetime inference bug when the
        // surrounding future is boxed; owning the items avoids it.
        #[allow(clippy::type_complexity)]
        let inputs: Vec<(
            usize,
            String,
            uuid::Uuid,
            Option<f64>,
            Option<Vec<serde_json::Value>>,
        )> = chunks
            .iter()
            .enumerate()
            .map(|(index, chunk)| {
                (
                    index,
                    chunk.text.clone(),
                    chunk.base.id,
                    chunk.base.importance_weight,
                    chunk.base.belongs_to_set.clone(),
                )
            })
            .collect();

        // Bounded-concurrency pipeline: at most `max_parallel` summarization
        // calls are in flight at once. This replaces the previous unbounded
        // per-chunk `tokio::spawn`, which opened one request per chunk on a large
        // document — a rate-limit and memory risk (issue #19). `buffer_unordered`
        // keeps the global cap while pipelining across chunks; results arrive out
        // of order, so each future carries its chunk index and we re-sort to
        // preserve the input order the callers expect.
        #[allow(clippy::type_complexity)]
        let results: Vec<Result<(usize, TextSummary), (usize, uuid::Uuid, CognifyError)>> =
            stream::iter(inputs)
                .map(
                    |(index, text, chunk_id, importance_weight, belongs_to_set)| {
                        let llm = Arc::clone(&self.llm);
                        let summary_schema = self.summary_schema.clone();
                        let model_name = model_name.clone();
                        let prompt = custom_prompt.clone();
                        async move {
                            // The per-chunk error is carried in the `Err` half of
                            // the item rather than `?`-ed out of the stream, so
                            // `collect` keeps draining.
                            let summarized = match summarize_one(
                                &llm,
                                &summary_schema,
                                &text,
                                prompt.as_deref(),
                            )
                            .await
                            {
                                Ok(summarized) => summarized,
                                Err(e) => return Err((index, chunk_id, e)),
                            };
                            let mut summary = TextSummary::from_summarized_content(
                                chunk_id, summarized, model_name,
                            );
                            // Port of summarize_text.py:81 — carry importance_weight from source chunk.
                            summary.base.importance_weight = importance_weight;
                            // Port of summarize_text.py:79 — inherit the source chunk's
                            // belongs_to_set (NodeSet objects + dataset id) so each
                            // TextSummary is scoped exactly like its chunk.
                            summary.base.belongs_to_set = belongs_to_set;
                            Ok((index, summary))
                        }
                    },
                )
                .buffer_unordered(self.max_parallel)
                .collect()
                .await;

        // `buffer_unordered` yields out of order; both halves are re-sorted by
        // the carried index so callers see input order on either side.
        let mut indexed: Vec<(usize, TextSummary)> = Vec::new();
        let mut failures: Vec<(usize, uuid::Uuid, CognifyError)> = Vec::new();
        for result in results {
            match result {
                Ok(pair) => indexed.push(pair),
                Err(failure) => failures.push(failure),
            }
        }
        indexed.sort_by_key(|(index, _)| *index);
        failures.sort_by_key(|(index, _, _)| *index);

        SummarizeOutcome {
            summaries: indexed.into_iter().map(|(_, summary)| summary).collect(),
            failures: failures
                .into_iter()
                .map(|(_, chunk_id, error)| (chunk_id, error))
                .collect(),
        }
    }

    /// Get a reference to the underlying LLM.
    pub fn llm(&self) -> &Arc<dyn Llm> {
        &self.llm
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use crate::config::validate_summary_schema;

    // Note: Tests that require LLM are in integration tests (tests/)
    // These are just structural tests

    #[test]
    #[allow(
        clippy::const_is_empty,
        reason = "intentional sanity check that the const is non-empty"
    )]
    fn test_default_prompt_not_empty() {
        assert!(!DEFAULT_SUMMARY_PROMPT.is_empty());
        assert!(DEFAULT_SUMMARY_PROMPT.contains("Summarize the chunk for retrieval"));
    }

    #[tokio::test]
    async fn summarize_chunks_bounds_concurrency_and_preserves_order() {
        use cognee_llm::{GenerationResponse, LlmResult, Message};
        use serde_json::{Value, json};
        use std::sync::atomic::{AtomicUsize, Ordering};
        use uuid::Uuid;

        /// Mock LLM that records the peak number of concurrent in-flight calls.
        struct ConcurrencyTracker {
            in_flight: AtomicUsize,
            max_seen: AtomicUsize,
            calls: AtomicUsize,
        }

        #[async_trait::async_trait]
        impl Llm for ConcurrencyTracker {
            async fn generate(
                &self,
                _messages: Vec<Message>,
                _options: Option<GenerationOptions>,
            ) -> LlmResult<GenerationResponse> {
                unreachable!("summarization uses structured output, not generate")
            }

            async fn create_structured_output_with_messages_raw(
                &self,
                _messages: Vec<Message>,
                _json_schema: &Value,
                _options: Option<GenerationOptions>,
            ) -> LlmResult<Value> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                let current = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                self.max_seen.fetch_max(current, Ordering::SeqCst);
                // Hold the "request" open so overlapping calls are observable.
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                self.in_flight.fetch_sub(1, Ordering::SeqCst);
                Ok(json!({ "summary": "s", "description": "d" }))
            }

            fn model(&self) -> &str {
                "mock-tracker"
            }
        }

        let tracker = Arc::new(ConcurrencyTracker {
            in_flight: AtomicUsize::new(0),
            max_seen: AtomicUsize::new(0),
            calls: AtomicUsize::new(0),
        });
        let llm: Arc<dyn Llm> = tracker.clone();

        let doc_id = Uuid::new_v4();
        let chunks: Vec<DocumentChunk> = (0..10)
            .map(|i| {
                let mut chunk = DocumentChunk::new(
                    Uuid::new_v4(),
                    format!("chunk text {i}"),
                    3,
                    i,
                    "paragraph_end".to_string(),
                    doc_id,
                );
                // Distinct non-default importance_weight per chunk to verify propagation.
                chunk.base.importance_weight = Some(0.1 * (i as f64 + 1.0));
                chunk
            })
            .collect();
        let expected: Vec<Option<Uuid>> = chunks.iter().map(|c| Some(c.base.id)).collect();
        let expected_weights: Vec<Option<f64>> =
            chunks.iter().map(|c| c.base.importance_weight).collect();

        let extractor = SummaryExtractor::new(llm).with_max_parallel(3);
        let outcome = extractor.summarize_chunks(&chunks, None).await;
        assert!(outcome.is_complete(), "no chunk should have failed");
        let summaries = outcome.summaries;

        // Every chunk produced one summary, in the original input order.
        assert_eq!(summaries.len(), 10);
        assert_eq!(tracker.calls.load(Ordering::SeqCst), 10);
        let got: Vec<Option<Uuid>> = summaries.iter().map(|s| s.made_from).collect();
        assert_eq!(got, expected, "summaries must preserve input chunk order");

        // Each summary carries its source chunk's importance_weight.
        let got_weights: Vec<Option<f64>> =
            summaries.iter().map(|s| s.base.importance_weight).collect();
        assert_eq!(
            got_weights, expected_weights,
            "importance_weight must propagate from chunk to summary"
        );

        // Concurrency was real (overlapping) but never exceeded the cap of 3.
        let peak = tracker.max_seen.load(Ordering::SeqCst);
        assert!(peak >= 2, "expected overlapping calls, peak was {peak}");
        assert!(peak <= 3, "concurrency cap exceeded: peak {peak} > 3");
    }

    /// A mock LLM that fails any call whose user message contains one of the
    /// markers and counts every call it saw.
    struct MarkerFailingLlm {
        markers: Vec<String>,
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl Llm for MarkerFailingLlm {
        async fn generate(
            &self,
            _messages: Vec<cognee_llm::Message>,
            _options: Option<GenerationOptions>,
        ) -> cognee_llm::LlmResult<cognee_llm::GenerationResponse> {
            unreachable!("summarization uses structured output, not generate")
        }

        async fn create_structured_output_with_messages_raw(
            &self,
            messages: Vec<cognee_llm::Message>,
            _json_schema: &serde_json::Value,
            _options: Option<GenerationOptions>,
        ) -> cognee_llm::LlmResult<serde_json::Value> {
            use std::sync::atomic::Ordering;
            self.calls.fetch_add(1, Ordering::SeqCst);
            // Let every call overlap so a cancelled sibling would be observable
            // as a missing call.
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            let text: String = messages.iter().map(|m| m.content.clone()).collect();
            if self.markers.iter().any(|marker| text.contains(marker)) {
                return Err(cognee_llm::LlmError::ApiError("simulated 429".to_string()));
            }
            Ok(serde_json::json!({ "summary": "s", "description": "d" }))
        }

        fn model(&self) -> &str {
            "mock-marker"
        }
    }

    fn marker_chunks(count: usize, failing: &[usize]) -> Vec<DocumentChunk> {
        let doc_id = uuid::Uuid::new_v4();
        (0..count)
            .map(|i| {
                let text = if failing.contains(&i) {
                    format!("chunk {i} FAILMARKER")
                } else {
                    format!("chunk {i} fine")
                };
                DocumentChunk::new(
                    uuid::Uuid::new_v4(),
                    text,
                    3,
                    i,
                    "paragraph_end".to_string(),
                    doc_id,
                )
            })
            .collect()
    }

    /// The regression test for the summarization defect: one failing chunk used
    /// to abandon the whole stream, throwing away the summaries already
    /// produced and cancelling the requests still in flight. The call count is
    /// the assertion that catches a return to `try_collect` — it cancels the
    /// futures that had not started, so fewer than ten calls reach the LLM.
    #[tokio::test]
    async fn summarize_chunks_keeps_completed_work_when_one_chunk_fails() {
        use std::sync::atomic::Ordering;

        let llm = Arc::new(MarkerFailingLlm {
            markers: vec!["FAILMARKER".to_string()],
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let chunks = marker_chunks(10, &[4]);
        let failing_chunk_id = chunks[4].base.id;
        let expected_ids: Vec<uuid::Uuid> = chunks
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != 4)
            .map(|(_, c)| c.base.id)
            .collect();

        let extractor = SummaryExtractor::new(llm.clone() as Arc<dyn Llm>).with_max_parallel(3);
        let outcome = extractor.summarize_chunks(&chunks, None).await;

        assert_eq!(outcome.summaries.len(), 9, "the completed work is kept");
        let got: Vec<uuid::Uuid> = outcome
            .summaries
            .iter()
            .filter_map(|s| s.made_from)
            .collect();
        assert_eq!(got, expected_ids, "summaries stay in input order");

        assert_eq!(outcome.failures.len(), 1);
        assert_eq!(outcome.failures[0].0, failing_chunk_id);

        assert_eq!(
            llm.calls.load(Ordering::SeqCst),
            10,
            "every chunk was attempted — nothing in flight was cancelled"
        );
    }

    /// Every failing chunk is reported, not just the first one to come back.
    #[tokio::test]
    async fn summarize_chunks_reports_every_failure() {
        use std::sync::atomic::Ordering;

        let llm = Arc::new(MarkerFailingLlm {
            markers: vec!["FAILMARKER".to_string()],
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let chunks = marker_chunks(10, &[1, 5, 9]);
        let mut expected_failures: Vec<uuid::Uuid> =
            vec![chunks[1].base.id, chunks[5].base.id, chunks[9].base.id];

        let extractor = SummaryExtractor::new(llm.clone() as Arc<dyn Llm>).with_max_parallel(4);
        let outcome = extractor.summarize_chunks(&chunks, None).await;

        assert_eq!(outcome.summaries.len(), 7);
        let mut got: Vec<uuid::Uuid> = outcome.failures.iter().map(|(id, _)| *id).collect();
        got.sort();
        expected_failures.sort();
        assert_eq!(got, expected_failures);
        assert_eq!(llm.calls.load(Ordering::SeqCst), 10);
    }

    #[tokio::test]
    async fn summarize_chunks_on_no_chunks_is_an_empty_outcome() {
        let llm: Arc<dyn Llm> = Arc::new(NoopLlm);
        let outcome = SummaryExtractor::new(llm).summarize_chunks(&[], None).await;
        assert!(outcome.summaries.is_empty());
        assert!(outcome.is_complete());
    }

    #[test]
    fn summary_prompt_matches_vendored_txt() {
        let vendored = include_str!("prompts/summarize_content.txt");
        assert_eq!(
            DEFAULT_SUMMARY_PROMPT, vendored,
            "const drifted from vendored .txt"
        );
        assert!(
            vendored.contains("Output two sections only"),
            "Python two-section structure marker missing"
        );
        assert!(
            vendored.contains("Max 200 tokens"),
            "token-limit marker missing"
        );
    }

    #[tokio::test]
    async fn summarize_chunks_inherits_chunk_belongs_to_set() {
        use cognee_llm::{GenerationResponse, LlmResult, Message};
        use serde_json::{Value, json};
        use uuid::Uuid;

        /// Minimal mock LLM returning a fixed structured summary.
        struct FixedLlm;

        #[async_trait::async_trait]
        impl Llm for FixedLlm {
            async fn generate(
                &self,
                _messages: Vec<Message>,
                _options: Option<GenerationOptions>,
            ) -> LlmResult<GenerationResponse> {
                unreachable!("summarization uses structured output, not generate")
            }

            async fn create_structured_output_with_messages_raw(
                &self,
                _messages: Vec<Message>,
                _json_schema: &Value,
                _options: Option<GenerationOptions>,
            ) -> LlmResult<Value> {
                Ok(json!({ "summary": "s", "description": "d" }))
            }

            fn model(&self) -> &str {
                "mock-fixed"
            }
        }

        let llm: Arc<dyn Llm> = Arc::new(FixedLlm);

        // Source chunk scoped by a NodeSet object plus a dataset-id entry.
        let dataset_id = Uuid::new_v4();
        let belongs_to_set = vec![
            json!({ "name": "my-node-set", "type": "NodeSet" }),
            json!(dataset_id.to_string()),
        ];

        let mut chunk = DocumentChunk::new(
            Uuid::new_v4(),
            "chunk text".to_string(),
            2,
            0,
            "paragraph_end".to_string(),
            Uuid::new_v4(),
        );
        chunk.base.belongs_to_set = Some(belongs_to_set.clone());

        let extractor = SummaryExtractor::new(llm);
        let outcome = extractor.summarize_chunks(&[chunk], None).await;
        assert!(outcome.failures.is_empty());
        let summaries = outcome.summaries;

        assert_eq!(summaries.len(), 1);
        assert_eq!(
            summaries[0].base.belongs_to_set,
            Some(belongs_to_set),
            "TextSummary must inherit the source chunk's full belongs_to_set"
        );
    }

    #[test]
    fn new_returns_no_schema() {
        // new() leaves summary_schema as None
        let llm: Arc<dyn Llm> = Arc::new(NoopLlm);
        let extractor = SummaryExtractor::new(llm);
        assert!(extractor.summary_schema.is_none());
    }

    #[test]
    fn new_with_schema_stores_schema() {
        let llm: Arc<dyn Llm> = Arc::new(NoopLlm);
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "summary": { "type": "string" } }
        });
        let extractor = SummaryExtractor::new_with_schema(llm, Some(schema.clone()));
        assert_eq!(extractor.summary_schema, Some(schema));
    }

    #[test]
    fn validate_summary_schema_accepts_valid() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "summary": { "type": "string" } }
        });
        assert!(validate_summary_schema(&schema).is_ok());
    }

    #[test]
    fn validate_summary_schema_rejects_missing_summary() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "other_field": { "type": "string" } }
        });
        assert!(validate_summary_schema(&schema).is_err());
    }

    #[test]
    fn validate_summary_schema_rejects_non_string_summary() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "summary": { "type": "integer" } }
        });
        assert!(validate_summary_schema(&schema).is_err());
    }

    #[test]
    fn validate_summary_schema_rejects_non_object() {
        let schema = serde_json::json!([1, 2, 3]);
        assert!(validate_summary_schema(&schema).is_err());
    }

    // Minimal no-op LLM for structural tests only.
    struct NoopLlm;

    #[async_trait::async_trait]
    impl Llm for NoopLlm {
        async fn generate(
            &self,
            _messages: Vec<cognee_llm::Message>,
            _options: Option<cognee_llm::types::GenerationOptions>,
        ) -> cognee_llm::LlmResult<cognee_llm::GenerationResponse> {
            unimplemented!()
        }
        async fn create_structured_output_with_messages_raw(
            &self,
            _messages: Vec<cognee_llm::Message>,
            _json_schema: &serde_json::Value,
            _options: Option<cognee_llm::types::GenerationOptions>,
        ) -> cognee_llm::LlmResult<serde_json::Value> {
            unimplemented!()
        }
        fn model(&self) -> &str {
            "noop"
        }
        fn max_context_length(&self) -> u32 {
            4096
        }
    }
}
