//! Ladybug graph database adapter.
//!
//! Implementation of GraphDBTrait using Ladybug (lbug) embedded graph database.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use lbug::{Connection, Database, SystemConfig, Value as LbugValue};
use serde::Serialize;
use serde_json::json;
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

        // Convert QueryResult iterator to Vec<Vec<serde_json::Value>>
        let rows: Vec<Vec<serde_json::Value>> = result
            .map(|row| row.into_iter().map(Self::lbug_value_to_json).collect())
            .collect();

        Ok(rows)
    }

    /// Convert lbug::Value to serde_json::Value
