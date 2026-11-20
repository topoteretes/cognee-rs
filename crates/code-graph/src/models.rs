//! The 15 code graph node types.
//!
//! Port of Python's `cognee/tasks/code_graph/models.py`. Python has one
//! `CodeRepository` and fourteen subclasses of `CodeGraphEntity` that add no
//! field of their own except `CodeSymbol.symbol_kind`. Here the fourteen share
//! one struct, [`CodeGraphEntity`], whose `base.data_type` is the Python class
//! name ([`CodeNodeType::class_name`]), so the stored `type` property is the
//! same in both SDKs.
//!
//! Serialised with serde, each node has the shape Python's
//! `get_graph_from_model` gives the graph node, with two exceptions spelled
//! out by [`CodeNode::graph_properties`]: `part_of` is not a property (Python
//! stores it as a `part_of` edge to the repository), and `belongs_to_set`
//! holds the node-set names, not the `NodeSet` points.

use std::collections::HashMap;

use cognee_models::{DataPoint, HasDataPoint};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

/// The fields every code node embeds: `["name"]`, as in Python.
pub const CODE_INDEX_FIELDS: &[&str] = &["name"];

/// `DataPoint` metadata of every code node: `{"index_fields": ["name"]}`.
pub(crate) fn code_metadata() -> HashMap<String, Value> {
    HashMap::from([(
        "index_fields".to_string(),
        Value::from(CODE_INDEX_FIELDS.to_vec()),
    )])
}

/// The type of a code node. One per Python model class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CodeNodeType {
    /// `CodeRepository`: the repository a snapshot was extracted from.
    CodeRepository,
    /// `CodeModule` (fact kind `module`).
    CodeModule,
    /// `CodeSymbol` (fact kind `symbol`).
    CodeSymbol,
    /// `ApiEndpoint` (fact kind `route`).
    ApiEndpoint,
    /// `StorageResource` (fact kind `storage`).
    StorageResource,
    /// `ExternalDependency` (fact kind `dependency`).
    ExternalDependency,
    /// `CodeService` (fact kind `service`).
    CodeService,
    /// `CodeTestReference` (fact kind `test_ref`).
    CodeTestReference,
    /// `CodeFileReference` (fact kind `file_ref`).
    CodeFileReference,
    /// `CodeInsight` (synthesized kind `insight`).
    CodeInsight,
    /// `CodeFlow` (synthesized kind `flow`).
    CodeFlow,
    /// `CodeIntent` (fact kind `intent`).
    CodeIntent,
    /// `CodeExtractionAccount` (fact kind `extraction`).
    CodeExtractionAccount,
    /// `CodeAssociation` (fact kind `association`).
    CodeAssociation,
    /// `CodeLintFinding` (fact kind `lint`).
    CodeLintFinding,
}

impl CodeNodeType {
    /// Every code node type, `CodeRepository` first.
    pub const ALL: [CodeNodeType; 15] = [
        Self::CodeRepository,
        Self::CodeModule,
        Self::CodeSymbol,
        Self::ApiEndpoint,
        Self::StorageResource,
        Self::ExternalDependency,
        Self::CodeService,
        Self::CodeTestReference,
        Self::CodeFileReference,
        Self::CodeInsight,
        Self::CodeFlow,
        Self::CodeIntent,
        Self::CodeExtractionAccount,
        Self::CodeAssociation,
        Self::CodeLintFinding,
    ];

