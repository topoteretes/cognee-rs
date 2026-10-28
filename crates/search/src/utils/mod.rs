pub mod access_tracking;
mod completion;
mod edge_type_id;
pub mod feedback_detection;
pub mod global_context;
mod resolve_edges_to_text;
mod session_messages;

pub use access_tracking::update_node_access_timestamps;
pub use completion::{
    DEFAULT_GRAPH_USER_PROMPT_TEMPLATE, DEFAULT_HYBRID_USER_PROMPT_TEMPLATE,
    DEFAULT_RAG_SYSTEM_PROMPT, DEFAULT_RAG_USER_PROMPT_TEMPLATE, EMPTY_GRAPH_MESSAGE,
    HYBRID_SMALL_WINDOW_SYSTEM_PROMPT, HYBRID_SMALL_WINDOW_USER_PROMPT_TEMPLATE,
    render_graph_user_prompt, render_user_prompt, resolve_system_prompt,
    skip_completion_on_empty_context,
};
pub(crate) use edge_type_id::edge_type_point_id;
pub use feedback_detection::detect_feedback;
pub use global_context::{
    DEFAULT_GLOBAL_CONTEXT_INDEX_TOP_K, build_global_context_prelude,
    format_global_context_prelude, load_root_text, search_top_global_context_summaries,
};
pub(crate) use resolve_edges_to_text::DEFAULT_STOP_WORDS;
pub use resolve_edges_to_text::resolve_edges_to_text as render_edges_context;
pub use session_messages::build_messages_with_history;
