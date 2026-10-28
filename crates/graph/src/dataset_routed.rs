//! The graph engine of per-dataset storage (`ENABLE_BACKEND_ACCESS_CONTROL`).
//!
//! [`DatasetRoutedGraphDB`] owns no store. Every call forwards to the engine of
//! the dataset in the current [`cognee_models::dataset_scope`], resolved
//! through a [`DatasetGraphResolver`]. A call outside every dataset scope
//! fails instead of reaching some shared store: under per-dataset storage
//! there is none, and guessing would read or write another dataset's data.
//! See `docs/design/per-dataset-storage.md`.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use uuid::Uuid;

use cognee_models::dataset_scope;

use crate::temporal::{TemporalAnchors, WindowTimestamp};
use crate::traits::{EdgeKey, GraphDBTrait, NeighborhoodScope, NodeTruthState};
use crate::{EdgeData, GraphDBError, GraphDBResult, GraphNode, NodeData};

/// The message of a graph call made outside every dataset scope.
pub const NO_DATASET_SCOPE: &str = "no dataset scope: per-dataset storage \
     (ENABLE_BACKEND_ACCESS_CONTROL) requires every graph access to run inside a dataset \
     (cognee_models::dataset_scope::with_dataset)";

/// Resolves a dataset's own graph engine.
#[async_trait]
pub trait DatasetGraphResolver: Send + Sync {
    /// The dataset's engine, provisioning its database on first use.
    async fn graph_for_dataset(&self, dataset_id: Uuid) -> GraphDBResult<Arc<dyn GraphDBTrait>>;
    /// The dataset's engine if it is already open; never provisions. For the
    /// synchronous paths.
    fn cached_graph(&self, dataset_id: Uuid) -> Option<Arc<dyn GraphDBTrait>>;
    /// Every open engine, for `flush` and `close` outside a dataset scope.
    fn cached_graphs(&self) -> Vec<Arc<dyn GraphDBTrait>>;
}

/// A graph engine that routes each call to the current dataset's engine.
pub struct DatasetRoutedGraphDB {
    resolver: Arc<dyn DatasetGraphResolver>,
}

impl DatasetRoutedGraphDB {
    pub fn new(resolver: Arc<dyn DatasetGraphResolver>) -> Self {
        Self { resolver }
    }

    async fn engine(&self) -> GraphDBResult<Arc<dyn GraphDBTrait>> {
        match dataset_scope::current_dataset() {
            Some(dataset_id) => self.resolver.graph_for_dataset(dataset_id).await,
            None => Err(GraphDBError::ConnectionError(NO_DATASET_SCOPE.to_string())),
        }
    }
}

#[async_trait]
impl GraphDBTrait for DatasetRoutedGraphDB {
    /// Each dataset's engine is initialized when it is built; outside a scope
    /// there is nothing to initialize (startup wiring calls this).
    async fn initialize(&self) -> GraphDBResult<()> {
        match dataset_scope::current_dataset() {
            Some(_) => self.engine().await?.initialize().await,
            None => Ok(()),
        }
    }

