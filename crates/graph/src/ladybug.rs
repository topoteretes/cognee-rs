//!
//! Implementation of GraphDBTrait using Ladybug (lbug) embedded graph database.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use lbug::{Connection, Database, SystemConfig, Value as LbugValue};
use serde_json::{Value, json};
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use crate::{EdgeData, GraphDBError, GraphDBResult, GraphDBTrait, GraphNode, NodeData};

/// Ladybug graph database adapter.
///
/// This adapter provides a complete implementation of GraphDBTrait using
/// the Ladybug (lbug) embedded graph database.
///
/// # Schema
///
/// The adapter creates the following schema:
///
/// ```cypher
/// CREATE NODE TABLE Node (
///     id STRING PRIMARY KEY,
///     name STRING,
///     type STRING,
///     created_at TIMESTAMP,
///     updated_at TIMESTAMP,
///     properties STRING  -- JSON-encoded additional properties
/// )
///
/// CREATE REL TABLE EDGE (
///     FROM Node TO Node,
///     relationship_name STRING,
///     created_at TIMESTAMP,
///     updated_at TIMESTAMP,
///     properties STRING  -- JSON-encoded edge properties
/// )
/// ```
///
/// # Example
///
/// ```ignore
/// use cognee_graph::{LadybugAdapter, GraphDBTrait};
/// use cognee_models::Entity;
///
/// let adapter = LadybugAdapter::new("./my_graph").await?;
/// adapter.initialize().await?;
///
/// // Add nodes
/// let entity = Entity::new("Alice", entity_type, Some("dataset-1"));
/// adapter.add_node(&entity).await?;
/// ```
pub struct LadybugAdapter {
    db_path: String,
    db: Arc<Database>,
}

impl LadybugAdapter {
    /// Number of columns in node query: id, name, type, properties
    pub const NODE_QUERY_COLUMN_COUNT: usize = 4;

    /// Number of columns in edge query: source_id, target_id, relationship_name, properties
    pub const EDGE_QUERY_COLUMN_COUNT: usize = 4;

    /// Create a new Ladybug adapter.
    ///
    /// # Arguments
    /// * `db_path` - Path to the database directory
    ///
    /// # Returns
    /// A new LadybugAdapter instance
    ///
    /// # Errors
    /// Returns GraphDBError::InitializationError if database creation fails
    ///
    pub async fn new(db_path: &str) -> GraphDBResult<Self> {
        let db = Database::new(db_path, SystemConfig::default()).map_err(|e| {
            GraphDBError::InitializationError(format!("Failed to create database: {}", e))
        })?;

        Ok(Self {
            db_path: db_path.to_string(),
            db: Arc::new(db),
        })
    }

    /// Get the database path.
    pub fn db_path(&self) -> &str {
        &self.db_path
    }

    /// Execute a query and convert results to JSON values.
    ///
    /// Helper method that executes a Cypher query and converts the QueryResult
    /// to a Vec of Vec of JSON values for easier processing.
    fn execute_query(&self, query: &str) -> GraphDBResult<Vec<Vec<serde_json::Value>>> {
        let conn = Connection::new(&self.db).map_err(|e| {
            GraphDBError::ConnectionError(format!("Failed to create connection: {}", e))
        })?;

        let result = conn
            .query(query)
            .map_err(|e| GraphDBError::QueryError(format!("Query failed: {}", e)))?;

        let rows: Vec<Vec<serde_json::Value>> = result
            .map(|row| row.into_iter().map(Self::lbug_value_to_json).collect())
            .collect();

        Ok(rows)
    }

    /// Convert lbug::Value to serde_json::Value
    /// Convert lbug::Value to serde_json::Value
    fn lbug_value_to_json(value: LbugValue) -> serde_json::Value {
        use LbugValue::*;
        match value {
            Null(_) => serde_json::Value::Null,
            Bool(b) => serde_json::Value::Bool(b),
            Int8(i) => json!(i),
            Int16(i) => json!(i),
            Int32(i) => json!(i),
            Int64(i) => json!(i),
            UInt8(i) => json!(i),
            UInt16(i) => json!(i),
            UInt32(i) => json!(i),
            UInt64(i) => json!(i),
            Int128(i) => json!(i.to_string()),
            UUID(u) => json!(u.to_string()),
            Float(f) => json!(f),
            Double(d) => json!(d),
            String(s) => serde_json::Value::String(s),
            Blob(b) => json!(format!("<blob {} bytes>", b.len())),
            Date(d) => json!(d.to_string()),
            Timestamp(ts) => json!(ts.to_string()),
            TimestampTz(ts) => json!(ts.to_string()),
            TimestampNs(ts) => json!(ts.to_string()),
            TimestampMs(ts) => json!(ts.to_string()),
            TimestampSec(ts) => json!(ts.to_string()),
            Interval(interval) => json!(format!("{:?}", interval)),
            InternalID(id) => json!(format!("{:?}", id)),
            Node(node) => {
                let mut obj = serde_json::Map::new();
                obj.insert("id".to_string(), json!(format!("{:?}", node.get_node_id())));
                obj.insert("label".to_string(), json!(node.get_label_name()));
                for (key, val) in node.get_properties() {
                    obj.insert(key.to_string(), Self::lbug_value_to_json(val.clone()));
                }
                serde_json::Value::Object(obj)
            }
            Rel(rel) => {
                let mut obj = serde_json::Map::new();
                obj.insert("label".to_string(), json!(rel.get_label_name()));
                for (key, val) in rel.get_properties() {
                    obj.insert(key.to_string(), Self::lbug_value_to_json(val.clone()));
                }
                serde_json::Value::Object(obj)
            }
            List(_, list) | Array(_, list) => {
                let arr: Vec<serde_json::Value> = list
                    .iter()
                    .map(|v| Self::lbug_value_to_json(v.clone()))
                    .collect();
                serde_json::Value::Array(arr)
            }
            Struct(fields) => {
                let mut obj = serde_json::Map::new();
                for (key, val) in fields {
                    obj.insert(key.clone(), Self::lbug_value_to_json(val.clone()));
                }
                serde_json::Value::Object(obj)
            }
            Map(_, pairs) => {
                let mut obj = serde_json::Map::new();
                for (i, (k, v)) in pairs.iter().enumerate() {
                    obj.insert(format!("key_{}", i), Self::lbug_value_to_json(k.clone()));
                    obj.insert(format!("val_{}", i), Self::lbug_value_to_json(v.clone()));
                }
                serde_json::Value::Object(obj)
            }
            Union { types, value } => {
                let mut obj = serde_json::Map::new();
                obj.insert("union_type_count".to_string(), json!(types.len()));
                obj.insert(
                    "value".to_string(),
                    Self::lbug_value_to_json((*value).clone()),
                );
                serde_json::Value::Object(obj)
            }
            Decimal(d) => json!(d.to_string()),
            RecursiveRel { nodes, rels } => {
                let mut obj = serde_json::Map::new();
                obj.insert("nodes".to_string(), json!(nodes.len()));
                obj.insert("rels".to_string(), json!(rels.len()));
                serde_json::Value::Object(obj)
            }
        }
    }
    /// Build a node properties JSON string from a serializable object.
    ///
    /// Extracts core fields (id, name, type) and serializes remaining fields
    /// as a JSON properties string.
    ///
    fn serialize_to_node_props(&self, node: &serde_json::Value) -> GraphDBResult<NodeProperties> {
        let mut props = if let serde_json::Value::Object(map) = node.clone() {
            map
        } else {
            return Err(GraphDBError::DatabaseError(
                "Expected JSON object".to_string(),
            ));
        };

        let created_at = props
            .get("created_at")
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        let updated_at = props
            .get("updated_at")
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        let id = props
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let name = props
            .remove("name")
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default();

        // Accept both "type" (DataPoint via #[serde(rename = "type")]) and
        // "data_type" (plain structs used in tests and older code).
        let node_type = props
            .get("type")
            .or_else(|| props.get("data_type"))
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown")
            .to_string();

        props.remove("id");
        props.remove("created_at");
        props.remove("updated_at");
        props.remove("type");
        props.remove("data_type");

        let properties_json =
            serde_json::to_string(&props).map_err(GraphDBError::SerializationError)?;

        Ok(NodeProperties {
            id,
            name,
            node_type,
            created_at,
            updated_at,
            properties: properties_json,
        })
    }

    /// Parse a node from query results.
    ///
    fn parse_node_data(&self, mut data: NodeData) -> GraphDBResult<NodeData> {
        if let Some(props_value) = data.remove("properties")
            && let Some(props_str) = props_value.as_str()
        {
            let additional_props: HashMap<Cow<'static, str>, serde_json::Value> =
                serde_json::from_str(props_str).map_err(GraphDBError::SerializationError)?;
            data.extend(additional_props);
        }
        Ok(data)
    }

