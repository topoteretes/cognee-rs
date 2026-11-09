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
//!   * `cognee/api/v1/forget/forget.py`
//!   * `cognee/tasks/codingagents/coding_rule_associations.py`
//!
//! The self-improvement rows (feedback weighting, session/trace persistence,
//! distillation, preferences, truth subspace, conflict review, global context
//! index) are **generated**, one per improve stage, from
//! [`IMPROVE_STAGE_DESCRIPTORS`] — Python generates them from
//! `cognee.modules.improve.DEFAULT_STAGES` + `_IMPROVE_STAGE_DESCRIPTORS`. A
//! test in `crates/lib` (`tests/visualization_improve_stages.rs`) asserts the
//! descriptor names and scopes equal the Rust improve registry
//! (`crates/lib/src/api/improve/stages.rs`), so the view cannot drift from what
//! `improve()` actually runs.
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

/// Node set written by the improve loop's `persist_session_qa` stage
/// (Python `cognee.modules.improve.constants.USER_SESSIONS_NODE_SET`).
pub const USER_SESSIONS_NODE_SET: &str = "user_sessions_from_cache";
/// Node set written by `persist_agent_traces`
/// (`AGENT_TRACE_FEEDBACKS_NODE_SET`).
pub const AGENT_TRACE_FEEDBACKS_NODE_SET: &str = "agent_trace_feedbacks";
/// Node set written by `distill_sessions` (`SESSION_LEARNINGS_NODE_SET`).
pub const SESSION_LEARNINGS_NODE_SET: &str = "session_learnings";
/// Node set written by `update_user_preferences` (`USER_PREFERENCES_NODE_SET`).
pub const USER_PREFERENCES_NODE_SET: &str = "user_preferences";
/// Node set of ingested / improved skills (`SKILLS_NODE_SET`).
pub const SKILLS_NODE_SET: &str = "skills";

/// The hand-curated operation rows, in Python's declaration order
/// (`operations_catalog.py` `_OPERATIONS`).
///
/// The self-improvement rows are *not* listed here: they are generated from
/// [`IMPROVE_STAGE_DESCRIPTORS`], one per improve stage, and appended after
/// these by [`catalog_operations`] / [`get_operations_catalog`].
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
        name: "consolidate_entity_descriptions",
        label: "consolidate descriptions",
        kind: "pipeline",
        scope: "whole",
        pipeline_name: Some("memify_pipeline"),
        summary: "Rewrites Entity descriptions from their neighborhood, summarizes each \
                  EntityType from its members, and writes is_a edge text. Deprecated: use \
                  review_conflicts.",
        effects: &[
            Effect::with_property("modifies", "Entity", "description"),
            Effect::with_property("modifies", "EntityType", "description"),
            // The field is Entity.is_a and the artifact is the entity->type
            // edge, so this effect belongs to Entity, not to the type node.
            Effect::with_property("modifies", "Entity", "is_a.edge_text"),
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

/// View copy for one improve stage: how the stage is shown in the Schema
/// tab's transformations layer.
///
/// Port of one value of Python's `_IMPROVE_STAGE_DESCRIPTORS`. Python keys the
/// descriptors by stage name and walks `DEFAULT_STAGES` (the improve registry)
/// to order them and to read `needs_sessions`. This crate cannot depend on the
/// crate that owns the registry (`cognee`, `crates/lib/src/api/improve/stages.rs`
/// depends on *this* crate), so each descriptor carries its stage name and
/// `needs_sessions` itself and [`IMPROVE_STAGE_DESCRIPTORS`] is in registry
/// order. A test in `crates/lib` pins both against the registry so they cannot
/// drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImproveStageDescriptor {
    /// The improve stage's wire name (`StageKind::name()`).
    pub name: &'static str,
    /// Human-readable label rendered on the operation node.
    pub label: &'static str,
    /// One-line description shown in the inspector.
    pub summary: &'static str,
    /// Pipeline whose `source_pipeline` provenance corroborates the row.
    pub pipeline_name: Option<&'static str>,
    /// Mirror of the registry's `needs_sessions`: session-fed stages have
    /// scope `subset`, graph-wide ones `whole`.
    pub needs_sessions: bool,
    /// The graph mutations this stage performs.
    pub effects: &'static [Effect],
}

