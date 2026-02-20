mod dataset_scope;
mod prepare_search_result;
mod search_execution_builder;
mod search_orchestrator;
mod search_type_tools;

pub const CONTEXT_LABEL_COMBINED: &str = "combined";
pub const CONTEXT_LABEL_DEFAULT: &str = "default";

pub use dataset_scope::{merge_scoped_contexts, scope_context_by_datasets};
pub use prepare_search_result::prepare_search_result;
pub use search_execution_builder::SearchBuilder;
pub use search_orchestrator::SearchOrchestrator;
pub use search_type_tools::SearchTypeRegistry;
