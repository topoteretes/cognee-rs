use std::collections::HashMap;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::time::Instant;

#[cfg(feature = "onnx_dynamic_library")]
use std::env;

use edge::EdgeShard;
use ort::session::{Session, builder::GraphOptimizationLevel};
use ort::value::Tensor;
use segment::data_types::vectors::{VectorInternal, VectorStructInternal};
use segment::types::{
    Distance, ExtendedPointId, Payload, PayloadStorageType, SegmentConfig, VectorDataConfig,
    VectorStorageType, WithPayloadInterface, WithVector,
};
use serde_json::json;
use shard::operations::CollectionUpdateOperations::PointOperation;
use shard::operations::point_ops::PointInsertOperationsInternal::PointsList;
use shard::operations::point_ops::PointOperations::UpsertPoints;
use shard::operations::point_ops::PointStructPersisted;
use shard::query::query_enum::QueryEnum;

const DATA_DIR: &str = "./target/embedding-demo-data";
const DEFAULT_MODEL_DIR: &str = "./target/models";
const BGE_ONNX_FILENAME: &str = "BGE-Small-v1.5-model_quantized.onnx";
const BERT_ONNX_FILENAME: &str = "bert-tiny.onnx";
const BGE_VECTOR_NAME: &str = "bge-embedding";
const BERT_VECTOR_NAME: &str = "bert-embedding";

struct ModelConfig {
    name: &'static str,
    onnx_path: PathBuf,
    output_dim: usize,
    vector_name: &'static str,
    id_offset: u64,
}

impl ModelConfig {
    fn bge_small() -> Self {
        Self {
            name: "BGE-Small-v1.5",
            onnx_path: PathBuf::from(DEFAULT_MODEL_DIR).join(BGE_ONNX_FILENAME),
            output_dim: 384,
            vector_name: BGE_VECTOR_NAME,
            id_offset: 0,
        }
    }

