//! Temporal rerank on top of [`HybridRetriever`]: the TEMPORAL search type.
//!
//! Port of Python cognee's `cognee/modules/retrieval/temporal_hybrid_retriever.py`
//! (`TemporalHybridRetriever`, SDK-828) and its helpers in
//! `temporal_hybrid/`. The hybrid candidate fetch and the query-interval
//! extraction run concurrently. The `Timestamp` nodes inside the window are
//! looked up ([`GraphDBTrait::get_timestamps_in_range`]) and the chunks
//! attached to them join the candidates after hybrid's own ([`expansion`]):
//! similarity barely sees dates, so the rows a window names are mostly outside
//! the fetched set. The graph is then asked which candidates are attached to a
//! `Timestamp` inside the window ([`GraphDBTrait::get_temporal_anchors`]), the
//! oversized candidate set is reordered so the anchored candidates come first
//! ([`HybridCandidates::prioritize`]), ranked by how tightly their best
//! matched timestamp fits the window ([`matching::tightness_rank`]): for
//! "18 March 1965" a chunk dated 1965-03-18 comes before one that only says
//! "1965", while dates at least as precise as the question rank alike and keep
//! their similarity order. Finally [`HybridCandidates::finalize`] — the same
//! step plain hybrid uses — cuts it to `top_k` and selects the facts against
//! the entities that survive the cut. Context formatting and completion are
//! plain hybrid's, unchanged.
//!
//! When there is nothing to rerank by, the result is the plain hybrid result
//! (cut to `top_k`) and the diagnostics say why ([`TemporalFallbackReason`]).
//!
//! This replaced an Event-based `TemporalRetriever` that read only the `Event`
//! nodes of the separate `temporal_cognify` pipeline and answered everything
//! else from triplet search. As in Python, graphs from that pipeline now get
//! plain-hybrid answers; graphs from default cognify, which writes `Timestamp`
//! nodes, get the rerank.
//!
//! [`GraphDBTrait::get_timestamps_in_range`]: cognee_graph::GraphDBTrait::get_timestamps_in_range
//! [`GraphDBTrait::get_temporal_anchors`]: cognee_graph::GraphDBTrait::get_temporal_anchors

mod expansion;
mod matching;

use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tracing::{debug, warn};

use cognee_embedding::EmbeddingEngine;
use cognee_graph::{GraphDBTrait, TemporalAnchors, WindowTimestamp};
use cognee_llm::{GenerationOptions, Llm, Message};
use cognee_session::SessionContext;
use cognee_vector::VectorDB;

use self::expansion::{CHUNK_COLLECTIONS, attached_node_ids, retrieve_in_collections};
use self::matching::{
    ExtractedWindow, extract_query_interval, tightest_first, tightness_rank, to_epoch_ms,
    window_span_ms,
};
use crate::retrievers::hybrid::{
    HybridBudget, HybridCandidates, HybridLanes, HybridRetriever, HybridView, result_id,
};
use crate::retrievers::{DatasetScoping, SearchRetriever};
use crate::types::{SearchContext, SearchError, SearchOutput, SearchParams, SearchType};

/// `top_k` when neither the request nor the constructor sets one — Python's
/// `TemporalHybridRetriever.__init__` default.
const DEFAULT_TOP_K: usize = 5;
/// `candidate_top_k` defaults to this many times `top_k`: the rerank needs
/// something left to promote after it passes over the off-window candidates.
const CANDIDATE_MULTIPLIER: usize = 4;

/// Logged when the window matched nothing in the candidate set. Python's
/// `FALLBACK_WARNING`, verbatim.
const FALLBACK_WARNING: &str =
    "No time-anchored data found for this question; answering with HYBRID search instead.";
/// Logged when the question yielded no time window, so the graph's dates were
/// never consulted. Python's `NO_WINDOW_WARNING`, verbatim. The reason stays in
/// the diagnostics, not in the log: it reflects what the extraction LLM made of
/// the question, not the question.
const NO_WINDOW_WARNING: &str =
    "Could not extract a time window from this question; answering with HYBRID search instead.";

