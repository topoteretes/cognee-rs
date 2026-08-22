//! Stage 2c of `improve()` — distill a finished session's Q&A into durable,
//! entity-anchored lesson documents in the permanent knowledge graph.
//!
//! Ported from:
//! - `/tmp/cognee-python/cognee/modules/session_distillation/distill.py`
//! - `/tmp/cognee-python/cognee/modules/session_distillation/models.py`
//!
//! Flow (curator calls parallel by batch; accept/write calls parallel by
//! lesson):
//!
//! 1. **LOAD**    session Q&A turns.
//! 2. **CURATE**  pack the timeline into batches; one curator LLM call per batch.
//! 3. **ACCEPT**  per proposed lesson: search prior lessons/entities, then a
//!    writer/rejecter LLM call.
//! 4. **PERSIST** render accepted lessons as markdown documents; add + cognify
//!    them, tagged with both the generic `session_learnings` node-set and the
//!    per-session `session_learnings:{session_id}` node-set.
//!
//! Everything is fail-open per unit: a failed curator batch, a failed writer
//! call, or a failed single session drops only its own work, never the whole
//! run.
//!
//! # Scope cut vs. Python (locked decision, see plan P2-05)
//!
//! Python's distillation also consumes gated *session-context entries*
//! (`SessionContextEntry`, driven by `harmful_count`/`confidence`). Rust has no
//! port of that subsystem, so this port consumes **only** Q&A turns
//! (`SessionQAEntry`). Consequently:
//! - `members` is always empty (no candidate-memory corpus to draw from), so
//!   the writer input's "MEMBER ENTRIES" section is never emitted;
//! - the top-level gate is `NoQaEntries` ("no non-empty Q&A"), a rename of
//!   Python's `no_gated_entries`;
//! - the two `models.py` tunables `MIN_GATE_CONFIDENCE` and
//!   `MAX_CANDIDATE_CHARS` (which only filter `SessionContextEntry` rows) are
//!   intentionally **not** ported, and Python's stage 2b2
//!   (`_extract_agent_context`) is out of scope.

use std::collections::HashMap;
use std::sync::Arc;

use cognee_core::CpuPool;
use cognee_database::{DatabaseConnection, PipelineRunRepository};
use cognee_embedding::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_ingestion::{AddParams, AddPipeline};
use cognee_llm::{Llm, LlmExt};
use cognee_models::DataInput;
use cognee_ontology::OntologyResolver;
use cognee_session::{SessionQAEntry, SessionStore};
use cognee_storage::StorageTrait;
use cognee_vector::VectorDB;
use futures::stream::{self, StreamExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tracing::{info, warn};
use uuid::Uuid;

use crate::config::CognifyConfig;
use crate::error::CognifyError;
use crate::tasks::cognify;

// ---------------------------------------------------------------------------
// Tunables (port of `models.py:15-35`)
// ---------------------------------------------------------------------------

/// How many timeline blocks are packed into one curator batch (`models.py:24`).
pub const CURATOR_BLOCKS_PER_BATCH: usize = 6;
/// Character cap applied to a collapsed Q&A question (`models.py:25`).
pub const MAX_QA_QUESTION_CHARS: usize = 1_200;
/// Character cap applied to a collapsed Q&A answer (`models.py:26`).
pub const MAX_QA_ANSWER_CHARS: usize = 1_200;
/// Bounded concurrency for the curator fan-out (`models.py:30`).
pub const CURATOR_CONCURRENCY: usize = 5;
/// Bounded concurrency for the writer fan-out (`models.py:31`).
pub const WRITER_CONCURRENCY: usize = 5;
/// Similar previously-persisted lessons fetched per proposed lesson
/// (`models.py:34`).
pub const NOVELTY_LESSONS_PER_LESSON: usize = 5;
/// Existing entity names fetched per proposed lesson (`models.py:35`).
pub const GLOSSARY_ENTITIES_PER_LESSON: usize = 20;

/// Node-set marking distillate documents in the graph (`distill.py:59`). Used
/// both to tag them on write and to scope the novelty search to previously
/// persisted lessons.
pub const DISTILLATE_NODE_SET: &str = "session_learnings";

/// Oversampling factor for the client-side `belongs_to_set` filter on the
/// novelty search. Rust's [`VectorDB::search_similar`] takes no server-side
/// node-name filter (Python's engine does), so we request more candidates than
/// `limit` and filter down afterwards. Heuristic, not an exact translation —
/// see the plan's Risks. `search_similar` clamps to the collection size, so an
/// over-large `top_k` is harmless on small collections.
const NOVELTY_SEARCH_OVERSAMPLE: usize = 20;

/// The per-session node-set tag (`truth_subspace/constants.py:6-7`,
/// `truth_session_node_set`). Kept as a local copy rather than depending on the
/// (possibly-absent) `cognee-truth-subspace` crate for one format string.
fn truth_session_node_set(session_id: &str) -> String {
    format!("session_learnings:{session_id}")
}

/// The full node-set list attached to **every** published lesson document:
/// both the generic tag and the per-session tag (`distill.py:378`).
fn distillate_node_set(session_id: &str) -> Vec<String> {
    vec![
        DISTILLATE_NODE_SET.to_string(),
        truth_session_node_set(session_id),
    ]
}

// ---------------------------------------------------------------------------
// Structured-output schemas (port of `models.py:41-98`)
// ---------------------------------------------------------------------------

/// One durable lesson the curator proposes from a session batch
/// (`models.py:41-50`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ProposedLesson {
    /// One standalone sentence capturing the durable learning.
    pub working_statement: String,
    /// Ids of the candidate memories this lesson draws from (may be empty). In
    /// this port they are always empty (no candidate-memory corpus).
    #[serde(default)]
    pub member_entry_ids: Vec<String>,
}

