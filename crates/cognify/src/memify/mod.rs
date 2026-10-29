//! Improve-stage building blocks.
//!
//! Despite the name (kept from the original memify pipeline, SDK-1039 D3),
//! this module no longer holds a public pipeline: the memify entry points,
//! triplet embedding and `TRIPLET_COMPLETION` are removed, as in Python
//! (SDK-1115). What remains is the set of stages `improve()` runs, matching
//! Python's `DEFAULT_STAGES` (`cognee/modules/improve/registry.py`):
//!
//! * [`feedback_weights`] -- apply session feedback to graph weights;
//! * [`persist_sessions`] -- persist session Q&A into the knowledge graph;
//! * [`distill_sessions`] -- distill sessions into curated lesson documents;
//! * [`build_truth_subspace`] -- build the truth-subspace centroids.
//!
//! Graph-to-session sync is gone, as in Python (`3401da38a`).
//!
//! Prefer calling them through `cognee::api::improve`.

pub mod build_truth_subspace;
pub mod distill_sessions;
pub mod feedback_weights;
pub mod persist_sessions;

pub use build_truth_subspace::{
    NODE_EMBED_BATCH_SIZE, REASON_BACKEND_UNSUPPORTED, TruthBuildStatus, TruthSubspaceResult,
    build_truth_subspace,
};
pub use distill_sessions::{
    CuratorBatchOutput, DistillError, DistillSessionsResult, DistillationResult,
    DistillationStatus, ProposedLesson, RejectionReason, WrittenLesson, distill_session,
    distill_sessions_in_knowledge_graph, render_lesson_document,
};
pub use feedback_weights::{
    FEEDBACK_WEIGHTS_APPLIED_KEY, FeedbackApplyResult, FeedbackError,
    apply_feedback_weights_pipeline, normalize_feedback_score, stream_update_weight,
};
pub use persist_sessions::{
    PersistSessionsError, PersistSessionsResult, USER_SESSIONS_NODE_SET,
    persist_sessions_in_knowledge_graph,
};
