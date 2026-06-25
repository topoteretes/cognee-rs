//! Graph extraction from DataPoint models.
//!
//! Port of Python's `cognee/modules/graph/utils/get_graph_from_model.py`.
//!
//! Since Rust lacks runtime reflection, we use a trait (`GraphExtractable`)
//! that each DataPoint type implements to declare its structural relationships.
//! `get_graph_from_model` then collects and deduplicates edges from a set of
//! extractable items.

mod extractable;

pub use extractable::{GraphExtractable, Relationship, get_graph_from_model};
