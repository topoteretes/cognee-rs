//! Catalog of cognee operations that transform the knowledge graph.
//!
//! Faithful port of
//! [`cognee/modules/visualization/operations_catalog.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/visualization/operations_catalog.py).
//!
//! This is the single source of truth for the schema view's "transformations"
//! impact-layer: it declares, per operation, what schema types / node sets it
//! **produces**, **enriches**, **modifies**, or **removes**. It is corroborated
//! at render time by the live graph provenance (`source_pipeline` /
//! `source_task` stamped on nodes), but the modify/remove semantics — which
//! leave no per-op trace on edges or weights — live here.
//!
//! Curated from the Python implementation:
//!   * `cognee/api/v1/cognify/cognify.py`
//!   * `cognee/modules/memify/run_memify_tasks.py` + `cognee/memify_pipelines/*`
//!   * `cognee/api/v1/improve/improve.py`
//!   * `cognee/api/v1/forget/forget.py`
//!   * `cognee/tasks/codingagents/coding_rule_associations.py`
//!
//! Effects use raw type names. `"Entity"` is expanded by
//! [`super::schema_graph::build_operation_layer`] to the semantic entity types
//! actually present (Person/Broker/Tool/…); other names match a present schema
//! type exactly. `target_node_set` additionally loose-matches a present type of
//! the same name.

use serde_json::{Map, Value};

/// A single graph-mutating effect declared by an [`Operation`].
///
/// Mirrors one dict inside a Python catalog entry's `"effects"` list
/// (`operations_catalog.py:37-43` and friends).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Effect {
    /// One of `produces` / `enriches` / `modifies` / `removes`.
    pub effect: &'static str,
    /// Raw schema type name the effect applies to. `"Entity"` is expanded to
    /// the semantic entity types present in the graph.
    pub target_type: &'static str,
    /// Optional node-set name; loose-matches a present type of the same name.
    pub target_node_set: Option<&'static str>,
    /// Optional node property the effect writes (only for `modifies`).
    pub property: Option<&'static str>,
}

impl Effect {
    /// Convenience constructor for effects that carry neither a node set nor a
    /// property (the common case).
    const fn new(effect: &'static str, target_type: &'static str) -> Self {
        Self {
            effect,
            target_type,
            target_node_set: None,
            property: None,
        }
    }

    /// Convenience constructor for an effect scoped to a node set.
    const fn with_node_set(
        effect: &'static str,
        target_type: &'static str,
        target_node_set: &'static str,
    ) -> Self {
        Self {
            effect,
            target_type,
            target_node_set: Some(target_node_set),
            property: None,
        }
    }

    /// Convenience constructor for an effect that writes a node property.
    const fn with_property(
        effect: &'static str,
        target_type: &'static str,
        property: &'static str,
    ) -> Self {
        Self {
            effect,
            target_type,
            target_node_set: None,
            property: Some(property),
        }
    }

    /// JSON shape of this effect, matching the Python dict key-for-key.
    ///
    /// Optional keys are omitted when absent, exactly like the Python literals
    /// that simply do not declare them.
    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        map.insert("effect".to_string(), Value::String(self.effect.to_string()));
        map.insert(
            "target_type".to_string(),
            Value::String(self.target_type.to_string()),
        );
        if let Some(node_set) = self.target_node_set {
            map.insert(
                "target_node_set".to_string(),
                Value::String(node_set.to_string()),
            );
        }
        if let Some(property) = self.property {
            map.insert("property".to_string(), Value::String(property.to_string()));
        }
        Value::Object(map)
    }
}

/// A catalog operation: one cognee entry point that mutates the graph.
///
/// Mirrors one dict in Python's `_OPERATIONS` list
/// (`operations_catalog.py:29-195`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Operation {
    /// Stable identifier; the emitted operation node id is `op:<name>`.
    pub name: &'static str,
    /// Human-readable label rendered on the operation node.
    pub label: &'static str,
    /// One of `pipeline` / `self_improve` / `lifecycle`.
    pub kind: &'static str,
    /// One of `whole` / `subset`.
    pub scope: &'static str,
    /// Pipeline this operation runs under, when it has one. Used to flag impact
    /// links as `observed` when the live provenance agrees.
    pub pipeline_name: Option<&'static str>,
    /// One-line description shown in the inspector.
    pub summary: &'static str,
    /// The graph mutations this operation performs.
    pub effects: &'static [Effect],
}