    /// Check if a node matches property filters.
    ///
    /// This method checks if a node matches the given attribute filters.
    /// Core fields (id, name, type) are already filtered in the query,
    /// so this method only checks additional property fields.
    ///
    /// # Arguments
    /// * `node` - The parsed node data to check
    /// * `filters` - HashMap of attribute filters (key -> list of values)
    ///
    /// # Returns
    /// `true` if the node matches all property filters, `false` otherwise
    ///
    fn matches_property_filters(
        &self,
        node: &NodeData,
        filters: &HashMap<Cow<'static, str>, Vec<serde_json::Value>>,
    ) -> bool {
        for (attr, values) in filters {
            // Skip core fields - already filtered in query
            if matches!(attr.as_ref(), "id" | "name" | "type") {
                continue;
            }

            // Check if node has this property and if it matches any value
            if let Some(node_value) = node.get(attr.as_ref()) {
                if !values.iter().any(|filter_value| node_value == filter_value) {
                    return false;
                }
            } else {
                // Property not present = doesn't match filter
                return false;
            }
        }
        true
    }
}

/// Helper struct for node properties extracted from DataPoint
struct NodeProperties {
    id: String,
    name: String,
    node_type: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    properties: String, // JSON-encoded
}

#[async_trait]
impl GraphDBTrait for LadybugAdapter {
    async fn initialize(&self) -> GraphDBResult<()> {
        let conn = Connection::new(&self.db).map_err(|e| {
            GraphDBError::ConnectionError(format!("Failed to create connection: {}", e))
        })?;

        // Try to install and load JSON extension (optional)
        // Ignore errors if extension is not available
        let _ = conn.query("INSTALL json");
        let _ = conn.query("LOAD EXTENSION json");

        // Create Node table
        let create_node_table = r#"
            CREATE NODE TABLE IF NOT EXISTS Node(
                id STRING PRIMARY KEY,
                name STRING,
                type STRING,
                created_at TIMESTAMP,
                updated_at TIMESTAMP,
                properties STRING
            )
        "#;

        conn.query(create_node_table).map_err(|e| {
            GraphDBError::InitializationError(format!("Failed to create Node table: {}", e))
        })?;

        // Create Edge relationship table
        let create_edge_table = r#"
            CREATE REL TABLE IF NOT EXISTS EDGE(
                FROM Node TO Node,
                relationship_name STRING,
                created_at TIMESTAMP,
                updated_at TIMESTAMP,
                properties STRING
            )
        "#;

        conn.query(create_edge_table).map_err(|e| {
            GraphDBError::InitializationError(format!("Failed to create EDGE table: {}", e))
        })?;

        Ok(())
    }

    async fn is_empty(&self) -> GraphDBResult<bool> {
        let results = self.execute_query("MATCH (n:Node) RETURN COUNT(n) AS count")?;

        // Parse the count from results
        if let Some(first_row) = results.first()
            && let Some(count_value) = first_row.first()
            && let Some(count) = count_value.as_i64()
        {
            return Ok(count == 0);
        }

        // If we can't parse, assume empty (safe default)
        Ok(true)
    }

    async fn query(
        &self,
        query: &str,
        params: Option<HashMap<Cow<'static, str>, serde_json::Value>>,
    ) -> GraphDBResult<Vec<Vec<serde_json::Value>>> {
        // Ladybug doesn't support parameterized queries
        if params.is_some() {
            return Err(GraphDBError::QueryError(
                "Ladybug adapter does not support parameterized queries".to_string(),
            ));
        }
        self.execute_query(query)
    }

    async fn delete_graph(&self) -> GraphDBResult<()> {
        let conn = Connection::new(&self.db).map_err(|e| {
            GraphDBError::ConnectionError(format!("Failed to create connection: {}", e))
        })?;

        // Delete all edges first
        conn.query("MATCH (a:Node)-[r:EDGE]->(b:Node) DELETE r")
            .map_err(|e| GraphDBError::QueryError(format!("Failed to delete edges: {}", e)))?;

        // Delete all nodes
        conn.query("MATCH (n:Node) DELETE n")
            .map_err(|e| GraphDBError::QueryError(format!("Failed to delete nodes: {}", e)))?;

        Ok(())
    }

    async fn has_node(&self, node_id: &str) -> GraphDBResult<bool> {
        let query = format!(
            "MATCH (n:Node) WHERE n.id = '{}' RETURN COUNT(n) AS count",
            node_id.replace("'", "\\'")
        );

        let results = self.execute_query(&query)?;

        // Parse the count from results
        if let Some(first_row) = results.first()
            && let Some(count_value) = first_row.first()
            && let Some(count) = count_value.as_i64()
        {
            return Ok(count > 0);
        }

        Ok(false)
    }