    /// The Python class name, stored as the node's `type`.
    pub fn class_name(self) -> &'static str {
        match self {
            Self::CodeRepository => "CodeRepository",
            Self::CodeModule => "CodeModule",
            Self::CodeSymbol => "CodeSymbol",
            Self::ApiEndpoint => "ApiEndpoint",
            Self::StorageResource => "StorageResource",
            Self::ExternalDependency => "ExternalDependency",
            Self::CodeService => "CodeService",
            Self::CodeTestReference => "CodeTestReference",
            Self::CodeFileReference => "CodeFileReference",
            Self::CodeInsight => "CodeInsight",
            Self::CodeFlow => "CodeFlow",
            Self::CodeIntent => "CodeIntent",
            Self::CodeExtractionAccount => "CodeExtractionAccount",
            Self::CodeAssociation => "CodeAssociation",
            Self::CodeLintFinding => "CodeLintFinding",
        }
    }

    /// The type stored under `class_name`, if it is a code node type.
    pub fn from_class_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.class_name() == name)
    }

    /// The model a fact of `kind` maps to: Python's `KIND_TO_MODEL`. `None`
    /// for a kind cognee has no model for (dropped, never an error).
    pub fn for_fact_kind(kind: &str) -> Option<Self> {
        Some(match kind {
            "module" => Self::CodeModule,
            "symbol" => Self::CodeSymbol,
            "route" => Self::ApiEndpoint,
            "storage" => Self::StorageResource,
            "dependency" => Self::ExternalDependency,
            "service" => Self::CodeService,
            "test_ref" => Self::CodeTestReference,
            "file_ref" => Self::CodeFileReference,
            "insight" => Self::CodeInsight,
            "flow" => Self::CodeFlow,
            "intent" => Self::CodeIntent,
            "extraction" => Self::CodeExtractionAccount,
            "association" => Self::CodeAssociation,
            "lint" => Self::CodeLintFinding,
            _ => return None,
        })
    }
}

/// The fact kinds Python's `KIND_TO_MODEL` maps, in its order.
pub const FACT_KINDS: [&str; 14] = [
    "module",
    "symbol",
    "route",
    "storage",
    "dependency",
    "service",
    "test_ref",
    "file_ref",
    "insight",
    "flow",
    "intent",
    "extraction",
    "association",
    "lint",
];

/// The repository an enola snapshot was extracted from. Python's
/// `CodeRepository`.
///
/// `last_snapshot_id`, `last_mapping_version` and `last_receipt` are stamped
/// only after a load completes (O3), so a crashed run is never mistaken for
/// an up-to-date one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeRepository {
    /// Base data point fields; `data_type` is `CodeRepository`.
    #[serde(flatten)]
    pub base: DataPoint,
    /// The repository label facts carry (or the directory name).
    pub name: String,
    /// The repository's directory for the primary repository, else its name.
    pub path: String,
    /// Snapshot identity of the last fully loaded ingestion.
    #[serde(default)]
    pub last_snapshot_id: Option<String>,
    /// `CODE_GRAPH_MAPPING_VERSION` that load ran with.
    #[serde(default)]
    pub last_mapping_version: Option<i64>,
    /// Counts of what the last load changed.
    #[serde(default)]
    pub last_delta: Option<Value>,
    /// Projection of the snapshot's `receipt.json`.
    #[serde(default)]
    pub last_receipt: Option<Value>,
}

/// One enola fact mapped into the graph: Python's `CodeGraphEntity` and its
/// fourteen subclasses, told apart by `base.data_type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeGraphEntity {
    /// Base data point fields; `data_type` is the Python class name.
    #[serde(flatten)]
    pub base: DataPoint,
    /// The fact's name.
    pub name: String,
    /// The fact's kind (`symbol`, `route`, ...).
    pub kind: String,
    /// The file the fact was read from.
    #[serde(default)]
    pub file_path: Option<String>,
    /// First line of the fact.
    #[serde(default)]
    pub line: Option<i64>,
    /// Last line of the fact.
    #[serde(default)]
    pub end_line: Option<i64>,
    /// The repository label.
    #[serde(default)]
    pub repo: Option<String>,
    /// The writer's own fact identity (32 hex chars), first occurrence's.
    #[serde(default)]
    pub enola_id: Option<String>,
    /// Prose describing the fact.
    #[serde(default)]
    pub description: Option<String>,
    /// The comment the author wrote on the symbol, when enola read one.
    #[serde(default)]
    pub doc: Option<String>,
    /// The fact's props, verbatim.
    #[serde(default)]
    pub fact_properties: Map<String, Value>,
    /// Fingerprint of the derived fields, for delta writes.
    #[serde(default)]
    pub fact_hash: Option<String>,
    /// `CodeSymbol.symbol_kind`. Only a `CodeSymbol` has the field; on every
    /// other type it stays `None` and is not serialised.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol_kind: Option<String>,
    /// Id of the [`CodeRepository`] the fact belongs to. Python's `part_of`
    /// field, which the graph stores as a `part_of` edge, not a property.
    #[serde(skip)]
    pub part_of: Option<Uuid>,
}