/// Why a TEMPORAL search answered with (or as) the plain hybrid result.
///
/// Python keeps these as strings on `last_reason`; [`Self::as_str`] gives the
/// same strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TemporalFallbackReason {
    /// The graph is empty: nothing was fetched and no LLM was called.
    EmptyGraph,
    /// The question names no time (both window sides open).
    NoTimeConstraint,
    /// The extracted window is not a real one: an impossible date, or a start
    /// at or after the end.
    InvalidInterval,
    /// No candidate is dated inside the window, and the graph has no
    /// `Timestamp` in it at all.
    NoTemporalMatch,
    /// The window has timestamps, but none of the candidates reach them — or
    /// the rerank ran and moved nothing (informational: the result is the
    /// baseline either way).
    NoCandidateOverlap,
}

impl TemporalFallbackReason {
    /// The snake_case name Python stores on `last_reason`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EmptyGraph => "empty_graph",
            Self::NoTimeConstraint => "no_time_constraint",
            Self::InvalidInterval => "invalid_interval",
            Self::NoTemporalMatch => "no_temporal_match",
            Self::NoCandidateOverlap => "no_candidate_overlap",
        }
    }
}

impl fmt::Display for TemporalFallbackReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What one TEMPORAL search did. Python keeps these on the retriever instance
/// (`last_interval`, `last_reason`, `last_anchors`, `last_expansion`,
/// `last_baseline`); the Rust retriever is shared across concurrent requests,
/// so it returns them with the result instead ([`TemporalHybridRetriever::retrieve`]).
#[derive(Debug, Clone, Default)]
pub struct TemporalDiagnostics {
    /// The extracted window `(start, end)`, start inclusive, end exclusive,
    /// UTC; a side is `None` when open or when no window was extracted.
    pub interval: (Option<DateTime<Utc>>, Option<DateTime<Utc>>),
    /// Why the result is (or equals) the plain hybrid baseline; `None` when
    /// the rerank changed it.
    pub reason: Option<TemporalFallbackReason>,
    /// Which candidates the graph found attached to the window.
    pub anchors: TemporalAnchors,
    /// The chunk ids the window's own timestamps added to the candidates.
    pub expansion: HashSet<String>,
    /// The plain hybrid result cut to `top_k`: what a fallback returns.
    pub baseline: SearchContext,
}

/// One TEMPORAL search's context and what produced it.
#[derive(Debug, Clone, Default)]
pub struct TemporalRetrieval {
    pub context: SearchContext,
    pub diagnostics: TemporalDiagnostics,
}

/// The two sizes one request runs with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Limits {
    /// What the result shows per lane.
    top_k: usize,
    /// What the hybrid fetch gathers per lane before the rerank cuts.
    candidate_top_k: usize,
}

/// Hybrid retrieval reranked by the question's time window, falling back to
/// plain hybrid — `SearchType::Temporal`. Port of Python's
/// `TemporalHybridRetriever`.
///
/// The hybrid candidate fetch (`candidate_top_k` per lane) runs concurrently
/// with an LLM call that extracts the window. The chunks attached to the
/// `Timestamp` nodes inside the window join the candidates, the candidates the
/// graph finds attached to the window move to the front — tightest date first,
/// similarity order among equally precise ones — and the same cut plain hybrid
/// uses keeps `top_k`. With nothing to rerank by, the result is the plain
/// hybrid result and [`TemporalDiagnostics::reason`] says why.
pub struct TemporalHybridRetriever {
    hybrid: HybridRetriever,
    llm: Arc<dyn Llm>,
    generation_options: Option<GenerationOptions>,
    top_k: Option<usize>,
    candidate_top_k: Option<usize>,
}

