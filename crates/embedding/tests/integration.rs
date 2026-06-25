#![cfg(feature = "onnx")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test code — panics are acceptable failures"
)]

use cognee_embedding::{
    config::OnnxEmbeddingConfig, engine::EmbeddingEngine, onnx::OnnxEmbeddingEngine,
};
use std::env;

// Helper to get model directory path
fn get_model_dir() -> String {
    if let Ok(model_dir) = env::var("COGNEE_TEST_MODEL_DIR") {
        return model_dir;
    }

    if let Ok(model_path) = env::var("COGNEE_E2E_EMBED_MODEL_PATH")
        && let Some(parent) = std::path::Path::new(&model_path).parent()
    {
        return parent.to_string_lossy().to_string();
    }

    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    format!("{manifest_dir}/../../target/models")
}

/// Load the ONNX engine, or return None to skip the test if the model file is absent.
fn load_bge_small() -> Option<OnnxEmbeddingEngine> {
    let config = OnnxEmbeddingConfig::bge_small(get_model_dir());
    match OnnxEmbeddingEngine::new(config) {
        Ok(engine) => Some(engine),
        Err(e) => {
            eprintln!("⚠️  Skipping ONNX test: {e}");
            None
        }
    }
}

#[tokio::test]
async fn test_full_embedding_pipeline() {
    let Some(engine) = load_bge_small() else {
        return;
    };

    let texts = vec!["Hello world", "Rust is awesome"];
    let embeddings = engine.embed(&texts).await.unwrap();

    assert_eq!(embeddings.len(), 2);
    assert_eq!(embeddings[0].len(), 384);
    assert_eq!(embeddings[1].len(), 384);

    for emb in &embeddings {
        let norm: f32 = emb.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 0.01,
            "Embedding not normalized: {norm}"
        );
    }
}

#[tokio::test]
async fn test_semantic_similarity() {
    let Some(engine) = load_bge_small() else {
        return;
    };

    let texts = vec![
        "machine learning",
        "artificial intelligence",
        "cooking recipes",
    ];

    let embeddings = engine.embed(&texts).await.unwrap();

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

    assert!(
        sim_ml_ai > sim_ml_cooking,
        "Expected ML-AI similarity ({sim_ml_ai}) > ML-Cooking similarity ({sim_ml_cooking})"
    );
}

#[tokio::test]
async fn test_batch_processing() {
    let Some(engine) = load_bge_small() else {
        return;
    };

    let texts: Vec<_> = (0..10)
        .map(|i| format!("This is test sentence number {i}"))
        .collect();

    let embeddings = engine
        .embed(&texts.iter().map(|s| s.as_str()).collect::<Vec<_>>())
        .await
        .unwrap();

    assert_eq!(embeddings.len(), 10);
    for emb in embeddings {
        assert_eq!(emb.len(), 384);
        let norm: f32 = emb.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 0.01);
    }
}

#[tokio::test]
async fn test_empty_batch() {
    let Some(engine) = load_bge_small() else {
        return;
    };

    let texts: Vec<&str> = vec![];
    let embeddings = engine.embed(&texts).await.unwrap();

    assert_eq!(embeddings.len(), 0);
}

#[tokio::test]
async fn test_long_text_truncation() {
    let Some(engine) = load_bge_small() else {
        return;
    };

    let long_text = "word ".repeat(1000);
    let texts = vec![long_text.as_str()];

    let embeddings = engine.embed(&texts).await.unwrap();

    assert_eq!(embeddings.len(), 1);
    assert_eq!(embeddings[0].len(), 384);
}
