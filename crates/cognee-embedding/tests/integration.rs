use cognee_embedding::{
    config::EmbeddingConfig, engine::EmbeddingEngine, onnx::OnnxEmbeddingEngine,
};
use std::env;

// Helper to get model directory path
fn get_model_dir() -> String {
    // Tests run from target/debug/deps/, so we need to go up to workspace root
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    format!("{}/../../examples/target/models", manifest_dir)
}

#[tokio::test]
#[ignore] // Requires model file - run with --ignored flag
async fn test_full_embedding_pipeline() {
    // 1. Load model (use BGE-Small from examples)
    let config = EmbeddingConfig::bge_small(get_model_dir());
    let engine = OnnxEmbeddingEngine::new(config)
        .expect("Failed to load model - run examples/embeddings.rs first");

    // 2. Embed batch
    let texts = vec!["Hello world".to_string(), "Rust is awesome".to_string()];
    let embeddings = engine.embed(&texts).await.unwrap();

    // 3. Verify dimensions
    assert_eq!(embeddings.len(), 2);
    assert_eq!(embeddings[0].len(), 384);
    assert_eq!(embeddings[1].len(), 384);

    // 4. Check normalization
    for emb in &embeddings {
        let norm: f32 = emb.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 0.01,
            "Embedding not normalized: {}",
            norm
        );
    }
}

#[tokio::test]
#[ignore]
async fn test_semantic_similarity() {
    let config = EmbeddingConfig::bge_small(get_model_dir());
    let engine = OnnxEmbeddingEngine::new(config).unwrap();

    let texts = vec![
        "machine learning".to_string(),
        "artificial intelligence".to_string(),
        "cooking recipes".to_string(),
    ];

    let embeddings = engine.embed(&texts).await.unwrap();

    // Cosine similarity (dot product of normalized vectors)
    let sim_ml_ai: f32 = embeddings[0]
        .iter()
        .zip(&embeddings[1])
        .map(|(a, b)| a * b)
        .sum();

    let sim_ml_cooking: f32 = embeddings[0]
        .iter()
        .zip(&embeddings[2])
        .map(|(a, b)| a * b)
        .sum();

    // ML and AI should be more similar than ML and cooking
    assert!(
        sim_ml_ai > sim_ml_cooking,
        "Expected ML-AI similarity ({}) > ML-Cooking similarity ({})",
        sim_ml_ai,
        sim_ml_cooking
    );
}

#[tokio::test]
#[ignore]
async fn test_batch_processing() {
    let config = EmbeddingConfig::bge_small(get_model_dir());
    let engine = OnnxEmbeddingEngine::new(config).unwrap();

    // Test with different batch sizes
    let texts: Vec<String> = (0..10)
        .map(|i| format!("This is test sentence number {}", i))
        .collect();

    let embeddings = engine.embed(&texts).await.unwrap();

    assert_eq!(embeddings.len(), 10);
    for emb in embeddings {
        assert_eq!(emb.len(), 384);
        let norm: f32 = emb.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 0.01);
    }
}

#[tokio::test]
#[ignore]
async fn test_empty_batch() {
    let config = EmbeddingConfig::bge_small(get_model_dir());
    let engine = OnnxEmbeddingEngine::new(config).unwrap();

    let texts: Vec<String> = vec![];
    let embeddings = engine.embed(&texts).await.unwrap();

    assert_eq!(embeddings.len(), 0);
}

#[tokio::test]
#[ignore]
async fn test_long_text_truncation() {
    let config = EmbeddingConfig::bge_small(get_model_dir());
    let engine = OnnxEmbeddingEngine::new(config).unwrap();

    // Create a very long text (will be truncated to max_sequence_length)
    let long_text = "word ".repeat(1000);
    let texts = vec![long_text];

    let embeddings = engine.embed(&texts).await.unwrap();

    assert_eq!(embeddings.len(), 1);
    assert_eq!(embeddings[0].len(), 384);
}