/// Proposed lessons from one curator batch call (`models.py:53-56`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct CuratorBatchOutput {
    /// Zero or more proposed lessons.
    #[serde(default)]
    pub lessons: Vec<ProposedLesson>,
}

/// Why a proposed lesson was rejected (`models.py:66`, a 3-value `Literal`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RejectionReason {
    /// A similar existing lesson already conveys this learning.
    AlreadyKnown,
    /// The lesson is session-local and not useful beyond this session.
    NotDurable,
    /// The member entries do not actually support the statement.
    Unsupported,
}

/// A per-lesson decision: accept (and write it) or reject (with a reason)
/// (`models.py:62-81`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WrittenLesson {
    /// `true` to persist this lesson, `false` to drop it.
    pub accept: bool,
    /// Why the lesson was rejected, when `accept` is `false`.
    #[serde(default)]
    pub reason: Option<RejectionReason>,
    /// Standalone, entity-anchored prose for the lesson, when accepted.
    #[serde(default)]
    pub statement: String,
    /// Glossary entity names used in the statement.
    #[serde(default)]
    pub entities: Vec<String>,
    /// One sentence naming the situation it was learned in.
    #[serde(default)]
    pub why_learned: String,
}

/// Terminal status of one [`distill_session`] call (`models.py:87-98`).
///
/// `NoQaEntries` is a **rename** of Python's `no_gated_entries`: the Rust gate
/// is "no non-empty Q&A", not "no gated context entries" (scope cut).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DistillationStatus {
    /// Lessons were curated, accepted, and published.
    Completed,
    /// The session had no non-empty Q&A to distill.
    NoQaEntries,
    /// The curator proposed no lessons.
    NoProposedLessons,
    /// Every proposed lesson was rejected by the writer.
    NoAcceptedLessons,
}

/// Outcome of one [`distill_session`] call (`models.py:87-98`).
#[derive(Debug, Clone)]
pub struct DistillationResult {
    /// The session that was distilled.
    pub session_id: String,
    /// The dataset the lessons landed in (only set on `Completed`).
    pub dataset_id: Option<Uuid>,
    /// Terminal status.
    pub status: DistillationStatus,
    /// The rendered lesson documents (non-empty only on `Completed`).
    pub documents: Vec<String>,
}

/// Aggregate summary of a multi-session distillation run.
#[derive(Debug, Clone, Default)]
pub struct DistillSessionsResult {
    /// Number of sessions that reached `Completed` (produced >= 1 document).
    pub sessions_distilled: usize,
    /// Total number of lesson documents published across all sessions.
    pub lessons_published: usize,
}

