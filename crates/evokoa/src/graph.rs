use std::borrow::Cow;
use std::collections::HashMap;

use async_trait::async_trait;
use cognee_graph::{EdgeData, GraphDBResult, GraphDBTrait, GraphNode, NodeData, PgGraphAdapter};
use sea_orm::{ConnectionTrait, DatabaseConnection};
use serde_json::Value;

/// pgGraph-accelerated graph adapter.
///
/// Cognee's existing Postgres tables remain authoritative. pgGraph is installed
/// and configured as a derived traversal index over `graph_node` and
/// `graph_edge`; CRUD delegates to the proven adapter.
pub struct EvokoaGraphAdapter {
    inner: PgGraphAdapter,
    db: DatabaseConnection,
}

impl EvokoaGraphAdapter {
    pub async fn from_connection(db: DatabaseConnection) -> GraphDBResult<Self> {
        let inner = PgGraphAdapter::from_connection(db.clone()).await?;
        Ok(Self { inner, db })
    }

    pub fn connection(&self) -> &DatabaseConnection {
        &self.db
    }

    async fn install_and_register(&self) -> GraphDBResult<()> {
        // Registration is deliberately idempotent at the adapter level. pgGraph
        // currently has no `IF NOT EXISTS` overload for mappings, so inspect its
        // catalogs through graph_map and register only an empty default graph.
        self.db
            .execute_unprepared("CREATE EXTENSION IF NOT EXISTS graph")
            .await
            .map_err(|e| {
                cognee_graph::GraphDBError::InitializationError(format!(
                    "pgGraph installation failed: {e}"
                ))
            })?;
        let registration = r#"
            DO $cognee$
            BEGIN
              IF NOT EXISTS (
                SELECT 1 FROM graph.registered_tables()
                WHERE pg_catalog.to_regclass(table_name) = 'graph_node'::regclass
              ) THEN
                PERFORM graph.add_table(
                  'graph_node'::regclass, 'id',
                  ARRAY['name','type','properties']
                );
              END IF;
              IF NOT EXISTS (
                SELECT 1 FROM graph.registered_edges()
                WHERE pg_catalog.to_regclass(from_table) = 'graph_edge'::regclass
              ) THEN
                PERFORM graph.add_edge(
                  'graph_edge'::regclass, 'source_id',
                  'graph_node'::regclass, 'target_id',
                  'related_to', false, NULL, 'relationship_name'
                );
              END IF;
            END
            $cognee$;
        "#;
        self.db
            .execute_unprepared(registration)
            .await
            .map_err(|e| {
                cognee_graph::GraphDBError::InitializationError(format!(
                    "pgGraph registration failed: {e}"
                ))
            })?;
        // pgGraph serializes maintenance operations. Registration, sync trigger
        // installation, and the initial build must not share one transaction.
        self.db
            .execute_unprepared("SELECT graph.enable_sync()")
            .await
            .map_err(|e| {
                cognee_graph::GraphDBError::InitializationError(format!(
                    "pgGraph sync setup failed: {e}"
                ))
            })?;
        self.db
            .execute_unprepared("SELECT * FROM graph.build()")
            .await
            .map_err(|e| {
                cognee_graph::GraphDBError::InitializationError(format!(
                    "pgGraph initial build failed: {e}"
                ))
            })?;
        Ok(())
    }
}

#[async_trait]
impl GraphDBTrait for EvokoaGraphAdapter {
    async fn initialize(&self) -> GraphDBResult<()> {
        self.inner.initialize().await?;
        self.install_and_register().await
    }

