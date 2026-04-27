pub mod default_impl;
pub mod scoped_watcher;
pub mod trait_def;
pub mod types;

pub use default_impl::DefaultPipelineRunRegistry;
pub use scoped_watcher::ScopedRunWatcher;
pub use trait_def::PipelineRunRegistry;
pub use types::{
    PipelineFuture, RegistryConfig, RegistryError, RunEvent, RunEventKind, RunHandle, RunOutcome,
    RunPhase, RunSpec,
};
