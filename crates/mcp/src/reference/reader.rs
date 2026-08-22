use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    DeltaHead, DeltaSnapshot, DeltaStore, REFERENCE_DATASET, ReferenceConfig,
    ReferenceEngineFactory, ReferenceEngineOpen, ReferenceError, ReferenceRecord,
    validate_published_generation,
};
use crate::engine::{RecallItem, RecallRequest};

const DEFAULT_TOP_K: usize = 3;
const MAX_TOP_K: usize = 10;
const MAX_QUERY_BYTES: usize = 8_192;
const HARD_MAX_ITEM_BYTES: usize = 2 * 1024;
const HARD_MAX_PAYLOAD_BYTES: usize = 8 * 1024;
const JSON_FILE_LIMIT: u64 = 16 * 1024 * 1024;
const SEARCH_TYPES: [&str; 4] = ["CHUNKS", "SUMMARIES", "GRAPH_COMPLETION", "RAG_COMPLETION"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceRecallRequest {
    pub query: String,
    #[serde(default = "default_top_k")]
    pub top_k: usize,
    #[serde(default = "default_search_type")]
    pub search_type: String,
    #[serde(default)]
    pub wait_for_previous: bool,
}

impl Default for ReferenceRecallRequest {
    fn default() -> Self {
        Self {
            query: String::new(),
            top_k: DEFAULT_TOP_K,
            search_type: default_search_type(),
            wait_for_previous: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReferenceRecallItem {
    pub source: String,
    pub content: String,
    pub score: Option<f64>,
    pub source_id: Option<String>,
    pub source_label: Option<String>,
    pub revision: Option<u64>,
    pub content_type: Option<String>,
    pub event_id: Option<String>,
    pub content_sha256: Option<String>,
    pub cognified: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceRecallMetadata {
    pub status: String,
    pub generation_id: Option<String>,
    pub included_through: u64,
    pub committed_head: u64,
    pub delta_examined: usize,
    pub corrupt_delta_skipped: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub graph_status: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReferenceRecallResponse {
    pub items: Vec<ReferenceRecallItem>,
    pub reference: ReferenceRecallMetadata,
    pub truncated: bool,
}

pub trait ReferenceReadHooks: Send + Sync {
    fn after_current_snapshot(&self) {}
    fn after_head_snapshot(&self) {}
}

#[derive(Debug, Default)]
struct NoopReadHooks;

impl ReferenceReadHooks for NoopReadHooks {}

pub struct ReferenceReader {
    config: ReferenceConfig,
    factory: Arc<dyn ReferenceEngineFactory>,
    hooks: Arc<dyn ReferenceReadHooks>,
}

impl ReferenceReader {
    pub fn new(config: ReferenceConfig, factory: Arc<dyn ReferenceEngineFactory>) -> Self {
        Self::with_hooks(config, factory, Arc::new(NoopReadHooks))
    }

    pub fn with_hooks(
        config: ReferenceConfig,
        factory: Arc<dyn ReferenceEngineFactory>,
        hooks: Arc<dyn ReferenceReadHooks>,
    ) -> Self {
        Self {
            config,
            factory,
            hooks,
        }
    }

    pub async fn recall(
        &self,
        request: ReferenceRecallRequest,
    ) -> Result<ReferenceRecallResponse, ReferenceError> {
        validate_request(&request)?;
        self.config.layout.validate_reader_root()?;
        DeltaStore::new(self.config.layout.clone(), self.config.limits).validate_schema()?;
        let top_k = request.top_k.min(MAX_TOP_K);
        let max_item_bytes = self.config.limits.max_item_bytes.min(HARD_MAX_ITEM_BYTES);
        let max_payload_bytes = self
            .config
            .limits
            .max_payload_bytes
            .min(HARD_MAX_PAYLOAD_BYTES);
        let identity = self.factory.identity();
        let mut degraded = false;
        let mut generation_error = None;
        let generation = if self.config.layout.current.exists() {
            match validate_published_generation(&self.config, Some(&identity)) {
                Ok(generation) => Some(generation),
                Err(ReferenceError::ModelMismatch) => return Err(ReferenceError::ModelMismatch),
                Err(error @ (ReferenceError::CorruptRecord | ReferenceError::Unavailable)) => {
                    degraded = true;
                    generation_error = Some(error);
                    None
                }
                Err(error) => return Err(error),
            }
        } else {
            None
        };
        self.hooks.after_current_snapshot();

        let included_through = generation
            .as_ref()
            .map_or(0, |value| value.included_through);
        let (snapshot, corrupt_sequences) = read_delta_snapshot(&self.config, included_through)?;
        let corrupt_delta_skipped = corrupt_sequences.len();
        degraded |= corrupt_delta_skipped > 0;
        self.hooks.after_head_snapshot();
        let delta_examined = usize::try_from(
            snapshot
                .head
                .highest_committed_sequence
                .saturating_sub(included_through),
        )
        .unwrap_or(usize::MAX);
        let latest_safe_sequence = corrupt_sequences.iter().copied().max();
        let safe_records = snapshot
            .records
            .iter()
            .filter(|record| latest_safe_sequence.is_none_or(|sequence| record.sequence > sequence))
            .cloned()
            .collect::<Vec<_>>();
        let latest = latest_delta_by_source(&safe_records);
        let mut truncated = false;
        let mut delta_items = latest
            .values()
            .filter(|record| semantic_match(&request.query, &record.source_label, &record.content))
            .map(|record| delta_item(record, &request.query, max_item_bytes, &mut truncated))
            .collect::<Vec<_>>();
        delta_items.sort_by(|left, right| {
            right
                .revision
                .cmp(&left.revision)
                .then_with(|| right.event_id.cmp(&left.event_id))
        });
        if corrupt_delta_skipped > 0 && delta_items.is_empty() {
            return Err(ReferenceError::CorruptRecord);
        }
        if delta_items.is_empty()
            && let Some(error) = generation_error
        {
            return Err(error);
        }

        let mut graph_status = None;
        let graph_items = if let Some(generation) = generation.as_ref() {
            let open = ReferenceEngineOpen {
                root: self
                    .config
                    .layout
                    .generations
                    .join(&generation.generation_id),
                dataset: REFERENCE_DATASET.to_owned(),
                read_only: true,
                user_agent: identity.user_agent.clone(),
            };
            match self.factory.open_reader(&open).await {
                Ok(mut engine) => {
                    let graph_request = RecallRequest {
                        query: request.query.clone(),
                        dataset: REFERENCE_DATASET.to_owned(),
                        session_id: None,
                        top_k,
                        search_type: Some(request.search_type.clone()),
                        auto_route: false,
                    };
                    let recalled = engine.recall(graph_request).await;
                    let closed = engine.close().await;
                    match (recalled, closed) {
                        (Ok(response), Ok(())) => response
                            .items
                            .into_iter()
                            .map(|item| {
                                graph_item(item, &request.query, max_item_bytes, &mut truncated)
                            })
                            .collect::<Vec<_>>(),
                        _ if !delta_items.is_empty() => {
                            degraded = true;
                            graph_status = Some("unavailable".to_owned());
                            Vec::new()
                        }
                        (Err(error), _) | (_, Err(error)) => return Err(error),
                    }
                }
                Err(_error) if !delta_items.is_empty() => {
                    degraded = true;
                    graph_status = Some("unavailable".to_owned());
                    Vec::new()
                }
                Err(error) => return Err(error),
            }
        } else {
            Vec::new()
        };

        let mut items = merge_items(delta_items, graph_items, &latest);
        if items.len() > top_k {
            items.truncate(top_k);
            truncated = true;
        }
        let mut response = ReferenceRecallResponse {
            items,
            reference: ReferenceRecallMetadata {
                status: if degraded { "degraded" } else { "ok" }.to_owned(),
                generation_id: generation.as_ref().map(|value| value.generation_id.clone()),
                included_through,
                committed_head: snapshot.head.highest_committed_sequence,
                delta_examined,
                corrupt_delta_skipped,
                graph_status,
            },
            truncated,
        };
        enforce_payload_budget(&mut response, max_payload_bytes);
        Ok(response)
    }
}

fn default_top_k() -> usize {
    DEFAULT_TOP_K
}

fn default_search_type() -> String {
    "CHUNKS".to_owned()
}

fn validate_request(request: &ReferenceRecallRequest) -> Result<(), ReferenceError> {
    if request.query.trim().is_empty()
        || request.query.len() > MAX_QUERY_BYTES
        || request.top_k == 0
        || !SEARCH_TYPES.contains(&request.search_type.as_str())
    {
        return Err(ReferenceError::InvalidInput);
    }
    Ok(())
}

fn read_delta_snapshot(
    config: &ReferenceConfig,
    included_through: u64,
) -> Result<(DeltaSnapshot, Vec<u64>), ReferenceError> {
    let head: DeltaHead = read_json_file(&config.layout.delta_head)?;
    if !head.verify_hash() || included_through > head.highest_committed_sequence {
        return Err(ReferenceError::CorruptRecord);
    }
    let mut records = Vec::new();
    let mut corrupt = Vec::new();
    for sequence in included_through.saturating_add(1)..=head.highest_committed_sequence {
        match read_delta_record(&config.layout.delta_events, sequence) {
            Ok(record) => records.push(record),
            Err(ReferenceError::CorruptRecord) => corrupt.push(sequence),
            Err(error) => return Err(error),
        }
    }
    Ok((DeltaSnapshot { head, records }, corrupt))
}

fn read_delta_record(directory: &Path, sequence: u64) -> Result<ReferenceRecord, ReferenceError> {
    let prefix = format!("{sequence:020}-");
    let mut matches = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            matches.push(entry.path());
        }
    }
    if matches.len() != 1 {
        return Err(ReferenceError::CorruptRecord);
    }
    let path = matches.pop().ok_or(ReferenceError::CorruptRecord)?;
    let metadata = fs::symlink_metadata(&path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > JSON_FILE_LIMIT
    {
        return Err(ReferenceError::CorruptRecord);
    }
    let record: ReferenceRecord = read_json_file(&path)?;
    let digest = record
        .event_id
        .strip_prefix("sha256:")
        .unwrap_or(&record.event_id);
    let expected_name = format!("{:020}-{digest}.json", record.sequence);
    if record.sequence != sequence
        || path.file_name().and_then(|name| name.to_str()) != Some(expected_name.as_str())
        || !record.verify()
    {
        return Err(ReferenceError::CorruptRecord);
    }
    Ok(record)
}

fn read_json_file<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, ReferenceError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > JSON_FILE_LIMIT
    {
        return Err(ReferenceError::CorruptRecord);
    }
    serde_json::from_slice(&fs::read(path)?).map_err(|_| ReferenceError::CorruptRecord)
}

fn latest_delta_by_source(records: &[ReferenceRecord]) -> BTreeMap<String, ReferenceRecord> {
    let mut latest = BTreeMap::<String, ReferenceRecord>::new();
    for record in records {
        match latest.get(&record.source_id) {
            Some(previous)
                if previous.revision > record.revision
                    || (previous.revision == record.revision
                        && previous.sequence >= record.sequence) => {}
            _ => {
                latest.insert(record.source_id.clone(), record.clone());
            }
        }
    }
    latest
}

fn semantic_match(query: &str, label: &str, content: &str) -> bool {
    let query_words = words(query);
    if query_words.is_empty() {
        return false;
    }
    let searchable = format!("{label} {content}");
    let content_words = words(&searchable);
    let matches = query_words.intersection(&content_words).count();
    matches > 0 && matches.saturating_mul(2) >= query_words.len()
}

fn words(value: &str) -> HashSet<String> {
    value
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| word.len() >= 2)
        .map(str::to_lowercase)
        .collect()
}

fn delta_item(
    record: &ReferenceRecord,
    query: &str,
    max_bytes: usize,
    truncated: &mut bool,
) -> ReferenceRecallItem {
    let (content, was_truncated) = excerpt(&record.content, query, max_bytes);
    *truncated |= was_truncated;
    ReferenceRecallItem {
        source: "reference_delta".to_owned(),
        content,
        score: None,
        source_id: Some(record.source_id.clone()),
        source_label: Some(record.source_label.clone()),
        revision: Some(record.revision),
        content_type: Some(record.content_type.clone()),
        event_id: Some(record.event_id.clone()),
        content_sha256: Some(record.content_sha256.clone()),
        cognified: false,
        metadata: BTreeMap::new(),
    }
}

fn excerpt(content: &str, query: &str, max_bytes: usize) -> (String, bool) {
    if content.len() <= max_bytes {
        return (content.to_owned(), false);
    }
    let match_start = first_matching_word(content, query).unwrap_or(0);
    let mut start = match_start.saturating_sub(max_bytes / 2);
    let mut end = start.saturating_add(max_bytes).min(content.len());
    if end == content.len() {
        start = end.saturating_sub(max_bytes);
    }
    while !content.is_char_boundary(start) {
        start = start.saturating_add(1);
    }
    while !content.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    (content[start..end].to_owned(), true)
}

fn first_matching_word(content: &str, query: &str) -> Option<usize> {
    let query_words = words(query);
    let mut word_start = None;
    for (index, character) in content
        .char_indices()
        .chain(std::iter::once((content.len(), ' ')))
    {
        if character.is_alphanumeric() {
            word_start.get_or_insert(index);
        } else if let Some(start) = word_start.take() {
            let word = content[start..index].to_lowercase();
            if word.len() >= 2 && query_words.contains(&word) {
                return Some(start);
            }
        }
    }
    None
}

fn graph_item(
    item: RecallItem,
    query: &str,
    max_bytes: usize,
    truncated: &mut bool,
) -> ReferenceRecallItem {
    let source_id = metadata_string(&item, "reference_source_id");
    let source_label = metadata_string(&item, "reference_label");
    let revision = item
        .metadata
        .get("reference_revision")
        .and_then(Value::as_u64);
    let content_type = metadata_string(&item, "reference_content_type")
        .or_else(|| metadata_string(&item, "content_type"));
    let event_id = item
        .event_id
        .clone()
        .or_else(|| metadata_string(&item, "cognee_external_event_id"));
    let content_sha256 = metadata_string(&item, "reference_content_sha256")
        .or_else(|| metadata_string(&item, "content_sha256"));
    let (content, was_truncated) = excerpt(&item.content, query, max_bytes);
    *truncated |= was_truncated;
    ReferenceRecallItem {
        source: "reference_graph".to_owned(),
        content,
        score: item.score,
        source_id,
        source_label,
        revision,
        content_type,
        event_id,
        content_sha256,
        cognified: true,
        metadata: BTreeMap::new(),
    }
}

fn metadata_string(item: &RecallItem, key: &str) -> Option<String> {
    item.metadata
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn merge_items(
    delta: Vec<ReferenceRecallItem>,
    graph: Vec<ReferenceRecallItem>,
    latest: &BTreeMap<String, ReferenceRecord>,
) -> Vec<ReferenceRecallItem> {
    let superseded = latest
        .values()
        .filter_map(|record| record.supersedes_event_id.clone())
        .collect::<BTreeSet<_>>();
    let graph = graph.into_iter().filter(|item| {
        if item
            .event_id
            .as_ref()
            .is_some_and(|event_id| superseded.contains(event_id))
        {
            return false;
        }
        let Some(source_id) = item.source_id.as_ref() else {
            return true;
        };
        let Some(delta) = latest.get(source_id) else {
            return true;
        };
        item.revision
            .is_some_and(|revision| revision >= delta.revision)
            && item.event_id.as_deref() == Some(delta.event_id.as_str())
    });

    let mut merged: Vec<ReferenceRecallItem> = Vec::new();
    let mut event_indexes = BTreeMap::<String, usize>::new();
    let mut content_indexes = BTreeMap::<String, usize>::new();
    for item in delta.into_iter().chain(graph) {
        let duplicate = item
            .event_id
            .as_ref()
            .and_then(|event_id| event_indexes.get(event_id).copied())
            .or_else(|| {
                content_indexes
                    .get(&normalized_content(&item.content))
                    .copied()
            });
        if let Some(index) = duplicate {
            retain_alternate_label(&mut merged[index], item.source_label.as_deref());
            continue;
        }
        let index = merged.len();
        if let Some(event_id) = item.event_id.as_ref() {
            event_indexes.insert(event_id.clone(), index);
        }
        content_indexes.insert(normalized_content(&item.content), index);
        merged.push(item);
    }
    merged
}

fn normalized_content(content: &str) -> String {
    content
        .split_whitespace()
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

fn retain_alternate_label(item: &mut ReferenceRecallItem, label: Option<&str>) {
    let Some(label) = label.filter(|label| Some(*label) != item.source_label.as_deref()) else {
        return;
    };
    let labels = item
        .metadata
        .entry("alternate_source_labels".to_owned())
        .or_insert_with(|| Value::Array(Vec::new()));
    let Some(labels) = labels.as_array_mut() else {
        return;
    };
    if !labels.iter().any(|value| value.as_str() == Some(label)) {
        labels.push(Value::String(label.to_owned()));
    }
}

fn enforce_payload_budget(response: &mut ReferenceRecallResponse, max_bytes: usize) {
    while serialized_len(response) > max_bytes && !response.items.is_empty() {
        response.items.pop();
        response.truncated = true;
    }
}

fn serialized_len(response: &ReferenceRecallResponse) -> usize {
    serde_json::to_vec(response).map_or(usize::MAX, |bytes| bytes.len())
}