/// Error type for [`distill_session`].
#[derive(Debug, Error)]
pub enum DistillError {
    /// A session-store read failed.
    #[error("Session error: {0}")]
    Session(#[from] cognee_session::SessionError),
    /// The add pipeline failed to ingest the rendered documents.
    #[error("Ingestion error: {0}")]
    Ingestion(String),
    /// Cognify of the published documents failed.
    #[error("Cognify error: {0}")]
    Cognify(#[from] CognifyError),
    /// The dataset could not be resolved after add (should not happen — add
    /// creates it).
    #[error("Dataset '{0}' not found after add")]
    DatasetNotFound(String),
}

// ---------------------------------------------------------------------------
// Batching (port of `build_curator_batches`, `distill.py:148-173`)
// ---------------------------------------------------------------------------

/// Collapse all interior whitespace to single spaces, then truncate to
/// `max_chars` characters. Mirrors Python's `" ".join(x.split())[:N]`.
fn collapse_and_truncate(s: &str, max_chars: usize) -> String {
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(max_chars).collect()
}

/// Pack the session timeline into coarse, size-safe chronological batches.
///
/// Q&A-only in this port (no candidate-entry corpus): per entry, collapse +
/// truncate `question`/`answer`, format `"User: {q}\nAssistant: {a}"`, skip if
/// both are empty, sort by `created_at`, and chunk into groups of
/// [`CURATOR_BLOCKS_PER_BATCH`] joined by `"\n\n"`.
pub fn build_curator_batches(entries: &[SessionQAEntry]) -> Vec<String> {
    let mut timeline: Vec<(chrono::DateTime<chrono::Utc>, String)> = Vec::new();
    for entry in entries {
        let question = collapse_and_truncate(&entry.question, MAX_QA_QUESTION_CHARS);
        let answer = collapse_and_truncate(&entry.answer, MAX_QA_ANSWER_CHARS);
        if question.is_empty() && answer.is_empty() {
            continue;
        }
        let block = format!("User: {question}\nAssistant: {answer}");
        timeline.push((entry.created_at, block));
    }

    // Stable sort by real timestamp — strictly better-ordered than Python's
    // string-timestamp sort on `row.get("time")`.
    timeline.sort_by_key(|(ts, _)| *ts);
    let blocks: Vec<String> = timeline.into_iter().map(|(_, block)| block).collect();

    blocks
        .chunks(CURATOR_BLOCKS_PER_BATCH)
        .map(|chunk| chunk.join("\n\n"))
        .collect()
}

// ---------------------------------------------------------------------------
// Curator (port of `curate_batch` / `propose_lessons`, `distill.py:176-207`)
// ---------------------------------------------------------------------------

/// Curator system prompt, vendored verbatim from Python's
/// `session_distillation_curator_system.txt`.
const CURATOR_PROMPT: &str = include_str!("prompts/session_distillation_curator_system.txt");
/// Writer/rejecter system prompt, vendored verbatim from Python's
/// `session_distillation_writer_system.txt`.
const WRITER_PROMPT: &str = include_str!("prompts/session_distillation_writer_system.txt");

/// One curator call over one batch slice. Fail-open → `vec![]`.
async fn curate_batch(llm: &dyn Llm, batch_text: &str) -> Vec<ProposedLesson> {
    match llm
        .create_structured_output::<CuratorBatchOutput>(batch_text, CURATOR_PROMPT, None)
        .await
    {
        Ok(output) => output.lessons,
        Err(e) => {
            warn!("distill: curator batch failed open: {e}");
            Vec::new()
        }
    }
}

/// Pack session inputs into curator batches, fan out one curator call per batch
/// (bounded by [`CURATOR_CONCURRENCY`]), and flatten the proposed lessons.
///
/// Uses `.collect()` (not `try_collect`) because each future returns a
/// `Vec<ProposedLesson>` directly and never a `Result` — one failed batch must
/// not abort the stream (fail-open).
async fn propose_lessons(llm: &Arc<dyn Llm>, entries: &[SessionQAEntry]) -> Vec<ProposedLesson> {
    let batches = build_curator_batches(entries);
    if batches.is_empty() {
        return Vec::new();
    }

    let per_batch: Vec<Vec<ProposedLesson>> = stream::iter(batches)
        .map(|batch| {
            let llm = Arc::clone(llm);
            async move { curate_batch(llm.as_ref(), &batch).await }
        })
        .buffer_unordered(CURATOR_CONCURRENCY)
        .collect()
        .await;

    per_batch.into_iter().flatten().collect()
}

// ---------------------------------------------------------------------------
// Search (port of `search_payload_texts`, `distill.py:210-247`)
// ---------------------------------------------------------------------------

/// Does this search result's `belongs_to_set` metadata mark it as a
/// `session_learnings` document?
///
/// NodeSet entries are JSON objects `{"id","name","type":"NodeSet"}` (built by
/// `classify_documents`), **not** bare strings — a result matches when any
/// element's `"name"` equals [`DISTILLATE_NODE_SET`]. Bare-string dataset-id
/// entries from base `DataPoint` constructors simply won't match.
fn belongs_to_session_learnings(metadata: &HashMap<String, Value>) -> bool {
    metadata
        .get("belongs_to_set")
        .and_then(|v| v.as_array())
        .is_some_and(|arr| {
            arr.iter().any(|el| {
                el.as_object()
                    .and_then(|o| o.get("name"))
                    .and_then(|n| n.as_str())
                    == Some(DISTILLATE_NODE_SET)
            })
        })
}

/// Embed `query_text`, vector-search one collection, and return de-duplicated
/// payload texts; `[]` on **any** failure (including `CollectionNotFound` for a
/// fresh dataset with no collection yet).
///
/// When `filter_session_learnings` is set (the novelty search only), results
/// are oversampled and filtered client-side by [`belongs_to_session_learnings`]
/// before dedup/truncation.
async fn search_payload_texts(
    vector_db: &dyn VectorDB,
    embedding_engine: &dyn EmbeddingEngine,
    data_type: &str,
    field_name: &str,
    query_text: &str,
    limit: usize,
    filter_session_learnings: bool,
) -> Vec<String> {
    let query_vector = match embedding_engine.embed(&[query_text]).await {
        Ok(mut vs) if !vs.is_empty() => vs.remove(0),
        Ok(_) => {
            warn!("distill: embed returned no vectors; search failing open");
            return Vec::new();
        }
        Err(e) => {
            warn!("distill: embed failed open for {data_type}_{field_name}: {e}");
            return Vec::new();
        }
    };

    // Oversample when a client-side filter applies so a true match is not
    // starved by non-matching candidates that outrank it.
    let top_k = if filter_session_learnings {
        limit.saturating_mul(NOVELTY_SEARCH_OVERSAMPLE)
    } else {
        limit
    };

    let results = match vector_db
        .search_similar(data_type, field_name, &query_vector, top_k)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            // A missing collection (fresh dataset) is the common case; treat
            // every error as empty (fail-open).
            info!("distill: search on {data_type}_{field_name} failed open: {e}");
            return Vec::new();
        }
    };