    async fn add_node_raw(&self, node: Value) -> GraphDBResult<()> {
        let props = self.serialize_to_node_props(&node)?;
        let conn = Connection::new(&self.db).map_err(|e| {
            GraphDBError::ConnectionError(format!("Failed to create connection: {}", e))
        })?;

        // Format timestamp for Ladybug
        let created_at_str = props.created_at.format("%Y-%m-%d %H:%M:%S%.6f").to_string();
        let updated_at_str = props.updated_at.format("%Y-%m-%d %H:%M:%S%.6f").to_string();

        let query = format!(
            r#"CREATE (:Node {{
                id: '{}',
                name: '{}',
                type: '{}',
                created_at: timestamp('{}'),
                updated_at: timestamp('{}'),
                properties: '{}'
            }})"#,
            props.id.replace("'", "\\'"),
            props.name.replace("'", "\\'"),
            props.node_type.replace("'", "\\'"),
            created_at_str,
            updated_at_str,
            props.properties.replace("'", "\\'")
        );

        conn.query(&query)
            .map_err(|e| GraphDBError::NodeError(format!("Failed to add node: {}", e)))?;

        Ok(())
    }

    async fn add_nodes_raw(&self, nodes: Vec<Value>) -> GraphDBResult<()> {
        // Batch insert optimization: process nodes in chunks to avoid
        // query size limits and improve performance
        const BATCH_SIZE: usize = 500;

        if nodes.is_empty() {
            return Ok(());
        }

        // Process in batches
        for chunk in nodes.chunks(BATCH_SIZE) {
            // Serialize all nodes in this chunk first to catch any serialization errors early
            let mut node_props = Vec::with_capacity(chunk.len());
            for node in chunk {
                node_props.push(self.serialize_to_node_props(node)?);
            }

            // Build batched query with multiple CREATE statements
            let conn = Connection::new(&self.db).map_err(|e| {
                GraphDBError::ConnectionError(format!("Failed to create connection: {}", e))
            })?;

            // Create multiple nodes in a single query
            let create_statements: Vec<String> = node_props
                .iter()
                .map(|props| {
                    let created_at_str =
                        props.created_at.format("%Y-%m-%d %H:%M:%S%.6f").to_string();
                    let updated_at_str =
                        props.updated_at.format("%Y-%m-%d %H:%M:%S%.6f").to_string();

                    format!(
                        r#"CREATE (:Node {{
                id: '{}',
                name: '{}',
                type: '{}',
                created_at: timestamp('{}'),
                updated_at: timestamp('{}'),
                properties: '{}'
            }})"#,
                        props.id.replace("'", "\\'"),
                        props.name.replace("'", "\\'"),
                        props.node_type.replace("'", "\\'"),
                        created_at_str,
                        updated_at_str,
                        props.properties.replace("'", "\\'")
                    )
                })
                .collect();

            // Execute all CREATE statements in a single query
            let batch_query = create_statements.join(";\n");

            conn.query(&batch_query).map_err(|e| {
                GraphDBError::NodeError(format!(
                    "Failed to add {} nodes in batch: {}",
                    chunk.len(),
                    e
                ))
            })?;
        }

        Ok(())
    }

    async fn delete_node(&self, node_id: &str) -> GraphDBResult<()> {
        let conn = Connection::new(&self.db).map_err(|e| {
            GraphDBError::ConnectionError(format!("Failed to create connection: {}", e))
        })?;

        let query = format!(
            "MATCH (n:Node) WHERE n.id = '{}' DETACH DELETE n",
            node_id.replace("'", "\\'")
        );

        conn.query(&query)
            .map_err(|e| GraphDBError::NodeError(format!("Failed to delete node: {}", e)))?;

        Ok(())
    }

    async fn delete_nodes(&self, node_ids: &[String]) -> GraphDBResult<()> {
        for node_id in node_ids {
            self.delete_node(node_id).await?;
        }
        Ok(())
    }

    async fn get_node(&self, node_id: &str) -> GraphDBResult<Option<NodeData>> {
        let query = format!(
            "MATCH (n:Node) WHERE n.id = '{}' RETURN n.id AS id, n.name AS name, n.type AS type, n.properties AS properties",
            node_id.replace("'", "\\'")
        );

        let results = self.execute_query(&query)?;

        // Parse the first row if it exists
        if let Some(row) = results.first()
            && row.len() >= 4
        {
            let mut node_data = NodeData::new();
            if let Some(id_str) = row[0].as_str() {
                node_data.insert(Cow::Borrowed("id"), json!(id_str));
            }
            if let Some(name_str) = row[1].as_str() {
                node_data.insert(Cow::Borrowed("name"), json!(name_str));
            }
            if let Some(type_str) = row[2].as_str() {
                node_data.insert(Cow::Borrowed("type"), json!(type_str));
            }
            if let Some(props_str) = row[3].as_str() {
                node_data.insert(Cow::Borrowed("properties"), json!(props_str));
            }
            return Ok(Some(self.parse_node_data(node_data)?));
        }

        Ok(None)
    }

    async fn get_nodes(&self, node_ids: &[String]) -> GraphDBResult<Vec<NodeData>> {
        let mut nodes = Vec::new();
        for node_id in node_ids {
            if let Some(node) = self.get_node(node_id).await? {
                nodes.push(node);
            }
        }
        Ok(nodes)
    }

    async fn has_edge(
        &self,
        source_id: &str,
        target_id: &str,
        relationship_name: &str,
    ) -> GraphDBResult<bool> {
        let query = format!(
            "MATCH (a:Node)-[r:EDGE]->(b:Node) WHERE a.id = '{}' AND b.id = '{}' AND r.relationship_name = '{}' RETURN COUNT(r) AS count",
            source_id.replace("'", "\\'"),
            target_id.replace("'", "\\'"),
            relationship_name.replace("'", "\\'")
        );

        let results = self.execute_query(&query)?;

        // Parse the count from results
        if let Some(first_row) = results.first()
            && let Some(count_value) = first_row.first()
            && let Some(count) = count_value.as_i64()
        {
            return Ok(count > 0);
        }

        Ok(false)
    }

    async fn has_edges(&self, edges: &[EdgeData]) -> GraphDBResult<Vec<EdgeData>> {
        let mut existing_edges = Vec::new();
        for edge in edges {
            if self.has_edge(&edge.0, &edge.1, &edge.2).await? {
                existing_edges.push(edge.clone());
            }
        }
        Ok(existing_edges)
    }

    async fn add_edge(
        &self,
        source_id: &str,
        target_id: &str,
        relationship_name: &str,
        properties: Option<HashMap<Cow<'static, str>, serde_json::Value>>,
    ) -> GraphDBResult<()> {
        let conn = Connection::new(&self.db).map_err(|e| {
            GraphDBError::ConnectionError(format!("Failed to create connection: {}", e))
        })?;

        let now = Utc::now();
        let timestamp_str = now.format("%Y-%m-%d %H:%M:%S%.6f").to_string();

        let props_json = if let Some(props) = properties {
            serde_json::to_string(&props).map_err(GraphDBError::SerializationError)?
        } else {
            "{}".to_string()
        };

        // First, match both nodes
        let query = format!(
            r#"MATCH (a:Node {{id: '{}'}}), (b:Node {{id: '{}'}})
            CREATE (a)-[:EDGE {{
                relationship_name: '{}',
                created_at: timestamp('{}'),
                updated_at: timestamp('{}'),
                properties: '{}'
            }}]->(b)"#,
            source_id.replace("'", "\\'"),
            target_id.replace("'", "\\'"),
            relationship_name.replace("'", "\\'"),
            timestamp_str,
            timestamp_str,
            props_json.replace("'", "\\'")
        );

        conn.query(&query)
            .map_err(|e| GraphDBError::EdgeError(format!("Failed to add edge: {}", e)))?;

        Ok(())
    }

    async fn add_edges(&self, edges: &[EdgeData]) -> GraphDBResult<()> {
        for edge in edges {
            self.add_edge(&edge.0, &edge.1, &edge.2, Some(edge.3.clone()))
                .await?;
        }
        Ok(())
    }

    async fn get_edges(&self, node_id: &str) -> GraphDBResult<Vec<EdgeData>> {
        let query = format!(
            "MATCH (n:Node)-[r:EDGE]-(m:Node) WHERE n.id = '{}' RETURN n.id AS source_id, m.id AS target_id, r.relationship_name AS rel_name, r.properties AS props",
            node_id.replace("'", "\\'")
        );

        let results = self.execute_query(&query)?;
        let mut edges = Vec::new();

        // Parse each row into EdgeData
        for row in results {
            if row.len() >= Self::EDGE_QUERY_COLUMN_COUNT {
                let source_id = row[0].as_str().unwrap_or("").to_string();
                let target_id = row[1].as_str().unwrap_or("").to_string();
                let rel_name = row[2].as_str().unwrap_or("").to_string();

                let props = if let Some(props_str) = row[3].as_str() {
                    serde_json::from_str::<HashMap<Cow<'static, str>, serde_json::Value>>(props_str)
                        .unwrap_or_default()
                } else {
                    HashMap::new()
                };

                edges.push((source_id, target_id, rel_name, props));
            }
        }

        Ok(edges)
    }

    async fn get_neighbors(&self, node_id: &str) -> GraphDBResult<Vec<NodeData>> {
        let query = format!(
            "MATCH (n:Node)-[r:EDGE]-(m:Node) WHERE n.id = '{}' RETURN DISTINCT m.id AS id, m.name AS name, m.type AS type, m.properties AS properties",
            node_id.replace("'", "\\'")
        );

        let results = self.execute_query(&query)?;
        let mut neighbors = Vec::new();

        // Parse each row into NodeData
        for row in results {
            if row.len() >= Self::NODE_QUERY_COLUMN_COUNT {
                let mut node_data = NodeData::new();
                if let Some(id_str) = row[0].as_str() {
                    node_data.insert(Cow::Borrowed("id"), json!(id_str));
                }
                if let Some(name_str) = row[1].as_str() {
                    node_data.insert(Cow::Borrowed("name"), json!(name_str));
                }
                if let Some(type_str) = row[2].as_str() {
                    node_data.insert(Cow::Borrowed("type"), json!(type_str));
                }
                if let Some(props_str) = row[3].as_str() {
                    node_data.insert(Cow::Borrowed("properties"), json!(props_str));
                }
                neighbors.push(self.parse_node_data(node_data)?);
            }
        }

        Ok(neighbors)
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
        /// Number of columns expected from query: 4 (source node) + 2 (edge) + 4 (target node) = 10
        const GET_CONNECTIONS_COLUMN_COUNT: usize = 10;

        let query = format!(
            r#"MATCH (n:Node)-[r:EDGE]-(m:Node)
            WHERE n.id = '{}'
            RETURN 
                n.id AS source_id, 
                n.name AS source_name, 
                n.type AS source_type, 
                n.properties AS source_props,
                r.relationship_name AS rel_name,
                r.properties AS rel_props,
                m.id AS target_id,
                m.name AS target_name,
                m.type AS target_type,
                m.properties AS target_props"#,
            node_id.replace("'", "\\'")
        );

        let results = self.execute_query(&query)?;
        let mut connections = Vec::new();

        for row in results {
            if row.len() >= GET_CONNECTIONS_COLUMN_COUNT {
                // Parse source node
                let mut source_node = NodeData::new();
                if let Some(id) = row[0].as_str() {
                    source_node.insert(Cow::Borrowed("id"), json!(id));
                }
                if let Some(name) = row[1].as_str() {
                    source_node.insert(Cow::Borrowed("name"), json!(name));
                }
                if let Some(node_type) = row[2].as_str() {
                    source_node.insert(Cow::Borrowed("type"), json!(node_type));
                }
                if let Some(props) = row[3].as_str() {
                    source_node.insert(Cow::Borrowed("properties"), json!(props));
                }
                let source_node = self.parse_node_data(source_node)?;

                // Parse edge properties
                let mut edge_props = HashMap::new();
                if let Some(rel_name) = row[4].as_str() {
                    edge_props.insert(Cow::Borrowed("relationship_name"), json!(rel_name));
                }
                if let Some(rel_props_str) = row[5].as_str() {
                    let additional_props: HashMap<Cow<'static, str>, serde_json::Value> =
                        serde_json::from_str(rel_props_str).unwrap_or_default();
                    edge_props.extend(additional_props);
                }

                // Parse target node
                let mut target_node = NodeData::new();
                if let Some(id) = row[6].as_str() {
                    target_node.insert(Cow::Borrowed("id"), json!(id));
                }
                if let Some(name) = row[7].as_str() {
                    target_node.insert(Cow::Borrowed("name"), json!(name));
                }
                if let Some(node_type) = row[8].as_str() {
                    target_node.insert(Cow::Borrowed("type"), json!(node_type));
                }
                if let Some(props) = row[9].as_str() {
                    target_node.insert(Cow::Borrowed("properties"), json!(props));
                }
                let target_node = self.parse_node_data(target_node)?;

                connections.push((source_node, edge_props, target_node));
            }
        }

        Ok(connections)
    }

    async fn get_graph_data(&self) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
        // Get all nodes
        let nodes_query = "MATCH (n:Node) RETURN n.id AS id, n.name AS name, n.type AS type, n.properties AS properties";
        let node_results = self.execute_query(nodes_query)?;

        let mut nodes = Vec::new();
        for row in node_results {
            if row.len() >= Self::NODE_QUERY_COLUMN_COUNT {
                let mut node_data = NodeData::new();
                if let Some(id_str) = row[0].as_str() {
                    node_data.insert(Cow::Borrowed("id"), json!(id_str));
                }
                if let Some(name_str) = row[1].as_str() {
                    node_data.insert(Cow::Borrowed("name"), json!(name_str));
                }
                if let Some(type_str) = row[2].as_str() {
                    node_data.insert(Cow::Borrowed("type"), json!(type_str));
                }
                if let Some(props_str) = row[3].as_str() {
                    node_data.insert(Cow::Borrowed("properties"), json!(props_str));
                }
                let parsed_node = self.parse_node_data(node_data)?;
                if let Some(id_str) = parsed_node.get("id").and_then(|v| v.as_str()) {
                    nodes.push((id_str.to_string(), parsed_node));
                }
            }
        }

        // Get all edges
        let edges_query = "MATCH (a:Node)-[r:EDGE]->(b:Node) RETURN a.id AS source_id, b.id AS target_id, r.relationship_name AS rel_name, r.properties AS props";
        let edge_results = self.execute_query(edges_query)?;

        let mut edges = Vec::new();
        for row in edge_results {
            if row.len() >= Self::EDGE_QUERY_COLUMN_COUNT {
                let source_id = row[0].as_str().unwrap_or("").to_string();
                let target_id = row[1].as_str().unwrap_or("").to_string();
                let rel_name = row[2].as_str().unwrap_or("").to_string();

                let props = if let Some(props_str) = row[3].as_str() {
                    serde_json::from_str::<HashMap<Cow<'static, str>, serde_json::Value>>(props_str)
                        .unwrap_or_default()
                } else {
                    HashMap::new()
                };

                edges.push((source_id, target_id, rel_name, props));
            }
        }

        Ok((nodes, edges))
    }

    async fn get_graph_metrics(
        &self,
        _include_optional: bool,
    ) -> GraphDBResult<HashMap<Cow<'static, str>, serde_json::Value>> {
        let mut metrics = HashMap::new();

        // Get node count
        let node_count_results = self.execute_query("MATCH (n:Node) RETURN COUNT(n) AS count")?;
        let node_count = node_count_results
            .first()
            .and_then(|row| row.first())
            .and_then(|v| v.as_i64())
            .unwrap_or(0);

        // Get edge count
        let edge_count_results =
            self.execute_query("MATCH ()-[r:EDGE]->() RETURN COUNT(r) AS count")?;
        let edge_count = edge_count_results
            .first()
            .and_then(|row| row.first())
            .and_then(|v| v.as_i64())
            .unwrap_or(0);

        metrics.insert(Cow::Borrowed("node_count"), json!(node_count));
        metrics.insert(Cow::Borrowed("edge_count"), json!(edge_count));

        Ok(metrics)
    }

    async fn get_filtered_graph_data(
        &self,
        attribute_filters: &HashMap<Cow<'static, str>, Vec<serde_json::Value>>,
    ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
        // If no filters, return entire graph
        if attribute_filters.is_empty() {
            return self.get_graph_data().await;
        }

        // Build WHERE clause for core fields (id, name, type)
        let mut where_clauses = Vec::new();

        for (attr, values) in attribute_filters {
            if values.is_empty() {
                continue;
            }

            // Check if this is a core field
            let is_core_field = matches!(attr.as_ref(), "id" | "name" | "type");

            if is_core_field {
                // Direct field match
                let value_clauses: Vec<String> = values
                    .iter()
                    .map(|v| {
                        if let Some(s) = v.as_str() {
                            format!("n.{} = '{}'", attr, s.replace("'", "\\'"))
                        } else {
                            format!("n.{} = {}", attr, v)
                        }
                    })
                    .collect();

                if value_clauses.len() == 1 {
                    where_clauses.push(value_clauses[0].clone());
                } else {
                    where_clauses.push(format!("({})", value_clauses.join(" OR ")));
                }
            }
            // Property fields are filtered in Rust after loading
        }

        // Build final query with optional WHERE clause
        let where_clause = if where_clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", where_clauses.join(" AND "))
        };

        // Get filtered nodes
        let nodes_query = format!(
            "MATCH (n:Node) {} RETURN n.id AS id, n.name AS name, n.type AS type, n.properties AS properties",
            where_clause
        );

        let node_results = self.execute_query(&nodes_query)?;

        // Parse nodes and collect IDs
        let mut nodes = Vec::new();
        let mut node_ids = Vec::new();

        for row in node_results {
            if row.len() >= Self::NODE_QUERY_COLUMN_COUNT {
                let mut node_data = NodeData::new();
                if let Some(id_str) = row[0].as_str() {
                    node_data.insert(Cow::Borrowed("id"), json!(id_str));
                }
                if let Some(name_str) = row[1].as_str() {
                    node_data.insert(Cow::Borrowed("name"), json!(name_str));
                }
                if let Some(type_str) = row[2].as_str() {
                    node_data.insert(Cow::Borrowed("type"), json!(type_str));
                }
                if let Some(props_str) = row[3].as_str() {
                    node_data.insert(Cow::Borrowed("properties"), json!(props_str));
                }

                let parsed_node = self.parse_node_data(node_data)?;

                // Apply property filters in Rust if needed
                if self.matches_property_filters(&parsed_node, attribute_filters)
                    && let Some(id_str) = parsed_node.get("id").and_then(|v| v.as_str())
                {
                    node_ids.push(id_str.to_string());
                    nodes.push((id_str.to_string(), parsed_node));
                }
            }
        }

        // Get edges connecting filtered nodes
        if node_ids.is_empty() {
            return Ok((nodes, Vec::new()));
        }

        // Build IN clause for edge query
        let id_list = node_ids
            .iter()
            .map(|id| format!("'{}'", id.replace("'", "\\'")))
            .collect::<Vec<_>>()
            .join(", ");

        let edges_query = format!(
            "MATCH (a:Node)-[r:EDGE]->(b:Node) WHERE a.id IN [{}] AND b.id IN [{}] RETURN a.id, b.id, r.relationship_name, r.properties",
            id_list, id_list
        );

        let edge_results = self.execute_query(&edges_query)?;
        let mut edges = Vec::new();

        for row in edge_results {
            if row.len() >= Self::EDGE_QUERY_COLUMN_COUNT {
                let source_id = row[0].as_str().unwrap_or("").to_string();
                let target_id = row[1].as_str().unwrap_or("").to_string();
                let rel_name = row[2].as_str().unwrap_or("").to_string();
                let props = if let Some(props_str) = row[3].as_str() {
                    serde_json::from_str(props_str).unwrap_or_default()
                } else {
                    HashMap::new()
                };
                edges.push((source_id, target_id, rel_name, props));
            }
        }

        Ok((nodes, edges))
    }

    async fn get_nodeset_subgraph(
        &self,
        node_type: &str,
        node_names: &[String],
    ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
        // Early return for empty node_names
        if node_names.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }

        // Build IN clause for names
        let name_list = node_names
            .iter()
            .map(|name| format!("'{}'", name.replace("'", "\\'")))
            .collect::<Vec<_>>()
            .join(", ");

        // Query for specific nodes
        let nodes_query = format!(
            "MATCH (n:Node) WHERE n.type = '{}' AND n.name IN [{}] RETURN n.id AS id, n.name AS name, n.type AS type, n.properties AS properties",
            node_type.replace("'", "\\'"),
            name_list
        );

        let node_results = self.execute_query(&nodes_query)?;

        let mut nodes = Vec::new();
        let mut node_ids = Vec::new();

        // Parse nodes
        for row in node_results {
            if row.len() >= Self::NODE_QUERY_COLUMN_COUNT {
                let mut node_data = NodeData::new();
                if let Some(id_str) = row[0].as_str() {
                    node_data.insert(Cow::Borrowed("id"), json!(id_str));
                    node_ids.push(id_str.to_string());
                }
                if let Some(name_str) = row[1].as_str() {
                    node_data.insert(Cow::Borrowed("name"), json!(name_str));
                }
                if let Some(type_str) = row[2].as_str() {
                    node_data.insert(Cow::Borrowed("type"), json!(type_str));
                }
                if let Some(props_str) = row[3].as_str() {
                    node_data.insert(Cow::Borrowed("properties"), json!(props_str));
                }

                let parsed_node = self.parse_node_data(node_data)?;
                if let Some(id_str) = parsed_node.get("id").and_then(|v| v.as_str()) {
                    nodes.push((id_str.to_string(), parsed_node));
                }
            }
        }

        // Get edges connecting these nodes
        if node_ids.is_empty() {
            return Ok((nodes, Vec::new()));
        }

        let id_list = node_ids
            .iter()
            .map(|id| format!("'{}'", id.replace("'", "\\'")))
            .collect::<Vec<_>>()
            .join(", ");

        let edges_query = format!(
            "MATCH (a:Node)-[r:EDGE]->(b:Node) WHERE a.id IN [{}] AND b.id IN [{}] RETURN a.id, b.id, r.relationship_name, r.properties",
            id_list, id_list
        );

        let edge_results = self.execute_query(&edges_query)?;
        let mut edges = Vec::new();

        for row in edge_results {
            if row.len() >= Self::EDGE_QUERY_COLUMN_COUNT {
                let source_id = row[0].as_str().unwrap_or("").to_string();
                let target_id = row[1].as_str().unwrap_or("").to_string();
                let rel_name = row[2].as_str().unwrap_or("").to_string();
                let props = if let Some(props_str) = row[3].as_str() {
                    serde_json::from_str(props_str).unwrap_or_default()
                } else {
                    HashMap::new()
                };
                edges.push((source_id, target_id, rel_name, props));
            }
        }

        Ok((nodes, edges))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GraphDBTraitExt;
    use serde::Serialize;
    use serial_test::serial;
    use tempfile::TempDir;

    /// Simple test node for testing batch operations
    #[derive(Debug, Clone, Serialize)]
    struct TestNode {
        id: String,
        name: String,
        data_type: String,
        created_at: String,
        updated_at: String,
        value: i32,
    }

    impl TestNode {
        fn new(id: &str, name: &str, value: i32) -> Self {
            let now = chrono::Utc::now().to_rfc3339();
            Self {
                id: id.to_string(),
                name: name.to_string(),
                data_type: "TestNode".to_string(),
                created_at: now.clone(),
                updated_at: now,
                value,
            }
        }
    }

    async fn setup_adapter() -> (LadybugAdapter, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let adapter = LadybugAdapter::new(db_path.to_str().unwrap())
            .await
            .unwrap();
        adapter.initialize().await.unwrap();
        (adapter, temp_dir)
    }

    #[tokio::test]
    #[serial]
    async fn test_adapter_creation() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");

        let adapter = LadybugAdapter::new(db_path.to_str().unwrap()).await;
        assert!(adapter.is_ok());
    }

    #[tokio::test]
    #[serial]
    async fn test_initialize() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test_init.db");

        let adapter = LadybugAdapter::new(db_path.to_str().unwrap())
            .await
            .unwrap();
        let result = adapter.initialize().await;
        if let Err(e) = &result {
            eprintln!("Initialization error: {:?}", e);
        }
        assert!(result.is_ok());
    }

    #[tokio::test]
    #[serial]
    async fn test_batch_insert_empty() {
        let (adapter, _temp_dir) = setup_adapter().await;

        // Test with empty slice
        let nodes: Vec<&TestNode> = vec![];
        let result = adapter.add_nodes(&nodes).await;
        assert!(result.is_ok());

        // Verify no nodes were added
        assert!(adapter.is_empty().await.unwrap());
    }

    #[tokio::test]
    #[serial]
    async fn test_batch_insert_single_node() {
        let (adapter, _temp_dir) = setup_adapter().await;

        let node = TestNode::new("test-1", "Node 1", 100);
        let nodes = vec![&node];

        let result = adapter.add_nodes(&nodes).await;
        assert!(result.is_ok());

        // Verify node was added
        assert!(!adapter.is_empty().await.unwrap());
        assert!(adapter.has_node("test-1").await.unwrap());
    }

    #[tokio::test]
    #[serial]
    async fn test_batch_insert_ten_nodes() {
        let (adapter, _temp_dir) = setup_adapter().await;

        let nodes: Vec<TestNode> = (0..10)
            .map(|i| TestNode::new(&format!("test-{}", i), &format!("Node {}", i), i * 10))
            .collect();
        let node_refs: Vec<&TestNode> = nodes.iter().collect();

        let result = adapter.add_nodes(&node_refs).await;
        assert!(result.is_ok());

        // Verify all nodes were added
        for i in 0..10 {
            assert!(
                adapter.has_node(&format!("test-{}", i)).await.unwrap(),
                "Node test-{} should exist",
                i
            );
        }

        // Check total count
        let metrics = adapter.get_graph_metrics(false).await.unwrap();
        assert_eq!(metrics.get("node_count").unwrap().as_i64().unwrap(), 10);
    }

    #[tokio::test]
    #[serial]
    async fn test_batch_insert_hundred_nodes() {
        let (adapter, _temp_dir) = setup_adapter().await;

        let nodes: Vec<TestNode> = (0..100)
            .map(|i| TestNode::new(&format!("test-{}", i), &format!("Node {}", i), i * 10))
            .collect();
        let node_refs: Vec<&TestNode> = nodes.iter().collect();

        let result = adapter.add_nodes(&node_refs).await;
        assert!(result.is_ok());

        // Verify count
        let metrics = adapter.get_graph_metrics(false).await.unwrap();
        assert_eq!(metrics.get("node_count").unwrap().as_i64().unwrap(), 100);

        // Spot check a few nodes
        assert!(adapter.has_node("test-0").await.unwrap());
        assert!(adapter.has_node("test-50").await.unwrap());
        assert!(adapter.has_node("test-99").await.unwrap());
    }

    #[tokio::test]
    #[serial]
    async fn test_batch_insert_thousand_nodes() {
        let (adapter, _temp_dir) = setup_adapter().await;

        let nodes: Vec<TestNode> = (0..1000)
            .map(|i| TestNode::new(&format!("test-{}", i), &format!("Node {}", i), i * 10))
            .collect();
        let node_refs: Vec<&TestNode> = nodes.iter().collect();

        let result = adapter.add_nodes(&node_refs).await;
        assert!(result.is_ok());

        // Verify count (should process in chunks of 500)
        let metrics = adapter.get_graph_metrics(false).await.unwrap();
        assert_eq!(metrics.get("node_count").unwrap().as_i64().unwrap(), 1000);

        // Spot check nodes across batch boundaries
        assert!(adapter.has_node("test-0").await.unwrap());
        assert!(adapter.has_node("test-499").await.unwrap()); // Last of first batch
        assert!(adapter.has_node("test-500").await.unwrap()); // First of second batch
        assert!(adapter.has_node("test-999").await.unwrap());
    }

    #[tokio::test]
    #[serial]
    async fn test_batch_insert_preserves_data() {
        let (adapter, _temp_dir) = setup_adapter().await;

        let nodes: Vec<TestNode> = vec![
            TestNode::new("test-a", "Alice", 100),
            TestNode::new("test-b", "Bob", 200),
            TestNode::new("test-c", "Charlie", 300),
        ];
        let node_refs: Vec<&TestNode> = nodes.iter().collect();

        adapter.add_nodes(&node_refs).await.unwrap();

        // Verify node data is preserved
        let alice = adapter.get_node("test-a").await.unwrap().unwrap();
        assert_eq!(alice.get("id").unwrap().as_str().unwrap(), "test-a");
        assert_eq!(alice.get("name").unwrap().as_str().unwrap(), "Alice");
        assert_eq!(alice.get("value").unwrap().as_i64().unwrap(), 100);

        let bob = adapter.get_node("test-b").await.unwrap().unwrap();
        assert_eq!(bob.get("name").unwrap().as_str().unwrap(), "Bob");
        assert_eq!(bob.get("value").unwrap().as_i64().unwrap(), 200);

        let charlie = adapter.get_node("test-c").await.unwrap().unwrap();
        assert_eq!(charlie.get("name").unwrap().as_str().unwrap(), "Charlie");
        assert_eq!(charlie.get("value").unwrap().as_i64().unwrap(), 300);
    }

    #[tokio::test]
    #[serial]
    async fn test_batch_vs_sequential_equivalence() {
        // Create two adapters
        let (adapter_batch, _temp_dir_batch) = setup_adapter().await;
        let (adapter_seq, _temp_dir_seq) = setup_adapter().await;

        let nodes: Vec<TestNode> = (0..20)
            .map(|i| TestNode::new(&format!("test-{}", i), &format!("Node {}", i), i * 10))
            .collect();
        let node_refs: Vec<&TestNode> = nodes.iter().collect();

        // Batch insert
        adapter_batch.add_nodes(&node_refs).await.unwrap();

        // Sequential insert
        for node in &node_refs {
            adapter_seq.add_node(node).await.unwrap();
        }

        // Both should have same node count
        let metrics_batch = adapter_batch.get_graph_metrics(false).await.unwrap();
        let metrics_seq = adapter_seq.get_graph_metrics(false).await.unwrap();

        assert_eq!(
            metrics_batch.get("node_count").unwrap(),
            metrics_seq.get("node_count").unwrap()
        );

        // Spot check that same nodes exist in both
        for i in 0..20 {
            let node_id = format!("test-{}", i);
            assert!(adapter_batch.has_node(&node_id).await.unwrap());
            assert!(adapter_seq.has_node(&node_id).await.unwrap());
        }
    }

    // Tests for get_connections()
    #[tokio::test]
    #[serial]
    async fn test_get_connections_node_with_no_connections() {
        let (adapter, _temp_dir) = setup_adapter().await;

        let node = TestNode::new("isolated", "Isolated Node", 42);
        adapter.add_node(&node).await.unwrap();

        let connections = adapter.get_connections("isolated").await.unwrap();
        assert_eq!(connections.len(), 0);
    }

    #[tokio::test]
    #[serial]
    async fn test_get_connections_outgoing_only() {
        let (adapter, _temp_dir) = setup_adapter().await;

        // Create nodes
        let node_a = TestNode::new("node-a", "Node A", 1);
        let node_b = TestNode::new("node-b", "Node B", 2);
        adapter.add_node(&node_a).await.unwrap();
        adapter.add_node(&node_b).await.unwrap();

        // Add edge A -> B
        adapter
            .add_edge("node-a", "node-b", "points_to", None)
            .await
            .unwrap();

        let connections = adapter.get_connections("node-a").await.unwrap();
        assert_eq!(connections.len(), 1);

        let (source, edge_props, target) = &connections[0];
        assert_eq!(source.get("id").unwrap().as_str().unwrap(), "node-a");
        assert_eq!(target.get("id").unwrap().as_str().unwrap(), "node-b");
        assert_eq!(
            edge_props
                .get("relationship_name")
                .unwrap()
                .as_str()
                .unwrap(),
            "points_to"
        );
    }

    #[tokio::test]
    #[serial]
    async fn test_get_connections_incoming_only() {
        let (adapter, _temp_dir) = setup_adapter().await;

        let node_a = TestNode::new("node-a", "Node A", 1);
        let node_b = TestNode::new("node-b", "Node B", 2);
        adapter.add_node(&node_a).await.unwrap();
        adapter.add_node(&node_b).await.unwrap();

        // Add edge A -> B (so B has incoming edge)
        adapter
            .add_edge("node-a", "node-b", "points_to", None)
            .await
            .unwrap();

        let connections = adapter.get_connections("node-b").await.unwrap();
        assert_eq!(connections.len(), 1);

        let (source, _, target) = &connections[0];
        assert_eq!(source.get("id").unwrap().as_str().unwrap(), "node-b");
        assert_eq!(target.get("id").unwrap().as_str().unwrap(), "node-a");
    }

    #[tokio::test]
    #[serial]
    async fn test_get_connections_bidirectional() {
        let (adapter, _temp_dir) = setup_adapter().await;

        let node_a = TestNode::new("node-a", "Node A", 1);
        let node_b = TestNode::new("node-b", "Node B", 2);
        adapter.add_node(&node_a).await.unwrap();
        adapter.add_node(&node_b).await.unwrap();

        // Add both directions
        adapter
            .add_edge("node-a", "node-b", "knows", None)
            .await
            .unwrap();
        adapter
            .add_edge("node-b", "node-a", "knows", None)
            .await
            .unwrap();

        let connections = adapter.get_connections("node-a").await.unwrap();
        assert_eq!(connections.len(), 2);

        // Both connections should involve node-a and node-b
        for (source, _, target) in &connections {
            let source_id = source.get("id").unwrap().as_str().unwrap();
            let target_id = target.get("id").unwrap().as_str().unwrap();
            assert_eq!(source_id, "node-a");
            assert!(target_id == "node-b" || target_id == "node-a");
        }
    }

    #[tokio::test]
    #[serial]
    async fn test_get_connections_self_loop() {
        let (adapter, _temp_dir) = setup_adapter().await;

        let node = TestNode::new("node-a", "Node A", 1);
        adapter.add_node(&node).await.unwrap();

        // Self-loop
        adapter
            .add_edge("node-a", "node-a", "references", None)
            .await
            .unwrap();

        let connections = adapter.get_connections("node-a").await.unwrap();
        // Self-loop appears twice due to bidirectional pattern (once as outgoing, once as incoming)
        assert_eq!(connections.len(), 2);

        // Both should be self-references
        for (source, edge_props, target) in &connections {
            assert_eq!(source.get("id").unwrap().as_str().unwrap(), "node-a");
            assert_eq!(target.get("id").unwrap().as_str().unwrap(), "node-a");
            assert_eq!(
                edge_props
                    .get("relationship_name")
                    .unwrap()
                    .as_str()
                    .unwrap(),
                "references"
            );
        }
    }

    #[tokio::test]
    #[serial]
    async fn test_get_connections_multiple_relationship_types() {
        let (adapter, _temp_dir) = setup_adapter().await;

        let node_a = TestNode::new("node-a", "Node A", 1);
        let node_b = TestNode::new("node-b", "Node B", 2);
        adapter.add_node(&node_a).await.unwrap();
        adapter.add_node(&node_b).await.unwrap();

        // Multiple edges with different relationships
        adapter
            .add_edge("node-a", "node-b", "knows", None)
            .await
            .unwrap();
        adapter
            .add_edge("node-a", "node-b", "works_with", None)
            .await
            .unwrap();
        adapter
            .add_edge("node-a", "node-b", "lives_near", None)
            .await
            .unwrap();

        let connections = adapter.get_connections("node-a").await.unwrap();
        assert_eq!(connections.len(), 3);

        // Collect all relationship names
        let rel_names: Vec<String> = connections
            .iter()
            .map(|(_, edge_props, _)| {
                edge_props
                    .get("relationship_name")
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();

        assert!(rel_names.contains(&"knows".to_string()));
        assert!(rel_names.contains(&"works_with".to_string()));
        assert!(rel_names.contains(&"lives_near".to_string()));
    }

    #[tokio::test]
    #[serial]
    async fn test_get_connections_edge_properties() {
        let (adapter, _temp_dir) = setup_adapter().await;

        let node_a = TestNode::new("node-a", "Node A", 1);
        let node_b = TestNode::new("node-b", "Node B", 2);
        adapter.add_node(&node_a).await.unwrap();
        adapter.add_node(&node_b).await.unwrap();

        // Add edge with custom properties
        let mut props = HashMap::new();
        props.insert(Cow::Borrowed("since"), json!(2020));
        props.insert(Cow::Borrowed("strength"), json!("strong"));

        adapter
            .add_edge("node-a", "node-b", "knows", Some(props))
            .await
            .unwrap();

        let connections = adapter.get_connections("node-a").await.unwrap();
        assert_eq!(connections.len(), 1);

        let (_, edge_props, _) = &connections[0];
        assert_eq!(
            edge_props
                .get("relationship_name")
                .unwrap()
                .as_str()
                .unwrap(),
            "knows"
        );
        assert_eq!(edge_props.get("since").unwrap().as_i64().unwrap(), 2020);
        assert_eq!(
            edge_props.get("strength").unwrap().as_str().unwrap(),
            "strong"
        );
    }

    #[tokio::test]
    #[serial]
    async fn test_get_connections_node_properties_expanded() {
        let (adapter, _temp_dir) = setup_adapter().await;

        // Create nodes with custom properties
        let node_a = TestNode::new("node-a", "Alice", 100);
        let node_b = TestNode::new("node-b", "Bob", 200);
        adapter.add_node(&node_a).await.unwrap();
        adapter.add_node(&node_b).await.unwrap();

        adapter
            .add_edge("node-a", "node-b", "knows", None)
            .await
            .unwrap();

        let connections = adapter.get_connections("node-a").await.unwrap();
        assert_eq!(connections.len(), 1);

        let (source, _, target) = &connections[0];

        // Verify source node properties
        assert_eq!(source.get("id").unwrap().as_str().unwrap(), "node-a");
        assert_eq!(source.get("name").unwrap().as_str().unwrap(), "Alice");
        assert_eq!(source.get("value").unwrap().as_i64().unwrap(), 100);

        // Verify target node properties
        assert_eq!(target.get("id").unwrap().as_str().unwrap(), "node-b");
        assert_eq!(target.get("name").unwrap().as_str().unwrap(), "Bob");
        assert_eq!(target.get("value").unwrap().as_i64().unwrap(), 200);
    }

    // Tests for get_filtered_graph_data

    #[tokio::test]
    #[serial]
    async fn test_get_filtered_graph_data_empty_filters() {
        let (adapter, _temp_dir) = setup_adapter().await;

        // Create some test nodes
        let node_a = TestNode::new("node-a", "Alice", 100);
        let node_b = TestNode::new("node-b", "Bob", 200);
        adapter.add_node(&node_a).await.unwrap();
        adapter.add_node(&node_b).await.unwrap();

        adapter
            .add_edge("node-a", "node-b", "knows", None)
            .await
            .unwrap();

        // Empty filters should return entire graph
        let filters = HashMap::new();
        let (nodes, edges) = adapter.get_filtered_graph_data(&filters).await.unwrap();

        assert_eq!(nodes.len(), 2);
        assert_eq!(edges.len(), 1);
    }

    #[tokio::test]
    #[serial]
    async fn test_get_filtered_graph_data_single_attribute_single_value() {
        let (adapter, _temp_dir) = setup_adapter().await;

        let node_a = TestNode::new("node-a", "Alice", 100);
        let node_b = TestNode::new("node-b", "Bob", 200);
        let node_c = TestNode::new("node-c", "Charlie", 300);
        adapter.add_node(&node_a).await.unwrap();
        adapter.add_node(&node_b).await.unwrap();
        adapter.add_node(&node_c).await.unwrap();

        // Filter by name = "Alice"
        let mut filters = HashMap::new();
        filters.insert(Cow::Borrowed("name"), vec![json!("Alice")]);

        let (nodes, _edges) = adapter.get_filtered_graph_data(&filters).await.unwrap();

        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].1.get("name").unwrap().as_str().unwrap(), "Alice");
    }

    #[tokio::test]
    #[serial]
    async fn test_get_filtered_graph_data_single_attribute_multiple_values() {
        let (adapter, _temp_dir) = setup_adapter().await;

        let node_a = TestNode::new("node-a", "Alice", 100);
        let node_b = TestNode::new("node-b", "Bob", 200);
        let node_c = TestNode::new("node-c", "Charlie", 300);
        adapter.add_node(&node_a).await.unwrap();
        adapter.add_node(&node_b).await.unwrap();
        adapter.add_node(&node_c).await.unwrap();

        // Filter by name IN ["Alice", "Charlie"] (OR logic)
        let mut filters = HashMap::new();
        filters.insert(
            Cow::Borrowed("name"),
            vec![json!("Alice"), json!("Charlie")],
        );

        let (nodes, _edges) = adapter.get_filtered_graph_data(&filters).await.unwrap();

        assert_eq!(nodes.len(), 2);
        let names: Vec<_> = nodes
            .iter()
            .map(|(_, n)| n.get("name").unwrap().as_str().unwrap())
            .collect();
        assert!(names.contains(&"Alice"));
        assert!(names.contains(&"Charlie"));
        assert!(!names.contains(&"Bob"));
    }

    #[tokio::test]
    #[serial]
    async fn test_get_filtered_graph_data_multiple_attributes_and_logic() {
        let (adapter, _temp_dir) = setup_adapter().await;

        // Create test nodes with different types
        #[derive(Serialize)]
        struct TypedNode {
            id: String,
            name: String,
            data_type: String, // This maps to the 'type' column in DB
            created_at: String,
            updated_at: String,
        }

        let node_a = TypedNode {
            id: "node-a".to_string(),
            name: "Alice".to_string(),
            data_type: "Person".to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        };

        let node_b = TypedNode {
            id: "node-b".to_string(),
            name: "Bob".to_string(),
            data_type: "Person".to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        };

        let node_c = TypedNode {
            id: "node-c".to_string(),
            name: "Charlie".to_string(),
            data_type: "Organization".to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        };

        adapter.add_node(&node_a).await.unwrap();
        adapter.add_node(&node_b).await.unwrap();
        adapter.add_node(&node_c).await.unwrap();

        // Filter by type = "Person" AND name IN ["Alice", "Bob"] (AND logic)
        let mut filters = HashMap::new();
        filters.insert(Cow::Borrowed("type"), vec![json!("Person")]);
        filters.insert(Cow::Borrowed("name"), vec![json!("Alice"), json!("Bob")]);

        let (nodes, _edges) = adapter.get_filtered_graph_data(&filters).await.unwrap();

        assert_eq!(nodes.len(), 2);
        for (_, node) in &nodes {
            assert_eq!(node.get("type").unwrap().as_str().unwrap(), "Person");
            let name = node.get("name").unwrap().as_str().unwrap();
            assert!(name == "Alice" || name == "Bob");
        }
    }

    #[tokio::test]
    #[serial]
    async fn test_get_filtered_graph_data_property_filters() {
        let (adapter, _temp_dir) = setup_adapter().await;

        let node_a = TestNode::new("node-a", "Alice", 100);
        let node_b = TestNode::new("node-b", "Bob", 200);
        let node_c = TestNode::new("node-c", "Charlie", 100);
        adapter.add_node(&node_a).await.unwrap();
        adapter.add_node(&node_b).await.unwrap();
        adapter.add_node(&node_c).await.unwrap();

        // Filter by value = 100 (property field, filtered in Rust)
        let mut filters = HashMap::new();
        filters.insert(Cow::Borrowed("value"), vec![json!(100)]);

        let (nodes, _edges) = adapter.get_filtered_graph_data(&filters).await.unwrap();

        assert_eq!(nodes.len(), 2);
        for (_, node) in &nodes {
            assert_eq!(node.get("value").unwrap().as_i64().unwrap(), 100);
        }
    }

    #[tokio::test]
    #[serial]
    async fn test_get_filtered_graph_data_mixed_core_and_property_filters() {
        let (adapter, _temp_dir) = setup_adapter().await;

        let node_a = TestNode::new("node-a", "Alice", 100);
        let node_b = TestNode::new("node-b", "Bob", 100);
        let node_c = TestNode::new("node-c", "Charlie", 200);
        adapter.add_node(&node_a).await.unwrap();
        adapter.add_node(&node_b).await.unwrap();
        adapter.add_node(&node_c).await.unwrap();

        // Filter by name IN ["Alice", "Bob"] (core) AND value = 100 (property)
        let mut filters = HashMap::new();
        filters.insert(Cow::Borrowed("name"), vec![json!("Alice"), json!("Bob")]);
        filters.insert(Cow::Borrowed("value"), vec![json!(100)]);

        let (nodes, _edges) = adapter.get_filtered_graph_data(&filters).await.unwrap();

        assert_eq!(nodes.len(), 2);
        for (_, node) in &nodes {
            let name = node.get("name").unwrap().as_str().unwrap();
            assert!(name == "Alice" || name == "Bob");
            assert_eq!(node.get("value").unwrap().as_i64().unwrap(), 100);
        }
    }

    #[tokio::test]
    #[serial]
    async fn test_get_filtered_graph_data_no_matches() {
        let (adapter, _temp_dir) = setup_adapter().await;

        let node_a = TestNode::new("node-a", "Alice", 100);
        let node_b = TestNode::new("node-b", "Bob", 200);
        adapter.add_node(&node_a).await.unwrap();
        adapter.add_node(&node_b).await.unwrap();

        // Filter by non-existent name
        let mut filters = HashMap::new();
        filters.insert(Cow::Borrowed("name"), vec![json!("NonExistent")]);

        let (nodes, edges) = adapter.get_filtered_graph_data(&filters).await.unwrap();

        assert_eq!(nodes.len(), 0);
        assert_eq!(edges.len(), 0);
    }

    #[tokio::test]
    #[serial]
    async fn test_get_filtered_graph_data_edges_between_filtered_nodes() {
        let (adapter, _temp_dir) = setup_adapter().await;

        let node_a = TestNode::new("node-a", "Alice", 100);
        let node_b = TestNode::new("node-b", "Bob", 200);
        let node_c = TestNode::new("node-c", "Charlie", 300);
        adapter.add_node(&node_a).await.unwrap();
        adapter.add_node(&node_b).await.unwrap();
        adapter.add_node(&node_c).await.unwrap();

        // Create edges: A -> B, B -> C, A -> C
        adapter
            .add_edge("node-a", "node-b", "knows", None)
            .await
            .unwrap();
        adapter
            .add_edge("node-b", "node-c", "knows", None)
            .await
            .unwrap();
        adapter
            .add_edge("node-a", "node-c", "knows", None)
            .await
            .unwrap();

        // Filter for Alice and Bob only
        let mut filters = HashMap::new();
        filters.insert(Cow::Borrowed("name"), vec![json!("Alice"), json!("Bob")]);

        let (nodes, edges) = adapter.get_filtered_graph_data(&filters).await.unwrap();

        assert_eq!(nodes.len(), 2);
        // Only edge A -> B should be returned (both nodes in filter)
        // Edge B -> C and A -> C should NOT be returned (Charlie not in filter)
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].0, "node-a"); // source
        assert_eq!(edges[0].1, "node-b"); // target
    }

    #[tokio::test]
    #[serial]
    async fn test_get_filtered_graph_data_multiple_relationship_types() {
        let (adapter, _temp_dir) = setup_adapter().await;

        let node_a = TestNode::new("node-a", "Alice", 100);
        let node_b = TestNode::new("node-b", "Bob", 200);
        adapter.add_node(&node_a).await.unwrap();
        adapter.add_node(&node_b).await.unwrap();

        // Create multiple edges with different relationship types
        adapter
            .add_edge("node-a", "node-b", "knows", None)
            .await
            .unwrap();
        adapter
            .add_edge("node-a", "node-b", "works_with", None)
            .await
            .unwrap();
        adapter
            .add_edge("node-a", "node-b", "friends_with", None)
            .await
            .unwrap();

        // Filter for both nodes
        let mut filters = HashMap::new();
        filters.insert(Cow::Borrowed("name"), vec![json!("Alice"), json!("Bob")]);

        let (nodes, edges) = adapter.get_filtered_graph_data(&filters).await.unwrap();

        assert_eq!(nodes.len(), 2);
        // All three edges should be returned
        assert_eq!(edges.len(), 3);

        let rel_names: Vec<_> = edges.iter().map(|(_, _, rel, _)| rel.as_str()).collect();
        assert!(rel_names.contains(&"knows"));
        assert!(rel_names.contains(&"works_with"));
        assert!(rel_names.contains(&"friends_with"));
    }

    // Tests for get_nodeset_subgraph

    #[tokio::test]
    #[serial]
    async fn test_get_nodeset_subgraph_empty_node_names() {
        let (adapter, _temp_dir) = setup_adapter().await;

        let node_a = TestNode::new("node-a", "Alice", 100);
        adapter.add_node(&node_a).await.unwrap();

        // Empty node_names should return empty result
        let (nodes, edges) = adapter.get_nodeset_subgraph("TestNode", &[]).await.unwrap();

        assert_eq!(nodes.len(), 0);
        assert_eq!(edges.len(), 0);
    }

    #[tokio::test]
    #[serial]
    async fn test_get_nodeset_subgraph_non_existent_type() {
        let (adapter, _temp_dir) = setup_adapter().await;

        let node_a = TestNode::new("node-a", "Alice", 100);
        adapter.add_node(&node_a).await.unwrap();

        // Non-existent type should return empty result
        let (nodes, edges) = adapter
            .get_nodeset_subgraph("NonExistentType", &["Alice".to_string()])
            .await
            .unwrap();

        assert_eq!(nodes.len(), 0);
        assert_eq!(edges.len(), 0);
    }

    #[tokio::test]
    #[serial]
    async fn test_get_nodeset_subgraph_non_existent_names() {
        let (adapter, _temp_dir) = setup_adapter().await;

        let node_a = TestNode::new("node-a", "Alice", 100);
        adapter.add_node(&node_a).await.unwrap();

        // Non-existent names should return empty result
        let (nodes, edges) = adapter
            .get_nodeset_subgraph("test", &["Bob".to_string(), "Charlie".to_string()])
            .await
            .unwrap();

        assert_eq!(nodes.len(), 0);
        assert_eq!(edges.len(), 0);
    }

    #[tokio::test]
    #[serial]
    async fn test_get_nodeset_subgraph_valid_single_node() {
        let (adapter, _temp_dir) = setup_adapter().await;

        let node_a = TestNode::new("node-a", "Alice", 100);
        let node_b = TestNode::new("node-b", "Bob", 200);
        adapter.add_node(&node_a).await.unwrap();
        adapter.add_node(&node_b).await.unwrap();

        // Get single node by type + name
        let (nodes, _edges) = adapter
            .get_nodeset_subgraph("TestNode", &["Alice".to_string()])
            .await
            .unwrap();

        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].1.get("name").unwrap().as_str().unwrap(), "Alice");
        assert_eq!(
            nodes[0].1.get("type").unwrap().as_str().unwrap(),
            "TestNode"
        );
    }

    #[tokio::test]
    #[serial]
    async fn test_get_nodeset_subgraph_valid_multiple_nodes() {
        let (adapter, _temp_dir) = setup_adapter().await;

        let node_a = TestNode::new("node-a", "Alice", 100);
        let node_b = TestNode::new("node-b", "Bob", 200);
        let node_c = TestNode::new("node-c", "Charlie", 300);
        adapter.add_node(&node_a).await.unwrap();
        adapter.add_node(&node_b).await.unwrap();
        adapter.add_node(&node_c).await.unwrap();

        // Get multiple nodes by type + names
        let (nodes, _edges) = adapter
            .get_nodeset_subgraph("TestNode", &["Alice".to_string(), "Charlie".to_string()])
            .await
            .unwrap();

        assert_eq!(nodes.len(), 2);
        let names: Vec<_> = nodes
            .iter()
            .map(|(_, n)| n.get("name").unwrap().as_str().unwrap())
            .collect();
        assert!(names.contains(&"Alice"));
        assert!(names.contains(&"Charlie"));
        assert!(!names.contains(&"Bob"));
    }

    #[tokio::test]
    #[serial]
    async fn test_get_nodeset_subgraph_edges_only_between_specified_nodes() {
        let (adapter, _temp_dir) = setup_adapter().await;

        let node_a = TestNode::new("node-a", "Alice", 100);
        let node_b = TestNode::new("node-b", "Bob", 200);
        let node_c = TestNode::new("node-c", "Charlie", 300);
        adapter.add_node(&node_a).await.unwrap();
        adapter.add_node(&node_b).await.unwrap();
        adapter.add_node(&node_c).await.unwrap();

        // Create edges: A -> B, B -> C, A -> C
        adapter
            .add_edge("node-a", "node-b", "knows", None)
            .await
            .unwrap();
        adapter
            .add_edge("node-b", "node-c", "knows", None)
            .await
            .unwrap();
        adapter
            .add_edge("node-a", "node-c", "knows", None)
            .await
            .unwrap();

        // Get subgraph for Alice and Bob only
        let (nodes, edges) = adapter
            .get_nodeset_subgraph("TestNode", &["Alice".to_string(), "Bob".to_string()])
            .await
            .unwrap();

        assert_eq!(nodes.len(), 2);
        // Only edge A -> B should be returned (both nodes in subgraph)
        // Edge B -> C and A -> C should NOT be returned (Charlie not in subgraph)
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].0, "node-a"); // source
        assert_eq!(edges[0].1, "node-b"); // target
    }

    #[tokio::test]
    #[serial]
    async fn test_get_nodeset_subgraph_no_edges_between_nodes() {
        let (adapter, _temp_dir) = setup_adapter().await;

        let node_a = TestNode::new("node-a", "Alice", 100);
        let node_b = TestNode::new("node-b", "Bob", 200);
        adapter.add_node(&node_a).await.unwrap();
        adapter.add_node(&node_b).await.unwrap();

        // No edges created
        let (nodes, edges) = adapter
            .get_nodeset_subgraph("TestNode", &["Alice".to_string(), "Bob".to_string()])
            .await
            .unwrap();

        assert_eq!(nodes.len(), 2);
        assert_eq!(edges.len(), 0);
    }

    #[tokio::test]
    #[serial]
    async fn test_get_nodeset_subgraph_densely_connected() {
        let (adapter, _temp_dir) = setup_adapter().await;

        let node_a = TestNode::new("node-a", "Alice", 100);
        let node_b = TestNode::new("node-b", "Bob", 200);
        let node_c = TestNode::new("node-c", "Charlie", 300);
        adapter.add_node(&node_a).await.unwrap();
        adapter.add_node(&node_b).await.unwrap();
        adapter.add_node(&node_c).await.unwrap();

        // Create a densely connected subgraph: all pairs connected
        adapter
            .add_edge("node-a", "node-b", "knows", None)
            .await
            .unwrap();
        adapter
            .add_edge("node-b", "node-a", "knows", None)
            .await
            .unwrap();
        adapter
            .add_edge("node-a", "node-c", "knows", None)
            .await
            .unwrap();
        adapter
            .add_edge("node-c", "node-a", "knows", None)
            .await
            .unwrap();
        adapter
            .add_edge("node-b", "node-c", "knows", None)
            .await
            .unwrap();
        adapter
            .add_edge("node-c", "node-b", "knows", None)
            .await
            .unwrap();

        // Get all three nodes
        let (nodes, edges) = adapter
            .get_nodeset_subgraph(
                "TestNode",
                &[
                    "Alice".to_string(),
                    "Bob".to_string(),
                    "Charlie".to_string(),
                ],
            )
            .await
            .unwrap();

        assert_eq!(nodes.len(), 3);
        // All 6 edges should be returned
        assert_eq!(edges.len(), 6);
    }

    #[tokio::test]
    #[serial]
    async fn test_get_nodeset_subgraph_filters_by_type() {
        let (adapter, _temp_dir) = setup_adapter().await;

        // Create nodes with different types
        #[derive(Serialize)]
        struct TypedNode {
            id: String,
            name: String,
            data_type: String,
            created_at: String,
            updated_at: String,
        }

        let person_a = TypedNode {
            id: "node-a".to_string(),
            name: "Alice".to_string(),
            data_type: "Person".to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        };

        let org_a = TypedNode {
            id: "node-b".to_string(),
            name: "Alice".to_string(), // Same name, different type
            data_type: "Organization".to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        };

        adapter.add_node(&person_a).await.unwrap();
        adapter.add_node(&org_a).await.unwrap();

        // Get only Person type with name Alice
        let (nodes, _edges) = adapter
            .get_nodeset_subgraph("Person", &["Alice".to_string()])
            .await
            .unwrap();

        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].1.get("name").unwrap().as_str().unwrap(), "Alice");
        assert_eq!(nodes[0].1.get("type").unwrap().as_str().unwrap(), "Person");
    }
}