impl TemporalHybridRetriever {
    /// Build the retriever.
    ///
    /// `top_k` is what the result shows per lane (default 5) and
    /// `candidate_top_k` what the hybrid fetch gathers before the rerank
    /// (default `top_k * 4`); a request's `top_k` and
    /// `retriever_specific_config.candidate_top_k` override them. The prompt
    /// and generation settings are the answering step's, as for
    /// [`HybridRetriever`]; the window extraction uses `llm` with the
    /// vendored `QUERY_INTERVAL_PROMPT`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        vector_db: Arc<dyn VectorDB>,
        embedding_engine: Arc<dyn EmbeddingEngine>,
        graph_db: Arc<dyn GraphDBTrait>,
        llm: Arc<dyn Llm>,
        top_k: Option<usize>,
        candidate_top_k: Option<usize>,
        system_prompt: Option<String>,
        system_prompt_path: Option<String>,
        user_prompt_template: Option<String>,
        generation_options: Option<GenerationOptions>,
    ) -> Self {
        Self {
            hybrid: HybridRetriever::new(
                vector_db,
                embedding_engine,
                graph_db,
                Arc::clone(&llm),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                system_prompt,
                system_prompt_path,
                user_prompt_template,
                generation_options.clone(),
            ),
            llm,
            generation_options,
            top_k,
            candidate_top_k,
        }
    }

    /// Set the answering step's context budget (`search.hybrid`), as for
    /// HYBRID_COMPLETION.
    #[must_use]
    pub fn with_budget(mut self, budget: HybridBudget) -> Self {
        self.hybrid = self.hybrid.with_budget(budget);
        self
    }

    /// Resolve `top_k` / `candidate_top_k` for one request. Python resolves
    /// `top_k: null` to 5 before the arithmetic and rejects a non-positive
    /// `top_k` or a `candidate_top_k` below it.
    fn limits(&self, params: &SearchParams) -> Result<Limits, SearchError> {
        let top_k = params.top_k.or(self.top_k).unwrap_or(DEFAULT_TOP_K);
        let candidate_top_k = params
            .candidate_top_k
            .or(self.candidate_top_k)
            .unwrap_or_else(|| top_k.saturating_mul(CANDIDATE_MULTIPLIER));
        if top_k == 0 || candidate_top_k < top_k {
            return Err(SearchError::InvalidInput(format!(
                "TEMPORAL limits must be positive with top_k <= candidate_top_k \
                 (top_k = {top_k}, candidate_top_k = {candidate_top_k})"
            )));
        }
        Ok(Limits {
            top_k,
            candidate_top_k,
        })
    }

    /// The hybrid fetch's settings: chunks and entities at `candidate_top_k`,
    /// facts at `top_k` (Python's constructor), everything else from the
    /// request as for plain hybrid. The summary lane follows the chunk lane
    /// (Python's TEMPORAL passes no `text_summaries_top_k`).
    fn lanes(&self, params: &SearchParams, limits: Limits) -> HybridLanes {
        HybridLanes {
            chunks_top_k: limits.candidate_top_k,
            entities_top_k: limits.candidate_top_k,
            facts_top_k: limits.top_k,
            text_summaries_top_k: None,
            ..self.hybrid.shared_lanes(params)
        }
    }

    /// Cut to `top_k`. The fetch sized the no-entity fact budget for
    /// `candidate_top_k` entities; this view shows `top_k`, so the budget
    /// follows `top_k` too. Port of `_finalize`.
    fn finalize(candidates: &HybridCandidates, limits: Limits, lanes: &HybridLanes) -> HybridView {
        candidates.finalize(
            limits.top_k,
            limits.top_k,
            Some(limits.top_k.saturating_mul(lanes.max_edges_per_entity)),
        )
    }

    /// Run one TEMPORAL search and report what it did. Port of
    /// `get_retrieved_objects`; [`SearchRetriever::get_context`] returns the
    /// context half.
    pub async fn retrieve(
        &self,
        query: &str,
        params: &SearchParams,
    ) -> Result<TemporalRetrieval, SearchError> {
        if query.trim().is_empty() {
            return Err(SearchError::InvalidInput(
                "query must not be blank".to_string(),
            ));
        }
        let limits = self.limits(params)?;
        let graph = self.hybrid.graph_db();

        // Checked before anything else so the empty-graph path makes no LLM or
        // embedding call. Unlike plain hybrid (which reports the empty graph
        // as `NotFound`), Python's TEMPORAL answers it with an empty result.
        if graph.is_empty().await? {
            return Ok(TemporalRetrieval {
                context: vec![],
                diagnostics: TemporalDiagnostics {
                    reason: Some(TemporalFallbackReason::EmptyGraph),
                    ..TemporalDiagnostics::default()
                },
            });
        }

        let lanes = self.lanes(params, limits);
        let (candidates, window) = tokio::try_join!(
            self.hybrid
                .fetch_candidates(query, &lanes, params.dataset_id),
            extract_query_interval(self.llm.as_ref(), query, self.generation_options.clone()),
        )?;
        self.rerank(candidates, window, limits, &lanes).await
    }

    /// Everything after the concurrent fetch: widen the candidates from the
    /// window, read the anchors, prioritize and cut — or fall back to the
    /// baseline with a reason.
    async fn rerank(
        &self,
        candidates: HybridCandidates,
        window: ExtractedWindow,
        limits: Limits,
        lanes: &HybridLanes,
    ) -> Result<TemporalRetrieval, SearchError> {
        let graph = self.hybrid.graph_db();
        let baseline = Self::finalize(&candidates, limits, lanes);
        let mut diagnostics = TemporalDiagnostics {
            baseline: baseline.clone().into_context(),
            ..TemporalDiagnostics::default()
        };

        let (start, end) = match window {
            ExtractedWindow::Window { start, end } => (start, end),
            ExtractedWindow::NoWindow(reason) => {
                diagnostics.reason = Some(reason);
                warn!("{NO_WINDOW_WARNING}");
                return Ok(Self::fallback(baseline.into_context(), diagnostics));
            }
        };
        diagnostics.interval = (start, end);
        let (start_ms, end_ms) = (to_epoch_ms(start), to_epoch_ms(end));

        // The window's own timestamps widen the candidate pool before the
        // anchors are read: similarity alone misses most rows dated inside a
        // window.
        let in_window = graph.get_timestamps_in_range(start_ms, end_ms).await?;
        let candidates = if in_window.is_empty() {
            candidates
        } else {
            let (widened, added) = self
                .expand_from_window(&in_window, candidates, limits, lanes)
                .await?;
            diagnostics.expansion = added;
            widened
        };

        let chunk_ids: Vec<String> = candidates
            .chunks
            .iter()
            .filter_map(result_id)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let entity_ids: Vec<String> = candidates
            .entities
            .iter()
            .filter(|entity| !entity.id.is_empty())
            .map(|entity| entity.id.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        diagnostics.anchors = graph
            .get_temporal_anchors(&chunk_ids, &entity_ids, start_ms, end_ms)
            .await?;
        if diagnostics.anchors.is_empty() {
            // Nothing in the candidate set is dated inside the window. Tell the
            // two cases apart: a window the graph has no time in at all, or one
            // whose matches could not be retrieved.
            diagnostics.reason = Some(if in_window.is_empty() {
                TemporalFallbackReason::NoTemporalMatch
            } else {
                TemporalFallbackReason::NoCandidateOverlap
            });
            warn!("{FALLBACK_WARNING}");
            return Ok(Self::fallback(baseline.into_context(), diagnostics));
        }

        let rank = tightness_rank(
            &diagnostics.anchors.chunk_timestamps,
            &in_window,
            window_span_ms(start, end),
        );
        let reranked = Self::finalize(
            &candidates.prioritize(
                &diagnostics.anchors.chunk_ids,
                &diagnostics.anchors.entity_ids,
                Some(&rank),
            ),
            limits,
            lanes,
        );
        if reranked.chunk_ids() == baseline.chunk_ids()
            && reranked.entity_ids() == baseline.entity_ids()
        {
            // Informational: the window matched, but nothing in the candidate
            // set moved. The result is the baseline either way.
            diagnostics.reason = Some(TemporalFallbackReason::NoCandidateOverlap);
        }
        debug!(
            reason = diagnostics.reason.map(TemporalFallbackReason::as_str),
            anchored_chunks = diagnostics.anchors.chunk_ids.len(),
            expansion = diagnostics.expansion.len(),
            "TEMPORAL rerank"
        );
        Ok(TemporalRetrieval {
            context: reranked.into_context(),
            diagnostics,
        })
    }

    /// The baseline as the result. `baseline` is the same context
    /// `diagnostics.baseline` already holds, moved in rather than cloned out
    /// of the diagnostics a second time.
    fn fallback(baseline: SearchContext, diagnostics: TemporalDiagnostics) -> TemporalRetrieval {
        debug!(
            reason = diagnostics.reason.map(TemporalFallbackReason::as_str),
            "TEMPORAL answered with the plain hybrid result"
        );
        TemporalRetrieval {
            context: baseline,
            diagnostics,
        }
    }

    /// Add the chunks attached to the in-window timestamps. Port of
    /// `_expand_from_window`.
    ///
    /// At most `candidate_top_k` attached chunks are added, tightest timestamp
    /// first, after hybrid's candidates so the fetch's ranking stays ahead
    /// within the anchored set. Entities are left to the entity lane (see
    /// [`expansion`]). Returns the widened candidates and the added ids.
    async fn expand_from_window(
        &self,
        in_window: &[WindowTimestamp],
        candidates: HybridCandidates,
        limits: Limits,
        lanes: &HybridLanes,
    ) -> Result<(HybridCandidates, HashSet<String>), SearchError> {
        let known: HashSet<String> = candidates.chunks.iter().filter_map(result_id).collect();
        // Tightest timestamps first: a bare year that also overlaps the window
        // must not spend the budget before the day the question names.
        let timestamps = tightest_first(in_window.to_vec());
        let seeds: Vec<String> = timestamps.iter().map(|ts| ts.id.clone()).collect();
        let (_nodes, edges) = self.hybrid.graph_db().get_neighborhood(&seeds, 1).await?;
        let attached: Vec<String> = attached_node_ids(&timestamps, &edges)
            .into_iter()
            .filter(|id| !known.contains(id))
            .take(limits.candidate_top_k)
            .collect();
        if attached.is_empty() {
            return Ok((candidates, HashSet::new()));
        }
        let chunks = retrieve_in_collections(
            self.hybrid.vector_db(),
            CHUNK_COLLECTIONS,
            &attached,
            lanes.node_name.as_deref(),
            &lanes.node_name_filter_operator,
        )
        .await?;
        let added = chunks.iter().filter_map(result_id).collect();
        Ok((candidates.extend(chunks), added))
    }
}

