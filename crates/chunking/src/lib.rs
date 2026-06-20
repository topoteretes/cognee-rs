//! Text chunking for Cognee, ported from the Python chunking hierarchy.
//!
//! Splits text through a word → sentence → paragraph hierarchy into
//! token-bounded chunks. Zero-copy where possible (chunks borrow `&str` slices
//! via byte-offset tracking).
//!
//! - [`text_chunker`] / [`cognify_pipeline`] — the chunking entry points
//! - [`token_counter`] — the [`token_counter::TokenCounter`] trait and its
//!   `WordCounter` / `HuggingFaceTokenCounter` / `TikTokenCounter` impls,
//!   selected by [`config`] (`TokenCounterKind::from_env`)

pub mod chunk_by_paragraph;
pub mod chunk_by_row;
pub mod chunk_by_sentence;
pub mod chunk_by_word;
pub mod cognify_pipeline;
pub mod config;
pub mod cut_type;
pub mod error;
pub mod text_chunker;
pub mod token_counter;

#[cfg(test)]
pub(crate) mod test_inputs;

pub use chunk_by_row::chunk_by_row;
pub use cognify_pipeline::ExtractTextChunksPipeline;
pub use config::TokenCounterKind;
pub use cut_type::CutType;
pub use error::ChunkingError;
pub use text_chunker::{NAMESPACE_OID, chunk_text};
#[cfg(feature = "hf-tokenizer")]
pub use token_counter::HuggingFaceTokenCounter;
#[cfg(feature = "tiktoken")]
pub use token_counter::TikTokenCounter;
pub use token_counter::{TokenCounter, WordCounter};
