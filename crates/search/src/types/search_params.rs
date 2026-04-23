use serde::{Deserialize, Serialize};

use crate::types::SearchRequest;

/// Per-request retriever behavior overrides.
///
/// All fields are optional. When `None`, the retriever falls back to its
/// constructor-time defaults. This lets callers override only the params
/// they care about on a per-request basis.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchParams {
    /// Max number of results to return from vector search.
    pub top_k: Option<usize>,

    /// Override the LLM system prompt text directly.
    pub system_prompt: Option<String>,

    /// Override the LLM system prompt by file path.
    pub system_prompt_path: Option<String>,

    /// Number of candidates for wide graph search (before re-ranking).
    pub wide_search_top_k: Option<usize>,

    /// Distance penalty applied during triplet scoring.
    pub triplet_distance_penalty: Option<f32>,

    /// Filter graph to nodes of this type.
    pub node_type: Option<String>,

    /// Filter graph to nodes with these names.
    pub node_name: Option<Vec<String>>,

    /// "OR" (default) or "AND" for multi-name filtering.
    pub node_name_filter_operator: Option<String>,

    /// Influence weight for feedback-based re-ranking.
    pub feedback_influence: Option<f32>,

    /// Maximum CoT iterations (GraphCompletionCot).
    pub max_iter: Option<usize>,

    /// Number of context extension rounds (GraphCompletionContextExtension).
    pub context_extension_rounds: Option<usize>,

    /// Optional JSON schema for structured LLM output.
    /// When `Some`, completion-generating retrievers return `SearchOutput::Structured`
    /// instead of `SearchOutput::Text`.
    pub response_schema: Option<serde_json::Value>,

    /// Number of hops from query result nodes to include in the graph context.
    pub neighborhood_depth: Option<usize>,

    /// Number of initial seed nodes for neighborhood expansion.
    pub neighborhood_seed_top_k: Option<usize>,
}

impl SearchParams {
    pub fn top_k_or(&self, default: usize) -> usize {
        self.top_k.unwrap_or(default)
    }

    pub fn wide_search_top_k_or(&self, default: usize) -> usize {
        self.wide_search_top_k.unwrap_or(default)
    }

    pub fn triplet_distance_penalty_or(&self, default: f32) -> f32 {
        self.triplet_distance_penalty.unwrap_or(default)
    }

    pub fn feedback_influence_or(&self, default: f32) -> f32 {
        self.feedback_influence.unwrap_or(default)
    }
}

impl From<&SearchRequest> for SearchParams {
    fn from(req: &SearchRequest) -> Self {
        Self {
            top_k: req.top_k,
            system_prompt: req.system_prompt.clone(),
            system_prompt_path: req.system_prompt_path.clone(),
            wide_search_top_k: req.wide_search_top_k,
            triplet_distance_penalty: req.triplet_distance_penalty,
            node_type: req.node_type.clone(),
            node_name: req.node_name.clone(),
            node_name_filter_operator: req.node_name_filter_operator.clone(),
            feedback_influence: req.feedback_influence,
            max_iter: req
                .retriever_specific_config
                .as_ref()
                .and_then(|c| c.get("max_iter"))
                .and_then(|v| v.as_u64())
                .map(|v| v as usize),
            context_extension_rounds: req
                .retriever_specific_config
                .as_ref()
                .and_then(|c| c.get("context_extension_rounds"))
                .and_then(|v| v.as_u64())
                .map(|v| v as usize),
            response_schema: req.response_schema.clone(),
            neighborhood_depth: req.neighborhood_depth,
            neighborhood_seed_top_k: req.neighborhood_seed_top_k,
        }
    }
}
