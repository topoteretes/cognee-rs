use std::sync::{Arc, RwLock};
use std::thread::{self, JoinHandle};

pub fn execute_task<TInput, TOutput, F>(
    task_name: &str,
    batch_size: Option<usize>,
    input: Arc<RwLock<Vec<Arc<TInput>>>>,
    output: Arc<RwLock<Vec<Arc<TOutput>>>>,
    process_fn: F,
) -> JoinHandle<()>
where
    TInput: Clone + Send + Sync + 'static,
    TOutput: Clone + Send + Sync + 'static,
    F: Fn(Vec<Arc<TInput>>) -> Vec<Arc<TOutput>> + Send + 'static,
{
    let task_name = task_name.to_string();

    thread::spawn(move || {
        let total_chunks = {
            let chunks_guard = input.read().unwrap();
            chunks_guard.len()
        };

        let batch_size = batch_size.unwrap_or(total_chunks);

        println!(
            "{} starting - moving {} chunks to result...",
            task_name, total_chunks
        );

        let mut total_processed = 0;

        for batch_start in (0..total_chunks).step_by(batch_size) {
            let batch_end = (batch_start + batch_size).min(total_chunks);

            let mut batch_results = Vec::with_capacity(batch_end - batch_start);
            {
                {
                    let chunks_guard = input.read().unwrap();
                    for i in batch_start..batch_end {
                        let chunk = Arc::clone(&chunks_guard[i]);
                        batch_results.push(chunk);
                    }
                }

                println!("Batch processing starts");
                let processed_batch = process_fn(batch_results);
                println!("Batch processing ends");

                {
                    let mut result_guard = output.write().unwrap();
                    result_guard.extend(processed_batch);
                }
            }

            total_processed += batch_end - batch_start;
            println!(
                "{}: processed {}/{} chunks (batch size: {})",
                task_name,
                total_processed,
                total_chunks,
                batch_end - batch_start
            );
        }

        println!("{} completed - moved chunks to result", task_name);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cognee_payload_with_parallel_tasks() {
        use crate::data::payload_types::cognee_payload::CogneePayload;
        use std::time::Duration;

        let transform_fn1 = |batch: Vec<Arc<String>>| -> Vec<Arc<String>> {
            let sleep_ms = 1000 + (rand::random::<u64>() % 1001);
            thread::sleep(Duration::from_millis(sleep_ms));
            batch
                .into_iter()
                .map(|arc_item| Arc::new(format!("task1_processed_{}", &*arc_item)))
                .collect()
        };

        let transform_fn2 = |batch: Vec<Arc<String>>| -> Vec<Arc<String>> {
            let sleep_ms = 1000 + (rand::random::<u64>() % 1001);
            thread::sleep(Duration::from_millis(sleep_ms));
            batch
                .into_iter()
                .map(|arc_item| Arc::new(format!("task2_processed_{}", &*arc_item)))
                .collect()
        };

        let initial_chunks: Vec<Arc<String>> = (0..1000)
            .map(|i| Arc::new(format!("chunk_{}", i)))
            .collect();

        let payload = CogneePayload::<String, String, String>::new(initial_chunks);

        let mut task_handles = Vec::new();

        let handle1 = execute_task(
            "Task1_ToResult1",
            Some(100),
            payload.chunks_arc(),
            payload.result1_arc(),
            transform_fn1,
        );
        task_handles.push(handle1);

        let handle2 = execute_task(
            "Task2_ToResult2",
            None,
            payload.chunks_arc(),
            payload.result2_arc(),
            transform_fn2,
        );
        task_handles.push(handle2);

        println!("Waiting for {} tasks to complete...", task_handles.len());
        for (i, handle) in task_handles.into_iter().enumerate() {
            handle.join().unwrap();
            println!("Task {} completed!", i + 1);
        }
        println!("All tasks completed!");

        let result1_arc = payload.result1_arc();
        let result2_arc = payload.result2_arc();
        let results1 = result1_arc.read().unwrap();
        let results2 = result2_arc.read().unwrap();

        assert_eq!(results1.len(), 1000);
        assert_eq!(results2.len(), 1000);
        assert_eq!(results1[0].as_str(), "task1_processed_chunk_0");
        assert_eq!(results2[0].as_str(), "task2_processed_chunk_0");

        println!(
            "Final results - Result1: {}, Result2: {}",
            results1.len(),
            results2.len()
        );
    }

    #[test]
    fn test_complex_pipeline_with_chained_tasks() {
        use crate::data::payload_types::cognee_payload::CogneePayload;
        use std::time::Duration;

        #[derive(Debug, Clone)]
        struct ProcessedChunk {
            id: usize,
            content: String,
            word_count: usize,
            processed_at: String,
        }

        #[derive(Debug, Clone)]
        struct AnalyzedResult {
            chunk_id: usize,
            analysis: String,
            score: f64,
            metadata: Vec<String>,
        }

        let stage1_transform = |batch: Vec<Arc<String>>| -> Vec<Arc<ProcessedChunk>> {
            let sleep_ms = 500 + (rand::random::<u64>() % 501);
            thread::sleep(Duration::from_millis(sleep_ms));

            batch
                .into_iter()
                .enumerate()
                .map(|(idx, arc_item)| {
                    let content = &*arc_item;
                    Arc::new(ProcessedChunk {
                        id: idx,
                        content: format!("processed_{}", content),
                        word_count: content.split('_').count(),
                        processed_at: format!("timestamp_{}", idx),
                    })
                })
                .collect()
        };

        let stage2_transform = |batch: Vec<Arc<ProcessedChunk>>| -> Vec<Arc<AnalyzedResult>> {
            let sleep_ms = 300 + (rand::random::<u64>() % 301);
            thread::sleep(Duration::from_millis(sleep_ms));

            batch
                .into_iter()
                .map(|arc_item| {
                    let processed = &*arc_item;
                    Arc::new(AnalyzedResult {
                        chunk_id: processed.id,
                        analysis: format!("analyzed_{}", processed.content),
                        score: (processed.word_count as f64) * 1.5,
                        metadata: vec![
                            format!("meta_{}", processed.id),
                            processed.processed_at.clone(),
                            format!("words_{}", processed.word_count),
                        ],
                    })
                })
                .collect()
        };

        let initial_chunks: Vec<Arc<String>> =
            (0..100).map(|i| Arc::new(format!("chunk_{}", i))).collect();

        let payload = CogneePayload::<String, ProcessedChunk, AnalyzedResult>::new(initial_chunks);

        println!("Starting Stage 1: chunks -> result1");
        let handle1 = execute_task(
            "Stage1_ChunksToProcessed",
            None,
            payload.chunks_arc(),
            payload.result1_arc(),
            stage1_transform,
        );

        handle1.join().unwrap();
        println!("Stage 1 completed!");

        println!("Starting Stage 2: result1 -> result2");
        let handle2 = execute_task(
            "Stage2_ProcessedToAnalyzed",
            Some(15),
            payload.result1_arc(),
            payload.result2_arc(),
            stage2_transform,
        );

        handle2.join().unwrap();
        println!("Stage 2 completed!");

        let result1_arc = payload.result1_arc();
        let result2_arc = payload.result2_arc();
        let results1 = result1_arc.read().unwrap();
        let results2 = result2_arc.read().unwrap();

        assert_eq!(results1.len(), 100);
        assert_eq!(results2.len(), 100);

        assert_eq!(results1[0].id, 0);
        assert_eq!(results1[0].content, "processed_chunk_0");
        assert_eq!(results1[0].word_count, 2);

        assert_eq!(results2[0].chunk_id, 0);
        assert_eq!(results2[0].analysis, "analyzed_processed_chunk_0");
        assert_eq!(results2[0].score, 3.0);
        assert_eq!(results2[0].metadata.len(), 3);

        println!("Pipeline Results:");
        println!("- Stage 1 (ProcessedChunk): {} items", results1.len());
        println!("- Stage 2 (AnalyzedResult): {} items", results2.len());
        println!("- First analyzed result: {:?}", results2[0]);
    }
}
