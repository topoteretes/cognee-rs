#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! The events of the streamed graph read — port of the event-order cases of
//! Python's `test_graph_stream.py` (the transport cases belong to the HTTP
//! server), plus a replica of cognee-frontend's `readGraphStream.ts`
//! validation, which every stream must pass.

use std::collections::{HashMap, HashSet};

use cognee_graph::{BoundedNeighborhood, BoundedNode, GraphDBTrait, MockGraphDB};
use cognee_visualization::VisualizationError;
use cognee_visualization::stream::{
    EVENT_CHUNK, EVENT_DONE, EVENT_META, EVENT_SUMMARY, GraphStreamEvent, GraphStreamEvents,
    GraphStreamMeta, STREAM_CHUNK_SIZE, error_event, prepare_graph_stream,
};
use cognee_visualization::subgraph::{BoundsError, SeedSource, VisualizationBounds};
use serde_json::{Map, Value, json};

/// A hub with `spokes` DocumentChunk spokes (Python `_star`).
async fn star(spokes: usize) -> MockGraphDB {
    let db = MockGraphDB::new();
    db.add_node_raw(json!({"id": "hub", "name": "hub", "type": "Entity"}))
        .await
        .unwrap();
    for index in 0..spokes {
        db.add_node_raw(json!({
            "id": format!("c{index}"),
            "type": "DocumentChunk",
            "text": format!("chunk {index} ").repeat(40),
        }))
        .await
        .unwrap();
        db.add_edge("hub", &format!("c{index}"), "contains", None)
            .await
            .unwrap();
    }
    db
}

fn hub_bounds() -> VisualizationBounds {
    VisualizationBounds {
        seed_node_ids: vec!["hub".to_string()],
        depth: 1,
        seed_top_k: 10,
        max_nodes: 100,
        ..Default::default()
    }
}

async fn events(
    db: &MockGraphDB,
    bounds: &VisualizationBounds,
    chunk_size: usize,
) -> Vec<GraphStreamEvent> {
    prepare_graph_stream(db, bounds, None, chunk_size)
        .await
        .expect("stream prepares")
        .collect()
}

/// The validation `cognee-frontend/src/modules/business/readGraphStream.ts`
/// applies, step for step: chunks must carry both arrays; summaries merge
/// per node; `done` totals must be numbers equal to what was received; every
/// node must have a summary entry. Returns the assembled nodes.
fn read_graph_stream(events: &[GraphStreamEvent]) -> Result<Vec<Value>, String> {
    let mut chunks = 0u64;
    let mut nodes: Vec<Value> = Vec::new();
    let mut links: Vec<Value> = Vec::new();
    let mut summaries: HashMap<String, Map<String, Value>> = HashMap::new();
    for GraphStreamEvent { event, data } in events {
        // Every event goes over the wire as JSON.
        let data: Value = serde_json::from_str(&serde_json::to_string(data).unwrap()).unwrap();
        match *event {
            "chunk" => {
                let (Some(n), Some(l)) = (data["nodes"].as_array(), data["links"].as_array())
                else {
                    return Err("unreadable chunk".into());
                };
                chunks += 1;
                nodes.extend(n.iter().filter(|v| v["id"].is_string()).cloned());
                links.extend(
                    l.iter()
                        .filter(|v| v["source"].is_string() && v["target"].is_string())
                        .cloned(),
                );
            }
            "summary" => {
                if let Some(entries) = data["nodes"].as_object() {
                    for (id, entry) in entries {
                        let Some(entry) = entry.as_object() else {
                            continue;
                        };
                        let merged = summaries.entry(id.clone()).or_default();
                        if entry.get("importance").is_some_and(Value::is_number) {
                            merged.insert("importance".into(), entry["importance"].clone());
                        }
                        if entry.get("label_priority").is_some_and(Value::is_boolean) {
                            merged.insert("label_priority".into(), entry["label_priority"].clone());
                        }
                        if entry.get("entity_type").is_some_and(Value::is_string) {
                            merged.insert("entity_type".into(), entry["entity_type"].clone());
                        }
                    }
                }
            }
            "error" => return Err(format!("error event: {data}")),
            "done" => {
                let (Some(dn), Some(dl), Some(dc)) = (
                    data["nodes"].as_u64(),
                    data["links"].as_u64(),
                    data["chunks"].as_u64(),
                ) else {
                    return Err("unreadable totals".into());
                };
                if nodes.len() as u64 != dn || links.len() as u64 != dl || chunks != dc {
                    return Err(format!(
                        "lost part of the graph: {} of {dn} nodes, {} of {dl} links, {chunks} of {dc} chunks",
                        nodes.len(),
                        links.len()
                    ));
                }
                let summarised = nodes
                    .iter()
                    .filter(|n| summaries.contains_key(n["id"].as_str().unwrap()))
                    .count();
                if summarised != nodes.len() {
                    return Err(format!("summary covered {summarised} of {}", nodes.len()));
                }
                return Ok(nodes
                    .into_iter()
                    .map(|mut n| {
                        let id = n["id"].as_str().unwrap().to_string();
                        if let (Some(obj), Some(extra)) = (n.as_object_mut(), summaries.get(&id)) {
                            for (k, v) in extra {
                                obj.insert(k.clone(), v.clone());
                            }
                        }
                        n
                    })
                    .collect());
            }
            _ => {}
        }
    }
    Err("ended before done".into())
}

