use async_trait::async_trait;
use std::sync::{Arc, Mutex};

use crate::engine::EmbeddingEngine;
use crate::error::{EmbeddingError, EmbeddingResult};

/// A mock embedding engine that returns zero vectors.
///
/// Useful for testing pipeline stages that depend on an `EmbeddingEngine`
/// without requiring a real model or network connection.
pub struct MockEmbeddingEngine {
    dimensions: usize,
    batch_size: usize,
    /// When `Some(n)`, the `n+1`-th call to `embed` (and every subsequent call)
    /// returns an `EmbeddingError::InferenceError`. `set_failure_after(0)` causes
    /// the very first call to fail.
    failure_after: Arc<Mutex<Option<usize>>>,
    /// Number of `embed` invocations observed.
    call_count: Arc<Mutex<usize>>,
}

impl MockEmbeddingEngine {
    /// Create a mock engine with the given output dimensionality and a default batch size of 100.
    pub fn new(dimensions: usize) -> Self {
        Self {
            dimensions,
            batch_size: 100,
            failure_after: Arc::new(Mutex::new(None)),
            call_count: Arc::new(Mutex::new(0)),
        }
    }

    /// Create a mock engine with explicit dimensionality and batch size.
    pub fn with_batch_size(dimensions: usize, batch_size: usize) -> Self {
        Self {
            dimensions,
            batch_size,
            failure_after: Arc::new(Mutex::new(None)),
            call_count: Arc::new(Mutex::new(0)),
        }
    }

    /// Configure the engine to fail after `n` successful `embed` calls.
    ///
    /// With `n = 0`, the very first call fails. With `n = 3`, the first three
    /// calls succeed and the fourth and beyond fail.
    pub fn set_failure_after(&self, n: usize) {
        let mut slot = self.failure_after.lock().unwrap(); // lock poison is unrecoverable
        *slot = Some(n);
    }
}

#[async_trait]
impl EmbeddingEngine for MockEmbeddingEngine {
    async fn embed(&self, texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
        // Track call count and optionally inject failure.
        let count_after = {
            let mut count = self.call_count.lock().unwrap(); // lock poison is unrecoverable
            *count += 1;
            *count
        };
        let failure_threshold = {
            let slot = self.failure_after.lock().unwrap(); // lock poison is unrecoverable
            *slot
        };
        if let Some(n) = failure_threshold
            && count_after > n
        {
            return Err(EmbeddingError::InferenceError(format!(
                "MockEmbeddingEngine: injected failure after {} successful call(s)",
                n
            )));
        }
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