impl Operation {
    /// JSON shape of this operation, matching the Python dict key-for-key.
    ///
    /// `pipeline_name` is omitted when `None`, exactly like the Python literals
    /// that do not declare it.
    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        map.insert("name".to_string(), Value::String(self.name.to_string()));
        map.insert("label".to_string(), Value::String(self.label.to_string()));
        map.insert("kind".to_string(), Value::String(self.kind.to_string()));
        map.insert("scope".to_string(), Value::String(self.scope.to_string()));
        if let Some(pipeline_name) = self.pipeline_name {
            map.insert(
                "pipeline_name".to_string(),
                Value::String(pipeline_name.to_string()),
            );
        }
        map.insert(
            "summary".to_string(),
            Value::String(self.summary.to_string()),
        );
        map.insert(
            "effects".to_string(),
            Value::Array(self.effects.iter().map(Effect::to_json).collect()),
        );
        Value::Object(map)
    }
}

/// The operation catalog, in Python's declaration order
/// (`operations_catalog.py:29-195`).
///
/// Declaration order is observable: [`super::schema_graph::build_operation_layer`]
/// emits operation nodes and impact links in this order.
pub static OPERATIONS: &[Operation] = &[
    Operation {
        name: "cognify",
        label: "cognify",
        kind: "pipeline",
        scope: "subset",
        pipeline_name: Some("cognify_pipeline"),
        summary: "Extracts a knowledge graph from raw documents.",
        effects: &[
            Effect::new("produces", "TextDocument"),
            Effect::new("produces", "DocumentChunk"),
            Effect::new("produces", "Entity"),
            Effect::new("produces", "EntityType"),
            Effect::new("produces", "TextSummary"),
        ],
    },
    Operation {
        name: "persist_sessions",
        label: "persist sessions",
        kind: "pipeline",
        scope: "subset",
        pipeline_name: Some("memify_pipeline"),
        summary: "Cognifies cached user Q&A sessions into the graph.",
        effects: &[
            Effect::with_node_set("produces", "Session", "user_sessions_from_cache"),
            Effect::with_node_set("produces", "Entity", "user_sessions_from_cache"),
        ],
    },
    Operation {
        name: "persist_agent_trace_feedbacks",
        label: "persist agent traces",
        kind: "pipeline",
        scope: "subset",
        pipeline_name: Some("memify_pipeline"),
        summary: "Cognifies agent trace feedback into the graph.",
        effects: &[Effect::with_node_set(
            "produces",
            "Entity",
            "agent_trace_feedbacks",
        )],
    },
    Operation {
        name: "apply_feedback_weights",
        label: "feedback weighting",
        kind: "self_improve",
        scope: "subset",
        pipeline_name: None,
        summary: "Re-weights used nodes/edges from session feedback (feedback_weight).",
        effects: &[
            Effect::with_property("modifies", "Entity", "feedback_weight"),
            Effect::with_property("modifies", "EntityType", "feedback_weight"),
        ],
    },
    Operation {
        name: "apply_frequency_weights",
        label: "frequency weighting",
        kind: "self_improve",
        scope: "subset",
        pipeline_name: None,
        summary: "Increments usage counts on used nodes/edges (frequency_weight).",
        effects: &[Effect::with_property(
            "modifies",
            "Entity",
            "frequency_weight",
        )],
    },
    Operation {
        name: "consolidate_entity_descriptions",
        label: "consolidate descriptions",
        kind: "pipeline",
        scope: "whole",
        pipeline_name: Some("memify_pipeline"),
        summary: "Rewrites Entity descriptions from their neighborhood.",
        effects: &[Effect::with_property("modifies", "Entity", "description")],
    },
    Operation {
        name: "global_context_index",
        label: "global context index",
        kind: "pipeline",
        scope: "whole",
        pipeline_name: Some("memify_pipeline"),
        summary: "Builds hierarchical context summaries for retrieval.",
        effects: &[
            Effect::new("produces", "GlobalContextSummary"),
            Effect::new("enriches", "TextSummary"),
        ],
    },
    Operation {
        name: "coding_rule_associations",
        label: "coding rules",
        kind: "pipeline",
        scope: "subset",
        pipeline_name: None,
        summary: "Extracts Rule nodes and links them to chunks.",
        effects: &[Effect::new("produces", "Rule")],
    },
    Operation {
        name: "improve",
        label: "improve (self-improve)",
        kind: "self_improve",
        scope: "subset",
        pipeline_name: None,
        summary: "Self-improvement loop: feedback weighting + persisting sessions/traces.",
        effects: &[
            Effect::with_property("modifies", "Entity", "feedback_weight"),
            Effect::with_node_set("produces", "Session", "user_sessions_from_cache"),
        ],
    },
    Operation {
        name: "improve_skill",
        label: "improve skill",
        kind: "self_improve",
        scope: "subset",
        pipeline_name: None,
        summary: "Proposes and applies improvements to a Skill's procedure.",
        effects: &[
            Effect::with_property("modifies", "Skill", "procedure"),
            Effect::new("produces", "SkillImprovementProposal"),
        ],
    },
    Operation {
        name: "temporal_graph",
        label: "temporal graph",
        kind: "pipeline",
        scope: "subset",
        pipeline_name: None,
        summary: "Extracts events and time-stamped relationships.",
        effects: &[Effect::new("produces", "Entity")],
    },
    Operation {
        name: "forget",
        label: "forget",
        kind: "lifecycle",
        scope: "subset",
        pipeline_name: None,
        summary: "Removes memory for a dataset/data item (graph nodes + edges).",
        effects: &[
            Effect::new("removes", "TextDocument"),
            Effect::new("removes", "DocumentChunk"),
            Effect::new("removes", "Entity"),
            Effect::new("removes", "EntityType"),
            Effect::new("removes", "TextSummary"),
        ],
    },
];