    /// Close every open dataset engine: the process-level teardown, never a
    /// per-dataset one. Like any closed store, the engines fail their next
    /// operation.
    async fn close(&self) -> GraphDBResult<()> {
        let mut first_error = None;
        for engine in self.resolver.cached_graphs() {
            if let Err(error) = engine.close().await {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// The current dataset's engine inside a scope; every open engine outside
    /// one (a periodic or teardown checkpoint). `true` when any flushed.
    async fn flush(&self) -> GraphDBResult<bool> {
        if dataset_scope::current_dataset().is_some() {
            return self.engine().await?.flush().await;
        }
        let mut flushed = false;
        for engine in self.resolver.cached_graphs() {
            flushed |= engine.flush().await?;
        }
        Ok(flushed)
    }

    async fn is_empty(&self) -> GraphDBResult<bool> {
        self.engine().await?.is_empty().await
    }

    async fn query(
        &self,
        query: &str,
        params: Option<HashMap<Cow<'static, str>, serde_json::Value>>,
    ) -> GraphDBResult<Vec<Vec<serde_json::Value>>> {
        self.engine().await?.query(query, params).await
    }

    async fn delete_graph(&self) -> GraphDBResult<()> {
        self.engine().await?.delete_graph().await
    }

    async fn has_node(&self, node_id: &str) -> GraphDBResult<bool> {
        self.engine().await?.has_node(node_id).await
    }

    async fn add_node_raw(&self, node: Value) -> GraphDBResult<()> {
        self.engine().await?.add_node_raw(node).await
    }

    async fn add_nodes_raw(&self, nodes: Vec<Value>) -> GraphDBResult<()> {
        self.engine().await?.add_nodes_raw(nodes).await
    }

    async fn delete_node(&self, node_id: &str) -> GraphDBResult<()> {
        self.engine().await?.delete_node(node_id).await
    }

    async fn delete_nodes(&self, node_ids: &[String]) -> GraphDBResult<()> {
        self.engine().await?.delete_nodes(node_ids).await
    }

    async fn get_node(&self, node_id: &str) -> GraphDBResult<Option<NodeData>> {
        self.engine().await?.get_node(node_id).await
    }

    async fn get_nodes(&self, node_ids: &[String]) -> GraphDBResult<Vec<NodeData>> {
        self.engine().await?.get_nodes(node_ids).await
    }

    async fn has_edge(
        &self,
        source_id: &str,
        target_id: &str,
        relationship_name: &str,
    ) -> GraphDBResult<bool> {
        self.engine()
            .await?
            .has_edge(source_id, target_id, relationship_name)
            .await
    }

    async fn has_edges(&self, edges: &[EdgeData]) -> GraphDBResult<Vec<EdgeData>> {
        self.engine().await?.has_edges(edges).await
    }

    async fn add_edge(
        &self,
        source_id: &str,
        target_id: &str,
        relationship_name: &str,
        properties: Option<HashMap<Cow<'static, str>, serde_json::Value>>,
    ) -> GraphDBResult<()> {
        self.engine()
            .await?
            .add_edge(source_id, target_id, relationship_name, properties)
            .await
    }

    async fn add_edges(&self, edges: &[EdgeData]) -> GraphDBResult<()> {
        self.engine().await?.add_edges(edges).await
    }

    async fn get_edges(&self, node_id: &str) -> GraphDBResult<Vec<EdgeData>> {
        self.engine().await?.get_edges(node_id).await
    }

    async fn get_neighbors(&self, node_id: &str) -> GraphDBResult<Vec<NodeData>> {
        self.engine().await?.get_neighbors(node_id).await
    }

    async fn get_connections(
        &self,
        node_id: &str,
    ) -> GraphDBResult<
        Vec<(
            NodeData,
            HashMap<Cow<'static, str>, serde_json::Value>,
            NodeData,
        )>,
    > {
        self.engine().await?.get_connections(node_id).await
    }

    async fn get_graph_data(&self) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
        self.engine().await?.get_graph_data().await
    }

    async fn get_graph_metrics(
        &self,
        include_optional: bool,
    ) -> GraphDBResult<HashMap<Cow<'static, str>, serde_json::Value>> {
        self.engine()
            .await?
            .get_graph_metrics(include_optional)
            .await
    }

    async fn get_filtered_graph_data(
        &self,
        attribute_filters: &HashMap<Cow<'static, str>, Vec<serde_json::Value>>,
    ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
        self.engine()
            .await?
            .get_filtered_graph_data(attribute_filters)
            .await
    }

    async fn get_nodeset_subgraph(
        &self,
        node_type: &str,
        node_names: &[String],
        node_name_filter_operator: &str,
    ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
        self.engine()
            .await?
            .get_nodeset_subgraph(node_type, node_names, node_name_filter_operator)
            .await
    }

    async fn get_degree_one_nodes(&self, node_type: &str) -> GraphDBResult<Vec<GraphNode>> {
        self.engine().await?.get_degree_one_nodes(node_type).await
    }

    async fn get_all_relationship_names(&self) -> GraphDBResult<HashSet<String>> {
        self.engine().await?.get_all_relationship_names().await
    }

    async fn get_zero_degree_edge_type_nodes(&self) -> GraphDBResult<Vec<GraphNode>> {
        self.engine().await?.get_zero_degree_edge_type_nodes().await
    }

    async fn update_node_property(
        &self,
        node_id: &str,
        key: &str,
        value: serde_json::Value,
    ) -> GraphDBResult<()> {
        self.engine()
            .await?
            .update_node_property(node_id, key, value)
            .await
    }

    async fn update_edge_property(
        &self,
        source_id: &str,
        target_id: &str,
        relationship_name: &str,
        key: &str,
        value: serde_json::Value,
    ) -> GraphDBResult<()> {
        self.engine()
            .await?
            .update_edge_property(source_id, target_id, relationship_name, key, value)
            .await
    }

    async fn get_node_feedback_weights(
        &self,
        node_ids: &[String],
    ) -> GraphDBResult<HashMap<String, f64>> {
        self.engine()
            .await?
            .get_node_feedback_weights(node_ids)
            .await
    }

    async fn set_node_feedback_weights(
        &self,
        updates: &HashMap<String, f64>,
    ) -> GraphDBResult<HashMap<String, bool>> {
        self.engine()
            .await?
            .set_node_feedback_weights(updates)
            .await
    }

    /// The scoped dataset's engine answers. When it cannot be resolved the
    /// error is returned, so the build reports the stage `errored` instead of
    /// skipping it as unsupported or mistaking it for an empty graph.
    async fn supports_truth_state(&self) -> GraphDBResult<bool> {
        self.engine().await?.supports_truth_state().await
    }

    async fn supports_feedback_weights(&self) -> GraphDBResult<bool> {
        self.engine().await?.supports_feedback_weights().await
    }

    async fn get_node_truth_state(
        &self,
        node_ids: &[String],
    ) -> GraphDBResult<HashMap<String, NodeTruthState>> {
        self.engine().await?.get_node_truth_state(node_ids).await
    }

    async fn set_node_truth_state(
        &self,
        updates: &HashMap<String, NodeTruthState>,
    ) -> GraphDBResult<HashMap<String, bool>> {
        self.engine().await?.set_node_truth_state(updates).await
    }

    async fn get_edge_feedback_weights(
        &self,
        edge_keys: &[EdgeKey],
    ) -> GraphDBResult<HashMap<EdgeKey, f64>> {
        self.engine()
            .await?
            .get_edge_feedback_weights(edge_keys)
            .await
    }

    async fn set_edge_feedback_weights(
        &self,
        updates: &HashMap<EdgeKey, f64>,
    ) -> GraphDBResult<HashMap<EdgeKey, bool>> {
        self.engine()
            .await?
            .set_edge_feedback_weights(updates)
            .await
    }

    async fn get_id_filtered_graph_data(
        &self,
        node_ids: &[String],
    ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
        self.engine()
            .await?
            .get_id_filtered_graph_data(node_ids)
            .await
    }

    async fn get_candidate_nodes_by_label(&self, needle: &str) -> GraphDBResult<Vec<GraphNode>> {
        self.engine()
            .await?
            .get_candidate_nodes_by_label(needle)
            .await
    }

    async fn get_neighborhood(
        &self,
        node_ids: &[String],
        depth: usize,
    ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
        self.engine().await?.get_neighborhood(node_ids, depth).await
    }

    async fn get_neighborhood_scoped(
        &self,
        node_ids: &[String],
        depth: usize,
        scope: &NeighborhoodScope,
    ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
        self.engine()
            .await?
            .get_neighborhood_scoped(node_ids, depth, scope)
            .await
    }

    async fn get_timestamps_in_range(
        &self,
        start_ms: Option<i64>,
        end_ms: Option<i64>,
    ) -> GraphDBResult<Vec<WindowTimestamp>> {
        self.engine()
            .await?
            .get_timestamps_in_range(start_ms, end_ms)
            .await
    }

    async fn get_temporal_anchors(
        &self,
        chunk_ids: &[String],
        entity_ids: &[String],
        start_ms: Option<i64>,
        end_ms: Option<i64>,
    ) -> GraphDBResult<TemporalAnchors> {
        self.engine()
            .await?
            .get_temporal_anchors(chunk_ids, entity_ids, start_ms, end_ms)
            .await
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;
    use crate::MockGraphDB;

    /// One mock graph per dataset, created on first use.
    #[derive(Default)]
    struct PerDataset {
        engines: Mutex<HashMap<Uuid, Arc<dyn GraphDBTrait>>>,
    }

    #[async_trait]
    impl DatasetGraphResolver for PerDataset {
        async fn graph_for_dataset(
            &self,
            dataset_id: Uuid,
        ) -> GraphDBResult<Arc<dyn GraphDBTrait>> {
            // lock poison is unrecoverable
            let mut engines = self.engines.lock().unwrap();
            Ok(Arc::clone(engines.entry(dataset_id).or_insert_with(|| {
                Arc::new(MockGraphDB::new()) as Arc<dyn GraphDBTrait>
            })))
        }

        fn cached_graph(&self, dataset_id: Uuid) -> Option<Arc<dyn GraphDBTrait>> {
            // lock poison is unrecoverable
            self.engines.lock().unwrap().get(&dataset_id).cloned()
        }

        fn cached_graphs(&self) -> Vec<Arc<dyn GraphDBTrait>> {
            // lock poison is unrecoverable
            self.engines.lock().unwrap().values().cloned().collect()
        }
    }

    fn node(id: &str) -> Value {
        json!({"id": id, "name": id, "type": "Entity"})
    }

    #[tokio::test]
    async fn truth_state_probe_forwards_to_the_scoped_engine() {
        let resolver = Arc::new(PerDataset::default());
        let graph = DatasetRoutedGraphDB::new(resolver.clone());
        let (supported, unsupported) = (Uuid::new_v4(), Uuid::new_v4());
        let mock = MockGraphDB::new();
        mock.set_supports_truth_state(false);
        resolver
            .engines
            .lock()
            .unwrap()
            .insert(unsupported, Arc::new(mock) as Arc<dyn GraphDBTrait>);

        assert!(
            dataset_scope::with_dataset(supported, graph.supports_truth_state())
                .await
                .unwrap()
        );
        assert!(
            !dataset_scope::with_dataset(unsupported, graph.supports_truth_state())
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn feedback_weights_probe_forwards_to_the_scoped_engine() {
        let resolver = Arc::new(PerDataset::default());
        let graph = DatasetRoutedGraphDB::new(resolver.clone());
        let (supported, unsupported) = (Uuid::new_v4(), Uuid::new_v4());
        let mock = MockGraphDB::new();
        mock.set_supports_feedback_weights(false);
        resolver
            .engines
            .lock()
            .unwrap()
            .insert(unsupported, Arc::new(mock) as Arc<dyn GraphDBTrait>);

        assert!(
            dataset_scope::with_dataset(supported, graph.supports_feedback_weights())
                .await
                .unwrap()
        );
        assert!(
            !dataset_scope::with_dataset(unsupported, graph.supports_feedback_weights())
                .await
                .unwrap()
        );
        assert!(graph.supports_feedback_weights().await.is_err());
    }

    /// Outside every dataset scope the engine cannot be resolved; the probe
    /// returns that error rather than guessing an answer.
    #[tokio::test]
    async fn truth_state_probe_errors_outside_a_dataset_scope() {
        let graph = DatasetRoutedGraphDB::new(Arc::new(PerDataset::default()));
        let error = graph.supports_truth_state().await.unwrap_err();
        assert!(error.to_string().contains("no dataset scope"), "{error}");
    }

    #[tokio::test]
    async fn each_dataset_sees_only_its_own_nodes() {
        let resolver = Arc::new(PerDataset::default());
        let graph = DatasetRoutedGraphDB::new(resolver.clone());
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());

        dataset_scope::with_dataset(a, graph.add_node_raw(node("only-in-a")))
            .await
            .unwrap();
        dataset_scope::with_dataset(b, graph.add_node_raw(node("only-in-b")))
            .await
            .unwrap();

        assert!(
            dataset_scope::with_dataset(a, graph.has_node("only-in-a"))
                .await
                .unwrap()
        );
        assert!(
            !dataset_scope::with_dataset(a, graph.has_node("only-in-b"))
                .await
                .unwrap()
        );
        assert!(
            !dataset_scope::with_dataset(b, graph.has_node("only-in-a"))
                .await
                .unwrap()
        );
        assert_eq!(resolver.cached_graphs().len(), 2);
    }

    #[tokio::test]
    async fn a_call_outside_every_scope_fails_closed() {
        let graph = DatasetRoutedGraphDB::new(Arc::new(PerDataset::default()));
        let error = graph.has_node("x").await.unwrap_err();
        assert!(error.to_string().contains("no dataset scope"), "{error}");
        // Process-level calls stay usable outside a scope.
        graph.initialize().await.unwrap();
        assert!(!graph.flush().await.unwrap());
        graph.close().await.unwrap();
    }

    #[tokio::test]
    async fn close_reaches_every_open_engine() {
        let resolver = Arc::new(PerDataset::default());
        let graph = DatasetRoutedGraphDB::new(resolver.clone());
        for _ in 0..2 {
            dataset_scope::with_dataset(Uuid::new_v4(), graph.is_empty())
                .await
                .unwrap();
        }
        assert_eq!(resolver.cached_graphs().len(), 2);
        graph.close().await.unwrap();
    }
}
