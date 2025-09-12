use cognee_rust::create_cognee_payload;
use cognee_rust::data::payload_base::PayloadBase;
use cognee_rust::data::payload_types::cognee_payload::PropertyStatus;
use cognee_rust::infrastructure::pipeline_executor::run_pipeline;
use cognee_rust::infrastructure::task::{TaskConfig, TaskConfigTrait};
use rand::Rng;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

// Create a 3-stage payload type
create_cognee_payload!(
    ThreeStagePayload,
    stage1_result: ProcessedText,
    stage2_result: AnalyzedText,
    stage3_result: FinalOutput
);

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct ProcessedText {
    id: usize,
    original_text: String,
    word_count: usize,
    processed_at: String,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct AnalyzedText {
    text_id: usize,
    sentiment: String,
    complexity_score: f64,
    keywords: Vec<String>,
    analysis_timestamp: String,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct FinalOutput {
    analysis_id: usize,
    summary: String,
    confidence: f64,
    recommendations: Vec<String>,
    final_timestamp: String,
}

// Stage 1: Text Processing
async fn stage1_text_processing(chunks: Vec<Arc<String>>) -> Vec<Arc<ProcessedText>> {
    let millis = rand::thread_rng().gen_range(1000..=2000);
    tokio::time::sleep(Duration::from_millis(millis)).await;

    let results: Vec<Arc<ProcessedText>> = chunks
        .into_iter()
        .enumerate()
        .map(|(idx, chunk)| {
            let text = chunk.as_str();
            Arc::new(ProcessedText {
                id: idx,
                original_text: text.to_string(),
                word_count: text.split_whitespace().count(),
                processed_at: format!("processed_at_{}", chrono::Utc::now().timestamp()),
            })
        })
        .collect();

    results
}

// Stage 2: Text Analysis
async fn stage2_text_analysis(processed_texts: Vec<Arc<ProcessedText>>) -> Vec<Arc<AnalyzedText>> {
    let millis = rand::thread_rng().gen_range(1500..=3000);
    tokio::time::sleep(Duration::from_millis(millis)).await;

    let results: Vec<Arc<AnalyzedText>> = processed_texts
        .into_iter()
        .map(|processed| {
            let sentiment = if processed.word_count > 10 {
                "positive"
            } else {
                "neutral"
            };
            let complexity_score = (processed.word_count as f64) * 0.1 + 0.5;
            let keywords = processed
                .original_text
                .split_whitespace()
                .filter(|word| word.len() > 3)
                .take(3)
                .map(|s| s.to_string())
                .collect();

            Arc::new(AnalyzedText {
                text_id: processed.id,
                sentiment: sentiment.to_string(),
                complexity_score,
                keywords,
                analysis_timestamp: format!("analyzed_at_{}", chrono::Utc::now().timestamp()),
            })
        })
        .collect();

    results
}

// Stage 3: Final Output Generation
async fn stage3_final_output(analyzed_texts: Vec<Arc<AnalyzedText>>) -> Vec<Arc<FinalOutput>> {
    let millis = rand::thread_rng().gen_range(800..=1500);
    tokio::time::sleep(Duration::from_millis(millis)).await;

    let results: Vec<Arc<FinalOutput>> = analyzed_texts
        .into_iter()
        .map(|analyzed| {
            let summary = format!(
                "Text {}: {} sentiment, complexity {:.2}, keywords: {}",
                analyzed.text_id,
                analyzed.sentiment,
                analyzed.complexity_score,
                analyzed.keywords.join(", ")
            );

            let confidence = if analyzed.complexity_score > 1.0 {
                0.9
            } else {
                0.7
            };
            let recommendations = vec![
                format!("Consider {} sentiment", analyzed.sentiment),
                format!(
                    "Complexity level: {}",
                    if analyzed.complexity_score > 1.0 {
                        "high"
                    } else {
                        "medium"
                    }
                ),
                "Review keywords for relevance".to_string(),
            ];

            Arc::new(FinalOutput {
                analysis_id: analyzed.text_id,
                summary,
                confidence,
                recommendations,
                final_timestamp: format!("final_at_{}", chrono::Utc::now().timestamp()),
            })
        })
        .collect();

    results
}

#[tokio::main]
async fn main() {
    dotenv::dotenv().ok();
    let _ = env_logger::builder().try_init();

    let stage1_task = TaskConfig::new(
        "Stage1_TextProcessing".to_string(),
        "chunks".to_string(),
        "stage1_result".to_string(),
        Some(3),
        stage1_text_processing,
    );

    let stage2_task = TaskConfig::new(
        "Stage2_TextAnalysis".to_string(),
        "stage1_result".to_string(),
        "stage2_result".to_string(),
        Some(2),
        stage2_text_analysis,
    );

    let stage3_task = TaskConfig::new(
        "Stage3_FinalOutput".to_string(),
        "stage2_result".to_string(),
        "stage3_result".to_string(),
        None,
        stage3_final_output,
    );

    let pipeline_tasks: Vec<Arc<dyn TaskConfigTrait>> = vec![
        Arc::new(stage1_task),
        Arc::new(stage2_task),
        Arc::new(stage3_task),
    ];

    run_pipeline(
        40,  // max_payloads
        20,  // max_concurrent_tasks
        100, // max_completed
        pipeline_tasks,
        std::marker::PhantomData::<ThreeStagePayload>,
    )
    .await;
}
