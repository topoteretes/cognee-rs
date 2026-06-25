//!
//! Implementation of GraphDBTrait using Ladybug (lbug) embedded graph database.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use cognee_utils::redact::redact;
use cognee_utils::tracing_keys::{COGNEE_DB_QUERY, COGNEE_DB_ROW_COUNT};
use lbug::{Connection, Database, SystemConfig, Value as LbugValue};
use serde_json::{Value, json};
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tracing::{Span, instrument};

/// Default max database size: 1 GiB.
///
/// Kuzu's built-in default is 8 TiB, which it reserves via a sparse mmap on
/// startup. Many container runtimes (including GitHub Actions runners) cap the
/// process address space well below 8 TiB, causing initialization to fail.
/// 1 GiB is sufficient for typical knowledge-graph workloads and works in all
/// CI environments. Override with `GRAPH_MAX_DB_SIZE` (bytes) for larger graphs.
const DEFAULT_MAX_DB_SIZE: u64 = 1024 * 1024 * 1024;

fn read_max_db_size() -> u64 {
    std::env::var("GRAPH_MAX_DB_SIZE")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_MAX_DB_SIZE)
}

use crate::{EdgeData, GraphDBError, GraphDBResult, GraphDBTrait, GraphNode, NodeData};

/// Strip control characters (`\u{0000}`–`\u{001F}`, except `\t`, `\n`, `\r`)
/// from all string values inside a `serde_json::Value` tree.
///
/// LLM-extracted text occasionally contains stray control characters.
/// They carry no semantic value and corrupt the JSON round-trip through
/// Ladybug's text property storage, so we remove them at the write boundary.
fn sanitize_json_value(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(s)
            if s.bytes()
                .any(|b| b < 0x20 && b != b'\t' && b != b'\n' && b != b'\r') =>
        {
            *s = s
                .chars()
                .filter(|&c| c >= '\u{0020}' || c == '\t' || c == '\n' || c == '\r')
                .collect();
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                sanitize_json_value(item);
            }
        }
        serde_json::Value::Object(map) => {
            for val in map.values_mut() {
                sanitize_json_value(val);
            }
        }
        _ => {}
    }
}

/// Defence-in-depth: escape unescaped control characters in a raw JSON
/// string read back from Ladybug, so that `serde_json::from_str` succeeds.
///
/// The primary sanitisation happens on _write_ (see [`sanitize_json_value`]),
/// but data that was stored before the write-side fix still needs this.
fn sanitize_json_control_chars(s: &str) -> Cow<'_, str> {
    if s.bytes().any(|b| b < 0x20) {
        let mut out = String::with_capacity(s.len());
        for ch in s.chars() {
            if ch < '\u{0020}' && ch != '\n' && ch != '\r' && ch != '\t' {
                out.push_str(&format!("\\u{:04x}", ch as u32));
            } else if ch == '\n' {
                out.push_str("\\n");
            } else if ch == '\r' {
                out.push_str("\\r");
            } else if ch == '\t' {
                out.push_str("\\t");
            } else {
                out.push(ch);
            }
        }
        Cow::Owned(out)
    } else {
        Cow::Borrowed(s)
    }
}