    async fn close(&self) -> GraphDBResult<()> {
        self.inner.close().await
    }
    async fn is_empty(&self) -> GraphDBResult<bool> {
        self.inner.is_empty().await
    }
    async fn query(
        &self,
        query: &str,
        params: Option<HashMap<Cow<'static, str>, Value>>,
    ) -> GraphDBResult<Vec<Vec<Value>>> {
        self.inner.query(query, params).await
    }
    async fn delete_graph(&self) -> GraphDBResult<()> {
        self.inner.delete_graph().await
    }
    async fn has_node(&self, node_id: &str) -> GraphDBResult<bool> {
        self.inner.has_node(node_id).await
    }
    async fn add_node_raw(&self, node: Value) -> GraphDBResult<()> {
        self.inner.add_node_raw(node).await
    }
    async fn add_nodes_raw(&self, nodes: Vec<Value>) -> GraphDBResult<()> {
        self.inner.add_nodes_raw(nodes).await
    }
    async fn delete_node(&self, node_id: &str) -> GraphDBResult<()> {
        self.inner.delete_node(node_id).await
    }
    async fn delete_nodes(&self, node_ids: &[String]) -> GraphDBResult<()> {
        self.inner.delete_nodes(node_ids).await
    }
    async fn get_node(&self, node_id: &str) -> GraphDBResult<Option<NodeData>> {
        self.inner.get_node(node_id).await
    }
    async fn get_nodes(&self, node_ids: &[String]) -> GraphDBResult<Vec<NodeData>> {
        self.inner.get_nodes(node_ids).await
    }
    async fn has_edge(
        &self,
        source_id: &str,
        target_id: &str,
        relationship_name: &str,
    ) -> GraphDBResult<bool> {
        self.inner
            .has_edge(source_id, target_id, relationship_name)
            .await
    }
    async fn has_edges(&self, edges: &[EdgeData]) -> GraphDBResult<Vec<EdgeData>> {
        self.inner.has_edges(edges).await
    }
    async fn add_edge(
        &self,
        source_id: &str,
        target_id: &str,
        relationship_name: &str,
        properties: Option<HashMap<Cow<'static, str>, Value>>,
    ) -> GraphDBResult<()> {
        self.inner
            .add_edge(source_id, target_id, relationship_name, properties)
            .await
    }
    async fn add_edges(&self, edges: &[EdgeData]) -> GraphDBResult<()> {
        self.inner.add_edges(edges).await
    }
    async fn get_edges(&self, node_id: &str) -> GraphDBResult<Vec<EdgeData>> {
        self.inner.get_edges(node_id).await
    }
    async fn get_neighbors(&self, node_id: &str) -> GraphDBResult<Vec<NodeData>> {
        self.inner.get_neighbors(node_id).await
    }
    async fn get_connections(
        &self,
        node_id: &str,
    ) -> GraphDBResult<Vec<(NodeData, HashMap<Cow<'static, str>, Value>, NodeData)>> {
        self.inner.get_connections(node_id).await
    }
    async fn get_graph_data(&self) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
        self.inner.get_graph_data().await
    }
    async fn get_graph_metrics(
        &self,
        include_optional: bool,
    ) -> GraphDBResult<HashMap<Cow<'static, str>, Value>> {
        self.inner.get_graph_metrics(include_optional).await
    }
    async fn get_filtered_graph_data(
        &self,
        filters: &HashMap<Cow<'static, str>, Vec<Value>>,
    ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
        self.inner.get_filtered_graph_data(filters).await
    }
    async fn get_nodeset_subgraph(
        &self,
        node_type: &str,
        node_names: &[String],
        op: &str,
    ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
        self.inner
            .get_nodeset_subgraph(node_type, node_names, op)
            .await
    }
    async fn update_node_property(
        &self,
        node_id: &str,
        property_name: &str,
        property_value: Value,
    ) -> GraphDBResult<()> {
        self.inner
            .update_node_property(node_id, property_name, property_value)
            .await
    }
    async fn update_edge_property(
        &self,
        source_id: &str,
        target_id: &str,
        relationship_name: &str,
        property_name: &str,
        property_value: Value,
    ) -> GraphDBResult<()> {
        self.inner
            .update_edge_property(
                source_id,
                target_id,
                relationship_name,
                property_name,
                property_value,
            )
            .await
    }
    async fn get_id_filtered_graph_data(
        &self,
        node_ids: &[String],
    ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
        self.inner.get_id_filtered_graph_data(node_ids).await
    }
    async fn get_neighborhood(
        &self,
        node_ids: &[String],
        depth: usize,
    ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
        self.inner.get_neighborhood(node_ids, depth).await
    }
}
