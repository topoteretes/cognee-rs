use async_trait::async_trait;

use crate::engine::EmbeddingEngine;
use crate::error::EmbeddingResult;

/// A mock embedding engine that returns zero vectors.
///
/// Useful for testing pipeline stages that depend on an `EmbeddingEngine`
/// without requiring a real model or network connection.
pub struct MockEmbeddingEngine {
    dimensions: usize,
    batch_size: usize,
}

impl MockEmbeddingEngine {
    /// Create a mock engine with the given output dimensionality and a default batch size of 100.
    pub fn new(dimensions: usize) -> Self {
        Self {
            dimensions,
            batch_size: 100,
        }
    }

    /// Create a mock engine with explicit dimensionality and batch size.
    pub fn with_batch_size(dimensions: usize, batch_size: usize) -> Self {
        Self {
            dimensions,
            batch_size,
        }
    }
}

#[async_trait]
impl EmbeddingEngine for MockEmbeddingEngine {
    async fn embed(&self, texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
        Ok(vec![vec![0.0_f32; self.dimensions]; texts.len()])
    }

    fn dimension(&self) -> usize {
        self.dimensions
    }

    fn batch_size(&self) -> usize {
        self.batch_size
    }

    fn max_sequence_length(&self) -> usize {
        usize::MAX
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_embed_returns_correct_count() {
        let engine = MockEmbeddingEngine::new(384);
        let texts = vec!["hello", "world", "foo"];
        let embeddings = engine
            .embed(&texts)
            .await
            .expect("embed must not fail for mock engine");
        assert_eq!(embeddings.len(), texts.len());
    }

    #[tokio::test]
    async fn test_embed_returns_correct_dimensions() {
        let engine = MockEmbeddingEngine::new(512);
        let texts = vec!["some text"];
        let embeddings = engine
            .embed(&texts)
            .await
            .expect("embed must not fail for mock engine");
        assert_eq!(embeddings[0].len(), 512);
    }

    #[tokio::test]
    async fn test_embed_returns_zero_vectors() {
        let engine = MockEmbeddingEngine::new(128);
        let texts = vec!["a", "b"];
        let embeddings = engine
            .embed(&texts)
            .await
            .expect("embed must not fail for mock engine");
        for vec in &embeddings {
            for &val in vec {
                assert_eq!(val, 0.0_f32);
            }
        }
    }

    #[tokio::test]
    async fn test_embed_empty_input() {
        let engine = MockEmbeddingEngine::new(384);
        let texts: Vec<&str> = vec![];
        let embeddings = engine
            .embed(&texts)
            .await
            .expect("embed must not fail for mock engine");
        assert_eq!(embeddings.len(), 0);
    }

    #[test]
    fn test_dimension() {
        let engine = MockEmbeddingEngine::new(256);
        assert_eq!(engine.dimension(), 256);
    }

    #[test]
    fn test_batch_size_default() {
        let engine = MockEmbeddingEngine::new(384);
        assert_eq!(engine.batch_size(), 100);
    }

    #[test]
    fn test_with_batch_size() {
        let engine = MockEmbeddingEngine::with_batch_size(384, 50);
        assert_eq!(engine.batch_size(), 50);
        assert_eq!(engine.dimension(), 384);
    }

    #[test]
    fn test_max_sequence_length() {
        let engine = MockEmbeddingEngine::new(384);
        assert_eq!(engine.max_sequence_length(), usize::MAX);
    }
}