impl ImproveStageDescriptor {
    /// `"subset"` for session-fed stages, `"whole"` for graph-wide ones.
    pub const fn scope(&self) -> &'static str {
        if self.needs_sessions {
            "subset"
        } else {
            "whole"
        }
    }

    /// The node sets the stage produces: sorted, de-duplicated
    /// `target_node_set`s of its `produces` effects.
    pub fn node_sets(&self) -> Vec<&'static str> {
        let mut sets: Vec<&'static str> = self
            .effects
            .iter()
            .filter(|effect| effect.effect == "produces")
            .filter_map(|effect| effect.target_node_set)
            .filter(|set| !set.is_empty())
            .collect();
        sets.sort_unstable();
        sets.dedup();
        sets
    }

    /// The stage as a catalog [`Operation`] (`kind` is always `self_improve`).
    pub const fn to_operation(&self) -> Operation {
        Operation {
            name: self.name,
            label: self.label,
            kind: "self_improve",
            scope: self.scope(),
            pipeline_name: self.pipeline_name,
            summary: self.summary,
            effects: self.effects,
        }
    }

    /// JSON shape of the generated catalog row, key-for-key with Python's
    /// `iter_improve_operations()` (`pipeline_name` last, and only when set).
    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        map.insert("name".to_string(), Value::String(self.name.to_string()));
        map.insert("label".to_string(), Value::String(self.label.to_string()));
        map.insert(
            "kind".to_string(),
            Value::String("self_improve".to_string()),
        );
        map.insert("scope".to_string(), Value::String(self.scope().to_string()));
        map.insert(
            "summary".to_string(),
            Value::String(self.summary.to_string()),
        );
        map.insert(
            "effects".to_string(),
            Value::Array(self.effects.iter().map(Effect::to_json).collect()),
        );
        map.insert(
            "node_sets".to_string(),
            Value::Array(
                self.node_sets()
                    .into_iter()
                    .map(|set| Value::String(set.to_string()))
                    .collect(),
            ),
        );
        if let Some(pipeline_name) = self.pipeline_name {
            map.insert(
                "pipeline_name".to_string(),
                Value::String(pipeline_name.to_string()),
            );
        }
        Value::Object(map)
    }
}

/// One descriptor per improve stage, in the improve registry's execution
/// order (Python `_IMPROVE_STAGE_DESCRIPTORS` walked in `DEFAULT_STAGES`
/// order).
pub static IMPROVE_STAGE_DESCRIPTORS: &[ImproveStageDescriptor] = &[
    ImproveStageDescriptor {
        name: "feedback_weights",
        label: "feedback weighting",
        summary: "Re-weights used nodes/edges from session feedback (feedback_weight).",
        pipeline_name: Some("memify_pipeline"),
        needs_sessions: true,
        effects: &[
            Effect::with_property("modifies", "Entity", "feedback_weight"),
            Effect::with_property("modifies", "EntityType", "feedback_weight"),
        ],
    },
    ImproveStageDescriptor {
        name: "persist_session_qa",
        label: "persist sessions",
        summary: "Cognifies cached user Q&A sessions into the graph.",
        pipeline_name: Some("memify_pipeline"),
        needs_sessions: true,
        effects: &[
            Effect::with_node_set("produces", "Session", USER_SESSIONS_NODE_SET),
            Effect::with_node_set("produces", "Entity", USER_SESSIONS_NODE_SET),
        ],
    },
    ImproveStageDescriptor {
        name: "persist_agent_traces",
        label: "persist agent traces",
        summary: "Cognifies agent trace feedback into the graph.",
        pipeline_name: Some("memify_pipeline"),
        needs_sessions: true,
        effects: &[Effect::with_node_set(
            "produces",
            "Entity",
            AGENT_TRACE_FEEDBACKS_NODE_SET,
        )],
    },
    ImproveStageDescriptor {
        name: "extract_agent_context",
        label: "extract agent context",
        summary: "Turns pending tool-call traces into agent-profile lessons (session context).",
        pipeline_name: None,
        needs_sessions: true,
        effects: &[],
    },
    ImproveStageDescriptor {
        name: "distill_sessions",
        label: "distill sessions",
        summary: "Curates gated session guidance into entity-anchored lessons.",
        pipeline_name: Some("cognify_pipeline"),
        needs_sessions: true,
        effects: &[Effect::with_node_set(
            "produces",
            "Entity",
            SESSION_LEARNINGS_NODE_SET,
        )],
    },
    ImproveStageDescriptor {
        name: "update_user_preferences",
        label: "user preferences",
        summary: "Folds ratings and stated preferences into per-user prefers weights.",
        pipeline_name: None,
        needs_sessions: true,
        effects: &[Effect::with_node_set(
            "produces",
            "UserPreference",
            USER_PREFERENCES_NODE_SET,
        )],
    },
    ImproveStageDescriptor {
        name: "build_truth_subspace",
        label: "truth subspace",
        summary: "Scores chunks against accepted lessons (truth_alignment coordinates).",
        pipeline_name: None,
        needs_sessions: true,
        effects: &[Effect::with_property(
            "modifies",
            "DocumentChunk",
            "truth_alignment",
        )],
    },
    ImproveStageDescriptor {
        name: "review_conflicts",
        label: "review conflicts",
        summary: "Reviews dated source facts and stores conflicts.",
        pipeline_name: Some("memify_pipeline"),
        needs_sessions: false,
        effects: &[Effect::new("produces", "FactConflict")],
    },
    ImproveStageDescriptor {
        name: "global_context_index",
        label: "global context index",
        summary: "Builds per-document and dataset-root context summaries for retrieval.",
        pipeline_name: Some("memify_pipeline"),
        needs_sessions: false,
        effects: &[Effect::new("produces", "GlobalContextSummary")],
    },
];