    let mut texts: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for result in results {
        if filter_session_learnings && !belongs_to_session_learnings(&result.metadata) {
            continue;
        }
        let text = result
            .metadata
            .get("text")
            .and_then(|v| v.as_str())
            .or_else(|| result.metadata.get("name").and_then(|v| v.as_str()));
        let Some(text) = text else { continue };
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        // Casefold divergence: `.to_lowercase()` vs Python `.casefold()` — same
        // accepted gap documented for P2-02's `learning_id`.
        let key = text.to_lowercase();
        if seen.insert(key) {
            texts.push(text.to_string());
            if texts.len() >= limit {
                break;
            }
        }
    }
    texts
}

// ---------------------------------------------------------------------------
// Writer (port of `build_writer_input` / `write_or_reject`, `distill.py:250-291`)
// ---------------------------------------------------------------------------

/// Assemble the writer LLM's text input from the proposed lesson plus the
/// search context. Each optional section is appended only when non-empty. In
/// this port `members` is always empty, so the "MEMBER ENTRIES" section is
/// never emitted.
fn build_writer_input(
    lesson: &ProposedLesson,
    members: &[String],
    prior_lessons: &[String],
    glossary: &[String],
) -> String {
    let mut sections = vec![format!("PROPOSED LESSON:\n{}", lesson.working_statement)];
    if !members.is_empty() {
        let body = members
            .iter()
            .map(|m| format!("- {m}"))
            .collect::<Vec<_>>()
            .join("\n");
        sections.push(format!("MEMBER ENTRIES:\n{body}"));
    }
    if !prior_lessons.is_empty() {
        let body = prior_lessons
            .iter()
            .map(|p| format!("- {p}"))
            .collect::<Vec<_>>()
            .join("\n");
        sections.push(format!("SIMILAR EXISTING LESSONS:\n{body}"));
    }
    if !glossary.is_empty() {
        let body = glossary
            .iter()
            .map(|n| format!("- {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        sections.push(format!("ENTITY GLOSSARY:\n{body}"));
    }
    sections.join("\n\n")
}

/// One writer/rejecter call for one proposed lesson. Fail-open → `None`.
async fn write_or_reject(
    llm: &dyn Llm,
    lesson: &ProposedLesson,
    members: &[String],
    prior_lessons: &[String],
    glossary: &[String],
) -> Option<WrittenLesson> {
    let text_input = build_writer_input(lesson, members, prior_lessons, glossary);
    match llm
        .create_structured_output::<WrittenLesson>(&text_input, WRITER_PROMPT, None)
        .await
    {
        Ok(written) => Some(written),
        Err(e) => {
            warn!("distill: writer call failed open: {e}");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Evaluate + accept (port of `evaluate_proposed_lesson` /
// `accept_proposed_lessons`, `distill.py:294-344`)
// ---------------------------------------------------------------------------

/// For one proposed lesson: run the novelty + glossary searches concurrently,
/// then the writer/rejecter call.
///
/// `members` is always empty in this port (scope cut) — there is no
/// candidate-memory corpus to look up.
async fn evaluate_proposed_lesson(
    llm: &dyn Llm,
    vector_db: &dyn VectorDB,
    embedding_engine: &dyn EmbeddingEngine,
    lesson: &ProposedLesson,
) -> Option<WrittenLesson> {
    let members: Vec<String> = Vec::new();

    let (prior_lessons, glossary) = tokio::join!(
        // Novelty: previously-persisted lessons, scoped to the
        // `session_learnings` node-set via client-side filtering.
        search_payload_texts(
            vector_db,
            embedding_engine,
            "DocumentChunk",
            "text",
            &lesson.working_statement,
            NOVELTY_LESSONS_PER_LESSON,
            true,
        ),
        // Glossary: existing entity names, no node-set filter.
        search_payload_texts(
            vector_db,
            embedding_engine,
            "Entity",
            "name",
            &lesson.working_statement,
            GLOSSARY_ENTITIES_PER_LESSON,
            false,
        ),
    );

    write_or_reject(llm, lesson, &members, &prior_lessons, &glossary).await
}

/// Fan out one writer evaluation per proposed lesson (bounded by
/// [`WRITER_CONCURRENCY`]) and keep only lessons the writer accepted with a
/// non-empty statement. `.collect()` (not `try_collect`) — one failed writer
/// call must not abort the stream (fail-open).
async fn accept_proposed_lessons(
    llm: &Arc<dyn Llm>,
    vector_db: &Arc<dyn VectorDB>,
    embedding_engine: &Arc<dyn EmbeddingEngine>,
    proposed: Vec<ProposedLesson>,
) -> Vec<WrittenLesson> {
    let decisions: Vec<Option<WrittenLesson>> = stream::iter(proposed)
        .map(|lesson| {
            let llm = Arc::clone(llm);
            let vector_db = Arc::clone(vector_db);
            let embedding_engine = Arc::clone(embedding_engine);
            async move {
                evaluate_proposed_lesson(
                    llm.as_ref(),
                    vector_db.as_ref(),
                    embedding_engine.as_ref(),
                    &lesson,
                )
                .await
            }
        })
        .buffer_unordered(WRITER_CONCURRENCY)
        .collect()
        .await;

    decisions
        .into_iter()
        .flatten()
        .filter(|lesson| lesson.accept && !lesson.statement.trim().is_empty())
        .collect()
}

// ---------------------------------------------------------------------------
// Render + publish (port of `render_lesson_document` /
// `publish_distilled_lessons`, `distill.py:347-381`)
// ---------------------------------------------------------------------------

/// Render ONE accepted lesson as a standalone markdown document. Deterministic
/// string formatting — byte-identical to Python given the same inputs.
pub fn render_lesson_document(
    lesson: &WrittenLesson,
    session_id: &str,
    distilled_on: &str,
) -> String {
    let statement = lesson.statement.trim();
    let why = lesson.why_learned.trim().trim_end_matches('.');
    let body = if why.is_empty() {
        statement.to_string()
    } else {
        format!("{statement} ({why}.)")
    };
    format!("# Session learning — {distilled_on} (session {session_id})\n\n{body}\n")
}

/// Render each accepted lesson, add them (tagged with both node-sets), resolve
/// the dataset UUID, and cognify them in one pass. Returns the rendered
/// documents.
#[allow(clippy::too_many_arguments)]
async fn publish_distilled_lessons(
    session_id: &str,
    dataset_name: &str,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
    accepted: &[WrittenLesson],
    add_pipeline: &AddPipeline,
    llm: Arc<dyn Llm>,
    storage: Arc<dyn StorageTrait>,
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    database: Arc<DatabaseConnection>,
    pipeline_run_repo: Arc<dyn PipelineRunRepository>,
    thread_pool: Arc<dyn CpuPool>,
    ontology_resolver: Arc<dyn OntologyResolver>,
    cognify_config: &CognifyConfig,
) -> Result<(Vec<String>, Uuid), DistillError> {
    let distilled_on = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let documents: Vec<String> = accepted
        .iter()
        .map(|lesson| render_lesson_document(lesson, session_id, &distilled_on))
        .collect();

    let params = AddParams {
        node_set: Some(distillate_node_set(session_id)),
        ..Default::default()
    };
    let inputs: Vec<DataInput> = documents.iter().cloned().map(DataInput::Text).collect();

    let add_result = add_pipeline
        .add_with_params(inputs, dataset_name, owner_id, tenant_id, &params)
        .await
        .map_err(|e| DistillError::Ingestion(e.to_string()))?;
    if add_result.is_empty() {
        return Err(DistillError::Ingestion(
            "add returned no rows for distilled lessons".to_string(),
        ));
    }

    let dataset_id = match cognee_database::ops::datasets::get_dataset_by_name(
        database.as_ref(),
        dataset_name,
        owner_id,
        tenant_id,
    )
    .await
    {
        Ok(Some(ds)) => ds.id,
        Ok(None) => return Err(DistillError::DatasetNotFound(dataset_name.to_string())),
        Err(e) => return Err(DistillError::Ingestion(e.to_string())),
    };

    cognify(
        add_result,
        dataset_id,
        Some(owner_id),
        None,
        tenant_id,
        llm,
        storage,
        graph_db,
        vector_db,
        embedding_engine,
        database,
        pipeline_run_repo,
        thread_pool,
        ontology_resolver,
        cognify_config,
    )
    .await?;

    Ok((documents, dataset_id))
}

// ---------------------------------------------------------------------------
// Single-session entry point (port of `distill_session`, `distill.py:384-405`)
// ---------------------------------------------------------------------------

/// Distill one finished session's Q&A into curated, entity-anchored lessons in
/// the dataset's knowledge graph.
///
/// Gate ordering: `NoQaEntries` → `NoProposedLessons` → `NoAcceptedLessons` →
/// `Completed`.
#[allow(clippy::too_many_arguments)]
pub async fn distill_session(
    session_id: &str,
    dataset_name: &str,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
    session_store: Arc<dyn SessionStore>,
    add_pipeline: &AddPipeline,
    llm: Arc<dyn Llm>,
    storage: Arc<dyn StorageTrait>,
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    database: Arc<DatabaseConnection>,
    pipeline_run_repo: Arc<dyn PipelineRunRepository>,
    thread_pool: Arc<dyn CpuPool>,
    ontology_resolver: Arc<dyn OntologyResolver>,
    cognify_config: &CognifyConfig,
) -> Result<DistillationResult, DistillError> {
    let user_id_str = owner_id.to_string();
    let entries = session_store
        .get_all_qa_entries(session_id, Some(&user_id_str))
        .await?;

    // Gate 1: no non-empty Q&A → NoQaEntries (no LLM call).
    let has_content = entries.iter().any(|e| {
        !collapse_and_truncate(&e.question, MAX_QA_QUESTION_CHARS).is_empty()
            || !collapse_and_truncate(&e.answer, MAX_QA_ANSWER_CHARS).is_empty()
    });
    if !has_content {
        return Ok(DistillationResult {
            session_id: session_id.to_string(),
            dataset_id: None,
            status: DistillationStatus::NoQaEntries,
            documents: Vec::new(),
        });
    }

    // Gate 2: curator proposed nothing → NoProposedLessons.
    let proposed = propose_lessons(&llm, &entries).await;
    if proposed.is_empty() {
        return Ok(DistillationResult {
            session_id: session_id.to_string(),
            dataset_id: None,
            status: DistillationStatus::NoProposedLessons,
            documents: Vec::new(),
        });
    }

    // Gate 3: every proposed lesson rejected → NoAcceptedLessons.
    let accepted = accept_proposed_lessons(&llm, &vector_db, &embedding_engine, proposed).await;
    if accepted.is_empty() {
        return Ok(DistillationResult {
            session_id: session_id.to_string(),
            dataset_id: None,
            status: DistillationStatus::NoAcceptedLessons,
            documents: Vec::new(),
        });
    }

    let (documents, dataset_id) = publish_distilled_lessons(
        session_id,
        dataset_name,
        owner_id,
        tenant_id,
        &accepted,
        add_pipeline,
        llm,
        storage,
        graph_db,
        vector_db,
        embedding_engine,
        database,
        pipeline_run_repo,
        thread_pool,
        ontology_resolver,
        cognify_config,
    )
    .await?;

    Ok(DistillationResult {
        session_id: session_id.to_string(),
        dataset_id: Some(dataset_id),
        status: DistillationStatus::Completed,
        documents,
    })
}

// ---------------------------------------------------------------------------
// Multi-session loop (port of `_distill_sessions`, `improve.py:390-429`)
// ---------------------------------------------------------------------------

/// Distill each session in turn, fail-open per session: an error on one session
/// is logged and never blocks the others.
#[allow(clippy::too_many_arguments)]
pub async fn distill_sessions_in_knowledge_graph(
    session_ids: &[String],
    dataset_name: &str,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
    session_store: Arc<dyn SessionStore>,
    add_pipeline: &AddPipeline,
    llm: Arc<dyn Llm>,
    storage: Arc<dyn StorageTrait>,
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    database: Arc<DatabaseConnection>,
    pipeline_run_repo: Arc<dyn PipelineRunRepository>,
    thread_pool: Arc<dyn CpuPool>,
    ontology_resolver: Arc<dyn OntologyResolver>,
    cognify_config: &CognifyConfig,
) -> DistillSessionsResult {
    let mut result = DistillSessionsResult::default();

    for sid in session_ids {
        match distill_session(
            sid,
            dataset_name,
            owner_id,
            tenant_id,
            Arc::clone(&session_store),
            add_pipeline,
            Arc::clone(&llm),
            Arc::clone(&storage),
            Arc::clone(&graph_db),
            Arc::clone(&vector_db),
            Arc::clone(&embedding_engine),
            Arc::clone(&database),
            Arc::clone(&pipeline_run_repo),
            Arc::clone(&thread_pool),
            Arc::clone(&ontology_resolver),
            cognify_config,
        )
        .await
        {
            Ok(r) => {
                info!(
                    session_id = sid,
                    status = ?r.status,
                    documents = r.documents.len(),
                    "distill: session distilled"
                );
                if !r.documents.is_empty() {
                    result.sessions_distilled += 1;
                    result.lessons_published += r.documents.len();
                }
            }
            Err(e) => {
                warn!(
                    session_id = sid,
                    "distill: session distillation failed (non-fatal): {e}"
                );
            }
        }
    }

    result
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    fn mk_entry(q: &str, a: &str, secs: i64) -> SessionQAEntry {
        SessionQAEntry {
            id: Uuid::new_v4(),
            external_event_id: None,
            session_id: "s1".into(),
            user_id: None,
            question: q.into(),
            answer: a.into(),
            context: None,
            created_at: chrono::DateTime::from_timestamp(secs, 0).unwrap(),
            feedback_text: None,
            feedback_score: None,
            used_graph_element_ids: None,
            memify_metadata: None,
        }
    }

    #[test]
    fn collapse_and_truncate_collapses_whitespace_and_caps_length() {
        assert_eq!(collapse_and_truncate("  a\n\t b   c ", 100), "a b c");
        assert_eq!(collapse_and_truncate("aaaa", 2), "aa");
        assert_eq!(collapse_and_truncate("   ", 100), "");
    }

    #[test]
    fn build_curator_batches_formats_sorts_and_chunks() {
        // Provide 7 entries out of chronological order; expect 2 batches
        // (6 + 1) with blocks ordered by created_at.
        let mut entries = Vec::new();
        for i in (0..7).rev() {
            entries.push(mk_entry(&format!("q{i}"), &format!("a{i}"), i as i64));
        }
        let batches = build_curator_batches(&entries);
        assert_eq!(batches.len(), 2, "7 blocks / 6 per batch = 2 batches");
        // First batch: blocks 0..6, chronological, joined by \n\n.
        assert!(batches[0].starts_with("User: q0\nAssistant: a0"));
        assert!(batches[0].contains("User: q5\nAssistant: a5"));
        assert!(batches[0].contains("\n\n"));
        // Second batch: only block 6.
        assert_eq!(batches[1], "User: q6\nAssistant: a6");
    }

    #[test]
    fn build_curator_batches_skips_empty_pairs() {
        let entries = vec![mk_entry("   ", "\n\t ", 0), mk_entry("real q", "real a", 1)];
        let batches = build_curator_batches(&entries);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0], "User: real q\nAssistant: real a");
    }

    #[test]
    fn build_curator_batches_all_empty_yields_no_batches() {
        let entries = vec![mk_entry("", "", 0), mk_entry("  ", "\t", 1)];
        assert!(build_curator_batches(&entries).is_empty());
    }

    #[test]
    fn render_lesson_document_with_why_learned_exact() {
        let lesson = WrittenLesson {
            accept: true,
            reason: None,
            statement: "  TerraScout indexes nightly.  ".to_string(),
            entities: vec!["TerraScout".to_string()],
            why_learned: "  learned while debugging the indexer.  ".to_string(),
        };
        let out = render_lesson_document(&lesson, "sess-1", "2026-07-27");
        assert_eq!(
            out,
            "# Session learning — 2026-07-27 (session sess-1)\n\nTerraScout indexes nightly. (learned while debugging the indexer.)\n"
        );
    }

    #[test]
    fn render_lesson_document_without_why_learned_exact() {
        let lesson = WrittenLesson {
            accept: true,
            reason: None,
            statement: "TerraScout indexes nightly.".to_string(),
            entities: vec![],
            why_learned: "   ".to_string(),
        };
        let out = render_lesson_document(&lesson, "sess-2", "2026-07-27");
        assert_eq!(
            out,
            "# Session learning — 2026-07-27 (session sess-2)\n\nTerraScout indexes nightly.\n"
        );
    }

    #[test]
    fn node_set_is_exactly_generic_plus_per_session() {
        let ns = distillate_node_set("abc");
        assert_eq!(
            ns,
            vec![
                "session_learnings".to_string(),
                "session_learnings:abc".to_string()
            ]
        );
    }

    #[test]
    fn belongs_to_session_learnings_matches_nodeset_object_not_bare_string() {
        // Bare-string dataset-id entry (base DataPoint constructor) → no match.
        let mut bare = HashMap::new();
        bare.insert(
            "belongs_to_set".to_string(),
            serde_json::json!(["some-dataset-uuid"]),
        );
        assert!(!belongs_to_session_learnings(&bare));

        // NodeSet object with matching name → match.
        let mut obj = HashMap::new();
        obj.insert(
            "belongs_to_set".to_string(),
            serde_json::json!([
                {"id": "x", "name": "other", "type": "NodeSet"},
                {"id": "y", "name": "session_learnings", "type": "NodeSet"}
            ]),
        );
        assert!(belongs_to_session_learnings(&obj));

        // NodeSet object with non-matching name → no match.
        let mut obj2 = HashMap::new();
        obj2.insert(
            "belongs_to_set".to_string(),
            serde_json::json!([{"id": "z", "name": "user_sessions_from_cache", "type": "NodeSet"}]),
        );
        assert!(!belongs_to_session_learnings(&obj2));

        // Missing key → no match.
        assert!(!belongs_to_session_learnings(&HashMap::new()));
    }

    #[test]
    fn build_writer_input_omits_empty_sections() {
        let lesson = ProposedLesson {
            working_statement: "S".to_string(),
            member_entry_ids: vec![],
        };
        // No members / no prior / no glossary → only the PROPOSED LESSON section.
        let out = build_writer_input(&lesson, &[], &[], &[]);
        assert_eq!(out, "PROPOSED LESSON:\nS");

        // Prior lessons + glossary appended; members still omitted.
        let out2 = build_writer_input(
            &lesson,
            &[],
            &["prior one".to_string()],
            &["Entity A".to_string()],
        );
        assert_eq!(
            out2,
            "PROPOSED LESSON:\nS\n\nSIMILAR EXISTING LESSONS:\n- prior one\n\nENTITY GLOSSARY:\n- Entity A"
        );
    }

    #[test]
    fn written_lesson_reason_deserializes_snake_case() {
        let json = r#"{"accept": false, "reason": "already_known"}"#;
        let wl: WrittenLesson = serde_json::from_str(json).unwrap();
        assert!(!wl.accept);
        assert_eq!(wl.reason, Some(RejectionReason::AlreadyKnown));
        assert_eq!(wl.statement, "");
    }

    // -----------------------------------------------------------------------
    // `search_payload_texts` is module-private (not `pub(crate)`), so it is
    // exercised here in the source-file unit test module — the smallest
    // reachable path — rather than from the `tests/` integration file.
    // -----------------------------------------------------------------------

    use cognee_embedding::MockEmbeddingEngine;
    use cognee_vector::{MockVectorDB, VectorPoint};

    /// Sixteen-element vector matching `MockEmbeddingEngine::new(16)`. The engine
    /// defaults to zero vectors, so every stored point scores identically and
    /// `search_similar`'s stable sort returns them in insertion order — making
    /// dedup/limit assertions deterministic.
    fn v16() -> Vec<f32> {
        vec![0.25_f32; 16]
    }

    fn session_learnings_tag() -> serde_json::Value {
        serde_json::json!([{"id": "1", "name": DISTILLATE_NODE_SET, "type": "NodeSet"}])
    }

    fn other_tag() -> serde_json::Value {
        serde_json::json!([{"id": "9", "name": "some_other_set", "type": "NodeSet"}])
    }

    /// `search_payload_texts` de-dups by casefold, honours the node-set filter,
    /// respects `limit`, skips blank/absent text, and fails open to `[]` on a
    /// failing embed or a missing collection.
    #[tokio::test]
    async fn search_payload_texts_dedups_limits_filters_and_fails_open() {
        let db = MockVectorDB::new();
        let engine = MockEmbeddingEngine::new(16);
        db.create_collection("DocumentChunk", "text", 16)
            .await
            .unwrap();

        // Insertion order defines result order (all points score equally):
        // 1 tagged "Lesson One", two tagged case-variants of it, one tagged blank,
        // one tagged "Lesson Two", and one NON-tagged "Untagged Lesson".
        let points = vec![
            VectorPoint::new(Uuid::new_v4(), v16())
                .with_metadata("text", serde_json::json!("Lesson One"))
                .with_metadata("belongs_to_set", session_learnings_tag()),
            VectorPoint::new(Uuid::new_v4(), v16())
                .with_metadata("text", serde_json::json!("lesson one"))
                .with_metadata("belongs_to_set", session_learnings_tag()),
            VectorPoint::new(Uuid::new_v4(), v16())
                .with_metadata("text", serde_json::json!("Untagged Lesson"))
                .with_metadata("belongs_to_set", other_tag()),
            VectorPoint::new(Uuid::new_v4(), v16())
                .with_metadata("text", serde_json::json!("   "))
                .with_metadata("belongs_to_set", session_learnings_tag()),
            VectorPoint::new(Uuid::new_v4(), v16())
                .with_metadata("text", serde_json::json!("Lesson Two"))
                .with_metadata("belongs_to_set", session_learnings_tag()),
            VectorPoint::new(Uuid::new_v4(), v16())
                .with_metadata("text", serde_json::json!("LESSON ONE"))
                .with_metadata("belongs_to_set", session_learnings_tag()),
        ];
        db.index_points("DocumentChunk", "text", &points)
            .await
            .unwrap();

        // filter = true (novelty search): only `session_learnings`-tagged rows,
        // case-variants collapsed to one, blank skipped, untagged excluded.
        let filtered =
            search_payload_texts(&db, &engine, "DocumentChunk", "text", "q", 10, true).await;
        assert_eq!(
            filtered,
            vec!["Lesson One".to_string(), "Lesson Two".to_string()],
            "filter=true keeps only tagged, deduped, non-blank texts"
        );

        // `limit` truncates after dedup/filter: limit=1 yields just the first.
        let limited =
            search_payload_texts(&db, &engine, "DocumentChunk", "text", "q", 1, true).await;
        assert_eq!(
            limited,
            vec!["Lesson One".to_string()],
            "limit=1 returns exactly one text"
        );

        // filter = false (glossary search): tags ignored, so the untagged row is
        // included; still deduped by casefold and blank still skipped.
        let unfiltered =
            search_payload_texts(&db, &engine, "DocumentChunk", "text", "q", 10, false).await;
        assert_eq!(
            unfiltered,
            vec![
                "Lesson One".to_string(),
                "Untagged Lesson".to_string(),
                "Lesson Two".to_string()
            ],
            "filter=false returns all deduped texts including the untagged one"
        );

        // Missing collection → fail open to [].
        let missing =
            search_payload_texts(&db, &engine, "NoSuchType", "name", "q", 10, false).await;
        assert!(
            missing.is_empty(),
            "a missing collection fails open to an empty result"
        );

        // Failing embed → fail open to [] (a fresh engine so we don't poison the
        // shared one, though it is unused hereafter).
        let failing_engine = MockEmbeddingEngine::new(16);
        failing_engine.set_failure_after(0);
        let embed_failed = search_payload_texts(
            &db,
            &failing_engine,
            "DocumentChunk",
            "text",
            "q",
            10,
            false,
        )
        .await;
        assert!(
            embed_failed.is_empty(),
            "a failing embed fails open to an empty result (no search attempted)"
        );
    }
}
