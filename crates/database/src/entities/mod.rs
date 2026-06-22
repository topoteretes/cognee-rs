// SeaORM entity definitions.
// Phase 2 entities (existing tables):
pub mod data;
pub mod dataset;
pub mod dataset_configuration;
pub mod dataset_data;
pub mod query;
pub mod result_log;

// Phase 3 entities (new tables from Python models):
pub mod edge;
pub mod graph_metrics;
pub mod node;
pub mod pipeline_run;
pub mod pipeline_run_payload_field;
pub mod task_run;

// Graph sync checkpoints (Stage 4 of improve()):
pub mod graph_sync_checkpoint;

// P6 sync_operations (cloud sync state):
pub mod sync_operation;

// P7 notebooks:
pub mod notebook;

// LIB-03 session lifecycle (consumed by LIB-05's `SessionLifecycleDb`):
pub mod session_model_usage;
pub mod session_record;

// Auth-related entities (acl, permission, principal, principal_configuration,
// role, role_default_permission, tenant, tenant_default_permission, user,
// user_api_key, user_default_permission, user_role, user_tenant) moved to
// the closed `cognee-access-control` crate as part of T2-move
// (oss-split-plan §4 S2).