#[tokio::test]
async fn events_arrive_in_order_and_links_never_run_ahead_of_nodes() {
    let db = star(25).await;
    let events = events(&db, &hub_bounds(), 10).await;

    let names: Vec<&str> = events.iter().map(|e| e.event).collect();
    assert_eq!(
        names,
        [
            "meta", "chunk", "chunk", "chunk", "summary", "summary", "summary", "done"
        ]
    );
    assert_eq!(
        events[0].data,
        json!({"seeds": ["hub"], "seed_source": "explicit", "depth": 1,
               "max_nodes": 100, "chunk_size": 10})
    );

    let mut sent = HashSet::new();
    for (index, chunk) in events.iter().filter(|e| e.event == EVENT_CHUNK).enumerate() {
        assert_eq!(chunk.data["index"], json!(index));
        let nodes = chunk.data["nodes"].as_array().unwrap();
        assert!(nodes.len() <= 10);
        for node in nodes {
            sent.insert(node["id"].as_str().unwrap().to_string());
            assert!(
                node.get("text").is_none(),
                "the compact shape, not the property bag"
            );
        }
        for link in chunk.data["links"].as_array().unwrap() {
            assert!(sent.contains(link["source"].as_str().unwrap()));
            assert!(sent.contains(link["target"].as_str().unwrap()));
        }
    }

    assert_eq!(
        events.last().unwrap().data,
        json!({"nodes": 26, "links": 25, "chunks": 3})
    );
    let summaries: Vec<&Value> = events
        .iter()
        .filter(|e| e.event == EVENT_SUMMARY)
        .map(|e| &e.data)
        .collect();
    assert!(
        summaries
            .iter()
            .all(|p| p["nodes"].as_object().unwrap().len() <= 10)
    );
    assert!(summaries[0].get("color_maps").is_some());
    assert_eq!(summaries[0]["color_maps"], json!({"node_set": {}}));
    assert!(summaries[1..].iter().all(|p| p.get("color_maps").is_none()));
    let merged: HashMap<&String, &Value> = summaries
        .iter()
        .flat_map(|p| p["nodes"].as_object().unwrap().iter())
        .collect();
    assert_eq!(
        merged.keys().map(|k| k.to_string()).collect::<HashSet<_>>(),
        sent
    );
    assert_eq!(merged[&"hub".to_string()]["importance"], json!(1.0));

    read_graph_stream(&events).expect("the frontend accepts the stream");
}