/// Python `iter_improve_operations()`: one JSON catalog row per improve stage,
/// in registry order.
pub fn iter_improve_operations() -> Vec<Value> {
    IMPROVE_STAGE_DESCRIPTORS
        .iter()
        .map(ImproveStageDescriptor::to_json)
        .collect()
}

/// Every catalog operation in Python's `get_operations_catalog()` order: the
/// curated [`OPERATIONS`] first, then one row per improve stage.
pub fn catalog_operations() -> impl Iterator<Item = Operation> {
    OPERATIONS.iter().copied().chain(
        IMPROVE_STAGE_DESCRIPTORS
            .iter()
            .map(ImproveStageDescriptor::to_operation),
    )
}

/// Return the operation catalog as JSON, mirroring Python's
/// `get_operations_catalog()`: the curated rows, then the generated improve
/// rows (which additionally carry `node_sets`).
///
/// Python hands back a `deepcopy` so callers may mutate it freely; the Rust
/// port simply builds fresh [`Value`]s on every call, which gives the same
/// isolation guarantee.
pub fn get_operations_catalog() -> Vec<Value> {
    OPERATIONS
        .iter()
        .map(Operation::to_json)
        .chain(iter_improve_operations())
        .collect()
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
        let names: Vec<&str> = catalog_operations().map(|op| op.name).collect();
        assert_eq!(
            names,
            vec![
                "cognify",
                "consolidate_entity_descriptions",
                "coding_rule_associations",
                "improve_skill",
                "forget",
                "feedback_weights",
                "persist_session_qa",
                "persist_agent_traces",
                "extract_agent_context",
                "distill_sessions",
                "update_user_preferences",
                "build_truth_subspace",
                "review_conflicts",
                "global_context_index",
            ]
        );
    }

    #[test]
    fn retired_rows_are_gone() {
        // memify (triplets) and temporal_graph left the Python catalog, and the
        // old improve/persist rows became generated improve-stage rows.
        for retired in [
            "memify",
            "temporal_graph",
            "improve",
            "persist_sessions",
            "persist_agent_trace_feedbacks",
            "apply_feedback_weights",
            "apply_frequency_weights",
        ] {
            assert!(
                catalog_operations().all(|op| op.name != retired),
                "{retired} must no longer be in the catalog"
            );
        }
    }

    #[test]
    fn catalog_uses_only_the_documented_enum_values() {
        for op in catalog_operations() {
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
        // Only generated improve rows may declare no effects
        // (extract_agent_context upstream).
        for op in OPERATIONS {
            assert!(!op.effects.is_empty(), "{} declares no effects", op.name);
        }
    }

    #[test]
    fn json_omits_absent_optional_keys() {
        let catalog = get_operations_catalog();
        assert_eq!(catalog.len(), OPERATIONS.len() + 9);

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
        // Curated rows carry no node_sets key.
        assert!(!cognify.as_object().unwrap().contains_key("node_sets"));

        // `coding_rule_associations` has no pipeline; the key must be absent
        // rather than null, matching the Python dict literal.
        let rules = catalog
            .iter()
            .find(|op| op["name"] == "coding_rule_associations")
            .expect("coding_rule_associations is in the catalog");
        assert!(!rules.as_object().unwrap().contains_key("pipeline_name"));

        // Same for a generated improve row without a pipeline.
        let truth = catalog
            .iter()
            .find(|op| op["name"] == "build_truth_subspace")
            .expect("build_truth_subspace is in the catalog");
        assert!(!truth.as_object().unwrap().contains_key("pipeline_name"));
        assert_eq!(truth["effects"][0]["property"], "truth_alignment");
    }

    #[test]
    fn consolidate_descriptions_declares_its_three_modifies_effects() {
        let op = OPERATIONS
            .iter()
            .find(|op| op.name == "consolidate_entity_descriptions")
            .expect("consolidate_entity_descriptions is in the catalog");
        let effects: Vec<(&str, &str, Option<&str>)> = op
            .effects
            .iter()
            .map(|e| (e.effect, e.target_type, e.property))
            .collect();
        assert_eq!(
            effects,
            vec![
                ("modifies", "Entity", Some("description")),
                ("modifies", "EntityType", Some("description")),
                ("modifies", "Entity", Some("is_a.edge_text")),
            ]
        );
    }

    // ── Ports of test_operations_catalog.py ─────────────────────────────────

    #[test]
    fn improve_rows_follow_registry_order_and_shape() {
        let rows = iter_improve_operations();
        assert_eq!(rows.len(), IMPROVE_STAGE_DESCRIPTORS.len());
        for (row, descriptor) in rows.iter().zip(IMPROVE_STAGE_DESCRIPTORS) {
            assert_eq!(row["name"], descriptor.name);
            assert_eq!(row["kind"], "self_improve");
            assert!(row["scope"] == "whole" || row["scope"] == "subset");
            assert!(!row["label"].as_str().unwrap().is_empty());
            assert!(!row["summary"].as_str().unwrap().is_empty());
            assert!(row["effects"].is_array());
            assert!(row["node_sets"].is_array());
        }
    }

    #[test]
    fn scope_comes_from_needs_sessions() {
        for descriptor in IMPROVE_STAGE_DESCRIPTORS {
            let expected = if descriptor.needs_sessions {
                "subset"
            } else {
                "whole"
            };
            assert_eq!(descriptor.to_json()["scope"], expected);
        }
    }

    #[test]
    fn node_sets_are_derived_from_produces_effects() {
        let rows = iter_improve_operations();
        let row = |name: &str| {
            rows.iter()
                .find(|row| row["name"] == name)
                .cloned()
                .expect("row present")
        };
        assert_eq!(
            row("persist_session_qa")["node_sets"],
            serde_json::json!(["user_sessions_from_cache"])
        );
        assert_eq!(
            row("build_truth_subspace")["node_sets"],
            serde_json::json!([])
        );
    }

    #[test]
    fn catalog_appends_improve_rows_after_the_curated_ones() {
        let names: Vec<String> = get_operations_catalog()
            .iter()
            .map(|op| op["name"].as_str().unwrap().to_string())
            .collect();
        let curated = OPERATIONS.len();
        for (index, descriptor) in IMPROVE_STAGE_DESCRIPTORS.iter().enumerate() {
            assert_eq!(names[curated + index], descriptor.name);
        }
    }

    #[test]
    fn conflict_review_catalog_describes_its_stored_outputs() {
        let row = iter_improve_operations()
            .into_iter()
            .find(|row| row["name"] == "review_conflicts")
            .expect("review_conflicts row");
        assert_eq!(row["pipeline_name"], "memify_pipeline");
        assert_eq!(
            row["effects"],
            serde_json::json!([{"effect": "produces", "target_type": "FactConflict"}])
        );
    }

    #[test]
    fn node_set_effects_carry_their_node_set() {
        let sessions = IMPROVE_STAGE_DESCRIPTORS
            .iter()
            .find(|op| op.name == "persist_session_qa")
            .expect("persist_session_qa is in the catalog");
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
