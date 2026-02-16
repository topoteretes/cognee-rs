//! Fact extraction module.
//!
//! This module provides functionality to extract structured facts (knowledge graphs)
//! from text using LLMs. It mirrors the Python cognee extraction architecture:
//! - Node, Edge, and KnowledgeGraph models (from shared/data_models.py)
//! - FactExtractor that uses the Llm trait to extract facts from text

pub mod extractor;
pub mod models;

pub use extractor::FactExtractor;
pub use models::{Edge, KnowledgeGraph, Node};