#[async_trait]
impl SearchRetriever for TemporalHybridRetriever {
    fn search_type(&self) -> SearchType {
        SearchType::Temporal
    }

    /// The items are plain hybrid's, carrying their own dataset membership.
    fn dataset_scoping(&self) -> DatasetScoping {
        DatasetScoping::Membership
    }

    #[cfg(test)]
    fn applied_tuning(&self) -> crate::retrievers::AppliedTuning {
        self.hybrid.applied_tuning()
    }

    async fn get_context(
        &self,
        query: &str,
        params: &SearchParams,
    ) -> Result<SearchContext, SearchError> {
        Ok(self.retrieve(query, params).await?.context)
    }

    /// Plain hybrid's prompt over the reranked context (Python inherits
    /// `get_context_from_objects` / `get_completion_from_context`).
    async fn completion_messages(
        &self,
        query: &str,
        context: &SearchContext,
        session: &SessionContext,
        params: &SearchParams,
    ) -> Result<Option<Vec<Message>>, SearchError> {
        self.hybrid
            .completion_messages(query, context, session, params)
            .await
    }

    async fn get_completion(
        &self,
        query: &str,
        context: Option<SearchContext>,
        session: &SessionContext,
        params: &SearchParams,
    ) -> Result<SearchOutput, SearchError> {
        let context = match context {
            Some(context) => context,
            None => self.get_context(query, params).await?,
        };
        // Hybrid's completion over a provided context fetches nothing; an
        // empty context skips the LLM (Python's
        // `skip_completion_on_empty_context`).
        self.hybrid
            .get_completion(query, Some(context), session, params)
            .await
    }

    /// TEMPORAL answers one query at a time (Python raises
    /// `NotImplementedError` for `query_batch`).
    async fn get_context_batch(
        &self,
        _queries: &[String],
        _params: &SearchParams,
    ) -> Result<Vec<SearchContext>, SearchError> {
        Err(SearchError::InvalidInput(
            "TEMPORAL does not support query_batch.".to_string(),
        ))
    }

    async fn get_completion_batch(
        &self,
        _queries: &[String],
        _contexts: Option<Vec<SearchContext>>,
        _session: &SessionContext,
        _params: &SearchParams,
    ) -> Result<Vec<SearchOutput>, SearchError> {
        Err(SearchError::InvalidInput(
            "TEMPORAL does not support query_batch.".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests;