/// Ladybug graph database adapter.
///
/// This adapter provides a complete implementation of GraphDBTrait using
/// the Ladybug (lbug) embedded graph database.
///
/// Parity note:
/// Python documents the default file-backed deployment model as a single
/// owning process for SQLite/Ladybug/LanceDB access (for example via the
/// API server `--api-url` mode and single-worker session locks), while also
/// supporting an opt-in Redis-backed shared Ladybug lock for multi-process
/// coordination. Rust currently matches the default single-process model:
/// overlapping writes are serialized in-process, and cross-process locking is
/// intentionally out of scope.
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
    // Single-process assumption: this lock serializes overlapping writes
    // within one process. Cross-process locking is intentionally out of scope.
    write_lock: Arc<Mutex<()>>,
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
        let config = SystemConfig::default().max_db_size(read_max_db_size());
        let db = Database::new(db_path, config).map_err(|e| {
            GraphDBError::InitializationError(format!("Failed to create database: {e}"))
        })?;

        Ok(Self {
            db_path: db_path.to_string(),
            db: Arc::new(db),
            write_lock: Arc::new(Mutex::new(())),
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
    #[instrument(
        name = "cognee.db.graph.query",
        level = "info",
        skip_all,
        fields(
            cognee.db.system = "ladybug",
            cognee.db.query = tracing::field::Empty,
            cognee.db.row_count = tracing::field::Empty,
        ),
        err,
    )]
    fn execute_query(&self, query: &str) -> GraphDBResult<Vec<Vec<serde_json::Value>>> {
        // Truncate-then-redact (locked decision 9). The 500-char
        // truncation must come BEFORE redact() so a redacted form
        // longer than 500 chars cannot be re-truncated and split the
        // literal `***REDACTED***` marker. Walk to the last UTF-8
        // char boundary at-or-before byte 500 so non-ASCII queries
        // do not panic on slicing.
        let truncated = if query.len() > 500 {
            let mut end = 500;
            while !query.is_char_boundary(end) {
                end -= 1;
            }
            &query[..end]
        } else {
            query
        };
        Span::current().record(COGNEE_DB_QUERY, redact(truncated).as_ref());

        let conn = Connection::new(&self.db).map_err(|e| {
            GraphDBError::ConnectionError(format!("Failed to create connection: {e}"))
        })?;

        let result = conn
            .query(query)
            .map_err(|e| GraphDBError::QueryError(format!("Query failed: {e}")))?;

        let rows: Vec<Vec<serde_json::Value>> = result
            .map(|row| row.into_iter().map(Self::lbug_value_to_json).collect())
            .collect();

        Span::current().record(COGNEE_DB_ROW_COUNT, rows.len() as i64);
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
                    obj.insert(format!("key_{i}"), Self::lbug_value_to_json(k.clone()));
                    obj.insert(format!("val_{i}"), Self::lbug_value_to_json(v.clone()));
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

        // Strip control characters from LLM-produced string values before
        // persisting, so the JSON round-trip through Ladybug stays clean.
        let mut props_value = serde_json::Value::Object(props);
        sanitize_json_value(&mut props_value);

        let properties_json =
            serde_json::to_string(&props_value).map_err(GraphDBError::SerializationError)?;

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
            let clean = sanitize_json_control_chars(props_str);
            let additional_props: HashMap<Cow<'static, str>, serde_json::Value> =
                serde_json::from_str(&clean).map_err(GraphDBError::SerializationError)?;
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

    fn escape_cypher_string(value: &str) -> String {
        value.replace('\\', "\\\\").replace('\'', "\\'")
    }

    fn upsert_node_with_conn(
        &self,
        conn: &Connection,
        props: &NodeProperties,
    ) -> GraphDBResult<()> {
        let id = Self::escape_cypher_string(&props.id);
        let name = Self::escape_cypher_string(&props.name);
        let node_type = Self::escape_cypher_string(&props.node_type);
        let properties = Self::escape_cypher_string(&props.properties);
        let created_at_str = props.created_at.format("%Y-%m-%d %H:%M:%S%.6f").to_string();
        let updated_at_str = props.updated_at.format("%Y-%m-%d %H:%M:%S%.6f").to_string();

        let exists_query = format!("MATCH (n:Node) WHERE n.id = '{id}' RETURN COUNT(n) AS count");
        let exists = conn
            .query(&exists_query)
            .map_err(|e| GraphDBError::NodeError(format!("Failed to check node existence: {e}")))?
            .next()
            .and_then(|row| row.into_iter().next())
            .and_then(|value| match value {
                LbugValue::Int64(count) => Some(count > 0),
                LbugValue::Int32(count) => Some(count > 0),
                LbugValue::UInt64(count) => Some(count > 0),
                LbugValue::UInt32(count) => Some(count > 0),
                _ => None,
            })
            .unwrap_or(false);

        let query = if exists {
            format!(
                r#"MATCH (n:Node) WHERE n.id = '{id}'
SET n.name = '{name}',
    n.type = '{node_type}',
    n.updated_at = timestamp('{updated_at_str}'),
    n.properties = '{properties}'"#
            )
        } else {
            format!(
                r#"CREATE (:Node {{
    id: '{id}',
    name: '{name}',
    type: '{node_type}',
    created_at: timestamp('{created_at_str}'),
    updated_at: timestamp('{updated_at_str}'),
    properties: '{properties}'
}})"#
            )
        };

        conn.query(&query).map_err(|e| {
            GraphDBError::NodeError(format!("Failed to upsert node '{}': {e}", props.id))
        })?;

        Ok(())
    }

    fn serialize_edge_properties(
        &self,
        properties: Option<HashMap<Cow<'static, str>, serde_json::Value>>,
    ) -> GraphDBResult<String> {
        if let Some(props) = properties {
            let mut val = serde_json::to_value(&props).map_err(GraphDBError::SerializationError)?;
            sanitize_json_value(&mut val);
            serde_json::to_string(&val).map_err(GraphDBError::SerializationError)
        } else {
            Ok("{}".to_string())
        }
    }

    fn upsert_edge_with_conn(
        &self,
        conn: &Connection,
        source_id: &str,
        target_id: &str,
        relationship_name: &str,
        properties_json: &str,
        updated_at: DateTime<Utc>,
    ) -> GraphDBResult<()> {
        let source_id = Self::escape_cypher_string(source_id);
        let target_id = Self::escape_cypher_string(target_id);
        let relationship_name = Self::escape_cypher_string(relationship_name);
        let properties_json = Self::escape_cypher_string(properties_json);
        let timestamp_str = updated_at.format("%Y-%m-%d %H:%M:%S%.6f").to_string();

        let exists_query = format!(
            "MATCH (a:Node)-[r:EDGE]->(b:Node) WHERE a.id = '{source_id}' AND b.id = '{target_id}' AND r.relationship_name = '{relationship_name}' RETURN COUNT(r) AS count"
        );
        let exists = conn
            .query(&exists_query)
            .map_err(|e| GraphDBError::EdgeError(format!("Failed to check edge existence: {e}")))?
            .next()
            .and_then(|row| row.into_iter().next())
            .and_then(|value| match value {
                LbugValue::Int64(count) => Some(count > 0),
                LbugValue::Int32(count) => Some(count > 0),
                LbugValue::UInt64(count) => Some(count > 0),
                LbugValue::UInt32(count) => Some(count > 0),
                _ => None,
            })
            .unwrap_or(false);

        let query = if exists {
            format!(
                r#"MATCH (a:Node)-[r:EDGE]->(b:Node)
WHERE a.id = '{source_id}' AND b.id = '{target_id}' AND r.relationship_name = '{relationship_name}'
SET r.updated_at = timestamp('{timestamp_str}'),
    r.properties = '{properties_json}'"#
            )
        } else {
            format!(
                r#"MATCH (a:Node {{id: '{source_id}'}}), (b:Node {{id: '{target_id}'}})
CREATE (a)-[:EDGE {{
    relationship_name: '{relationship_name}',
    created_at: timestamp('{timestamp_str}'),
    updated_at: timestamp('{timestamp_str}'),
    properties: '{properties_json}'
}}]->(b)"#
            )
        };

        conn.query(&query).map_err(|e| {
            GraphDBError::EdgeError(format!(
                "Failed to upsert edge {source_id} -[{relationship_name}]-> {target_id}: {e}"
            ))
        })?;

        Ok(())
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
            GraphDBError::ConnectionError(format!("Failed to create connection: {e}"))
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
            GraphDBError::InitializationError(format!("Failed to create Node table: {e}"))
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
            GraphDBError::InitializationError(format!("Failed to create EDGE table: {e}"))
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
            GraphDBError::ConnectionError(format!("Failed to create connection: {e}"))
        })?;

        // Delete all edges first
        conn.query("MATCH (a:Node)-[r:EDGE]->(b:Node) DELETE r")
            .map_err(|e| GraphDBError::QueryError(format!("Failed to delete edges: {e}")))?;

        // Delete all nodes
        conn.query("MATCH (n:Node) DELETE n")
            .map_err(|e| GraphDBError::QueryError(format!("Failed to delete nodes: {e}")))?;

        Ok(())
    }

    async fn has_node(&self, node_id: &str) -> GraphDBResult<bool> {
        let query = format!(
            "MATCH (n:Node) WHERE n.id = '{}' RETURN COUNT(n) AS count",
            node_id.replace('\\', "\\\\").replace('\'', "\\'")
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
        let _write_guard = self.write_lock.lock().map_err(|_| {
            GraphDBError::ConnectionError("Ladybug write lock poisoned".to_string())
        })?;
        let conn = Connection::new(&self.db).map_err(|e| {
            GraphDBError::ConnectionError(format!("Failed to create connection: {e}"))
        })?;

        self.upsert_node_with_conn(&conn, &props)
    }

    async fn add_nodes_raw(&self, nodes: Vec<Value>) -> GraphDBResult<()> {
        // Batch insert optimization: process nodes in chunks to avoid
        // query size limits and improve performance
        const BATCH_SIZE: usize = 500;

        if nodes.is_empty() {
            return Ok(());
        }

        let _write_guard = self.write_lock.lock().map_err(|_| {
            GraphDBError::ConnectionError("Ladybug write lock poisoned".to_string())
        })?;

        let conn = Connection::new(&self.db).map_err(|e| {
            GraphDBError::ConnectionError(format!("Failed to create connection: {e}"))
        })?;

        // Process in batches. Each batch is a single `UNWIND … MERGE` query
        // (mirrors the Python Ladybug adapter) instead of a per-node
        // exists-check + CREATE/SET — one round-trip per batch, and MERGE makes
        // it idempotent without the extra COUNT query.
        for chunk in nodes.chunks(BATCH_SIZE) {
            let mut items = Vec::with_capacity(chunk.len());
            for node in chunk {
                let p = self.serialize_to_node_props(node)?;
                items.push(format!(
                    "{{id:'{}', name:'{}', type:'{}', properties:'{}', created_at:'{}', updated_at:'{}'}}",
                    Self::escape_cypher_string(&p.id),
                    Self::escape_cypher_string(&p.name),
                    Self::escape_cypher_string(&p.node_type),
                    Self::escape_cypher_string(&p.properties),
                    p.created_at.format("%Y-%m-%d %H:%M:%S%.6f"),
                    p.updated_at.format("%Y-%m-%d %H:%M:%S%.6f"),
                ));
            }
            let query = format!(
                "UNWIND [{}] AS node \
                 MERGE (n:Node {{id: node.id}}) \
                 ON CREATE SET n.name = node.name, n.type = node.type, \
                     n.created_at = timestamp(node.created_at), \
                     n.updated_at = timestamp(node.updated_at), n.properties = node.properties \
                 ON MATCH SET n.name = node.name, n.type = node.type, \
                     n.updated_at = timestamp(node.updated_at), n.properties = node.properties",
                items.join(", ")
            );
            conn.query(&query).map_err(|e| {
                GraphDBError::NodeError(format!(
                    "Failed to batch-upsert {} nodes: {e}",
                    chunk.len()
                ))
            })?;
        }

        Ok(())
    }

    async fn delete_node(&self, node_id: &str) -> GraphDBResult<()> {
        let conn = Connection::new(&self.db).map_err(|e| {
            GraphDBError::ConnectionError(format!("Failed to create connection: {e}"))
        })?;

        let query = format!(
            "MATCH (n:Node) WHERE n.id = '{}' DETACH DELETE n",
            node_id.replace('\\', "\\\\").replace('\'', "\\'")
        );

        conn.query(&query)
            .map_err(|e| GraphDBError::NodeError(format!("Failed to delete node: {e}")))?;

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
            node_id.replace('\\', "\\\\").replace('\'', "\\'")
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
            source_id.replace('\\', "\\\\").replace('\'', "\\'"),
            target_id.replace('\\', "\\\\").replace('\'', "\\'"),
            relationship_name.replace('\\', "\\\\").replace('\'', "\\'")
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
        let _write_guard = self.write_lock.lock().map_err(|_| {
            GraphDBError::ConnectionError("Ladybug write lock poisoned".to_string())
        })?;
        let conn = Connection::new(&self.db).map_err(|e| {
            GraphDBError::ConnectionError(format!("Failed to create connection: {e}"))
        })?;

        let now = Utc::now();
        let props_json = self.serialize_edge_properties(properties)?;

        self.upsert_edge_with_conn(
            &conn,
            source_id,
            target_id,
            relationship_name,
            &props_json,
            now,
        )
    }

    async fn add_edges(&self, edges: &[EdgeData]) -> GraphDBResult<()> {
        if edges.is_empty() {
            return Ok(());
        }

        let _write_guard = self.write_lock.lock().map_err(|_| {
            GraphDBError::ConnectionError("Ladybug write lock poisoned".to_string())
        })?;
        let conn = Connection::new(&self.db).map_err(|e| {
            GraphDBError::ConnectionError(format!("Failed to create connection: {e}"))
        })?;

        // One `UNWIND … MATCH … MERGE` query per batch (mirrors the Python
        // Ladybug adapter), replacing the per-edge exists-check + CREATE/SET.
        const BATCH_SIZE: usize = 500;
        let now = Utc::now().format("%Y-%m-%d %H:%M:%S%.6f").to_string();
        for chunk in edges.chunks(BATCH_SIZE) {
            let mut items = Vec::with_capacity(chunk.len());
            for edge in chunk {
                let props_json = self.serialize_edge_properties(Some(edge.3.clone()))?;
                items.push(format!(
                    "{{from_id:'{}', to_id:'{}', relationship_name:'{}', properties:'{}', created_at:'{now}', updated_at:'{now}'}}",
                    Self::escape_cypher_string(&edge.0),
                    Self::escape_cypher_string(&edge.1),
                    Self::escape_cypher_string(&edge.2),
                    Self::escape_cypher_string(&props_json),
                ));
            }
            let query = format!(
                "UNWIND [{}] AS edge \
                 MATCH (a:Node), (b:Node) WHERE a.id = edge.from_id AND b.id = edge.to_id \
                 MERGE (a)-[r:EDGE {{relationship_name: edge.relationship_name}}]->(b) \
                 ON CREATE SET r.created_at = timestamp(edge.created_at), \
                     r.updated_at = timestamp(edge.updated_at), r.properties = edge.properties \
                 ON MATCH SET r.updated_at = timestamp(edge.updated_at), r.properties = edge.properties",
                items.join(", ")
            );
            conn.query(&query).map_err(|e| {
                GraphDBError::EdgeError(format!(
                    "Failed to batch-upsert {} edges: {e}",
                    chunk.len()
                ))
            })?;
        }
        Ok(())
    }

    async fn get_edges(&self, node_id: &str) -> GraphDBResult<Vec<EdgeData>> {
        let query = format!(
            "MATCH (n:Node)-[r:EDGE]-(m:Node) WHERE n.id = '{}' RETURN n.id AS source_id, m.id AS target_id, r.relationship_name AS rel_name, r.properties AS props",
            node_id.replace('\\', "\\\\").replace('\'', "\\'")
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
                    let clean = sanitize_json_control_chars(props_str);
                    serde_json::from_str::<HashMap<Cow<'static, str>, serde_json::Value>>(&clean)
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
            node_id.replace('\\', "\\\\").replace('\'', "\\'")
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
            node_id.replace('\\', "\\\\").replace('\'', "\\'")
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
                    let clean = sanitize_json_control_chars(rel_props_str);
                    let additional_props: HashMap<Cow<'static, str>, serde_json::Value> =
                        serde_json::from_str(&clean).unwrap_or_default();
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
                    let clean = sanitize_json_control_chars(props_str);
                    serde_json::from_str::<HashMap<Cow<'static, str>, serde_json::Value>>(&clean)
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
                            format!(
                                "n.{} = '{}'",
                                attr,
                                s.replace('\\', "\\\\").replace('\'', "\\'")
                            )
                        } else {
                            format!("n.{attr} = {v}")
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
            "MATCH (n:Node) {where_clause} RETURN n.id AS id, n.name AS name, n.type AS type, n.properties AS properties"
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
            .map(|id| format!("'{}'", id.replace('\\', "\\\\").replace('\'', "\\'")))
            .collect::<Vec<_>>()
            .join(", ");

        let edges_query = format!(
            "MATCH (a:Node)-[r:EDGE]->(b:Node) WHERE a.id IN [{id_list}] AND b.id IN [{id_list}] RETURN a.id, b.id, r.relationship_name, r.properties"
        );

        let edge_results = self.execute_query(&edges_query)?;
        let mut edges = Vec::new();

        for row in edge_results {
            if row.len() >= Self::EDGE_QUERY_COLUMN_COUNT {
                let source_id = row[0].as_str().unwrap_or("").to_string();
                let target_id = row[1].as_str().unwrap_or("").to_string();
                let rel_name = row[2].as_str().unwrap_or("").to_string();
                let props = if let Some(props_str) = row[3].as_str() {
                    let clean = sanitize_json_control_chars(props_str);
                    serde_json::from_str(&clean).unwrap_or_default()
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
        node_name_filter_operator: &str,
    ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
        use std::collections::HashSet;

        // Early return for empty node_names
        if node_names.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }

        // Build IN clause for names
        let name_list = node_names
            .iter()
            .map(|name| format!("'{}'", name.replace('\\', "\\\\").replace('\'', "\\'")))
            .collect::<Vec<_>>()
            .join(", ");

        // Query for specific (primary) nodes matching type and names
        let nodes_query = format!(
            "MATCH (n:Node) WHERE n.type = '{}' AND n.name IN [{}] RETURN n.id AS id, n.name AS name, n.type AS type, n.properties AS properties",
            node_type.replace('\\', "\\\\").replace('\'', "\\'"),
            name_list
        );

        let node_results = self.execute_query(&nodes_query)?;

        let mut primary_node_ids: Vec<String> = Vec::new();

        // Parse primary node IDs
        for row in &node_results {
            if row.len() >= Self::NODE_QUERY_COLUMN_COUNT
                && let Some(id_str) = row[0].as_str()
            {
                primary_node_ids.push(id_str.to_string());
            }
        }

        if primary_node_ids.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }

        // Gather 1-hop neighbor IDs per primary node, applying OR/AND logic.
        let use_and = node_name_filter_operator.eq_ignore_ascii_case("AND");
        let mut neighbor_id_set: Option<HashSet<String>> = None;

        for primary_id in &primary_node_ids {
            let escaped_id = primary_id.replace('\\', "\\\\").replace('\'', "\\'");
            let neighbor_query = format!(
                "MATCH (n:Node)-[:EDGE]-(nbr:Node) WHERE n.id = '{escaped_id}' RETURN DISTINCT nbr.id"
            );
            let nbr_results = self.execute_query(&neighbor_query)?;
            let nbr_ids: HashSet<String> = nbr_results
                .iter()
                .filter_map(|row| row.first().and_then(|v| v.as_str()).map(str::to_string))
                .collect();

            match neighbor_id_set {
                None => {
                    neighbor_id_set = Some(nbr_ids);
                }
                Some(ref mut existing) => {
                    if use_and {
                        // AND: keep only IDs present in all sets (intersection)
                        existing.retain(|id| nbr_ids.contains(id));
                    } else {
                        // OR: add all neighbor IDs (union)
                        existing.extend(nbr_ids);
                    }
                }
            }
        }

        // Union of primary nodes and their filtered neighbors
        let mut all_ids: HashSet<String> = primary_node_ids.iter().cloned().collect();
        if let Some(nbr_set) = neighbor_id_set {
            all_ids.extend(nbr_set);
        }

        let id_list = all_ids
            .iter()
            .map(|id| format!("'{}'", id.replace('\\', "\\\\").replace('\'', "\\'")))
            .collect::<Vec<_>>()
            .join(", ");

        // Fetch full node data for all IDs
        let all_nodes_query = format!(
            "MATCH (n:Node) WHERE n.id IN [{id_list}] RETURN n.id AS id, n.name AS name, n.type AS type, n.properties AS properties"
        );
        let all_node_results = self.execute_query(&all_nodes_query)?;

        let mut nodes: Vec<GraphNode> = Vec::new();
        for row in all_node_results {
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

        // Get all edges between nodes in the final set
        let edges_query = format!(
            "MATCH (a:Node)-[r:EDGE]->(b:Node) WHERE a.id IN [{id_list}] AND b.id IN [{id_list}] RETURN a.id, b.id, r.relationship_name, r.properties"
        );

        let edge_results = self.execute_query(&edges_query)?;
        let mut edges = Vec::new();

        for row in edge_results {
            if row.len() >= Self::EDGE_QUERY_COLUMN_COUNT {
                let source_id = row[0].as_str().unwrap_or("").to_string();
                let target_id = row[1].as_str().unwrap_or("").to_string();
                let rel_name = row[2].as_str().unwrap_or("").to_string();
                let props = if let Some(props_str) = row[3].as_str() {
                    let clean = sanitize_json_control_chars(props_str);
                    serde_json::from_str(&clean).unwrap_or_default()
                } else {
                    HashMap::new()
                };
                edges.push((source_id, target_id, rel_name, props));
            }
        }

        Ok((nodes, edges))
    }

    // -----------------------------------------------------------------
    // In-place property updates
    // -----------------------------------------------------------------
    //
    // Ladybug stores node/edge custom properties as a JSON string column
    // named `properties`. To update a single key without cascading
    // deletes, we read the current JSON, merge the new value, and rewrite
    // the column via a single MATCH ... SET ... statement.
    async fn update_node_property(
        &self,
        node_id: &str,
        key: &str,
        value: serde_json::Value,
    ) -> GraphDBResult<()> {
        let read_query = format!(
            "MATCH (n:Node) WHERE n.id = '{}' RETURN n.properties AS properties",
            node_id.replace('\\', "\\\\").replace('\'', "\\'")
        );
        let rows = self.execute_query(&read_query)?;
        let mut props: serde_json::Map<String, serde_json::Value> = if let Some(row) = rows.first()
            && let Some(props_str) = row.first().and_then(|v| v.as_str())
        {
            let clean = sanitize_json_control_chars(props_str);
            serde_json::from_str(&clean).unwrap_or_default()
        } else {
            return Err(GraphDBError::NodeError(format!(
                "Node not found: {node_id}"
            )));
        };
        props.insert(key.to_string(), value);
        let mut props_val = serde_json::Value::Object(props);
        sanitize_json_value(&mut props_val);
        let props_json =
            serde_json::to_string(&props_val).map_err(GraphDBError::SerializationError)?;

        let now = Utc::now();
        let ts = now.format("%Y-%m-%d %H:%M:%S%.6f").to_string();
        let conn = Connection::new(&self.db).map_err(|e| {
            GraphDBError::ConnectionError(format!("Failed to create connection: {e}"))
        })?;
        let set_query = format!(
            "MATCH (n:Node) WHERE n.id = '{}' SET n.properties = '{}', n.updated_at = timestamp('{}')",
            node_id.replace('\\', "\\\\").replace('\'', "\\'"),
            props_json.replace('\\', "\\\\").replace('\'', "\\'"),
            ts
        );
        conn.query(&set_query)
            .map_err(|e| GraphDBError::NodeError(format!("Failed to update node property: {e}")))?;
        Ok(())
    }

    async fn update_edge_property(
        &self,
        source_id: &str,
        target_id: &str,
        relationship_name: &str,
        key: &str,
        value: serde_json::Value,
    ) -> GraphDBResult<()> {
        let src_esc = source_id.replace('\\', "\\\\").replace('\'', "\\'");
        let tgt_esc = target_id.replace('\\', "\\\\").replace('\'', "\\'");
        let rel_esc = relationship_name.replace('\\', "\\\\").replace('\'', "\\'");

        let read_query = format!(
            "MATCH (a:Node)-[r:EDGE]->(b:Node) WHERE a.id = '{src_esc}' AND b.id = '{tgt_esc}' AND r.relationship_name = '{rel_esc}' RETURN r.properties AS properties"
        );
        let rows = self.execute_query(&read_query)?;
        let mut props: serde_json::Map<String, serde_json::Value> = if let Some(row) = rows.first()
            && let Some(props_str) = row.first().and_then(|v| v.as_str())
        {
            let clean = sanitize_json_control_chars(props_str);
            serde_json::from_str(&clean).unwrap_or_default()
        } else {
            return Err(GraphDBError::EdgeError(format!(
                "Edge not found: {source_id} -[{relationship_name}]-> {target_id}"
            )));
        };
        props.insert(key.to_string(), value);
        let mut props_val = serde_json::Value::Object(props);
        sanitize_json_value(&mut props_val);
        let props_json =
            serde_json::to_string(&props_val).map_err(GraphDBError::SerializationError)?;

        let now = Utc::now();
        let ts = now.format("%Y-%m-%d %H:%M:%S%.6f").to_string();
        let conn = Connection::new(&self.db).map_err(|e| {
            GraphDBError::ConnectionError(format!("Failed to create connection: {e}"))
        })?;
        let set_query = format!(
            "MATCH (a:Node)-[r:EDGE]->(b:Node) WHERE a.id = '{src_esc}' AND b.id = '{tgt_esc}' AND r.relationship_name = '{rel_esc}' SET r.properties = '{}', r.updated_at = timestamp('{ts}')",
            props_json.replace('\\', "\\\\").replace('\'', "\\'")
        );
        conn.query(&set_query)
            .map_err(|e| GraphDBError::EdgeError(format!("Failed to update edge property: {e}")))?;
        Ok(())
    }

    // -----------------------------------------------------------------
    // Batch feedback-weight methods — read the JSON properties column in
    // bulk and write individual in-place updates. We keep the writes in
    // individual statements (one per element) rather than a single
    // UNWIND because each node/edge carries its own merged JSON blob.
    // -----------------------------------------------------------------
    async fn get_node_feedback_weights(
        &self,
        node_ids: &[String],
    ) -> GraphDBResult<HashMap<String, f64>> {
        if node_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let id_list = node_ids
            .iter()
            .map(|id| format!("'{}'", id.replace('\\', "\\\\").replace('\'', "\\'")))
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            "MATCH (n:Node) WHERE n.id IN [{id_list}] RETURN n.id AS id, n.properties AS properties"
        );
        let rows = self.execute_query(&query)?;
        let mut out = HashMap::with_capacity(rows.len());
        for row in rows {
            if row.len() < 2 {
                continue;
            }
            let id = match row[0].as_str() {
                Some(s) => s.to_string(),
                None => continue,
            };
            if let Some(props_str) = row[1].as_str() {
                let clean = sanitize_json_control_chars(props_str);
                if let Ok(map) =
                    serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&clean)
                    && let Some(v) = map.get("feedback_weight").and_then(|v| v.as_f64())
                {
                    out.insert(id, v);
                }
            }
        }
        Ok(out)
    }

    async fn set_node_feedback_weights(
        &self,
        updates: &HashMap<String, f64>,
    ) -> GraphDBResult<HashMap<String, bool>> {
        let mut out = HashMap::with_capacity(updates.len());
        for (id, w) in updates {
            let ok = self
                .update_node_property(id, "feedback_weight", serde_json::json!(w))
                .await
                .is_ok();
            out.insert(id.clone(), ok);
        }
        Ok(out)
    }

    async fn get_edge_feedback_weights(
        &self,
        edge_keys: &[crate::traits::EdgeKey],
    ) -> GraphDBResult<HashMap<crate::traits::EdgeKey, f64>> {
        if edge_keys.is_empty() {
            return Ok(HashMap::new());
        }
        let mut out = HashMap::with_capacity(edge_keys.len());
        // Per-edge lookup: Ladybug's property filter on relationships is
        // simplest when driven by (source_id, target_id, rel_name) tuples.
        for key in edge_keys {
            let src_esc = key.0.replace('\\', "\\\\").replace('\'', "\\'");
            let tgt_esc = key.1.replace('\\', "\\\\").replace('\'', "\\'");
            let rel_esc = key.2.replace('\\', "\\\\").replace('\'', "\\'");
            let query = format!(
                "MATCH (a:Node)-[r:EDGE]->(b:Node) WHERE a.id = '{src_esc}' AND b.id = '{tgt_esc}' AND r.relationship_name = '{rel_esc}' RETURN r.properties AS properties"
            );
            let rows = self.execute_query(&query)?;
            if let Some(row) = rows.first()
                && let Some(props_str) = row.first().and_then(|v| v.as_str())
            {
                let clean = sanitize_json_control_chars(props_str);
                if let Ok(map) =
                    serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&clean)
                    && let Some(v) = map.get("feedback_weight").and_then(|v| v.as_f64())
                {
                    out.insert(key.clone(), v);
                }
            }
        }
        Ok(out)
    }

    async fn set_edge_feedback_weights(
        &self,
        updates: &HashMap<crate::traits::EdgeKey, f64>,
    ) -> GraphDBResult<HashMap<crate::traits::EdgeKey, bool>> {
        let mut out = HashMap::with_capacity(updates.len());
        for (key, w) in updates {
            let ok = self
                .update_edge_property(
                    &key.0,
                    &key.1,
                    &key.2,
                    "feedback_weight",
                    serde_json::json!(w),
                )
                .await
                .is_ok();
            out.insert(key.clone(), ok);
        }
        Ok(out)
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use crate::GraphDBTraitExt;
    use serde::Serialize;
    use serial_test::serial;
    use tempfile::TempDir;
    use tokio::task::JoinSet;

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

    /// Regression: lbug supports a single batched `UNWIND … MERGE` (inline
    /// struct list) and it is idempotent — the basis for the batched
    /// `add_nodes_raw`/`add_edges` upserts that replaced per-item exists-check
    /// + CREATE/SET.
    #[tokio::test]
    #[serial]
    async fn ladybug_supports_idempotent_unwind_merge_batch() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("merge.db");
        let config = SystemConfig::default().max_db_size(read_max_db_size());
        let db = Database::new(db_path.to_str().unwrap(), config).unwrap();
        let conn = Connection::new(&db).unwrap();
        conn.query(
            "CREATE NODE TABLE IF NOT EXISTS Node(id STRING PRIMARY KEY, name STRING, \
             type STRING, created_at TIMESTAMP, updated_at TIMESTAMP, properties STRING)",
        )
        .unwrap();

        let batch = r#"
            UNWIND [
              {id:'a', name:'Alice', type:'P', properties:'{}', created_at:'2026-01-01 00:00:00.000000', updated_at:'2026-01-01 00:00:00.000000'},
              {id:'b', name:'Bob',   type:'P', properties:'{}', created_at:'2026-01-01 00:00:00.000000', updated_at:'2026-01-01 00:00:00.000000'}
            ] AS node
            MERGE (n:Node {id: node.id})
            ON CREATE SET n.name = node.name, n.type = node.type,
                n.created_at = timestamp(node.created_at), n.updated_at = timestamp(node.updated_at),
                n.properties = node.properties
            ON MATCH SET n.name = node.name, n.type = node.type,
                n.updated_at = timestamp(node.updated_at), n.properties = node.properties
        "#;

        conn.query(batch)
            .expect("UNWIND+MERGE batch should be supported by lbug");
        // Run again — MERGE must be idempotent (no duplicate-PK error, still 2 nodes).
        conn.query(batch)
            .expect("re-running MERGE batch must not error");

        let count = conn
            .query("MATCH (n:Node) RETURN COUNT(n) AS c")
            .unwrap()
            .next()
            .and_then(|row| row.into_iter().next())
            .and_then(|v| match v {
                LbugValue::Int64(c) => Some(c),
                LbugValue::Int32(c) => Some(c as i64),
                LbugValue::UInt64(c) => Some(c as i64),
                _ => None,
            })
            .unwrap();
        assert_eq!(count, 2, "MERGE must upsert, not duplicate");
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
            eprintln!("Initialization error: {e:?}");
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
            .map(|i| TestNode::new(&format!("test-{i}"), &format!("Node {i}"), i * 10))
            .collect();
        let node_refs: Vec<&TestNode> = nodes.iter().collect();

        let result = adapter.add_nodes(&node_refs).await;
        assert!(result.is_ok());

        // Verify all nodes were added
        for i in 0..10 {
            assert!(
                adapter.has_node(&format!("test-{i}")).await.unwrap(),
                "Node test-{i} should exist"
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
            .map(|i| TestNode::new(&format!("test-{i}"), &format!("Node {i}"), i * 10))
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
            .map(|i| TestNode::new(&format!("test-{i}"), &format!("Node {i}"), i * 10))
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
    async fn test_add_node_upserts_duplicate_id() {
        let (adapter, _temp_dir) = setup_adapter().await;

        let original = TestNode::new("dup-node", "Original", 100);
        let replacement = TestNode::new("dup-node", "Updated", 200);

        adapter.add_node(&original).await.unwrap();
        adapter.add_node(&replacement).await.unwrap();

        let node = adapter.get_node("dup-node").await.unwrap().unwrap();
        assert_eq!(node.get("name").unwrap().as_str().unwrap(), "Updated");
        assert_eq!(node.get("value").unwrap().as_i64().unwrap(), 200);

        let metrics = adapter.get_graph_metrics(false).await.unwrap();
        assert_eq!(metrics.get("node_count").unwrap().as_i64().unwrap(), 1);
    }

    #[tokio::test]
    #[serial]
    async fn test_batch_vs_sequential_equivalence() {
        // Create two adapters
        let (adapter_batch, _temp_dir_batch) = setup_adapter().await;
        let (adapter_seq, _temp_dir_seq) = setup_adapter().await;

        let nodes: Vec<TestNode> = (0..20)
            .map(|i| TestNode::new(&format!("test-{i}"), &format!("Node {i}"), i * 10))
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
            let node_id = format!("test-{i}");
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
    async fn test_add_edge_upserts_duplicate_key() {
        let (adapter, _temp_dir) = setup_adapter().await;

        let node_a = TestNode::new("edge-a", "Node A", 1);
        let node_b = TestNode::new("edge-b", "Node B", 2);
        adapter.add_node(&node_a).await.unwrap();
        adapter.add_node(&node_b).await.unwrap();

        let mut original_props = HashMap::new();
        original_props.insert(Cow::Borrowed("since"), json!(2020));

        let mut replacement_props = HashMap::new();
        replacement_props.insert(Cow::Borrowed("since"), json!(2024));
        replacement_props.insert(Cow::Borrowed("strength"), json!("high"));

        adapter
            .add_edge("edge-a", "edge-b", "knows", Some(original_props))
            .await
            .unwrap();
        adapter
            .add_edge("edge-a", "edge-b", "knows", Some(replacement_props))
            .await
            .unwrap();

        let edges = adapter.get_edges("edge-a").await.unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].2, "knows");
        assert_eq!(edges[0].3.get("since").unwrap().as_i64().unwrap(), 2024);
        assert_eq!(
            edges[0].3.get("strength").unwrap().as_str().unwrap(),
            "high"
        );
    }

    #[tokio::test]
    #[serial]
    async fn test_concurrent_upsert_regression_single_process() {
        let (adapter, _temp_dir) = setup_adapter().await;
        let adapter = Arc::new(adapter);

        let mut node_tasks = JoinSet::new();
        for idx in 0..16 {
            let adapter = Arc::clone(&adapter);
            node_tasks.spawn(async move {
                let node = TestNode::new("race-node", &format!("Name-{idx}"), idx);
                adapter.add_node(&node).await
            });
        }

        while let Some(join_result) = node_tasks.join_next().await {
            let op_result = join_result.expect("node task should not panic");
            assert!(op_result.is_ok(), "node upsert should not fail");
        }

        let race_node = adapter
            .get_node("race-node")
            .await
            .unwrap()
            .expect("race-node should exist");
        assert!(
            race_node.get("name").and_then(|v| v.as_str()).is_some(),
            "race-node should preserve string fields"
        );

        let mut edge_tasks = JoinSet::new();
        for idx in 0..16 {
            let adapter = Arc::clone(&adapter);
            edge_tasks.spawn(async move {
                let mut props = HashMap::new();
                props.insert(Cow::Borrowed("iteration"), json!(idx));
                adapter
                    .add_edge("race-node", "race-node", "self", Some(props))
                    .await
            });
        }

        while let Some(join_result) = edge_tasks.join_next().await {
            let op_result = join_result.expect("edge task should not panic");
            assert!(op_result.is_ok(), "edge upsert should not fail");
        }

        assert!(
            adapter
                .has_edge("race-node", "race-node", "self")
                .await
                .unwrap(),
            "self edge should exist"
        );
        let metrics = adapter.get_graph_metrics(false).await.unwrap();
        assert_eq!(
            metrics.get("edge_count").unwrap().as_i64().unwrap(),
            1,
            "self edge should remain idempotent"
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
        let (nodes, edges) = adapter
            .get_nodeset_subgraph("TestNode", &[], "OR")
            .await
            .unwrap();

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
            .get_nodeset_subgraph("NonExistentType", &["Alice".to_string()], "OR")
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
            .get_nodeset_subgraph("test", &["Bob".to_string(), "Charlie".to_string()], "OR")
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

        // Get single node by type + name (no edges, so no neighbors)
        let (nodes, _edges) = adapter
            .get_nodeset_subgraph("TestNode", &["Alice".to_string()], "OR")
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

        // Get multiple nodes by type + names (no edges, so no neighbors)
        let (nodes, _edges) = adapter
            .get_nodeset_subgraph(
                "TestNode",
                &["Alice".to_string(), "Charlie".to_string()],
                "OR",
            )
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

        // Get subgraph for Alice and Bob — with OR mode their neighbors are also included.
        // Alice's neighbors (via A→B, A→C): Bob, Charlie
        // Bob's neighbors (via A→B, B→C): Alice, Charlie
        // Union: Alice + Bob + Charlie
        let (nodes, edges) = adapter
            .get_nodeset_subgraph("TestNode", &["Alice".to_string(), "Bob".to_string()], "OR")
            .await
            .unwrap();

        assert_eq!(nodes.len(), 3);
        // All three edges connect nodes in the subgraph: A→B, B→C, A→C
        assert_eq!(edges.len(), 3);
    }

    #[tokio::test]
    #[serial]
    async fn test_get_nodeset_subgraph_no_edges_between_nodes() {
        let (adapter, _temp_dir) = setup_adapter().await;

        let node_a = TestNode::new("node-a", "Alice", 100);
        let node_b = TestNode::new("node-b", "Bob", 200);
        adapter.add_node(&node_a).await.unwrap();
        adapter.add_node(&node_b).await.unwrap();

        // No edges created — neighbors will be empty, only primary nodes returned
        let (nodes, edges) = adapter
            .get_nodeset_subgraph("TestNode", &["Alice".to_string(), "Bob".to_string()], "OR")
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

        // Get all three nodes (OR mode — neighbors already all included in primary set)
        let (nodes, edges) = adapter
            .get_nodeset_subgraph(
                "TestNode",
                &[
                    "Alice".to_string(),
                    "Bob".to_string(),
                    "Charlie".to_string(),
                ],
                "OR",
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

        // Get only Person type with name Alice (no edges, so no neighbors)
        let (nodes, _edges) = adapter
            .get_nodeset_subgraph("Person", &["Alice".to_string()], "OR")
            .await
            .unwrap();

        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].1.get("name").unwrap().as_str().unwrap(), "Alice");
        assert_eq!(nodes[0].1.get("type").unwrap().as_str().unwrap(), "Person");
    }
}
