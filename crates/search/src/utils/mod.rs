mod completion;
mod resolve_edges_to_text;
mod session_messages;

pub use completion::{
    DEFAULT_RAG_SYSTEM_PROMPT, DEFAULT_RAG_USER_PROMPT_TEMPLATE, render_user_prompt,
    resolve_system_prompt,
};
pub use resolve_edges_to_text::resolve_edges_to_text as render_edges_context;
pub use session_messages::build_messages_with_history;
