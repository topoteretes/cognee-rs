use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::types::SearchType;

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
    Ack { message: String },
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
        }
    }
}
