#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "mock infrastructure — panics are acceptable"
)]

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};

use crate::engine::EmbeddingEngine;
use crate::error::{EmbeddingError, EmbeddingResult};

/// Controls how [`MockEmbeddingEngine`] produces vector components.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MockVectorMode {
    /// Every component is `0.0` (default; preserves legacy test behavior).
    #[default]
    Zero,
    /// Components are derived deterministically from `sha256(text)`, mirroring
    /// the Python benchmark mock so that the same text always yields the same
    /// vector and similar text yields stable neighbors.
    Deterministic,
}

/// A mock embedding engine.
///
/// Useful for testing pipeline stages that depend on an `EmbeddingEngine`
/// without requiring a real model or network connection. By default it returns
/// zero vectors ([`MockVectorMode::Zero`]); in [`MockVectorMode::Deterministic`]
/// it derives content-stable vectors from `sha256(text)`.
pub struct MockEmbeddingEngine {
    dimensions: usize,
    batch_size: usize,
    mode: MockVectorMode,
    /// When `Some(n)`, the `n+1`-th call to `embed` (and every subsequent call)
    /// returns an `EmbeddingError::InferenceError`. `set_failure_after(0)` causes
    /// the very first call to fail.
    failure_after: Arc<Mutex<Option<usize>>>,
    /// Number of `embed` invocations observed.
    call_count: Arc<Mutex<usize>>,
}

impl MockEmbeddingEngine {
    /// Create a mock engine with the given output dimensionality and a default batch size of 100.
    ///
    /// Defaults to [`MockVectorMode::Zero`].
    pub fn new(dimensions: usize) -> Self {
        Self {
            dimensions,
            batch_size: 100,
            mode: MockVectorMode::Zero,
            failure_after: Arc::new(Mutex::new(None)),
            call_count: Arc::new(Mutex::new(0)),
        }
    }

    /// Create a mock engine with explicit dimensionality and batch size.
    ///
    /// Defaults to [`MockVectorMode::Zero`].
    pub fn with_batch_size(dimensions: usize, batch_size: usize) -> Self {
        Self {
            dimensions,
            batch_size,
            mode: MockVectorMode::Zero,
            failure_after: Arc::new(Mutex::new(None)),
            call_count: Arc::new(Mutex::new(0)),
        }
    }

    /// Create a mock engine that produces deterministic, content-stable vectors
    /// derived from `sha256(text)` (see [`MockVectorMode::Deterministic`]).
    pub fn deterministic(dimensions: usize) -> Self {
        Self {
            dimensions,
            batch_size: 100,
            mode: MockVectorMode::Deterministic,
            failure_after: Arc::new(Mutex::new(None)),
            call_count: Arc::new(Mutex::new(0)),
        }
    }

    /// Override the vector-generation mode, consuming and returning `self`.
    pub fn with_mode(mut self, mode: MockVectorMode) -> Self {
        self.mode = mode;
        self
    }

    /// Compute a single deterministic vector from `text`, mirroring the Python
    /// benchmark mock: `sha256(text)` digest, little-endian `f32` windows of the
    /// digest, scaled by `1e38` and clamped to `[-1.0, 1.0]`.
    ///
    /// Non-finite values (NaN/inf) produced by `f32::from_le_bytes` are mapped to
    /// `0.0` so downstream cosine math stays well-defined.
    fn deterministic_vector(&self, text: &str) -> Vec<f32> {
        let digest = Sha256::digest(text.as_bytes());
        let len = digest.len();
        let mut vec = Vec::with_capacity(self.dimensions);
        for i in 0..self.dimensions {
            let offset = (i * 4) % len;
            // Right-pad with 0x00 to a full 4-byte window (matches Python's
            // `ljust(4, b"\x00")`); `offset` is always a multiple of 4 < len, so
            // this only matters defensively.
            let mut chunk = [0u8; 4];
            let end = (offset + 4).min(len);
            chunk[..end - offset].copy_from_slice(&digest[offset..end]);
            let raw = f32::from_le_bytes(chunk);
            let scaled = raw / 1e38_f32;
            let val = if scaled.is_finite() {
                scaled.clamp(-1.0, 1.0)
            } else {
                0.0
            };
            vec.push(val);
        }
        vec
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
                "MockEmbeddingEngine: injected failure after {n} successful call(s)"
            )));
        }
        match self.mode {
            MockVectorMode::Zero => Ok(vec![vec![0.0_f32; self.dimensions]; texts.len()]),
            MockVectorMode::Deterministic => {
                Ok(texts.iter().map(|t| self.deterministic_vector(t)).collect())
            }
        }
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

    #[tokio::test]
    async fn test_deterministic_same_input_identical() {
        let engine = MockEmbeddingEngine::deterministic(384);
        let a = engine
            .embed(&["hello world"])
            .await
            .expect("embed must not fail for mock engine");
        let b = engine
            .embed(&["hello world"])
            .await
            .expect("embed must not fail for mock engine");
        assert_eq!(a, b);
    }

    #[tokio::test]
    async fn test_deterministic_different_inputs_differ() {
        let engine = MockEmbeddingEngine::deterministic(384);
        let out = engine
            .embed(&["hello world", "goodbye world"])
            .await
            .expect("embed must not fail for mock engine");
        assert_ne!(out[0], out[1]);
    }

    #[tokio::test]
    async fn test_deterministic_finite_and_clamped() {
        let engine = MockEmbeddingEngine::deterministic(512);
        let out = engine
            .embed(&["some representative text"])
            .await
            .expect("embed must not fail for mock engine");
        assert_eq!(out[0].len(), 512);
        for &val in &out[0] {
            assert!(val.is_finite(), "component must be finite, got {val}");
            assert!(
                (-1.0..=1.0).contains(&val),
                "component {val} out of [-1, 1]"
            );
        }
    }

    #[tokio::test]
    async fn test_deterministic_dimensionality() {
        let engine = MockEmbeddingEngine::deterministic(128);
        let out = engine
            .embed(&["abc"])
            .await
            .expect("embed must not fail for mock engine");
        assert_eq!(out[0].len(), 128);
        assert_eq!(engine.dimension(), 128);
    }

    #[tokio::test]
    async fn test_with_mode_selects_deterministic() {
        let engine = MockEmbeddingEngine::new(64).with_mode(MockVectorMode::Deterministic);
        let out = engine
            .embed(&["x"])
            .await
            .expect("embed must not fail for mock engine");
        // Deterministic vectors are not all-zero for typical inputs.
        assert!(out[0].iter().any(|&v| v != 0.0));
    }

    #[tokio::test]
    async fn test_zero_mode_still_returns_zeros() {
        // Regression guard: default mode must remain zero vectors.
        let engine = MockEmbeddingEngine::new(128);
        let out = engine
            .embed(&["a", "b"])
            .await
            .expect("embed must not fail for mock engine");
        for vec in &out {
            for &val in vec {
                assert_eq!(val, 0.0_f32);
            }
        }
    }
}