/// Return the operation catalog as JSON, mirroring Python's
/// `get_operations_catalog()` (`operations_catalog.py:198-200`).
///
/// Python hands back a `deepcopy` so callers may mutate it freely; the Rust
/// port simply builds fresh [`Value`]s on every call, which gives the same
/// isolation guarantee. Internal callers such as
/// [`super::schema_graph::build_operation_layer`] read [`OPERATIONS`] directly
/// and skip the JSON round-trip.
pub fn get_operations_catalog() -> Vec<Value> {
    OPERATIONS.iter().map(Operation::to_json).collect()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    #[test]
    fn catalog_preserves_python_declaration_order() {
        let names: Vec<&str> = OPERATIONS.iter().map(|op| op.name).collect();
        assert_eq!(
            names,
            vec![
                "cognify",
                "persist_sessions",
                "persist_agent_trace_feedbacks",
                "apply_feedback_weights",
                "apply_frequency_weights",
                "consolidate_entity_descriptions",
                "global_context_index",
                "coding_rule_associations",
                "improve",
                "improve_skill",
                "temporal_graph",
                "forget",
            ]
        );
    }

    #[test]
    fn catalog_uses_only_the_documented_enum_values() {
        for op in OPERATIONS {
            assert!(
                ["pipeline", "self_improve", "lifecycle"].contains(&op.kind),
                "unexpected kind on {}: {}",
                op.name,
                op.kind
            );
            assert!(
                ["whole", "subset"].contains(&op.scope),
                "unexpected scope on {}: {}",
                op.name,
                op.scope
            );
            assert!(!op.effects.is_empty(), "{} declares no effects", op.name);
            for effect in op.effects {
                assert!(
                    ["produces", "enriches", "modifies", "removes"].contains(&effect.effect),
                    "unexpected effect on {}: {}",
                    op.name,
                    effect.effect
                );
                assert!(!effect.target_type.is_empty());
            }
        }
    }

    #[test]
    fn json_omits_absent_optional_keys() {
        let catalog = get_operations_catalog();
        assert_eq!(catalog.len(), 12);

        let cognify = &catalog[0];
        assert_eq!(cognify["name"], "cognify");
        assert_eq!(cognify["pipeline_name"], "cognify_pipeline");
        assert_eq!(cognify["effects"][0]["effect"], "produces");
        assert_eq!(cognify["effects"][0]["target_type"], "TextDocument");
        let first_effect = cognify["effects"][0]
            .as_object()
            .expect("effect is an object");
        assert!(!first_effect.contains_key("property"));
        assert!(!first_effect.contains_key("target_node_set"));

        // `apply_feedback_weights` has no pipeline; the key must be absent
        // rather than null, matching the Python dict literal.
        let feedback = catalog
            .iter()
            .find(|op| op["name"] == "apply_feedback_weights")
            .expect("apply_feedback_weights is in the catalog");
        let feedback_obj = feedback.as_object().expect("operation is an object");
        assert!(!feedback_obj.contains_key("pipeline_name"));
        assert_eq!(feedback["effects"][0]["property"], "feedback_weight");
    }

    #[test]
    fn node_set_effects_carry_their_node_set() {
        let sessions = OPERATIONS
            .iter()
            .find(|op| op.name == "persist_sessions")
            .expect("persist_sessions is in the catalog");
        assert_eq!(sessions.effects.len(), 2);
        assert_eq!(sessions.effects[0].target_type, "Session");
        assert_eq!(
            sessions.effects[0].target_node_set,
            Some("user_sessions_from_cache")
        );
        assert_eq!(sessions.effects[1].target_type, "Entity");
        assert_eq!(
            sessions.effects[1].target_node_set,
            Some("user_sessions_from_cache")
        );
    }
}
