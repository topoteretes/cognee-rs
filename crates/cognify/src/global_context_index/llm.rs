//! The opt-in LLM method: bucket and root summaries above a dataset's
//! `TextSummary` nodes (Python's `load.py`, `update.py`, `build.py`,
//! `summarize.py` and `bucketing/vector/placement.py`).
//!
//! Level 0 places each new text summary into the existing bucket whose
//! children are nearest (mean cosine distance at most the threshold, bucket
//! not full), or opens a new bucket. When a level holds more buckets than
//! `max_bucket_size`, the next level up clusters them greedily. The root is
//! regenerated when anything below it moved, or when there is none. Every
//! changed bucket gets a fresh LLM summary.
//!
//! Only Python's default `bucketing_strategy="vector"` is ported; the
//! experimental graph bucketing is not.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use cognee_graph::{EdgeData, GraphNode};
use cognee_llm::{Llm, LlmExt};
use cognee_models::GlobalContextSummary;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::info;

use super::persist::IndexStore;
use super::{
    SUMMARIZED_IN, SUMMARY_GRAPH_NODE_TYPES, TEXT_SUMMARY_COLLECTION, create_bucket_id,
    create_root_summary_id,
};
use crate::error::CognifyError;

/// The bucket prompt, copied verbatim from Python's
/// `global_context_bucket_summary.txt`.
const BUCKET_SUMMARY_PROMPT: &str = include_str!("prompts/global_context_bucket_summary.txt");
/// The root prompt, copied verbatim from Python's
/// `global_context_root_summary.txt`.
const ROOT_SUMMARY_PROMPT: &str = include_str!("prompts/global_context_root_summary.txt");

/// The LLM's answer for one summary (Python's `GlobalContextSummaryContent`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GlobalContextSummaryContent {
    pub summary: String,
}

/// Options of the LLM method, with Python's defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct LlmIndexOptions {
    /// Most children a bucket takes, and most buckets the top level holds
    /// under the root. At least 2.
    pub max_bucket_size: usize,
    /// Largest mean cosine distance at which an item joins an existing
    /// bucket. Finite and non-negative.
    pub placement_distance_threshold: f64,
    /// Delete the index and place every text summary again.
    pub rebuild: bool,
}

impl Default for LlmIndexOptions {
    fn default() -> Self {
        Self {
            max_bucket_size: 20,
            placement_distance_threshold: 0.5,
            rebuild: false,
        }
    }
}

impl LlmIndexOptions {
    /// Python's `validate_global_context_index_config`, for the options Rust has.
    pub fn validate(&self) -> Result<(), CognifyError> {
        if self.max_bucket_size < 2 {
            return Err(CognifyError::ConfigError(
                "max_bucket_size must be at least 2.".to_string(),
            ));
        }
        if !self.placement_distance_threshold.is_finite() || self.placement_distance_threshold < 0.0
        {
            return Err(CognifyError::ConfigError(
                "placement_distance_threshold must be a finite non-negative number.".to_string(),
            ));
        }
        Ok(())
    }
}

/// A text summary, bucket or root as the index sees it (Python's `SummaryNode`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SummaryNode {
    pub id: String,
    pub text: String,
    /// `TextSummary` or `GlobalContextSummary`.
    pub node_type: String,
    pub level: Option<i64>,
    pub is_root: bool,
    pub dataset_id: Option<String>,
    /// The parent this node is placed in.
    pub global_context_bucket_id: Option<String>,
    pub child_ids: BTreeSet<String>,
    pub graph_bucket_entity_ids: Option<BTreeSet<String>>,
}

impl SummaryNode {
    fn is_text_summary(&self) -> bool {
        self.node_type == "TextSummary"
    }
}

/// The index as the graph holds it: every node by id, plus which are text
/// summaries, buckets and the root (Python's `GlobalContextIndexUpdateData`).
#[derive(Debug, Clone, Default)]
pub struct IndexInput {
    pub nodes: HashMap<String, SummaryNode>,
    pub text_summaries: Vec<String>,
    pub buckets: Vec<String>,
    pub root: Option<String>,
}