    fn bert_tiny() -> Self {
        Self {
            name: "BERT-Tiny",
            onnx_path: PathBuf::from(DEFAULT_MODEL_DIR).join(BERT_ONNX_FILENAME),
            output_dim: 384,
            vector_name: BERT_VECTOR_NAME,
            id_offset: 1_000,
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    println!("Cognee-Rust: On-Device Text Embeddings + Qdrant Edge\n\n\n");

    let model_dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_MODEL_DIR.to_string());
    let model_dir = PathBuf::from(model_dir);

    // Initialize ONNX Runtime environment
    #[cfg(feature = "onnx_dynamic_library")]
    if let Ok(path) = env::var("ORT_DYLIB_PATH") {
        ort::init_from(path)?.commit();
    } else {
        ort::init().commit();
    }

    #[cfg(not(feature = "onnx_dynamic_library"))]
    ort::init().commit();

    println!("Initializing Qdrant Edge shard...");
    let shard = setup_qdrant_shard()?;
    println!("Qdrant shard ready\n");

    let texts = vec![
        "The quick brown fox jumps over the lazy dog",
        "Neural networks are powerful machine learning models",
        "On-device AI enables privacy-preserving applications",
        "Quantized models run efficiently on embedded devices",
    ];

    println!("Processing with BGE-Small-v1.5...");
    process_texts_with_model(&shard, &texts, ModelConfig::bge_small(), &model_dir)?;

    println!("\nProcessing with BERT-Tiny...");
    process_texts_with_model(&shard, &texts, ModelConfig::bert_tiny(), &model_dir)?;

    println!("\nQuerying embeddings from Qdrant...");
    query_embeddings(&shard, &model_dir)?;

    println!("\nDemo completed successfully!");

    Ok(())
}

fn setup_qdrant_shard() -> Result<EdgeShard, Box<dyn Error>> {
    if Path::new(DATA_DIR).exists() {
        std::fs::remove_dir_all(DATA_DIR)?;
    }
    std::fs::create_dir_all(DATA_DIR)?;

    let config = SegmentConfig {
        vector_data: {
            let mut m = HashMap::new();
            // BGE-Small: 384 dimensions
            m.insert(
                BGE_VECTOR_NAME.to_string(),
                VectorDataConfig {
                    size: 384,
                    distance: Distance::Cosine,
                    storage_type: VectorStorageType::ChunkedMmap,
                    index: Default::default(),
                    quantization_config: None,
                    multivector_config: None,
                    datatype: None,
                },
            );
            // BERT-Tiny: 384 dimensions
            m.insert(
                BERT_VECTOR_NAME.to_string(),
                VectorDataConfig {
                    size: 384,
                    distance: Distance::Cosine,
                    storage_type: VectorStorageType::ChunkedMmap,
                    index: Default::default(),
                    quantization_config: None,
                    multivector_config: None,
                    datatype: None,
                },
            );
            m
        },
        sparse_vector_data: HashMap::new(),
        payload_storage_type: PayloadStorageType::Mmap,
    };

    Ok(EdgeShard::load(Path::new(DATA_DIR), Some(config))?)
}

fn process_texts_with_model(
    shard: &EdgeShard,
    texts: &[&str],
    model: ModelConfig,
    model_dir: &Path,
) -> Result<(), Box<dyn Error>> {
    println!("  Model: {}", model.name);
    println!("  Dimensions: {}", model.output_dim);

    // Use build-time cached model
    let model_path = model_dir.join(model.onnx_path.file_name().ok_or("Invalid model path")?);
    if !model_path.exists() {
        return Err(format!(
            "Model not found at {:?}. Run `cargo build` to generate models in target/models.",
            model_path
        )
        .into());
    }
    println!("  Model path: {:?}", model_path);
    let model_size_bytes = std::fs::metadata(&model_path)?.len();
    println!(
        "  Model size: {:.2} MB",
        model_size_bytes as f64 / (1024.0 * 1024.0)
    );

    // Load ONNX session
    println!("  Loading ONNX session...");
    let mut session = Session::builder()?
        .with_optimization_level(GraphOptimizationLevel::Level3)?
        .commit_from_file(&model_path)?;
    println!("  ✓ Model loaded");
    println!("  Processing {} texts:", texts.len());

    let mut points = Vec::new();
    let mut total_duration = std::time::Duration::ZERO;

    for (idx, text) in texts.iter().enumerate() {
        let start = Instant::now();

        // Run ONNX embedding
        let embedding = create_embedding(text, &mut session, model.output_dim)?;
        let normalized = l2_normalize(&embedding);

        let duration = start.elapsed();
        total_duration += duration;

        println!(
            "    [{}] {} | {:?} | norm: {:.4}",
            idx + 1,
            text,
            duration,
            compute_norm(&normalized)
        );

        // Create point for Qdrant
        let mut vectors = HashMap::new();
        vectors.insert(
            model.vector_name.to_string(),
            VectorInternal::from(normalized.clone()),
        );
        let point = PointStructPersisted {
            id: ExtendedPointId::NumId((idx as u64) + 1 + model.id_offset),
            vector: VectorStructInternal::Named(vectors).into(),
            payload: Some(create_payload(text, model.name)),
        };

        points.push(point);
    }

    // Upsert to Qdrant
    println!("\n    Upserting {} embeddings to Qdrant...", points.len());
    shard.update(PointOperation(UpsertPoints(PointsList(points))))?;

    let avg_duration = total_duration.as_millis() as f64 / texts.len() as f64;
    println!(
        "    Completed | Avg latency: {:.2}ms | Total: {:?}",
        avg_duration, total_duration
    );

    Ok(())
}

fn query_embeddings(shard: &EdgeShard, model_dir: &Path) -> Result<(), Box<dyn Error>> {
    // Use build-time cached model
    let model = ModelConfig::bge_small();
    let model_path = model_dir.join(model.onnx_path.file_name().ok_or("Invalid model path")?);
    if !model_path.exists() {
        return Err(format!(
            "Model not found at {:?}. Run `cargo build` to generate models in target/models.",
            model_path
        )
        .into());
    }

    // Create query embedding (real model)
    let mut session = Session::builder()?
        .with_optimization_level(GraphOptimizationLevel::Level3)?
        .commit_from_file(&model_path)?;
    let query_text = "machine learning algorithms";
    let query_embedding = create_embedding(query_text, &mut session, 384)?;
    let normalized = l2_normalize(&query_embedding);

    println!("  Query: \"{}\"", query_text);
    println!("  Using: {}", BGE_VECTOR_NAME);

    let query_vec: VectorInternal = VectorInternal::Dense(normalized);
    let results = shard.query(shard::query::ShardQueryRequest {
        prefetches: vec![],
        query: Some(shard::query::ScoringQuery::Vector(QueryEnum::Nearest(
            segment::data_types::vectors::NamedQuery {
                query: query_vec,
                using: Some(BGE_VECTOR_NAME.to_string()),
            },
        ))),
        filter: None,
        score_threshold: None,
        limit: 10,
        offset: 0,
        params: None,
        with_vector: WithVector::Bool(false),
        with_payload: WithPayloadInterface::Bool(true),
    })?;

    println!("  Results:");
    for (idx, p) in results.iter().enumerate() {
        println!("    [{}] ID: {}, Score: {:.4}", idx + 1, p.id, p.score);
        if let Some(payload) = &p.payload
            && let Some(text_val) = payload.0.get("text")
        {
            println!("         Text: {}", text_val);
        }
    }

    Ok(())
}

fn create_embedding(
    text: &str,
    session: &mut Session,
    output_dim: usize,
) -> Result<Vec<f32>, Box<dyn Error>> {
    let tokens = simple_tokenize(text);
    let seq_len = tokens.len();

    let input_ids = Tensor::from_array((vec![1usize, seq_len], tokens))?;
    let attention_mask = Tensor::from_array((vec![1usize, seq_len], vec![1i64; seq_len]))?;
    let token_type_ids = Tensor::from_array((vec![1usize, seq_len], vec![0i64; seq_len]))?;

    let outputs = session.run(ort::inputs! {
        "input_ids" => input_ids,
        "attention_mask" => attention_mask,
        "token_type_ids" => token_type_ids,
    })?;

    let (shape, data) = outputs[0].try_extract_tensor::<f32>()?;

    if shape.len() == 3 {
        let seq = shape[1] as usize;
        let hidden = shape[2] as usize;
        let mut pooled = vec![0.0f32; output_dim];

        for s in 0..seq {
            for (h, pooled) in pooled.iter_mut().enumerate().take(output_dim.min(hidden)) {
                let idx = s * hidden + h;
                *pooled += data[idx];
            }
        }
        for val in &mut pooled {
            *val /= seq as f32;
        }
        Ok(pooled)
    } else if shape.len() == 2 {
        Ok(data.iter().take(output_dim).copied().collect())
    } else {
        Err("Unexpected output shape".into())
    }
}

fn simple_tokenize(text: &str) -> Vec<i64> {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let mut token_ids = vec![101]; // [CLS]

    for token in tokens {
        let hash = token
            .chars()
            .fold(0u64, |acc, c| acc.wrapping_mul(31).wrapping_add(c as u64));
        token_ids.push((hash % 28000 + 1000) as i64);
    }

    token_ids.push(102); // [SEP]

    while token_ids.len() < 128 {
        token_ids.push(0);
    }
    token_ids.truncate(128);

    token_ids
}

fn l2_normalize(vec: &[f32]) -> Vec<f32> {
    let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        vec.iter().map(|x| x / norm).collect()
    } else {
        vec.to_vec()
    }
}

fn compute_norm(vec: &[f32]) -> f32 {
    vec.iter().map(|x| x * x).sum::<f32>().sqrt()
}

fn create_payload(text: &str, model_name: &str) -> Payload {
    let mut payload = Payload::default();
    payload.0.insert("text".to_string(), json!(text));
    payload.0.insert("model".to_string(), json!(model_name));
    payload
        .0
        .insert("timestamp".to_string(), json!(chrono::Utc::now()));
    payload
}