impl CodeGraphEntity {
    /// The node's type, read from `base.data_type`.
    pub fn node_type(&self) -> Option<CodeNodeType> {
        CodeNodeType::from_class_name(&self.base.data_type)
    }
}

/// A node `map_facts_to_data_points` returns.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum CodeNode {
    /// The repository node.
    Repository(CodeRepository),
    /// A fact's node.
    Entity(CodeGraphEntity),
}

impl CodeNode {
    /// The node id.
    pub fn id(&self) -> Uuid {
        self.data_point().id
    }

    /// The node name.
    pub fn name(&self) -> &str {
        match self {
            Self::Repository(repository) => &repository.name,
            Self::Entity(entity) => &entity.name,
        }
    }

    /// The node type.
    pub fn node_type(&self) -> Option<CodeNodeType> {
        match self {
            Self::Repository(_) => Some(CodeNodeType::CodeRepository),
            Self::Entity(entity) => entity.node_type(),
        }
    }

    /// The repository node, if this is one.
    pub fn as_repository(&self) -> Option<&CodeRepository> {
        match self {
            Self::Repository(repository) => Some(repository),
            Self::Entity(_) => None,
        }
    }

    /// The fact node, if this is one.
    pub fn as_entity(&self) -> Option<&CodeGraphEntity> {
        match self {
            Self::Repository(_) => None,
            Self::Entity(entity) => Some(entity),
        }
    }

    /// The node's properties as Python's graph node carries them:
    /// `belongs_to_set` reduced to the node-set names, no `part_of`.
    pub fn graph_properties(&self) -> Map<String, Value> {
        let mut properties = match serde_json::to_value(self) {
            Ok(Value::Object(map)) => map,
            _ => Map::new(),
        };
        if let Some(Value::Array(sets)) = properties.get("belongs_to_set") {
            let names: Vec<Value> = sets
                .iter()
                .filter_map(|set| match set {
                    Value::String(name) => Some(Value::from(name.clone())),
                    Value::Object(set) => set.get("name").cloned(),
                    _ => None,
                })
                .collect();
            properties.insert("belongs_to_set".into(), Value::Array(names));
        }
        properties
    }
}

impl HasDataPoint for CodeNode {
    fn data_point(&self) -> &DataPoint {
        match self {
            Self::Repository(repository) => &repository.base,
            Self::Entity(entity) => &entity.base,
        }
    }

    fn data_point_mut(&mut self) -> &mut DataPoint {
        match self {
            Self::Repository(repository) => &mut repository.base,
            Self::Entity(entity) => &mut entity.base,
        }
    }
}

impl HasDataPoint for CodeRepository {
    fn data_point(&self) -> &DataPoint {
        &self.base
    }

    fn data_point_mut(&mut self) -> &mut DataPoint {
        &mut self.base
    }
}

impl HasDataPoint for CodeGraphEntity {
    fn data_point(&self) -> &DataPoint {
        &self.base
    }

    fn data_point_mut(&mut self) -> &mut DataPoint {
        &mut self.base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_fact_kind_maps_to_a_distinct_entity_type() {
        let types: Vec<CodeNodeType> = FACT_KINDS
            .iter()
            .filter_map(|kind| CodeNodeType::for_fact_kind(kind))
            .collect();
        assert_eq!(types.len(), 14);
        assert_eq!(&CodeNodeType::ALL[1..], types.as_slice());
        assert_eq!(CodeNodeType::for_fact_kind("topic"), None);
        for node_type in CodeNodeType::ALL {
            assert_eq!(
                CodeNodeType::from_class_name(node_type.class_name()),
                Some(node_type)
            );
        }
    }
}
