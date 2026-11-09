//! The events of the streamed `GET /visualize/json` (SDK-787), as pure data.
//!
//! Port of `stream_graph_events` in Python's
//! `cognee/modules/visualization/graph_stream.py`. The SSE transport itself
//! (keepalives, the in-flight cap, the lifetime, cancellation) belongs to the
//! HTTP server; this module produces the ordered `(event, data)` pairs it
//! sends, and the constants and error payloads it needs to match Python.
//!
//! Events, in order:
//!
//! 1. `meta` — `{seeds, seed_source, depth, max_nodes, chunk_size}`;
//! 2. one `chunk` per `chunk_size` nodes — `{index, nodes, links}` in the
//!    compact shape ([`compact_chunk`]); every link's endpoints were already
//!    sent;
//! 3. one or more `summary` events — `{nodes: {id: {importance,
//!    label_priority[, entity_type]}}}` with at most `chunk_size` entries each,
//!    the first also carrying `color_maps: {node_set}`. Always at least one,
//!    even for an empty graph;
//! 4. `done` — `{nodes, links, chunks}` totals.
//!
//! A failure after the response started is one `error` event,
//! [`error_event`]`(message, status)`.

use std::collections::VecDeque;

use cognee_graph::{BoundedNeighborhood, EdgeData, GraphDBTrait, GraphNode};
use serde_json::{Map, Value, json};

use crate::error::VisualizationError;
use crate::preprocessor::{CompactGraphAccumulator, compact_chunk, compact_property_keys};
use crate::subgraph::{
    BoundsError, SeedSearcher, SeedSource, VisualizationBounds, entity_ids, lookup_entity_types,
    resolve_seed_node_ids, set_semantic_type,
};

/// Nodes per `chunk` event (Python `STREAM_CHUNK_SIZE`).
pub const STREAM_CHUNK_SIZE: usize = 1000;
/// Idle interval after which the transport sends a keepalive comment (Python
/// `KEEPALIVE_SECONDS`).
pub const STREAM_KEEPALIVE_SECONDS: u64 = 15;
/// Streams one process may hold at once; the next is refused with a 503
/// (Python `MAX_STREAMS_IN_FLIGHT`).
pub const MAX_STREAMS_IN_FLIGHT: usize = 8;
/// How long one stream may keep its permit and frames (Python
/// `STREAM_LIFETIME_SECONDS`).
pub const STREAM_LIFETIME_SECONDS: u64 = 300;

/// The `error` message for a non-API failure after the response started
/// (sent with status 409, never the raw error text).
pub const STREAM_FAILURE_MESSAGE: &str = "Failed to build the visualization payload";
/// The `error` message when the lifetime expired (sent with status 408).
pub const STREAM_EXPIRED_MESSAGE: &str = "The graph stream was not read in time and was closed.";
/// Status for [`STREAM_FAILURE_MESSAGE`].
pub const STREAM_FAILURE_STATUS: u16 = 409;
/// Status for [`STREAM_EXPIRED_MESSAGE`].
pub const STREAM_EXPIRED_STATUS: u16 = 408;

/// Event names, as sent in the SSE `event:` field.
pub const EVENT_META: &str = "meta";
/// See [`EVENT_META`].
pub const EVENT_CHUNK: &str = "chunk";
/// See [`EVENT_META`].
pub const EVENT_SUMMARY: &str = "summary";
/// See [`EVENT_META`].
pub const EVENT_DONE: &str = "done";
/// See [`EVENT_META`].
pub const EVENT_ERROR: &str = "error";

/// One streamed event: its SSE name and its JSON data.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphStreamEvent {
    /// `meta`, `chunk`, `summary`, `done` or `error`.
    pub event: &'static str,
    /// The event's JSON payload.
    pub data: Value,
}

/// The `error` event: `{message, status}`.
pub fn error_event(message: &str, status: u16) -> GraphStreamEvent {
    GraphStreamEvent {
        event: EVENT_ERROR,
        data: json!({"message": message, "status": status}),
    }
}

/// The `meta` event's data.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GraphStreamMeta {
    /// The resolved seeds.
    pub seeds: Vec<String>,
    /// Where they came from.
    pub seed_source: SeedSource,
    /// The expansion depth.
    pub depth: usize,
    /// The node cap.
    pub max_nodes: usize,
    /// Nodes per chunk (and summary entries per summary event).
    pub chunk_size: usize,
}

impl GraphStreamMeta {
    fn to_value(&self) -> Value {
        json!({
            "seeds": self.seeds,
            "seed_source": self.seed_source.as_str(),
            "depth": self.depth,
            "max_nodes": self.max_nodes,
            "chunk_size": self.chunk_size,
        })
    }
}

/// The ordered events of one streamed read, built lazily: each `next()`
/// compacts at most one chunk.
///
/// Built from a [`GraphStreamMeta`] and the read's chunks (or a whole
/// [`BoundedNeighborhood`], chunked with Python's `chunk_members` contract).
/// Node data should carry `_semantic_type` where the store resolved it
/// ([`prepare_graph_stream`] does that); without it a chunk falls back to its
/// own `is_a` links and the summary corrects the rest.
#[derive(Debug)]
pub struct GraphStreamEvents {
    meta: Option<Value>,
    chunk_size: usize,
    chunks: std::vec::IntoIter<(Vec<GraphNode>, Vec<EdgeData>)>,
    index: usize,
    accumulator: CompactGraphAccumulator,
    tail: Option<VecDeque<GraphStreamEvent>>,
}

