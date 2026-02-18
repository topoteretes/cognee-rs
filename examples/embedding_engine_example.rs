use cognee_embedding::{
    config::EmbeddingConfig, engine::EmbeddingEngine, onnx::OnnxEmbeddingEngine,
};
use std::error::Error;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    println!("Cognee Embedding Engine Example\n");

    // 1. Configure engine (using BGE-Small model from examples)
    let config = EmbeddingConfig::bge_small("examples/target/models");

    println!("Model: {}", config.model_name);
    println!("Tokenizer: {:?}", config.tokenizer_path);
    println!("Dimensions: {}", config.dimensions);
    println!("Max sequence length: {}\n", config.max_sequence_length);

    // 2. Create engine (will auto-download model and tokenizer if missing)
    println!("Initializing engine...");
    let engine = OnnxEmbeddingEngine::with_auto_download(config).await?;
    println!();

    // 3. Embed batch
    let texts = vec![
        "Cognee transforms documents into AI memory",
        "Knowledge graphs enable semantic search",
        "ONNX Runtime provides efficient inference",
    ];

    println!("Embedding {} texts...", texts.len());
    let start = std::time::Instant::now();
    let embeddings = engine.embed(&texts).await?;
    let duration = start.elapsed();

    println!("✓ Embeddings generated in {:?}\n", duration);

    // 4. Display results
    for (text, embedding) in texts.iter().zip(embeddings.iter()) {
        let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        println!("Text: {}", text);
        println!("  Dimension: {}", embedding.len());
        println!("  L2 Norm: {:.6}", norm);
        println!("  First 5 values: {:?}", &embedding[..5]);
        println!();
    }

    // 5. Compute semantic similarities
    println!("Semantic Similarities:");
    for i in 0..embeddings.len() {
        for j in (i + 1)..embeddings.len() {
            let similarity: f32 = embeddings[i]
                .iter()
                .zip(&embeddings[j])
                .map(|(a, b)| a * b)
                .sum();
            println!("  Text {} <-> Text {}: {:.4}", i + 1, j + 1, similarity);
        }
    }

    Ok(())
}