/// Python's `int(attributes.get("level"))`, `None` when it does not parse.
fn parse_level(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f.trunc() as i64)),
        Value::String(s) => s.trim().parse().ok(),
        Value::Bool(b) => Some(i64::from(*b)),
        _ => None,
    }
}

/// Python's `is_root_global_context_summary`: a bool, `"true"`, or truthiness.
fn parse_is_root(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s.eq_ignore_ascii_case("true"),
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

fn text_of(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// Whether `parent` may hold `child` (Python's `is_valid_context_parent`):
/// a text summary sits in a non-root bucket; a bucket sits in the bucket one
/// level up, or in the root.
fn is_valid_context_parent(child: &SummaryNode, parent: &SummaryNode) -> bool {
    if child.id == parent.id || child.is_root || parent.node_type != GlobalContextSummary::TYPE {
        return false;
    }
    if child.is_text_summary() {
        return !parent.is_root;
    }
    if child.node_type != GlobalContextSummary::TYPE {
        return false;
    }
    if parent.is_root {
        return true;
    }
    matches!((child.level, parent.level), (Some(c), Some(p)) if p == c + 1)
}

/// Read the index from graph rows (Python's `extract_context_index_input`).
///
/// A node's parent comes from its first valid `summarized_in` edge, never
/// from a stored marker; another dataset's index nodes are skipped.
pub fn extract_context_index_input(
    nodes: &[GraphNode],
    edges: &[EdgeData],
    dataset_id: &str,
) -> IndexInput {
    let mut input = IndexInput::default();
    for (node_id, attributes) in nodes {
        match attributes.get("type").and_then(Value::as_str) {
            Some("TextSummary") => {
                input.text_summaries.push(node_id.clone());
                input.nodes.insert(
                    node_id.clone(),
                    SummaryNode {
                        id: node_id.clone(),
                        text: text_of(attributes.get("text")),
                        node_type: "TextSummary".to_string(),
                        ..Default::default()
                    },
                );
            }
            Some(GlobalContextSummary::TYPE) => {
                let node_dataset = attributes.get("dataset_id").filter(|v| !v.is_null());
                if node_dataset.is_some_and(|v| text_of(Some(v)) != dataset_id) {
                    continue;
                }
                let level = parse_level(attributes.get("level"));
                let graph_bucket_entity_ids = match attributes.get("graph_bucket_entity_ids") {
                    Some(Value::Array(ids)) if level == Some(0) => {
                        Some(ids.iter().map(|id| text_of(Some(id))).collect())
                    }
                    _ => None,
                };
                let node = SummaryNode {
                    id: node_id.clone(),
                    text: text_of(attributes.get("text")),
                    node_type: GlobalContextSummary::TYPE.to_string(),
                    level,
                    is_root: parse_is_root(attributes.get("is_root")),
                    dataset_id: Some(
                        node_dataset.map_or_else(|| dataset_id.to_string(), |v| text_of(Some(v))),
                    ),
                    graph_bucket_entity_ids,
                    ..Default::default()
                };
                if node.is_root {
                    input.root = Some(node_id.clone());
                } else {
                    input.buckets.push(node_id.clone());
                }
                input.nodes.insert(node_id.clone(), node);
            }
            _ => {}
        }
    }
    // A root that lost to a later one is not part of the index.
    let known: HashSet<&String> = input
        .text_summaries
        .iter()
        .chain(&input.buckets)
        .chain(&input.root)
        .collect();
    input.nodes.retain(|id, _| known.contains(id));

    for (source, target, relationship, attributes) in edges {
        let name = attributes
            .get("relationship_name")
            .or_else(|| attributes.get("relationship_type"))
            .and_then(Value::as_str)
            .unwrap_or(relationship);
        if name != SUMMARIZED_IN {
            continue;
        }
        let (Some(child), Some(parent)) = (input.nodes.get(source), input.nodes.get(target)) else {
            continue;
        };
        if child.global_context_bucket_id.is_none() && is_valid_context_parent(child, parent) {
            let parent_id = parent.id.clone();
            if let Some(child) = input.nodes.get_mut(source) {
                child.global_context_bucket_id = Some(parent_id);
            }
        }
    }

    let children: Vec<String> = input
        .text_summaries
        .iter()
        .chain(&input.buckets)
        .cloned()
        .collect();
    let parents: Vec<String> = input.buckets.iter().chain(&input.root).cloned().collect();
    rebuild_child_ids(&mut input.nodes, &children, &parents);
    input
}

/// Python's `rebuild_child_ids_from_parent_pointers`.
fn rebuild_child_ids(
    nodes: &mut HashMap<String, SummaryNode>,
    children: &[String],
    parents: &[String],
) {
    let parent_set: HashSet<&String> = parents.iter().collect();
    for parent in parents {
        if let Some(node) = nodes.get_mut(parent) {
            node.child_ids.clear();
        }
    }
    for child in children {
        let Some(parent) = nodes
            .get(child)
            .and_then(|n| n.global_context_bucket_id.clone())
        else {
            continue;
        };
        if parent_set.contains(&parent)
            && let Some(node) = nodes.get_mut(&parent)
        {
            node.child_ids.insert(child.clone());
        }
    }
}

/// Build or extend the LLM index (Python's `update_global_context_index`).
/// Returns the nodes it wrote.
pub async fn update_global_context_index(
    store: &IndexStore<'_>,
    llm: &Arc<dyn Llm>,
    options: &LlmIndexOptions,
) -> Result<Vec<GlobalContextSummary>, CognifyError> {
    options.validate()?;
    let filters = HashMap::from([(
        std::borrow::Cow::Borrowed("type"),
        SUMMARY_GRAPH_NODE_TYPES
            .iter()
            .map(|t| Value::from(*t))
            .collect(),
    )]);
    let (nodes, edges) = store.graph_db.get_filtered_graph_data(&filters).await?;
    let mut input = extract_context_index_input(&nodes, &edges, &store.dataset_key());
    let text_summaries: Vec<String> = input
        .text_summaries
        .iter()
        .filter(|id| !input.nodes[*id].text.is_empty())
        .cloned()
        .collect();

    if options.rebuild {
        let index_ids: Vec<String> = input.buckets.iter().chain(&input.root).cloned().collect();
        store.delete_nodes(&index_ids).await?;
        for id in &index_ids {
            input.nodes.remove(id);
        }
        input.buckets.clear();
        input.root = None;
        for id in &text_summaries {
            if let Some(node) = input.nodes.get_mut(id) {
                node.global_context_bucket_id = None;
            }
        }
    } else if input.buckets.iter().any(|id| {
        let bucket = &input.nodes[id];
        bucket.level == Some(0) && bucket.graph_bucket_entity_ids.is_some()
    }) {
        return Err(CognifyError::ConfigError(
            "Vector incremental placement cannot extend graph-built level-0 buckets. \
             Use rebuild=True to rebuild the index with vector bucketing."
                .to_string(),
        ));
    }

    if text_summaries.is_empty() {
        return Ok(Vec::new());
    }
    let new_summaries: Vec<String> = if options.rebuild {
        text_summaries.clone()
    } else {
        text_summaries
            .iter()
            .filter(|id| input.nodes[*id].global_context_bucket_id.is_none())
            .cloned()
            .collect()
    };
    if new_summaries.is_empty() {
        info!("No new TextSummary nodes to place in global context index.");
        return Ok(Vec::new());
    }

    let mut build = IndexBuild {
        store,
        llm,
        options,
        dataset_id: store.dataset_key(),
        nodes: input.nodes,
    };
    let (datapoints, assignments) = build
        .build_context_index(new_summaries, text_summaries, &input.buckets, input.root)
        .await?;
    store.persist_edges(&[(SUMMARIZED_IN, assignments)]).await?;
    Ok(datapoints)
}

/// A child id → parent id placement.
type Assignment = (String, String);

/// One build over an in-memory copy of the index.
struct IndexBuild<'s, 'a> {
    store: &'s IndexStore<'a>,
    llm: &'s Arc<dyn Llm>,
    options: &'s LlmIndexOptions,
    dataset_id: String,
    nodes: HashMap<String, SummaryNode>,
}

/// An ordered set of ids (Python's insertion-ordered dict keys).
#[derive(Default)]
struct OrderedIds(Vec<String>);

impl OrderedIds {
    fn insert(&mut self, id: &str) {
        if !self.0.iter().any(|existing| existing == id) {
            self.0.push(id.to_string());
        }
    }
}

/// A search hit: the point id and its cosine distance.
type Neighbor = (String, f64);

impl IndexBuild<'_, '_> {
    fn node(&self, id: &str) -> Option<&SummaryNode> {
        self.nodes.get(id)
    }

    fn child_count(&self, id: &str) -> usize {
        self.node(id).map_or(0, |n| n.child_ids.len())
    }

    /// Bottom-up build from the new text summaries (Python's
    /// `build_context_index`).
    async fn build_context_index(
        &mut self,
        new_text_summaries: Vec<String>,
        text_summaries_all: Vec<String>,
        existing_buckets: &[String],
        existing_root: Option<String>,
    ) -> Result<(Vec<GlobalContextSummary>, Vec<Assignment>), CognifyError> {
        let mut by_level: HashMap<i64, Vec<String>> = HashMap::new();
        for id in existing_buckets {
            if let Some(level) = self.node(id).and_then(|n| n.level) {
                by_level.entry(level).or_default().push(id.clone());
            }
        }

        let mut items_changed = new_text_summaries;
        let mut items_all = text_summaries_all;
        let mut level = 0i64;
        let mut all_assignments = Vec::new();
        let mut all_datapoints = Vec::new();
        let top_level_buckets = loop {
            let existing = by_level.get(&level).cloned().unwrap_or_default();
            let (to_persist, assignments) = if !existing.is_empty() || level == 0 {
                self.assign_items_to_buckets(&items_changed, &existing, level)
                    .await?
            } else {
                self.create_buckets_for_level(&items_all, level).await?
            };
            all_assignments.extend(assignments);

            let children: HashSet<String> = items_all.iter().cloned().collect();
            let datapoints = self.summarize_buckets(&to_persist, &children).await?;
            self.store.persist_summaries(&datapoints).await?;
            all_datapoints.extend(datapoints);

            // merge_existing_and_new_buckets
            let mut at_level = existing.clone();
            at_level.extend(
                to_persist
                    .iter()
                    .filter(|id| !existing.contains(id))
                    .cloned(),
            );
            if at_level.len() <= self.options.max_bucket_size {
                break at_level;
            }
            items_changed = to_persist;
            items_all = at_level;
            level += 1;
        };

        // should_update_root
        if !top_level_buckets.is_empty() && (!all_assignments.is_empty() || existing_root.is_none())
        {
            let children: Vec<&SummaryNode> = top_level_buckets
                .iter()
                .filter_map(|id| self.node(id))
                .collect();
            let text = self.summarize(&children, ROOT_SUMMARY_PROMPT).await?;
            let root = GlobalContextSummary::new(
                create_root_summary_id(&self.dataset_id),
                text,
                self.dataset_id.clone(),
                level + 1,
                true,
                None,
            );
            self.store
                .persist_summaries(std::slice::from_ref(&root))
                .await?;
            let root_id = root.base.id.to_string();
            for id in &top_level_buckets {
                if let Some(bucket) = self.nodes.get_mut(id)
                    && bucket.global_context_bucket_id.as_deref() != Some(root_id.as_str())
                {
                    bucket.global_context_bucket_id = Some(root_id.clone());
                    all_assignments.push((id.clone(), root_id.clone()));
                }
            }
            all_datapoints.push(root);
        }
        Ok((all_datapoints, all_assignments))
    }

    /// The `{prompt}` summary of `children` (Python's `summarize_with_prompt`).
    async fn summarize(
        &self,
        children: &[&SummaryNode],
        prompt: &str,
    ) -> Result<String, CognifyError> {
        let input = children
            .iter()
            .enumerate()
            .map(|(index, child)| format!("Input {}:\n{}", index + 1, child.text))
            .collect::<Vec<_>>()
            .join("\n\n");
        let content: GlobalContextSummaryContent = self
            .llm
            .create_structured_output(&input, prompt, None)
            .await
            .map_err(CognifyError::from_llm)?;
        Ok(content.summary)
    }

    /// Fresh summaries for `buckets`, over their children that are in
    /// `children` in id order (Python's `generate_bucket_summary_datapoints`).
    async fn summarize_buckets(
        &mut self,
        buckets: &[String],
        children: &HashSet<String>,
    ) -> Result<Vec<GlobalContextSummary>, CognifyError> {
        let this = &*self;
        let texts = futures::future::try_join_all(buckets.iter().map(|bucket_id| {
            let bucket_children: Vec<&SummaryNode> = this
                .node(bucket_id)
                .map(|bucket| {
                    bucket
                        .child_ids
                        .iter()
                        .filter(|id| children.contains(*id))
                        .filter_map(|id| this.node(id))
                        .collect()
                })
                .unwrap_or_default();
            async move {
                this.summarize(&bucket_children, BUCKET_SUMMARY_PROMPT)
                    .await
            }
        }))
        .await?;

        let mut datapoints = Vec::with_capacity(buckets.len());
        for (bucket_id, text) in buckets.iter().zip(texts) {
            let Some(bucket) = self.nodes.get_mut(bucket_id) else {
                continue;
            };
            bucket.text = text;
            let id = uuid::Uuid::parse_str(bucket_id).map_err(|e| {
                CognifyError::ConfigError(format!("bucket id {bucket_id} is not a UUID: {e}"))
            })?;
            let mut datapoint = GlobalContextSummary::new(
                id,
                bucket.text.clone(),
                self.dataset_id.clone(),
                bucket.level.unwrap_or(0),
                false,
                None,
            );
            datapoint.graph_bucket_entity_ids = bucket
                .graph_bucket_entity_ids
                .as_ref()
                .filter(|_| bucket.level == Some(0))
                .map(|ids| ids.iter().cloned().collect());
            datapoints.push(datapoint);
        }
        Ok(datapoints)
    }

    /// The collection a level places its items by: text summaries for
    /// level 0, the index's own buckets above it.
    fn source_collection(level: i64) -> (&'static str, &'static str) {
        if level == 0 {
            TEXT_SUMMARY_COLLECTION
                .split_once('_')
                .unwrap_or(("TextSummary", "text"))
        } else {
            (GlobalContextSummary::TYPE, "text")
        }
    }

    /// Each item's nearest points in the level's collection, as cosine
    /// distances (Python's `prefetch_nearest_neighbors`). A missing
    /// collection has no neighbours.
    async fn prefetch_nearest_neighbors(
        &self,
        items: &[String],
        placed_child_count: usize,
        level: i64,
    ) -> Result<HashMap<String, Vec<Neighbor>>, CognifyError> {
        if items.is_empty() {
            return Ok(HashMap::new());
        }
        let (data_type, field) = Self::source_collection(level);
        let vector_db = self.store.vector_db;
        if !vector_db
            .has_collection(data_type, field)
            .await
            .map_err(|e| CognifyError::VectorDBError(e.to_string()))?
        {
            return Ok(HashMap::new());
        }
        let limit = 50.max((placed_child_count + items.len()) * 2);
        let texts: Vec<&str> = items
            .iter()
            .map(|id| self.node(id).map_or("", |n| n.text.as_str()))
            .collect();
        let mut vectors = Vec::with_capacity(texts.len());
        for batch in texts.chunks(self.store.embedding_engine.batch_size().max(1)) {
            vectors.extend(
                self.store
                    .embedding_engine
                    .embed(batch)
                    .await
                    .map_err(|e| CognifyError::EmbeddingError(e.to_string()))?,
            );
        }
        let mut nearest = HashMap::with_capacity(items.len());
        for (id, vector) in items.iter().zip(vectors) {
            let results = vector_db
                .search_similar(data_type, field, &vector, limit)
                .await
                .map_err(|e| CognifyError::VectorDBError(e.to_string()))?;
            nearest.insert(
                id.clone(),
                results
                    .into_iter()
                    .map(|r| (r.id.to_string(), 1.0 - f64::from(r.score)))
                    .collect(),
            );
        }
        Ok(nearest)
    }

    /// The nearest bucket that has room and whose children are all within
    /// reach and close enough on average (Python's
    /// `choose_existing_bucket_for_item`).
    fn choose_existing_bucket(
        &self,
        bucket_ids: &[String],
        nearest: &[Neighbor],
    ) -> Option<String> {
        let parent_of: HashMap<&String, &String> = bucket_ids
            .iter()
            .filter_map(|bucket_id| self.node(bucket_id).map(|bucket| (bucket_id, bucket)))
            .flat_map(|(bucket_id, bucket)| bucket.child_ids.iter().map(move |c| (c, bucket_id)))
            .collect();
        if parent_of.is_empty() {
            return None;
        }
        let mut distances: Vec<(&String, Vec<f64>)> = Vec::new();
        for (id, distance) in nearest {
            let Some(bucket_id) = parent_of.get(id) else {
                continue;
            };
            match distances.iter_mut().find(|(b, _)| b == bucket_id) {
                Some((_, list)) => list.push(*distance),
                None => distances.push((bucket_id, vec![*distance])),
            }
        }

        let mut best: Option<(&String, f64)> = None;
        for (bucket_id, list) in distances {
            let size = self.child_count(bucket_id);
            // A child outside the prefetched window cannot be scored fairly.
            if list.len() != size || size >= self.options.max_bucket_size {
                continue;
            }
            let mean = list.iter().sum::<f64>() / list.len() as f64;
            if mean > self.options.placement_distance_threshold {
                continue;
            }
            let better = match best {
                None => true,
                Some((best_id, best_mean)) => {
                    mean < best_mean || (mean == best_mean && size < self.child_count(best_id))
                }
            };
            if better {
                best = Some((bucket_id, mean));
            }
        }
        best.map(|(id, _)| id.clone())
    }

    fn new_bucket(&mut self, seed: &str, level: i64) -> String {
        let id = create_bucket_id(&self.dataset_id, level, &[seed]).to_string();
        self.nodes.insert(
            id.clone(),
            SummaryNode {
                id: id.clone(),
                node_type: GlobalContextSummary::TYPE.to_string(),
                level: Some(level),
                dataset_id: Some(self.dataset_id.clone()),
                child_ids: BTreeSet::from([seed.to_string()]),
                ..Default::default()
            },
        );
        self.set_parent(seed, &id);
        id
    }

    fn set_parent(&mut self, child: &str, parent: &str) {
        if let Some(bucket) = self.nodes.get_mut(parent) {
            bucket.child_ids.insert(child.to_string());
        }
        if let Some(node) = self.nodes.get_mut(child) {
            node.global_context_bucket_id = Some(parent.to_string());
        }
    }

    /// Place items into this level's buckets, extending one or opening a new
    /// one; an item already placed flags its parent for a fresh summary
    /// (Python's `assign_items_to_buckets`).
    async fn assign_items_to_buckets(
        &mut self,
        items: &[String],
        existing_buckets: &[String],
        level: i64,
    ) -> Result<(Vec<String>, Vec<Assignment>), CognifyError> {
        let mut bucket_ids = existing_buckets.to_vec();
        let placed: usize = existing_buckets.iter().map(|id| self.child_count(id)).sum();
        let (with_parent, needs_placement): (Vec<String>, Vec<String>) =
            items.iter().cloned().partition(|id| {
                self.node(id)
                    .is_some_and(|n| n.global_context_bucket_id.is_some())
            });
        let nearest = self
            .prefetch_nearest_neighbors(&needs_placement, placed, level)
            .await?;

        let mut to_persist = OrderedIds::default();
        let mut assignments = Vec::new();
        for item in &with_parent {
            if let Some(parent) = self
                .node(item)
                .and_then(|n| n.global_context_bucket_id.as_ref())
                .filter(|p| bucket_ids.contains(p))
            {
                to_persist.insert(parent);
            }
        }
        for item in &needs_placement {
            let neighbors = nearest.get(item).map(Vec::as_slice).unwrap_or_default();
            let bucket = match self.choose_existing_bucket(&bucket_ids, neighbors) {
                Some(bucket) => {
                    self.set_parent(item, &bucket);
                    bucket
                }
                None => {
                    let bucket = self.new_bucket(item, level);
                    bucket_ids.push(bucket.clone());
                    bucket
                }
            };
            assignments.push((item.clone(), bucket.clone()));
            to_persist.insert(&bucket);
        }
        Ok((to_persist.0, assignments))
    }

    /// Greedy nearest-neighbour clustering for a new level: seed a bucket
    /// with the next unclustered item, attach its nearest unclustered
    /// neighbours, then top up in input order (Python's
    /// `create_buckets_for_level`).
    async fn create_buckets_for_level(
        &mut self,
        items: &[String],
        level: i64,
    ) -> Result<(Vec<String>, Vec<Assignment>), CognifyError> {
        if items.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        let nearest = self
            .prefetch_nearest_neighbors(items, items.len(), level)
            .await?;
        let item_set: HashSet<&String> = items.iter().collect();
        let mut unclustered: HashSet<String> = items.iter().cloned().collect();
        let mut to_persist = OrderedIds::default();
        let mut assignments = Vec::new();
        let max = self.options.max_bucket_size;

        for seed in items {
            if !unclustered.remove(seed) {
                continue;
            }
            let bucket = self.new_bucket(seed, level);
            assignments.push((seed.clone(), bucket.clone()));
            let neighbors = nearest.get(seed).map(Vec::as_slice).unwrap_or_default();
            let candidates = neighbors
                .iter()
                .map(|(id, _)| id)
                .filter(|id| *id != seed && item_set.contains(id))
                // Then top up in input order.
                .chain(items.iter());
            for candidate in candidates {
                if self.child_count(&bucket) >= max {
                    break;
                }
                if unclustered.remove(candidate) {
                    self.set_parent(candidate, &bucket);
                    assignments.push((candidate.clone(), bucket.clone()));
                }
            }
            to_persist.insert(&bucket);
        }
        Ok((to_persist.0, assignments))
    }
}

/// Ports of the `extract_context_index_input` cases of Python's
/// `test_global_context_index.py`.
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use std::borrow::Cow;

    use serde_json::json;

    use super::*;

    const DS: &str = "ds-1";

    fn node(id: &str, props: Value) -> GraphNode {
        let Value::Object(map) = props else {
            panic!("object")
        };
        (
            id.to_string(),
            map.into_iter().map(|(k, v)| (Cow::Owned(k), v)).collect(),
        )
    }

    fn text_summary(id: &str) -> GraphNode {
        node(id, json!({"type": "TextSummary", "text": "Chunk summary"}))
    }

    fn bucket(id: &str, level: Option<i64>, is_root: bool) -> GraphNode {
        node(
            id,
            json!({"type": "GlobalContextSummary", "text": "Global context summary",
                   "dataset_id": DS, "level": level, "is_root": is_root}),
        )
    }

    fn summarized_in(child: &str, parent: &str) -> EdgeData {
        (
            child.to_string(),
            parent.to_string(),
            SUMMARIZED_IN.to_string(),
            HashMap::from([(Cow::Borrowed("relationship_name"), json!(SUMMARIZED_IN))]),
        )
    }

    fn parent_of(input: &IndexInput, id: &str) -> Option<String> {
        input.nodes[id].global_context_bucket_id.clone()
    }

    fn children_of(input: &IndexInput, id: &str) -> Vec<String> {
        input.nodes[id].child_ids.iter().cloned().collect()
    }

    #[test]
    fn uses_summary_edges_for_assignment() {
        let input = extract_context_index_input(
            &[text_summary("t"), bucket("b", Some(0), false)],
            &[summarized_in("t", "b")],
            DS,
        );
        assert_eq!(input.text_summaries, ["t"]);
        assert_eq!(input.buckets, ["b"]);
        assert_eq!(children_of(&input, "b"), ["t"]);
        assert_eq!(parent_of(&input, "t").as_deref(), Some("b"));
    }

    #[test]
    fn ignores_a_stored_bucket_marker_without_an_edge() {
        let mut child = text_summary("t");
        child
            .1
            .insert(Cow::Borrowed("global_context_bucket_id"), json!("b"));
        let input = extract_context_index_input(&[child, bucket("b", Some(0), false)], &[], DS);
        assert_eq!(parent_of(&input, "t"), None);
        assert!(children_of(&input, "b").is_empty());
    }

    #[test]
    fn ignores_text_summary_to_root_edge() {
        let input = extract_context_index_input(
            &[text_summary("t"), bucket("r", Some(1), true)],
            &[summarized_in("t", "r")],
            DS,
        );
        assert_eq!(input.root.as_deref(), Some("r"));
        assert!(children_of(&input, "r").is_empty());
        assert_eq!(parent_of(&input, "t"), None);
    }

    #[test]
    fn ignores_stale_edge_to_missing_parent() {
        let input =
            extract_context_index_input(&[text_summary("t")], &[summarized_in("t", "gone")], DS);
        assert_eq!(parent_of(&input, "t"), None);
    }

    #[test]
    fn uses_adjacent_bucket_and_root_edges_only() {
        let input = extract_context_index_input(
            &[
                bucket("b0", Some(0), false),
                bucket("b1", Some(1), false),
                bucket("far", Some(0), false),
                bucket("b2", Some(2), false),
                bucket("lone", Some(0), false),
                bucket("r", Some(3), true),
            ],
            &[
                summarized_in("b0", "b1"),
                summarized_in("far", "b2"),
                summarized_in("lone", "r"),
            ],
            DS,
        );
        assert_eq!(parent_of(&input, "b0").as_deref(), Some("b1"));
        assert_eq!(children_of(&input, "b1"), ["b0"]);
        assert_eq!(parent_of(&input, "far"), None);
        assert!(children_of(&input, "b2").is_empty());
        assert_eq!(parent_of(&input, "lone").as_deref(), Some("r"));
        assert_eq!(children_of(&input, "r"), ["lone"]);
    }

    #[test]
    fn filters_buckets_by_dataset_id() {
        let other = node(
            "other",
            json!({"type": "GlobalContextSummary", "text": "x", "dataset_id": "ds-2", "level": 0}),
        );
        let nodes = [bucket("mine", Some(0), false), other];
        assert_eq!(
            extract_context_index_input(&nodes, &[], DS).buckets,
            ["mine"]
        );
        assert!(
            extract_context_index_input(&nodes, &[], "dataset-name")
                .buckets
                .is_empty()
        );
    }

    #[test]
    fn rehydrates_graph_bucket_entity_state_on_level_zero_only() {
        let with_ids = |id: &str, level: i64, ids: Value| {
            node(
                id,
                json!({"type": "GlobalContextSummary", "dataset_id": DS, "level": level,
                       "graph_bucket_entity_ids": ids}),
            )
        };
        let input = extract_context_index_input(
            &[
                bucket("missing", Some(0), false),
                with_ids("null", 0, Value::Null),
                with_ids("empty", 0, json!([])),
                with_ids("populated", 0, json!(["entity-b", "entity-a"])),
                with_ids("upper", 1, json!(["ignored"])),
            ],
            &[],
            DS,
        );
        let ids = |id: &str| input.nodes[id].graph_bucket_entity_ids.clone();
        assert_eq!(ids("missing"), None);
        assert_eq!(ids("null"), None);
        assert_eq!(ids("empty"), Some(BTreeSet::new()));
        assert_eq!(
            ids("populated"),
            Some(BTreeSet::from([
                "entity-a".to_string(),
                "entity-b".to_string()
            ]))
        );
        assert_eq!(ids("upper"), None);
    }

    #[test]
    fn options_are_validated() {
        assert!(LlmIndexOptions::default().validate().is_ok());
        for bad in [
            LlmIndexOptions {
                max_bucket_size: 1,
                ..Default::default()
            },
            LlmIndexOptions {
                placement_distance_threshold: -0.1,
                ..Default::default()
            },
            LlmIndexOptions {
                placement_distance_threshold: f64::NAN,
                ..Default::default()
            },
        ] {
            assert!(bad.validate().is_err());
        }
    }
}
