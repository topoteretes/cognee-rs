//! Unified public API for Cognee-Rust.
//!
//! This crate provides a single entry point by re-exporting the core operations:
//! - add (`AddPipeline`)
//! - cognify (`cognify()` free function and related types)
//! - search (`SearchBuilder`/`SearchOrchestrator` and related types)
//!
//! ## OpenTelemetry support
//!
//! Cognee emits structured spans for every pipeline stage, search retriever,
//! and HTTP route. To export them to an OTLP collector (Grafana Tempo,
//! Honeycomb, Dash0, in-cluster `otel-collector`, ...), enable the
//! `telemetry` cargo feature and set `OTEL_EXPORTER_OTLP_ENDPOINT`:
//!
//! ```ignore
//! use cognee_lib::telemetry::{init_telemetry, TelemetryGuard};
//! use cognee_lib::config::{ConfigManager, Settings};
//! use tracing_subscriber::Registry;
//!
//! let settings: Settings = ConfigManager::from_env().settings().clone();
//! let (_layer, _guard) = init_telemetry::<Registry>(&settings)
//!     .expect("telemetry init");
//! // ... compose `_layer` onto your subscriber; spans are flushed when
//! // `_guard` is dropped.
//! ```
//!
//! See [`docs/observability/opentelemetry.md`](https://github.com/topoteretes/cognee-rust/blob/main/docs/observability/opentelemetry.md)
//! for the full operator guide, env-var reference, and deployment recipes.

pub mod core {
    pub use cognee_core::*;
}

pub mod add {
    pub use cognee_ingestion::{
        AddParams, AddPipeline, ContentHasher, HashAlgorithm, ProcessedInput, build_add_pipeline,
        build_add_pipeline_with_acl, generate_data_id, generate_dataset_id, make_persist_data_task,
        make_persist_data_task_with_acl, make_process_input_task, persist_data,
        persist_data_with_acl, process_input,
    };
}

pub mod cognify {
    #[cfg(feature = "hf-tokenizer")]
    pub use cognee_chunking::HuggingFaceTokenCounter;
    #[cfg(feature = "tiktoken")]
    pub use cognee_chunking::TikTokenCounter;
    pub use cognee_chunking::{
        ChunkingError, CutType, TokenCounter, TokenCounterKind, WordCounter,
    };
    pub use cognee_cognify::*;
}

pub mod search {
    pub use cognee_search::*;
}

pub mod delete {
    pub use cognee_delete::*;
}

pub mod models {
    pub use cognee_models::*;
}

pub mod storage {
    pub use cognee_storage::*;
}

pub mod database {
    pub use cognee_database::*;
}

pub mod graph {
    #[cfg(feature = "ladybug")]
    pub use cognee_graph::LadybugAdapter;
    #[cfg(any(test, feature = "testing"))]
    pub use cognee_graph::MockGraphDB;
    pub use cognee_graph::{
        EdgeData, GraphDBError, GraphDBResult, GraphDBTrait, GraphDBTraitExt, GraphEdge, GraphNode,
        NodeData,
    };
}

pub mod vector {
    #[cfg(feature = "testing")]
    pub use cognee_vector::MockVectorDB;
    #[cfg(feature = "pgvector")]
    pub use cognee_vector::PgVectorAdapter;
    #[cfg(feature = "qdrant")]
    pub use cognee_vector::QdrantAdapter;
    pub use cognee_vector::{
        CollectionConfig, DistanceMetric, SearchResult, VectorDB, VectorDBError, VectorDBResult,
        VectorPoint,
    };
}

pub mod embedding {
    pub use cognee_embedding::utils::{
        handle_embedding_response, is_embeddable, sanitize_embedding_inputs,
    };
    pub use cognee_embedding::{
        EmbeddingConfig, EmbeddingEngine, EmbeddingError, EmbeddingProvider, EmbeddingResult,
        MockEmbeddingEngine, OllamaEmbeddingEngine, OpenAICompatibleEmbeddingEngine,
    };
    #[cfg(feature = "onnx")]
    pub use cognee_embedding::{
        ModelUrls, OnnxEmbeddingConfig, OnnxEmbeddingEngine, download_model, ensure_model_exists,
        ensure_tokenizer_exists,
    };
}

