pub mod noop;
pub mod repository;
pub mod sea_orm_impl;

pub use noop::NoopPipelineRunRepository;
pub use repository::{
    GraphBuildEvidence, PipelineRunClaim, PipelineRunEntry, PipelineRunListQuery,
    PipelineRunRepository, PipelineRunVisibility, PipelineRunWithAttributionRow,
};
pub use sea_orm_impl::SeaOrmPipelineRunRepository;

/// The `pipeline_runs` name of `add`, which stages data and writes no graph.
pub const ADD_PIPELINE_NAME: &str = "add_pipeline";

/// Rows that record a change rather than a pipeline run: a delete's
/// `forget` and the `improve` row an improve run that wrote to the graph
/// leaves (`cognee::api::improve`). Python records both with a NULL
/// `pipeline_name`, so readers that look for pipeline runs skip them.
pub const CHANGE_RECORD_PIPELINE_NAMES: [&str; 2] = ["forget", "improve"];
