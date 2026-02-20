//! Text summarization module.
//!
//! Port of Python's cognee/tasks/summarization/ and
//! cognee/infrastructure/llm/extraction/extract_summary.py
//!
//! Provides LLM-based hierarchical text summarization for document chunks.

pub mod extractor;
pub mod models;

pub use extractor::SummaryExtractor;
pub use models::{SummarizedContent, TextSummary};
