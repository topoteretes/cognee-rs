use crate::data::payload_types::cognee_payload::PropertyStatus;
use log::{debug, info};
use std::future::Future;
use std::sync::{Arc, RwLock};
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub enum LoopSignal {
    TaskCompleted,
    NewPayloadAdded,
    Shutdown,
}

#[allow(clippy::too_many_arguments)]
pub fn create_task<TInput, TOutput, F, Fut>(
    task_name: &str,
    batch_size: Option<usize>,
    input: Arc<RwLock<Vec<Arc<TInput>>>>,
    output: Option<Arc<RwLock<Vec<Arc<TOutput>>>>>,
    property_status: Arc<std::sync::Mutex<std::collections::HashMap<String, PropertyStatus>>>,
    output_property_name: &str,
    process_fn: F,
    signal_sender: Option<mpsc::UnboundedSender<LoopSignal>>,
) -> impl Future<Output = ()>
where
    TInput: Clone + Send + Sync + 'static,
    TOutput: Clone + Send + Sync + 'static,
    F: Fn(Vec<Arc<TInput>>) -> Fut + Send + 'static,
    Fut: Future<Output = Vec<Arc<TOutput>>> + Send + 'static,
{
    let task_name = task_name.to_string();
    let output_property_name = output_property_name.to_string();

    async move {
        // Set property status to Processing at the beginning
        {
            let mut status = property_status.lock().unwrap();
            status.insert(output_property_name.clone(), PropertyStatus::Processing);
        }

        let total_chunks = {
            let chunks_guard = input.read().unwrap();
            chunks_guard.len()
        };

        let batch_size = batch_size.unwrap_or(total_chunks);

        info!("{task_name} starting - moving {total_chunks} chunks to result...");

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

                debug!("Batch processing starts");
                let processed_batch = process_fn(batch_results).await;
                debug!("Batch processing ends");

                if let Some(output_arc) = &output {
                    info!("Writing {batch_end} batches...");
                    let mut result_guard = output_arc.write().unwrap();
                    result_guard.extend(processed_batch);
                    info!("Writing batches...");
                }
            }

            total_processed += batch_end - batch_start;
            info!(
                "{}: processed {}/{} chunks (batch size: {})",
                task_name,
                total_processed,
                total_chunks,
                batch_end - batch_start
            );
        }

        // Set property status to Done at the end
        {
            let mut status = property_status.lock().unwrap();
            status.insert(output_property_name.clone(), PropertyStatus::Done);
        }

        info!("{task_name} completed - moved chunks to result");
        if let Some(sender) = signal_sender {
            let _ = sender.send(LoopSignal::TaskCompleted);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::create_cognee_payload;
    use crate::data::payload_base::PayloadBase;
    use crate::data::payload_types::cognee_payload::PropertyStatus;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, RwLock};

    #[tokio::test]
    async fn test_cognee_payload_with_parallel_tasks() {
        dotenv::dotenv().ok(); // Load .env file
        let _ = env_logger::builder().is_test(true).try_init();
        use crate::data::payload_types::cognee_payload::CogneePayload;
        use std::time::Duration;

        let transform_fn1 = |batch: Vec<Arc<String>>| async move {
            let sleep_ms = 1000 + (rand::random::<u64>() % 1001);
            tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
            batch
                .into_iter()
                .map(|arc_item| Arc::new(format!("task1_processed_{}", &*arc_item)))
                .collect()
        };

        let transform_fn2 = |batch: Vec<Arc<String>>| async move {
            let sleep_ms = 1000 + (rand::random::<u64>() % 1001);
            tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
            batch
                .into_iter()
                .map(|arc_item| Arc::new(format!("task2_processed_{}", &*arc_item)))
                .collect()
        };

        let initial_chunks: Vec<Arc<String>> =
            (0..1000).map(|i| Arc::new(format!("chunk_{i}"))).collect();

        let payload = CogneePayload::<String, String, String>::new(initial_chunks);

        let mut task_handles = Vec::new();

        let task_future1 = create_task(
            "Task1_ToResult1",
            Some(100),
            *payload.get_arc("chunks").unwrap().downcast().unwrap(),
            Some(*payload.get_arc("result1").unwrap().downcast().unwrap()),
            *payload
                .get_arc("property_status")
                .unwrap()
                .downcast()
                .unwrap(),
            "result1",
            transform_fn1,
            None,
        );
        let handle1 = tokio::spawn(task_future1);
        task_handles.push(handle1);

        let task_future2 = create_task(
            "Task2_ToResult2",
            None,
            *payload.get_arc("chunks").unwrap().downcast().unwrap(),
            Some(*payload.get_arc("result2").unwrap().downcast().unwrap()),
            *payload
                .get_arc("property_status")
                .unwrap()
                .downcast()
                .unwrap(),
            "result2",
            transform_fn2,
            None,
        );
        let handle2 = tokio::spawn(task_future2);
        task_handles.push(handle2);

        info!("Waiting for {} tasks to complete...", task_handles.len());
        for (i, handle) in task_handles.into_iter().enumerate() {
            handle.await.unwrap();
            info!("Task {} completed!", i + 1);
        }
        info!("All tasks completed!");

        let result1_arc: Arc<RwLock<Vec<Arc<String>>>> =
            *payload.get_arc("result1").unwrap().downcast().unwrap();
        let result2_arc: Arc<RwLock<Vec<Arc<String>>>> =
            *payload.get_arc("result2").unwrap().downcast().unwrap();
        let results1 = result1_arc.read().unwrap();
        let results2 = result2_arc.read().unwrap();

        assert_eq!(results1.len(), 1000);
        assert_eq!(results2.len(), 1000);
        assert_eq!(results1[0].as_str(), "task1_processed_chunk_0");
        assert_eq!(results2[0].as_str(), "task2_processed_chunk_0");

        info!(
            "Final results - Result1: {}, Result2: {}",
            results1.len(),
            results2.len()
        );
    }

    #[tokio::test]
    async fn test_complex_pipeline_with_chained_tasks() {
        dotenv::dotenv().ok(); // Load .env file
        let _ = env_logger::builder().is_test(true).try_init();
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

        let stage1_transform = |batch: Vec<Arc<String>>| async move {
            let sleep_ms = 500 + (rand::random::<u64>() % 501);
            tokio::time::sleep(Duration::from_millis(sleep_ms)).await;

            batch
                .into_iter()
                .enumerate()
                .map(|(idx, arc_item)| {
                    let content = &*arc_item;
                    Arc::new(ProcessedChunk {
                        id: idx,
                        content: format!("processed_{content}"),
                        word_count: content.split('_').count(),
                        processed_at: format!("timestamp_{idx}"),
                    })
                })
                .collect()
        };

        let stage2_transform = |batch: Vec<Arc<ProcessedChunk>>| async move {
            let sleep_ms = 300 + (rand::random::<u64>() % 301);
            tokio::time::sleep(Duration::from_millis(sleep_ms)).await;

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
            (0..100).map(|i| Arc::new(format!("chunk_{i}"))).collect();

        let payload = CogneePayload::<String, ProcessedChunk, AnalyzedResult>::new(initial_chunks);

        info!("Starting Stage 1: chunks -> result1");
        let task_future1 = create_task(
            "Stage1_ChunksToProcessed",
            None,
            *payload.get_arc("chunks").unwrap().downcast().unwrap(),
            Some(*payload.get_arc("result1").unwrap().downcast().unwrap()),
            *payload
                .get_arc("property_status")
                .unwrap()
                .downcast()
                .unwrap(),
            "result1",
            stage1_transform,
            None,
        );
        let handle1 = tokio::spawn(task_future1);

        handle1.await.unwrap();
        info!("Stage 1 completed!");

        info!("Starting Stage 2: result1 -> result2");
        let task_future2 = create_task(
            "Stage2_ProcessedToAnalyzed",
            Some(15),
            *payload.get_arc("result1").unwrap().downcast().unwrap(),
            Some(*payload.get_arc("result2").unwrap().downcast().unwrap()),
            *payload
                .get_arc("property_status")
                .unwrap()
                .downcast()
                .unwrap(),
            "result2",
            stage2_transform,
            None,
        );
        let handle2 = tokio::spawn(task_future2);

        handle2.await.unwrap();
        info!("Stage 2 completed!");

        let result1_arc: Arc<RwLock<Vec<Arc<ProcessedChunk>>>> =
            *payload.get_arc("result1").unwrap().downcast().unwrap();
        let result2_arc: Arc<RwLock<Vec<Arc<AnalyzedResult>>>> =
            *payload.get_arc("result2").unwrap().downcast().unwrap();
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

        info!("Pipeline Results:");
        info!("- Stage 1 (ProcessedChunk): {} items", results1.len());
        info!("- Stage 2 (AnalyzedResult): {} items", results2.len());
        info!("- First analyzed result: {:?}", results2[0]);
    }

    #[tokio::test]
    async fn test_task_with_no_output() {
        dotenv::dotenv().ok();
        let _ = env_logger::builder().is_test(true).try_init();
        use crate::data::payload_types::cognee_payload::CogneePayload;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Duration;

        static SIDE_EFFECT_COUNTER: AtomicUsize = AtomicUsize::new(0);

        let side_effect_task = |batch: Vec<Arc<String>>| async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            for item in &batch {
                info!("Side effect processing: {}", &**item);
                SIDE_EFFECT_COUNTER.fetch_add(1, Ordering::SeqCst);
            }
            // The return value doesn't matter since output is None
            vec![Arc::new("this won't be written anywhere".to_string())]
        };

        let initial_chunks: Vec<Arc<String>> =
            (0..10).map(|i| Arc::new(format!("chunk_{i}"))).collect();

        let payload = CogneePayload::<String, String, String>::new(initial_chunks);

        info!("Starting test with no output parameter");

        let task_future = create_task(
            "NoOutputTask",
            Some(3),
            *payload.get_arc("chunks").unwrap().downcast().unwrap(),
            None, // No output storage!
            *payload
                .get_arc("property_status")
                .unwrap()
                .downcast()
                .unwrap(),
            "custom_task_status",
            side_effect_task,
            None,
        );
        let handle = tokio::spawn(task_future);

        handle.await.unwrap();

        let result1_arc: Arc<RwLock<Vec<Arc<String>>>> =
            *payload.get_arc("result1").unwrap().downcast().unwrap();
        let results1 = result1_arc.read().unwrap();
        let result2_arc: Arc<RwLock<Vec<Arc<String>>>> =
            *payload.get_arc("result2").unwrap().downcast().unwrap();
        let results2 = result2_arc.read().unwrap();

        assert_eq!(results1.len(), 0);
        assert_eq!(results2.len(), 0);

        assert_eq!(SIDE_EFFECT_COUNTER.load(Ordering::SeqCst), 10);

        info!("No output task completed successfully");
        info!("- Result1 output: {} items", results1.len());
        info!("- Result2 output: {} items", results2.len());
        info!(
            "- Side effects processed: {} items",
            SIDE_EFFECT_COUNTER.load(Ordering::SeqCst)
        );
    }

    // Dynamic CogneePayload Tests
    #[tokio::test]
    async fn test_dynamic_cognee_payload_with_parallel_tasks() {
        dotenv::dotenv().ok(); // Load .env file
        let _ = env_logger::builder().is_test(true).try_init();
        use std::time::Duration;

        // Create a dynamic payload type with 2 result fields for testing
        create_cognee_payload!(
            TestDynamicPayload,
            result1: String,
            result2: String
        );

        let transform_fn1 = |batch: Vec<Arc<String>>| async move {
            let sleep_ms = 1000 + (rand::random::<u64>() % 1001);
            tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
            batch
                .into_iter()
                .map(|arc_item| Arc::new(format!("dynamic_task1_processed_{}", &*arc_item)))
                .collect()
        };

        let transform_fn2 = |batch: Vec<Arc<String>>| async move {
            let sleep_ms = 1000 + (rand::random::<u64>() % 1001);
            tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
            batch
                .into_iter()
                .map(|arc_item| Arc::new(format!("dynamic_task2_processed_{}", &*arc_item)))
                .collect()
        };

        let initial_chunks: Vec<Arc<String>> = (0..1000)
            .map(|i| Arc::new(format!("dynamic_chunk_{i}")))
            .collect();

        let payload = TestDynamicPayload::new(initial_chunks);

        let mut task_handles = Vec::new();

        let task_future1 = create_task(
            "DynamicTask1_ToResult1",
            Some(100),
            *payload.get_arc("chunks").unwrap().downcast().unwrap(),
            Some(*payload.get_arc("result1").unwrap().downcast().unwrap()),
            *payload
                .get_arc("property_status")
                .unwrap()
                .downcast()
                .unwrap(),
            "result1",
            transform_fn1,
            None,
        );
        let handle1 = tokio::spawn(task_future1);
        task_handles.push(handle1);

        let task_future2 = create_task(
            "DynamicTask2_ToResult2",
            None,
            *payload.get_arc("chunks").unwrap().downcast().unwrap(),
            Some(*payload.get_arc("result2").unwrap().downcast().unwrap()),
            *payload
                .get_arc("property_status")
                .unwrap()
                .downcast()
                .unwrap(),
            "result2",
            transform_fn2,
            None,
        );
        let handle2 = tokio::spawn(task_future2);
        task_handles.push(handle2);

        info!(
            "Waiting for {} dynamic tasks to complete...",
            task_handles.len()
        );
        for (i, handle) in task_handles.into_iter().enumerate() {
            handle.await.unwrap();
            info!("Dynamic task {} completed!", i + 1);
        }
        info!("All dynamic tasks completed!");

        let result1_arc: Arc<RwLock<Vec<Arc<String>>>> =
            *payload.get_arc("result1").unwrap().downcast().unwrap();
        let result2_arc: Arc<RwLock<Vec<Arc<String>>>> =
            *payload.get_arc("result2").unwrap().downcast().unwrap();
        let results1 = result1_arc.read().unwrap();
        let results2 = result2_arc.read().unwrap();

        assert_eq!(results1.len(), 1000);
        assert_eq!(results2.len(), 1000);
        assert_eq!(
            results1[0].as_str(),
            "dynamic_task1_processed_dynamic_chunk_0"
        );
        assert_eq!(
            results2[0].as_str(),
            "dynamic_task2_processed_dynamic_chunk_0"
        );

        info!(
            "Final dynamic results - Result1: {}, Result2: {}",
            results1.len(),
            results2.len()
        );
    }

    #[tokio::test]
    async fn test_dynamic_complex_pipeline_with_chained_tasks() {
        dotenv::dotenv().ok(); // Load .env file
        let _ = env_logger::builder().is_test(true).try_init();
        use std::time::Duration;

        #[derive(Debug, Clone)]
        struct DynamicProcessedChunk {
            id: usize,
            content: String,
            word_count: usize,
            processed_at: String,
        }

        #[derive(Debug, Clone)]
        struct DynamicAnalyzedResult {
            chunk_id: usize,
            analysis: String,
            score: f64,
            metadata: Vec<String>,
        }

        // Create a dynamic payload type with custom result types
        create_cognee_payload!(
            DynamicPipelinePayload,
            result1: DynamicProcessedChunk,
            result2: DynamicAnalyzedResult
        );

        let stage1_transform = |batch: Vec<Arc<String>>| async move {
            let sleep_ms = 500 + (rand::random::<u64>() % 501);
            tokio::time::sleep(Duration::from_millis(sleep_ms)).await;

            batch
                .into_iter()
                .enumerate()
                .map(|(idx, arc_item)| {
                    let content = &*arc_item;
                    Arc::new(DynamicProcessedChunk {
                        id: idx,
                        content: format!("dynamic_processed_{content}"),
                        word_count: content.split('_').count(),
                        processed_at: format!("dynamic_timestamp_{idx}"),
                    })
                })
                .collect()
        };

        let stage2_transform = |batch: Vec<Arc<DynamicProcessedChunk>>| async move {
            let sleep_ms = 300 + (rand::random::<u64>() % 301);
            tokio::time::sleep(Duration::from_millis(sleep_ms)).await;

            batch
                .into_iter()
                .map(|arc_item| {
                    let processed = &*arc_item;
                    Arc::new(DynamicAnalyzedResult {
                        chunk_id: processed.id,
                        analysis: format!("dynamic_analyzed_{}", processed.content),
                        score: (processed.word_count as f64) * 2.0,
                        metadata: vec![
                            format!("dynamic_meta_{}", processed.id),
                            processed.processed_at.clone(),
                            format!("dynamic_words_{}", processed.word_count),
                        ],
                    })
                })
                .collect()
        };

        let initial_chunks: Vec<Arc<String>> = (0..100)
            .map(|i| Arc::new(format!("dynamic_chunk_{i}")))
            .collect();

        let payload = DynamicPipelinePayload::new(initial_chunks);

        info!("Starting Dynamic Stage 1: chunks -> result1");
        let task_future1 = create_task(
            "DynamicStage1_ChunksToProcessed",
            None,
            *payload.get_arc("chunks").unwrap().downcast().unwrap(),
            Some(*payload.get_arc("result1").unwrap().downcast().unwrap()),
            *payload
                .get_arc("property_status")
                .unwrap()
                .downcast()
                .unwrap(),
            "result1",
            stage1_transform,
            None,
        );
        let handle1 = tokio::spawn(task_future1);

        handle1.await.unwrap();
        info!("Dynamic Stage 1 completed!");

        info!("Starting Dynamic Stage 2: result1 -> result2");
        let task_future2 = create_task(
            "DynamicStage2_ProcessedToAnalyzed",
            Some(15),
            *payload.get_arc("result1").unwrap().downcast().unwrap(),
            Some(*payload.get_arc("result2").unwrap().downcast().unwrap()),
            *payload
                .get_arc("property_status")
                .unwrap()
                .downcast()
                .unwrap(),
            "result2",
            stage2_transform,
            None,
        );
        let handle2 = tokio::spawn(task_future2);

        handle2.await.unwrap();
        info!("Dynamic Stage 2 completed!");

        let result1_arc: Arc<RwLock<Vec<Arc<DynamicProcessedChunk>>>> =
            *payload.get_arc("result1").unwrap().downcast().unwrap();
        let result2_arc: Arc<RwLock<Vec<Arc<DynamicAnalyzedResult>>>> =
            *payload.get_arc("result2").unwrap().downcast().unwrap();
        let results1 = result1_arc.read().unwrap();
        let results2 = result2_arc.read().unwrap();

        assert_eq!(results1.len(), 100);
        assert_eq!(results2.len(), 100);

        assert_eq!(results1[0].id, 0);
        assert_eq!(results1[0].content, "dynamic_processed_dynamic_chunk_0");
        assert_eq!(results1[0].word_count, 3);

        assert_eq!(results2[0].chunk_id, 0);
        assert_eq!(
            results2[0].analysis,
            "dynamic_analyzed_dynamic_processed_dynamic_chunk_0"
        );
        assert_eq!(results2[0].score, 6.0);
        assert_eq!(results2[0].metadata.len(), 3);

        info!("Dynamic Pipeline Results:");
        info!(
            "- Dynamic Stage 1 (DynamicProcessedChunk): {} items",
            results1.len()
        );
        info!(
            "- Dynamic Stage 2 (DynamicAnalyzedResult): {} items",
            results2.len()
        );
        info!("- First dynamic analyzed result: {:?}", results1[0]);
        info!("- Second dynamic analyzed result: {:?}", results2[0]);
    }

    #[tokio::test]
    async fn test_dynamic_task_with_no_output() {
        dotenv::dotenv().ok();
        let _ = env_logger::builder().is_test(true).try_init();
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Duration;

        // Create a dynamic payload type for testing
        create_cognee_payload!(
            DynamicNoOutputPayload,
            result1: String,
            result2: String
        );

        static DYNAMIC_SIDE_EFFECT_COUNTER: AtomicUsize = AtomicUsize::new(0);

        let dynamic_side_effect_task = |batch: Vec<Arc<String>>| async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            for item in &batch {
                info!("Dynamic side effect processing: {}", &**item);
                DYNAMIC_SIDE_EFFECT_COUNTER.fetch_add(1, Ordering::SeqCst);
            }
            // The return value doesn't matter since output is None
            vec![Arc::new("this won't be written anywhere".to_string())]
        };

        let initial_chunks: Vec<Arc<String>> = (0..10)
            .map(|i| Arc::new(format!("dynamic_chunk_{i}")))
            .collect();

        let payload = DynamicNoOutputPayload::new(initial_chunks);

        info!("Starting dynamic test with no output parameter");

        let task_future = create_task(
            "DynamicNoOutputTask",
            Some(3),
            *payload.get_arc("chunks").unwrap().downcast().unwrap(),
            None, // No output storage!
            *payload
                .get_arc("property_status")
                .unwrap()
                .downcast()
                .unwrap(),
            "dynamic_custom_task_status",
            dynamic_side_effect_task,
            None,
        );
        let handle = tokio::spawn(task_future);

        handle.await.unwrap();

        let result1_arc: Arc<RwLock<Vec<Arc<String>>>> =
            *payload.get_arc("result1").unwrap().downcast().unwrap();
        let results1 = result1_arc.read().unwrap();
        let result2_arc: Arc<RwLock<Vec<Arc<String>>>> =
            *payload.get_arc("result2").unwrap().downcast().unwrap();
        let results2 = result2_arc.read().unwrap();

        assert_eq!(results1.len(), 0);
        assert_eq!(results2.len(), 0);

        assert_eq!(DYNAMIC_SIDE_EFFECT_COUNTER.load(Ordering::SeqCst), 10);

        info!("Dynamic no output task completed successfully");
        info!("- Dynamic Result1 output: {} items", results1.len());
        info!("- Dynamic Result2 output: {} items", results2.len());
        info!(
            "- Dynamic side effects processed: {} items",
            DYNAMIC_SIDE_EFFECT_COUNTER.load(Ordering::SeqCst)
        );
    }
}