pub mod llm {
    pub use cognee_llm::*;
}

pub mod ontology {
    pub use cognee_ontology::*;
}

#[cfg(feature = "visualization")]
pub mod visualization {
    pub use cognee_visualization::*;
}

#[cfg(feature = "visualization")]
pub use cognee_visualization::{VisualizationError, visualize};

#[cfg(feature = "cloud")]
pub mod cloud {
    //! Re-export of [`cognee_cloud`] for callers that want the full
    //! surface (state helpers, credential types, management API
    //! client, etc.) under `cognee::cloud::…`.
    pub use cognee_cloud::*;
}

#[cfg(feature = "cloud")]
pub use cognee_cloud::{
    CloudClient, CloudCredentials, CloudError, CloudResult, ServeConfig, disconnect, serve,
    serve_cloud, serve_url,
};

#[cfg(feature = "server")]
pub mod http {
    //! HTTP server surface. Available only when the `server` feature is enabled.
    //! Consumers who only need the embedded server inside their own binary should
    //! prefer this re-export over taking a direct dependency on `cognee-http-server`,
    //! to keep their dependency closure aligned with the rest of the cognee crates.
    pub use cognee_http_server::*;
}

pub mod session;

pub mod api;
pub mod component_manager;
pub mod config;
pub mod context;
pub mod error;

pub mod telemetry;

pub use api::notebooks::{
    NotebookError, create_notebook, delete_notebook, list_notebooks, update_notebook,
};
pub use api::{DatasetDb, DatasetError, DatasetManager};
pub use component_manager::ComponentManager;
pub use config::{ConfigError, ConfigManager, Settings};
pub use context::PipelineContext;
pub use error::ComponentError;

pub mod prelude {
    pub use crate::add::AddPipeline;
    pub use crate::api::DatasetManager;
    pub use crate::api::{
        ApiError, DatasetRef, ForgetResult, ForgetTarget, ImproveParams, ImproveResult,
        PruneResult, PruneTarget, RecallItem, RecallResult, RecallSource, RememberItemInfo,
        RememberResult, RememberStatus, UpdateResult, forget, improve, prune_data, prune_system,
        recall, remember, update,
    };
    pub use crate::cognify::{CognifyConfig, cognify};
    pub use crate::cognify::{MemifyConfig, MemifyResult, run_memify};
    pub use crate::core::{
        AsyncRuntime, CancellationHandle, CancellationToken, CpuPool, CpuPoolExt, ExecutionError,
        NoopWatcher, Pipeline, PipelineWatcher, ProgressToken, RayonThreadPool, RetryDelay,
        RetryPolicy, Task, TaskContext, TaskContextBuilder, TaskInfo, Value, execute,
        execute_blocking, execute_in_background,
    };
    pub use crate::database::{
        AclDb, DatabaseConnection, DeleteDb, IngestDb, RoleDb, SearchHistoryDb, TenantDb, UserDb,
    };
    pub use crate::graph::GraphDBTrait;
    pub use crate::llm::Llm;
    pub use crate::models::{Data, DataInput, Dataset};
    pub use crate::search::{SearchBuilder, SearchOrchestrator, SearchRequest, SearchType};
    pub use crate::storage::{LocalStorage, StorageTrait};
    pub use crate::vector::VectorDB;
    #[cfg(feature = "cloud")]
    pub use crate::{
        CloudClient, CloudCredentials, CloudError, ServeConfig, disconnect, serve, serve_cloud,
        serve_url,
    };
    pub use uuid::Uuid;
}

pub use add::{
    AddParams, AddPipeline, ContentHasher, ProcessedInput, build_add_pipeline,
    make_persist_data_task, make_process_input_task, persist_data, process_input,
};
pub use cognee_cognify::*;
pub use cognee_delete::*;
pub use cognee_search::*;

pub use cognee_core;
pub use cognee_database;
pub use cognee_delete;
pub use cognee_embedding;
pub use cognee_graph;
pub use cognee_llm;
pub use cognee_models;
pub use cognee_ontology;
pub use cognee_session;
pub use cognee_storage;
pub use cognee_vector;
pub use uuid;
