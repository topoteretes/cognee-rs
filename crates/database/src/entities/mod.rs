// SeaORM entity definitions.
// Phase 2 entities (existing tables):
pub mod artifact_reference;
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
pub mod task_run;
