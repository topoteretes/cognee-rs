pub mod db_watcher;
pub mod default_impl;
pub mod ids;
pub mod run_info;
pub mod scoped_watcher;
pub mod trait_def;
pub mod types;

pub use db_watcher::DbPipelineWatcher;
pub use default_impl::DefaultPipelineRunRegistry;
pub use ids::{pipeline_id, pipeline_run_id};
pub use run_info::{
    recovery_lock_of, run_info_for_completed, run_info_for_errored, run_info_for_started,
};
pub use scoped_watcher::ScopedRunWatcher;
pub use trait_def::PipelineRunRegistry;
pub use types::{
    PipelineFuture, RegistryConfig, RegistryError, RunEvent, RunEventKind, RunHandle, RunOutcome,
    RunPhase, RunSpec,
};