#[tokio::test]
async fn chunk_nodes_are_named_from_their_text() {
    let db = star(3).await;
    let events = events(&db, &hub_bounds(), 10).await;
    let names: HashMap<String, String> = events
        .iter()
        .filter(|e| e.event == EVENT_CHUNK)
        .flat_map(|e| e.data["nodes"].as_array().unwrap().clone())
        .map(|n| {
            (
                n["id"].as_str().unwrap().to_string(),
                n["name"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert!(names["c1"].starts_with("chunk 1 chunk 1"));
    assert_eq!(names["c1"].chars().count(), 120);
}

#[tokio::test]
async fn no_seeds_is_an_empty_graph_not_an_error() {
    let db = MockGraphDB::new();
    let events = events(&db, &VisualizationBounds::default(), STREAM_CHUNK_SIZE).await;
    let names: Vec<&str> = events.iter().map(|e| e.event).collect();
    assert_eq!(names, [EVENT_META, EVENT_SUMMARY, EVENT_DONE]);
    assert_eq!(events[0].data["seed_source"], json!("none"));
    assert_eq!(
        events[1].data,
        json!({"nodes": {}, "color_maps": {"node_set": {}}})
    );
    assert_eq!(events[2].data, json!({"nodes": 0, "links": 0, "chunks": 0}));
    assert_eq!(read_graph_stream(&events).unwrap(), Vec::<Value>::new());
}

#[tokio::test]
async fn degree_seeding_and_entity_types_reach_the_stream() {
    // alice is_a Person; seeded on the chunk at depth 1 the Person node is
    // outside the read, yet alice streams typed.
    let db = MockGraphDB::new();
    for node in [
        json!({"id": "alice", "type": "Entity", "name": "Alice"}),
        json!({"id": "person", "type": "EntityType", "name": "Person"}),
        json!({"id": "chunk", "type": "DocumentChunk", "text": "Alice"}),
    ] {
        db.add_node_raw(node).await.unwrap();
    }
    db.add_edge("alice", "person", "is_a", None).await.unwrap();
    db.add_edge("chunk", "alice", "contains", None)
        .await
        .unwrap();
    let bounds = VisualizationBounds {
        seed_node_ids: vec!["chunk".to_string()],
        depth: 1,
        ..Default::default()
    };
    let events = events(&db, &bounds, 1).await;
    let nodes = read_graph_stream(&events).unwrap();
    let alice = nodes.iter().find(|n| n["id"] == "alice").unwrap();
    assert_eq!(alice["entity_type"], json!("Person"));
    assert!(nodes.iter().all(|n| n["id"] != "person"));
    // The internal key never reaches the wire.
    assert!(alice.get("_semantic_type").is_none());

    // No seed at all: the highest-degree node seeds the read.
    let events = prepare_graph_stream(&db, &VisualizationBounds::default(), None, 10)
        .await
        .unwrap()
        .collect::<Vec<_>>();
    assert_eq!(
        events[0].data["seed_source"],
        json!(SeedSource::Degree.as_str())
    );
    assert_eq!(events[0].data["seeds"][0], json!("alice"));
    read_graph_stream(&events).unwrap();
}

#[test]
fn a_late_entity_type_is_corrected_and_the_frontend_applies_it() {
    // Built by hand: alice in chunk 0 untyped, her is_a link and type in chunk 1.
    let node = |id: &str, ty: &str, name: &str| BoundedNode {
        id: id.to_string(),
        hops: 0,
        data: [("type".into(), json!(ty)), ("name".into(), json!(name))]
            .into_iter()
            .collect(),
    };
    let neighborhood = BoundedNeighborhood {
        nodes: vec![
            node("alice", "Entity", "Alice"),
            node("person", "EntityType", "Person"),
        ],
        edges: vec![(
            "alice".to_string(),
            "person".to_string(),
            "is_a".to_string(),
            HashMap::new(),
        )],
    };
    let meta = GraphStreamMeta {
        seeds: vec!["alice".to_string()],
        seed_source: SeedSource::Explicit,
        depth: 1,
        max_nodes: 10,
        chunk_size: 1,
    };
    let events: Vec<_> = GraphStreamEvents::new(meta, neighborhood).collect();
    let first_chunk = &events[1].data;
    assert_eq!(first_chunk["nodes"][0]["entity_type"], json!("Entity"));
    assert_eq!(first_chunk["links"], json!([]));
    let nodes = read_graph_stream(&events).unwrap();
    assert_eq!(nodes[0]["entity_type"], json!("Person"));
    assert_eq!(nodes[0]["label_priority"], json!(true));
}

#[test]
fn the_frontend_check_rejects_a_stream_that_lost_a_chunk() {
    let meta = GraphStreamMeta {
        seeds: vec!["a".to_string()],
        seed_source: SeedSource::Explicit,
        depth: 1,
        max_nodes: 10,
        chunk_size: 1,
    };
    let neighborhood = BoundedNeighborhood {
        nodes: ["a", "b"]
            .iter()
            .map(|id| BoundedNode {
                id: id.to_string(),
                hops: 0,
                data: HashMap::new(),
            })
            .collect(),
        edges: vec![],
    };
    let mut events: Vec<_> = GraphStreamEvents::new(meta, neighborhood).collect();
    assert!(read_graph_stream(&events).is_ok());
    events.remove(2); // the second chunk
    assert!(
        read_graph_stream(&events)
            .unwrap_err()
            .contains("lost part")
    );
}

#[tokio::test]
async fn a_full_read_does_not_stream_and_bad_bounds_fail_before_any_event() {
    let db = star(2).await;
    let err = prepare_graph_stream(&db, &VisualizationBounds::full_graph(), None, 10)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        VisualizationError::InvalidBounds(BoundsError::FullStream)
    ));
    let bad = VisualizationBounds {
        depth: 0,
        ..hub_bounds()
    };
    assert!(prepare_graph_stream(&db, &bad, None, 10).await.is_err());
}

#[test]
fn error_event_shape() {
    let event = error_event("Failed to build the visualization payload", 409);
    assert_eq!(event.event, "error");
    assert_eq!(
        event.data,
        json!({"message": "Failed to build the visualization payload", "status": 409})
    );
}
