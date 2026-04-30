// SeaORM entity definitions.
// Phase 2 entities (existing tables):
pub mod data;
pub mod dataset;
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

// ACL entities (principals, permissions, acls):
pub mod acl;
pub mod permission;
pub mod principal;

// User / Tenant / Role entities:
pub mod role;
pub mod tenant;
pub mod user;
pub mod user_api_key;
pub mod user_role;
pub mod user_tenant;

// Graph sync checkpoints (Stage 4 of improve()):
pub mod graph_sync_checkpoint;

// P5 default-permission tables and per-user named JSON blobs:
pub mod principal_configuration;
pub mod role_default_permission;
pub mod tenant_default_permission;
pub mod user_default_permission;

// P6 sync_operations (cloud sync state):
pub mod sync_operation;

// P7 notebooks:
pub mod notebook;

// LIB-03 session lifecycle (consumed by LIB-05's `SessionLifecycleDb`):
pub mod session_model_usage;
pub mod session_record;
