mod content_hasher;
mod id_generation;
mod loader_registry;
pub mod loaders;
pub mod pipeline;
pub mod url_crawler;

pub use content_hasher::{ContentHasher, HashAlgorithm};
pub use id_generation::{generate_data_id, generate_dataset_id};
pub use loader_registry::get_loader_name;
pub use loaders::{DocumentLoader, LoaderError, LoaderOutput, LoaderRegistry};
pub use pipeline::{
    AddPipeline, ProcessedInput, build_add_pipeline, build_add_pipeline_with_acl,
    make_persist_data_task, make_persist_data_task_with_acl, make_process_input_task, persist_data,
    persist_data_with_acl, process_input,
};