impl GraphStreamEvents {
    /// The events for `neighborhood`, cut into `meta.chunk_size` chunks.
    pub fn new(meta: GraphStreamMeta, neighborhood: BoundedNeighborhood) -> Self {
        let chunks = neighborhood.into_chunks(meta.chunk_size);
        Self::from_chunks(meta, chunks)
    }

    /// The events for chunks already cut. Each chunk's links must refer only
    /// to nodes of that or an earlier chunk.
    pub fn from_chunks(
        meta: GraphStreamMeta,
        chunks: Vec<(Vec<GraphNode>, Vec<EdgeData>)>,
    ) -> Self {
        Self {
            chunk_size: meta.chunk_size.max(1),
            meta: Some(meta.to_value()),
            chunks: chunks.into_iter(),
            index: 0,
            accumulator: CompactGraphAccumulator::new(),
            tail: None,
        }
    }

    /// The `summary` events and `done`, from everything accumulated.
    fn build_tail(&self) -> VecDeque<GraphStreamEvent> {
        let summary = self.accumulator.summary();
        let entries: Vec<(String, Value)> = summary
            .get("nodes")
            .and_then(Value::as_object)
            .map(|nodes| nodes.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        let color_maps = summary.get("color_maps").cloned().unwrap_or(json!({}));

        let mut tail = VecDeque::new();
        // One summary event per chunk_size nodes, and at least one.
        let parts = entries.len().div_ceil(self.chunk_size).max(1);
        let mut entries = entries.into_iter();
        for part in 0..parts {
            let nodes: Map<String, Value> = entries.by_ref().take(self.chunk_size).collect();
            let mut data = Map::new();
            data.insert("nodes".to_string(), Value::Object(nodes));
            if part == 0 {
                data.insert("color_maps".to_string(), color_maps.clone());
            }
            tail.push_back(GraphStreamEvent {
                event: EVENT_SUMMARY,
                data: Value::Object(data),
            });
        }
        tail.push_back(GraphStreamEvent {
            event: EVENT_DONE,
            data: json!({
                "nodes": self.accumulator.node_count(),
                "links": self.accumulator.link_count(),
                "chunks": self.index,
            }),
        });
        tail
    }
}

impl Iterator for GraphStreamEvents {
    type Item = GraphStreamEvent;

    fn next(&mut self) -> Option<GraphStreamEvent> {
        if let Some(meta) = self.meta.take() {
            return Some(GraphStreamEvent {
                event: EVENT_META,
                data: meta,
            });
        }
        if self.tail.is_none() {
            if let Some((nodes, edges)) = self.chunks.next() {
                let (nodes, links) = compact_chunk(&nodes, &edges);
                self.accumulator.add(&nodes, &links);
                let index = self.index;
                self.index += 1;
                return Some(GraphStreamEvent {
                    event: EVENT_CHUNK,
                    data: json!({"index": index, "nodes": nodes, "links": links}),
                });
            }
            self.tail = Some(self.build_tail());
        }
        self.tail.as_mut().and_then(VecDeque::pop_front)
    }
}

/// Everything a streamed read does before its first event: resolve the seeds,
/// read their bounded neighbourhood projected to
/// [`COMPACT_PROPERTY_KEYS`](crate::preprocessor::COMPACT_PROPERTY_KEYS), and
/// type its entities from the store. Python's `stream_graph_events` up to the
/// first chunk.
///
/// Fails before anything is sent (so the route keeps a real status code):
/// invalid bounds, `full` (only bounded reads stream), a seed search or a
/// store read failing. A failed entity-type lookup is not an error (logged;
/// the chunks fall back to their `is_a` links). No seeds is an empty graph:
/// `meta`, one `summary`, `done`.
///
/// `chunk_size` 0 is treated as 1.
pub async fn prepare_graph_stream(
    graph_db: &dyn GraphDBTrait,
    bounds: &VisualizationBounds,
    seed_searcher: Option<&dyn SeedSearcher>,
    chunk_size: usize,
) -> Result<GraphStreamEvents, VisualizationError> {
    if bounds.full {
        return Err(BoundsError::FullStream.into());
    }
    bounds.validate()?;
    let (seeds, seed_source) = resolve_seed_node_ids(graph_db, bounds, seed_searcher).await?;
    let meta = GraphStreamMeta {
        seeds,
        seed_source,
        depth: bounds.depth,
        max_nodes: bounds.max_nodes,
        chunk_size: chunk_size.max(1),
    };
    if meta.seeds.is_empty() {
        return Ok(GraphStreamEvents::new(meta, BoundedNeighborhood::default()));
    }
    let keys = compact_property_keys();
    let mut neighborhood = graph_db
        .get_bounded_neighborhood(&meta.seeds, meta.depth, meta.max_nodes, Some(&keys))
        .await?;
    // Python looks the types up per chunk, latching off after one failure;
    // with the whole read in hand one lookup gives the same types.
    let ids = entity_ids(neighborhood.nodes.iter().map(|n| (n.id.as_str(), &n.data)));
    if let Some(types) = lookup_entity_types(graph_db, &ids).await {
        for node in &mut neighborhood.nodes {
            set_semantic_type(&node.id, &mut node.data, &types);
        }
    }
    Ok(GraphStreamEvents::new(meta, neighborhood))
}
