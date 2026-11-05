use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::types::{EvidenceReference, SearchType};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchItem {
    pub id: Option<Uuid>,
    pub score: Option<f32>,
    pub payload: Value,
}

pub type SearchContext = Vec<SearchItem>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    pub node_set: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchGraphNode {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchGraphEdge {
    pub source: String,
    pub target: String,
    pub relationship: String,
    pub weight: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchGraph {
    pub nodes: Vec<SearchGraphNode>,
    pub edges: Vec<SearchGraphEdge>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum SearchOutput {
    Items(Vec<SearchItem>),
    Text(String),
    Texts(Vec<String>),
    GraphQueryRows(Vec<Vec<Value>>),
    Rules(Vec<Rule>),
    Ack {
        message: String,
    },
    /// Structured JSON output produced when a `response_schema` is provided.
    /// The value conforms to the JSON schema supplied in `SearchParams::response_schema`.
    Structured(Value),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponse {
    pub search_type: SearchType,
    pub result: SearchOutput,
    pub context: Option<HashMap<String, SearchContext>>,
    pub graphs: Option<HashMap<String, SearchGraph>>,
    pub diagnostics: Option<HashMap<String, Value>>,
    pub datasets: Option<Vec<Uuid>>,
    pub only_context: bool,
    pub use_combined_context: bool,
    pub verbose: bool,
    /// The user message an `only_context` search stands in for: the question
    /// and the retrieved context rendered through the retriever's template.
    /// Also the `result` (as `Text`) when set. `None` unless `only_context`
    /// is on, the retriever sends one templated prompt, and retrieval found
    /// something — Python's `SearchResultPayload.user_prompt`.
    #[serde(default)]
    pub user_prompt: Option<String>,
    /// The system message that goes with [`user_prompt`](Self::user_prompt);
    /// set and unset with it. Carries the session history when a session is
    /// active, since that is where the Rust completion sends it.
    #[serde(default)]
    pub system_prompt: Option<String>,
    /// For an `only_context` search over several datasets without
    /// `use_combined_context`: the prompt built from each dataset's own
    /// context, keyed by dataset id like [`context`](Self::context). Python
    /// builds one prompt per dataset in its fan-out. A dataset whose context
    /// is empty, or whose retriever sends no single prompt, has no entry.
    /// [`user_prompt`](Self::user_prompt) stays the first dataset's — the one
    /// the completion answers from.
    #[serde(default)]
    pub prompts: Option<HashMap<String, SearchPrompt>>,
    /// Structured references to the artifacts (chunks, graph nodes and edges)
    /// placed into the completion's context, in rank order. Empty unless the
    /// request set `include_references` and the retriever reports evidence
    /// (RAG and the graph completion family) — Python's
    /// `SearchResultPayload.evidence`. Serializes snake_case, as Python's
    /// `model_dump` does.
    #[serde(default)]
    pub evidence: Vec<EvidenceReference>,
}

/// The two messages a completion sends, as an `only_context` search returns
/// them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchPrompt {
    pub user_prompt: String,
    pub system_prompt: String,
}

impl SearchResponse {
    pub fn from_output(search_type: SearchType, output: SearchOutput) -> Self {
        Self {
            search_type,
            result: output,
            context: None,
            graphs: None,
            diagnostics: None,
            datasets: None,
            only_context: false,
            use_combined_context: false,
            verbose: false,
            user_prompt: None,
            system_prompt: None,
            prompts: None,
            evidence: Vec::new(),
        }
    }
}
