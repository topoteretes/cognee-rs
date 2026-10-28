//! Knowledge-graph extraction pipeline (classify → chunk → extract → summarize → index) and the memify enrichment pipeline.

/// Configuration module for the cognify pipeline.
pub mod config;
/// Dataset resolver module.
pub mod dataset_resolver;
/// Runtime custom graph schemas and DataPoints.
pub mod dynamic_graph;
/// Whole-graph repair for `EdgeType` vector rows a killed run never wrote.
pub mod edge_reindex;
/// Error types module.
pub mod error;
/// Fact extraction module.
pub mod fact_extraction;
/// Failure-handling vocabulary (the two axes, the failure report).
pub mod failure;
/// The global context index (bucket and root summaries above a dataset).
pub mod global_context_index;
/// LLM-free graph extraction backends.
pub mod graph_backend;
/// Graph extraction module.
pub mod graph_extraction;
/// Graph integration module.
pub mod graph_integration;
/// The JSON graph-model spec DSL, compiled into a graph schema.
pub mod graph_spec;
/// Memify pipeline module.
pub mod memify;
/// Pipeline orchestration module.
pub mod pipeline;
/// Qualification module.
pub mod qualification;
/// Run-orchestration policy: what a finished run sweeps, marks and records.
pub mod rollback;
/// Summarization module.
pub mod summarization;
/// Pipeline tasks module.
pub mod tasks;
/// Temporal extraction module.
pub mod temporal_extraction;
/// Deterministic date hints for the default graph prompt.
pub mod temporal_hints;

pub use temporal_extraction::{TemporalEntityEnricher, TemporalEventExtractor};
/// Triplet creation module.
pub mod triplet_creation;

pub use config::{
    ChunkAttachment, ChunkStrategy, CognifyConfig, ConfigError, CustomChunker, ExtractionStrategy,
    GraphBackendHandle, parse_graph_schema,
};
pub use dataset_resolver::{DatasetRef, DatasetResolver, cognify_dataset_refs, cognify_datasets};
pub use dynamic_graph::{
    DynamicDataPoint, DynamicGraph, DynamicGraphError, DynamicGraphSchema, DynamicModelSchema,
    DynamicRelationship, FROM_IDENTITY_KEYWORD, StoredDynamicGraph,
};
pub use edge_reindex::{EdgeReindexOptions, EdgeReindexReport, reindex_edge_types};
pub use error::CognifyError;
pub use fact_extraction::{Edge, FactExtractor, GraphModel, KnowledgeGraph, Node};
pub use failure::{
    FailurePolicy, FailureReport, FailureStage, FailureStop, RollbackScope, StageFailure,
};
pub use global_context_index::{
    GlobalContextIndexMethod, IndexStore, LlmIndexOptions, build_global_context_index,
};
pub use graph_backend::{
    ChunkExtractionError, ChunkGraphExtractor, ChunkGraphResult, ChunkRef, ExtractionContext,
    GraphBackendError, MockChunkGraphExtractor,
};
pub use graph_extraction::{GraphExtractable, Relationship, get_graph_from_model};
pub use graph_integration::{
    DeduplicationResult, EdgeResolutionStats, Expansion, GraphEdgePair, GraphNodePair,
    TimestampNode, deduplicate_nodes_and_edges, expand_extracted_graphs,
    expand_with_nodes_and_edges, expand_with_nodes_and_edges_with_stats,
};
pub use graph_spec::{
    Cardinality, EntitySpec, EnumFieldSpec, FieldSpec, GraphSchemaOptions, GraphSchemaSpec,
    GraphSpecError, InverseSpec, MAX_ENTITIES, MAX_FIELDS_PER_ENTITY, PrimitiveFieldSpec,
    PrimitiveType, RelationFieldSpec, RelationSpec, graph_model_from_spec,
    graph_schema_from_spec_value, graph_spec_to_json_schema,
};
pub use memify::{
    CuratorBatchOutput, DistillError, DistillSessionsResult, DistillationResult,
    DistillationStatus, FeedbackApplyResult, FeedbackError, MemifyConfig, MemifyError,
    MemifyResult, MemifyTask, NODE_EMBED_BATCH_SIZE, PersistSessionsError, PersistSessionsResult,
    ProposedLesson, REASON_BACKEND_UNSUPPORTED, RejectionReason, SyncError, SyncResult,
    TruthBuildStatus, TruthSubspaceResult, WrittenLesson, apply_feedback_weights_pipeline,
    build_memify_index_only_pipeline, build_truth_subspace, distill_session,
    distill_sessions_in_knowledge_graph, memify as run_memify, persist_sessions_in_knowledge_graph,
    render_lesson_document, sync_graph_to_session,
};

/// Re-export of the truth-subspace default slot capacity so callers (e.g.
/// `crates/lib`'s `improve()` Stage 2d) can pass it without depending on
/// `cognee-truth-subspace` directly.
pub use cognee_truth_subspace::DEFAULT_K;
pub use pipeline::{CognifyResult, IndexedFieldsStats};
pub use qualification::{Qualification, check_pipeline_run_qualification};
pub use summarization::{SummarizeOutcome, SummarizedContent, SummaryExtractor, TextSummary};
pub use tasks::{
    AttributedDynamicGraph, AttributedEvent, CLAIM_STALE_AFTER, ClassifiedDocuments, CognifyInput,
    DEFAULT_LEDGER_USER_ID, DynamicExtractionData, ExtractedChunks, ExtractedGraphData,
    ExtractedTemporalEvents, LedgerIdentity, SummarizedChunks, SummarizedData, add_data_points,
    add_temporal_data_points, build_cognify_pipeline, build_temporal_cognify_pipeline,
    classify_documents, cognify, create_web_page_nodes, extract_chunks_from_documents,
    extract_dlt_fk_edges, extract_dynamic_graphs_from_data, extract_graph_from_data,
    extract_temporal_events, make_add_data_points_task, make_add_temporal_data_points_task,
    make_classify_documents_task, make_extract_chunks_task, make_extract_graph_and_summarize_task,
    make_extract_graph_and_summarize_task_with_rank, make_extract_graph_task,
    make_extract_temporal_events_task, make_summarize_text_task, summarize_text,
};
pub use triplet_creation::{
    create_triplets_from_graph, create_triplets_from_graph_with_timestamps,
};
