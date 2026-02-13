// ============================================================================
// Task Configuration and Execution
// ============================================================================

use crate::data::PayloadTrait;
use crate::data::PropertyStatus;
use log::{debug, info};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum LoopSignal {
    TaskCompleted,
    NewPayloadAdded,
    Shutdown,
}

#[allow(clippy::too_many_arguments)]
pub fn create_task<TInput, TOutput, F, Fut>(
    task_name: String,
    batch_size: Option<usize>,
    input: Arc<RwLock<Vec<Arc<TInput>>>>,
    output: Option<Arc<RwLock<Vec<Arc<TOutput>>>>>,
    property_status: Arc<std::sync::Mutex<std::collections::HashMap<String, PropertyStatus>>>,
    output_property_name: String,
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

// Type alias for complex return type to improve readability
type TaskFuture = Pin<Box<dyn Future<Output = ()> + Send>>;
type TaskResult = Result<TaskFuture, Box<dyn std::error::Error>>;

// Process function type for task execution
pub type ProcessFn<TInput, TOutput> = Arc<
    dyn Fn(Vec<Arc<TInput>>) -> Pin<Box<dyn Future<Output = Vec<Arc<TOutput>>> + Send>>
        + Send
        + Sync,
>;

// ------------------------------
// Trait for task configuration
// ------------------------------
pub trait TaskConfigTrait: Send + Sync {
    fn name(&self) -> &str;
    fn input_property_name(&self) -> &str;
    fn output_property_name(&self) -> &str;
    fn batch_size(&self) -> Option<usize>;

    // Method to create a task with concrete types
    fn create_task_future(
        &self,
        payload: &dyn PayloadTrait,
        signal_tx: Option<mpsc::UnboundedSender<LoopSignal>>,
    ) -> TaskResult;
}

// ------------------------------
// Task configuration structure
// ------------------------------
pub struct TaskConfig<TInput, TOutput> {
    pub name: String,
    pub batch_size: Option<usize>,
    pub input_property_name: String,
    pub output_property_name: String,
    pub process_fn: ProcessFn<TInput, TOutput>,
}

impl<TInput, TOutput> TaskConfig<TInput, TOutput>
where
    TInput: Clone + Send + Sync + 'static,
    TOutput: Clone + Send + Sync + 'static,
{
    pub fn new<F, Fut>(
        name: String,
        input_property_name: String,
        output_property_name: String,
        batch_size: Option<usize>,
        process_fn: F,
    ) -> Self
    where
        F: Fn(Vec<Arc<TInput>>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Vec<Arc<TOutput>>> + Send + 'static,
    {
        Self {
            name,
            input_property_name,
            output_property_name,
            batch_size,
            process_fn: Arc::new(move |input| Box::pin(process_fn(input))),
        }
    }
}

// Implement the trait for TaskConfig
impl<TInput, TOutput> TaskConfigTrait for TaskConfig<TInput, TOutput>
where
    TInput: Clone + Send + Sync + 'static,
    TOutput: Clone + Send + Sync + 'static,
{
    fn name(&self) -> &str {
        &self.name
    }

    fn input_property_name(&self) -> &str {
        &self.input_property_name
    }

    fn output_property_name(&self) -> &str {
        &self.output_property_name
    }

    fn batch_size(&self) -> Option<usize> {
        self.batch_size
    }

    fn create_task_future(
        &self,
        payload: &dyn PayloadTrait,
        signal_tx: Option<mpsc::UnboundedSender<LoopSignal>>,
    ) -> TaskResult {
        let input_arc = payload
            .payload_get_arc(self.input_property_name())
            .map_err(|_| "Input property not found")?
            .downcast::<Arc<RwLock<Vec<Arc<TInput>>>>>()
            .map_err(|_| "Failed to downcast input")?;

        let output_arc = payload
            .payload_get_arc(self.output_property_name())
            .map_err(|_| "Output property not found")?
            .downcast::<Arc<RwLock<Vec<Arc<TOutput>>>>>()
            .map_err(|_| "Failed to downcast output")?;

        let property_status = payload
            .payload_get_arc("property_status")
            .map_err(|_| "Property status not found")?
            .downcast::<Arc<Mutex<HashMap<String, PropertyStatus>>>>()
            .map_err(|_| "Failed to downcast property status")?;

        // Create a wrapper function that matches the expected signature
        let process_fn_wrapper = {
            let process_fn = self.process_fn.clone();
            move |input: Vec<Arc<TInput>>| {
                let process_fn = process_fn.clone();
                Box::pin(async move { process_fn(input).await })
            }
        };

        let task_future = create_task(
            self.name().to_string(),
            self.batch_size(),
            *input_arc,
            Some(*output_arc),
            *property_status,
            self.output_property_name().to_string(),
            process_fn_wrapper,
            signal_tx,
        );

        Ok(Box::pin(task_future))
    }
}

// ============================================================================
// Pipeline Executor
// ============================================================================

use crate::data::{PayloadConstructor, PayloadTrait as PayloadTraitForExecutor};
use std::time::Duration;
use tokio::task::JoinHandle;
use uuid::Uuid;

/// Pipeline executor that manages the execution of multiple payloads through configured tasks
pub async fn run_pipeline<T>(
    max_payloads: usize,
    max_concurrent_tasks: usize,
    max_completed: usize,
    pipeline_tasks: Vec<Arc<dyn TaskConfigTrait>>,
    _payload_type: std::marker::PhantomData<T>,
) where
    T: PayloadTraitForExecutor + PayloadConstructor + Clone + Send + Sync + 'static,
{
    ///////// Scheduler related resources
    let (signal_tx, mut signal_rx) = mpsc::unbounded_channel::<LoopSignal>();
    let payloads: Arc<RwLock<Vec<Arc<T>>>> = Arc::new(RwLock::new(Vec::new()));
    let mut payload_counters: HashMap<Uuid, usize> = HashMap::new();

    // List to keep track of active tasks
    let mut active_tasks: Vec<JoinHandle<()>> = Vec::new();

    // Counters
    let mut counter = 0;
    let mut completed_payloads = 0;

    loop {
        tokio::select! {
            signal = signal_rx.recv() => {
                match signal {
                    Some(LoopSignal::TaskCompleted) => {
                        println!("Received dynamic task completion signal - checking for work...");

                    }
                    Some(LoopSignal::NewPayloadAdded) => {
                        println!("Received new dynamic payload signal - checking for work...");

                    }
                    Some(LoopSignal::Shutdown) => {
                        println!("Received shutdown signal");
                        break;
                    }
                    None => {
                        println!("Signal channel closed");
                        break;
                    }
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(5000)) => {
                println!("Periodic check for dynamic work - no signals received for 5s");
            }
        }

        let current_size = payloads.read().unwrap().len();

        // Adds new payload to the queue if there is space left
        if current_size < max_payloads && counter < max_completed {
            counter += 1;

            // Create a default payload using the generic type T
            let chunks = vec![
                Arc::new(format!("Default Chunk A from payload {counter}")),
                Arc::new(format!("Default Chunk B from payload {counter}")),
            ];
            let payload = Arc::new(T::new(chunks));
            let payload_id = payload.payload_id();

            payloads.write().unwrap().push(Arc::clone(&payload));
            payload_counters.insert(payload_id, counter);

            println!(
                "Added dynamic payload {} to list (size: {}/{})",
                counter,
                current_size + 1,
                max_payloads
            );

            // Send signal that we added a payload
            let _ = signal_tx.send(LoopSignal::NewPayloadAdded);
        }

        let mut payloads_to_write = Vec::new();
        {
            let mut payload_list = payloads.write().unwrap();
            let mut payload_ids_to_remove = Vec::new();

            for (index, payload) in payload_list.iter().enumerate() {
                let payload_id = payload.payload_id();

                // This is the case when the payload is fully completed - check if ALL properties are done
                let all_properties_done = payload
                    .payload_get_all_property_statuses()
                    .iter()
                    .all(|(_, status)| matches!(status, PropertyStatus::Done));

                if all_properties_done {
                    let payload_counter = payload_counters.get(&payload_id).copied().unwrap_or(0);
                    println!(
                        "Dynamic payload {} (ID: {}, counter: {}) fully completed!",
                        index + 1,
                        payload_id,
                        payload_counter
                    );

                    payload_ids_to_remove.push(payload_id);
                    completed_payloads += 1;
                    continue;
                }

                for task in &pipeline_tasks {
                    let input_property_name = task.input_property_name();
                    let output_property_name = task.output_property_name();

                    let task_name = task.name();

                    let input_status = payload.payload_get_property_status(input_property_name);
                    let output_status = payload.payload_get_property_status(output_property_name);

                    println!(
                        "Task: {task_name:?} has of {input_property_name:?} with status of: {input_status:?} and output  {output_property_name:?} with status of : {output_status:?}"
                    );

                    if let (Some(input_status), Some(output_status)) = (input_status, output_status)
                        && matches!(input_status, PropertyStatus::Done)
                        && matches!(output_status, PropertyStatus::Empty)
                        && active_tasks.len() < max_concurrent_tasks
                    {
                        payload.payload_set_property_status(
                            task.output_property_name(),
                            PropertyStatus::Processing,
                        );

                        println!(
                            "Creating dynamic task '{}' for payload {} (ID: {})",
                            task.name(),
                            index + 1,
                            payload_id
                        );

                        // Use the trait method to create the task
                        match task.create_task_future(&**payload, Some(signal_tx.clone())) {
                            Ok(task_future) => {
                                let handle = tokio::spawn(task_future);
                                active_tasks.push(handle);
                            }
                            Err(e) => {
                                eprintln!("Failed to create task '{}': {}", task.name(), e);
                                // Reset the property status on error
                                payload.payload_set_property_status(
                                    task.output_property_name(),
                                    PropertyStatus::Empty,
                                );
                            }
                        }
                    }
                }
            }

            // Remove completed payloads
            for payload_id in payload_ids_to_remove {
                if let Some(pos) = payload_list
                    .iter()
                    .position(|p| p.payload_id() == payload_id)
                {
                    let payload = Arc::clone(&payload_list[pos]);
                    let counter = payload_counters.get(&payload_id).copied().unwrap_or(0);
                    payloads_to_write.push((payload, counter));

                    payload_list.remove(pos);
                    payload_counters.remove(&payload_id);
                    println!("Removed completed dynamic payload with ID: {payload_id}");
                }
            }
        }

        // Write JSON files after releasing the lock
        for (_payload, counter) in payloads_to_write {
            println!("Would write dynamic payload #{counter} to JSON file.");
        }

        let before_count = active_tasks.len();
        active_tasks.retain(|handle| !handle.is_finished());
        let after_count = active_tasks.len();

        // Show task status
        if before_count != after_count || !active_tasks.is_empty() {
            println!(
                "Dynamic Status: {} active tasks, {} payloads in queue, {} completed",
                active_tasks.len(),
                payloads.read().unwrap().len(),
                completed_payloads
            );
        }

        if completed_payloads >= max_completed {
            println!("Reached dynamic completion target: {completed_payloads} payloads processed");
            break;
        }
    }

    // Let all tasks finish ()
    for handle in active_tasks {
        handle.await.unwrap();
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::PayloadBase;
    use crate::data::PropertyStatus;
    use crate::data::create_cognee_payload;
    use rand::Rng;
    use serde::{Deserialize, Serialize};
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, RwLock};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use tokio::sync::mpsc;
    use tokio::task::JoinHandle;
    use tokio::time::sleep;
    use uuid::Uuid;

    // ============================================================================
    // Task Tests
    // ============================================================================

    #[tokio::test]
    async fn test_cognee_payload_with_parallel_tasks() {
        dotenv::dotenv().ok(); // Load .env file
        let _ = env_logger::builder().is_test(true).try_init();
        use crate::data::CogneePayload;
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
            "Task1_ToResult1".to_string(),
            Some(100),
            *payload.get_arc("chunks").unwrap().downcast().unwrap(),
            Some(*payload.get_arc("result1").unwrap().downcast().unwrap()),
            *payload
                .get_arc("property_status")
                .unwrap()
                .downcast()
                .unwrap(),
            "result1".to_string(),
            transform_fn1,
            None,
        );
        let handle1 = tokio::spawn(task_future1);
        task_handles.push(handle1);

        let task_future2 = create_task(
            "Task2_ToResult2".to_string(),
            None,
            *payload.get_arc("chunks").unwrap().downcast().unwrap(),
            Some(*payload.get_arc("result2").unwrap().downcast().unwrap()),
            *payload
                .get_arc("property_status")
                .unwrap()
                .downcast()
                .unwrap(),
            "result2".to_string(),
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
        use crate::data::CogneePayload;
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
            "Stage1_ChunksToProcessed".to_string(),
            None,
            *payload.get_arc("chunks").unwrap().downcast().unwrap(),
            Some(*payload.get_arc("result1").unwrap().downcast().unwrap()),
            *payload
                .get_arc("property_status")
                .unwrap()
                .downcast()
                .unwrap(),
            "result1".to_string(),
            stage1_transform,
            None,
        );
        let handle1 = tokio::spawn(task_future1);

        handle1.await.unwrap();
        info!("Stage 1 completed!");

        info!("Starting Stage 2: result1 -> result2");
        let task_future2 = create_task(
            "Stage2_ProcessedToAnalyzed".to_string(),
            Some(15),
            *payload.get_arc("result1").unwrap().downcast().unwrap(),
            Some(*payload.get_arc("result2").unwrap().downcast().unwrap()),
            *payload
                .get_arc("property_status")
                .unwrap()
                .downcast()
                .unwrap(),
            "result2".to_string(),
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
        use crate::data::CogneePayload;
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
            "NoOutputTask".to_string(),
            Some(3),
            *payload.get_arc("chunks").unwrap().downcast().unwrap(),
            None, // No output storage!
            *payload
                .get_arc("property_status")
                .unwrap()
                .downcast()
                .unwrap(),
            "custom_task_status".to_string(),
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
            "DynamicTask1_ToResult1".to_string(),
            Some(100),
            *payload.get_arc("chunks").unwrap().downcast().unwrap(),
            Some(*payload.get_arc("result1").unwrap().downcast().unwrap()),
            *payload
                .get_arc("property_status")
                .unwrap()
                .downcast()
                .unwrap(),
            "result1".to_string(),
            transform_fn1,
            None,
        );
        let handle1 = tokio::spawn(task_future1);
        task_handles.push(handle1);

        let task_future2 = create_task(
            "DynamicTask2_ToResult2".to_string(),
            None,
            *payload.get_arc("chunks").unwrap().downcast().unwrap(),
            Some(*payload.get_arc("result2").unwrap().downcast().unwrap()),
            *payload
                .get_arc("property_status")
                .unwrap()
                .downcast()
                .unwrap(),
            "result2".to_string(),
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
            "DynamicStage1_ChunksToProcessed".to_string(),
            None,
            *payload.get_arc("chunks").unwrap().downcast().unwrap(),
            Some(*payload.get_arc("result1").unwrap().downcast().unwrap()),
            *payload
                .get_arc("property_status")
                .unwrap()
                .downcast()
                .unwrap(),
            "result1".to_string(),
            stage1_transform,
            None,
        );
        let handle1 = tokio::spawn(task_future1);

        handle1.await.unwrap();
        info!("Dynamic Stage 1 completed!");

        info!("Starting Dynamic Stage 2: result1 -> result2");
        let task_future2 = create_task(
            "DynamicStage2_ProcessedToAnalyzed".to_string(),
            Some(15),
            *payload.get_arc("result1").unwrap().downcast().unwrap(),
            Some(*payload.get_arc("result2").unwrap().downcast().unwrap()),
            *payload
                .get_arc("property_status")
                .unwrap()
                .downcast()
                .unwrap(),
            "result2".to_string(),
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
            "DynamicNoOutputTask".to_string(),
            Some(3),
            *payload.get_arc("chunks").unwrap().downcast().unwrap(),
            None, // No output storage!
            *payload
                .get_arc("property_status")
                .unwrap()
                .downcast()
                .unwrap(),
            "dynamic_custom_task_status".to_string(),
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

    // ============================================================================
    // Pipeline Executor Tests
    // ============================================================================

    // Create different payload types for testing
    create_cognee_payload!(
        SimpleTestPayload,
        result1: String,
        result2: String
    );

    create_cognee_payload!(
        ComplexTestPayload,
        result1: ProcessedData,
        result2: FinalResult
    );

    #[derive(Debug, Clone)]
    #[allow(dead_code)]
    pub struct ProcessedData {
        pub id: usize,
        pub content: String,
        pub word_count: usize,
        pub processed_at: String,
    }

    #[derive(Debug, Clone)]
    #[allow(dead_code)]
    pub struct FinalResult {
        pub chunk_id: usize,
        pub analysis: String,
        pub score: f64,
        pub metadata: Vec<String>,
    }

    // Test transformation functions
    async fn simple_stage1_transform(chunks: Vec<Arc<String>>) -> Vec<Arc<String>> {
        println!("Simple Stage1: processing {} chunks", chunks.len());
        let millis = rand::thread_rng().gen_range(100..=500);
        tokio::time::sleep(Duration::from_millis(millis)).await;

        let results: Vec<Arc<String>> = chunks
            .into_iter()
            .map(|chunk| Arc::new(format!("Simple-Stage1: {chunk}")))
            .collect();

        println!(
            "Simple Stage1: finished after {} ms, produced {} results",
            millis,
            results.len()
        );
        results
    }

    async fn simple_stage2_transform(result1: Vec<Arc<String>>) -> Vec<Arc<String>> {
        println!("Simple Stage2: processing {} items", result1.len());
        let millis = rand::thread_rng().gen_range(100..=300);
        tokio::time::sleep(Duration::from_millis(millis)).await;

        let results: Vec<Arc<String>> = result1
            .into_iter()
            .map(|item| Arc::new(format!("Simple-Stage2-Final: {item}")))
            .collect();

        println!(
            "Simple Stage2: finished after {} ms, produced {} results",
            millis,
            results.len()
        );
        results
    }

    async fn complex_stage1_transform(chunks: Vec<Arc<String>>) -> Vec<Arc<ProcessedData>> {
        println!("Complex Stage1: processing {} chunks", chunks.len());
        let millis = rand::thread_rng().gen_range(200..=600);
        tokio::time::sleep(Duration::from_millis(millis)).await;

        let results: Vec<Arc<ProcessedData>> = chunks
            .into_iter()
            .enumerate()
            .map(|(idx, chunk)| {
                Arc::new(ProcessedData {
                    id: idx,
                    content: format!("Complex-Processed: {chunk}"),
                    word_count: chunk.split(' ').count(),
                    processed_at: format!("timestamp_{idx}"),
                })
            })
            .collect();

        println!(
            "Complex Stage1: finished after {} ms, produced {} results",
            millis,
            results.len()
        );
        results
    }

    async fn complex_stage2_transform(result1: Vec<Arc<ProcessedData>>) -> Vec<Arc<FinalResult>> {
        println!("Complex Stage2: processing {} items", result1.len());
        let millis = rand::thread_rng().gen_range(150..=400);
        tokio::time::sleep(Duration::from_millis(millis)).await;

        let results: Vec<Arc<FinalResult>> = result1
            .into_iter()
            .map(|processed| {
                Arc::new(FinalResult {
                    chunk_id: processed.id,
                    analysis: format!("Complex-Analyzed: {}", processed.content),
                    score: (processed.word_count as f64) * 1.5,
                    metadata: vec![
                        format!("meta_{}", processed.id),
                        processed.processed_at.clone(),
                        format!("words_{}", processed.word_count),
                    ],
                })
            })
            .collect();

        println!(
            "Complex Stage2: finished after {} ms, produced {} results",
            millis,
            results.len()
        );
        results
    }

    #[tokio::test]
    async fn test_simple_pipeline_executor() {
        dotenv::dotenv().ok();
        let _ = env_logger::builder().is_test(true).try_init();

        println!("=== Testing Simple Pipeline Executor ===");

        let stage1_task = TaskConfig::new(
            "SimpleStage1_ChunksToProcessed".to_string(),
            "chunks".to_string(),
            "result1".to_string(),
            Some(5), // batch size of 5
            simple_stage1_transform,
        );

        let stage2_task = TaskConfig::new(
            "SimpleStage2_ProcessedToFinal".to_string(),
            "result1".to_string(),
            "result2".to_string(),
            None, // no batch size limit
            simple_stage2_transform,
        );

        let pipeline_tasks: Vec<Arc<dyn TaskConfigTrait>> =
            vec![Arc::new(stage1_task), Arc::new(stage2_task)];

        run_pipeline(
            3, // max_payloads
            2, // max_concurrent_tasks
            4, // max_completed
            pipeline_tasks,
            std::marker::PhantomData::<SimpleTestPayload>,
        )
        .await;

        println!("=== Simple Pipeline Executor Test Completed ===");
    }

    #[tokio::test]
    async fn test_complex_pipeline_executor() {
        dotenv::dotenv().ok();
        let _ = env_logger::builder().is_test(true).try_init();

        println!("=== Testing Complex Pipeline Executor ===");

        let stage1_task = TaskConfig::new(
            "ComplexStage1_ChunksToProcessed".to_string(),
            "chunks".to_string(),
            "result1".to_string(),
            Some(3), // batch size of 3
            complex_stage1_transform,
        );

        let stage2_task = TaskConfig::new(
            "ComplexStage2_ProcessedToFinal".to_string(),
            "result1".to_string(),
            "result2".to_string(),
            Some(2), // batch size of 2
            complex_stage2_transform,
        );

        let pipeline_tasks: Vec<Arc<dyn TaskConfigTrait>> =
            vec![Arc::new(stage1_task), Arc::new(stage2_task)];

        run_pipeline(
            2, // max_payloads
            1, // max_concurrent_tasks
            3, // max_completed
            pipeline_tasks,
            std::marker::PhantomData::<ComplexTestPayload>,
        )
        .await;

        println!("=== Complex Pipeline Executor Test Completed ===");
    }

    #[tokio::test]
    async fn test_high_concurrency_pipeline() {
        dotenv::dotenv().ok();
        let _ = env_logger::builder().is_test(true).try_init();

        println!("=== Testing High Concurrency Pipeline ===");

        let stage1_task = TaskConfig::new(
            "HighConcurrencyStage1".to_string(),
            "chunks".to_string(),
            "result1".to_string(),
            Some(2), // small batch size
            simple_stage1_transform,
        );

        let stage2_task = TaskConfig::new(
            "HighConcurrencyStage2".to_string(),
            "result1".to_string(),
            "result2".to_string(),
            Some(1), // very small batch size
            simple_stage2_transform,
        );

        let pipeline_tasks: Vec<Arc<dyn TaskConfigTrait>> =
            vec![Arc::new(stage1_task), Arc::new(stage2_task)];

        run_pipeline(
            6, // max_payloads
            5, // max_concurrent_tasks
            8, // max_completed
            pipeline_tasks,
            std::marker::PhantomData::<SimpleTestPayload>,
        )
        .await;

        println!("=== High Concurrency Pipeline Test Completed ===");
    }

    #[tokio::test]
    async fn test_mixed_payload_types() {
        dotenv::dotenv().ok();
        let _ = env_logger::builder().is_test(true).try_init();

        println!("=== Testing Mixed Payload Types ===");

        // Test with SimpleTestPayload
        let simple_stage1 = TaskConfig::new(
            "MixedSimpleStage1".to_string(),
            "chunks".to_string(),
            "result1".to_string(),
            Some(4),
            simple_stage1_transform,
        );

        let simple_stage2 = TaskConfig::new(
            "MixedSimpleStage2".to_string(),
            "result1".to_string(),
            "result2".to_string(),
            None,
            simple_stage2_transform,
        );

        let simple_tasks: Vec<Arc<dyn TaskConfigTrait>> =
            vec![Arc::new(simple_stage1), Arc::new(simple_stage2)];

        println!("Running with SimpleTestPayload...");
        run_pipeline(
            2,
            2,
            3,
            simple_tasks,
            std::marker::PhantomData::<SimpleTestPayload>,
        )
        .await;

        // Test with ComplexTestPayload
        let complex_stage1 = TaskConfig::new(
            "MixedComplexStage1".to_string(),
            "chunks".to_string(),
            "result1".to_string(),
            Some(3),
            complex_stage1_transform,
        );

        let complex_stage2 = TaskConfig::new(
            "MixedComplexStage2".to_string(),
            "result1".to_string(),
            "result2".to_string(),
            Some(2),
            complex_stage2_transform,
        );

        let complex_tasks: Vec<Arc<dyn TaskConfigTrait>> =
            vec![Arc::new(complex_stage1), Arc::new(complex_stage2)];

        println!("Running with ComplexTestPayload...");
        run_pipeline(
            2,
            1,
            2,
            complex_tasks,
            std::marker::PhantomData::<ComplexTestPayload>,
        )
        .await;

        println!("=== Mixed Payload Types Test Completed ===");
    }

    // ============================================================================
    // Dynamic Pipeline Tests
    // ============================================================================

    // Create a dynamic payload type for testing
    create_cognee_payload!(
        DynamicPipelineTestPayload,
        result1: String,
        result2: String
    );

    #[derive(Serialize, Deserialize, Debug)]
    struct CompletedPayload {
        counter: usize,
        original_chunks: Vec<String>,
        stage1_results: Vec<String>,
        stage2_results: Vec<String>,
        metadata: PayloadMetadata,
    }

    #[derive(Serialize, Deserialize, Debug)]
    struct PayloadMetadata {
        total_chunks: usize,
        completion_timestamp: String,
    }

    // Function to write completed dynamic payload to JSON file
    async fn write_dynamic_payload_to_json(
        payload: &DynamicPipelineTestPayload,
        counter: usize,
        output_dir: &Path,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let chunks: Vec<Arc<String>> = *payload.get_copy("chunks").unwrap().downcast().unwrap();
        let result1: Vec<Arc<String>> = *payload.get_copy("result1").unwrap().downcast().unwrap();
        let result2: Vec<Arc<String>> = *payload.get_copy("result2").unwrap().downcast().unwrap();

        let original_chunks: Vec<String> = chunks.iter().map(|c| c.as_str().to_string()).collect();
        let stage1_results: Vec<String> = result1.iter().map(|r| r.as_str().to_string()).collect();
        let stage2_results: Vec<String> = result2.iter().map(|r| r.as_str().to_string()).collect();

        let completed_payload = CompletedPayload {
            counter,
            original_chunks,
            stage1_results,
            stage2_results,
            metadata: PayloadMetadata {
                total_chunks: chunks.len(),
                completion_timestamp: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs()
                    .to_string(),
            },
        };

        let filename = output_dir.join(format!("dynamic_result_{counter}.json"));
        let json_content = serde_json::to_string_pretty(&completed_payload)?;

        tokio::fs::write(&filename, json_content).await?;
        println!(
            "Written dynamic payload #{counter} to {}",
            filename.display()
        );

        Ok(())
    }

    async fn stage1_transform_async(chunks: Vec<Arc<String>>) -> Vec<Arc<String>> {
        println!("Dynamic Task1 started: processing {} chunks", chunks.len());

        // Random sleep between 2s and 2s
        let millis = rand::thread_rng().gen_range(2000..=2000);
        sleep(Duration::from_millis(millis)).await;

        let results: Vec<Arc<String>> = chunks
            .into_iter()
            .map(|chunk| Arc::new(format!("Dynamic Stage1-Processed: {chunk}")))
            .collect();

        println!(
            "Dynamic Task1 finished after {} ms, produced {} results",
            millis,
            results.len()
        );

        results
    }

    async fn stage2_transform_async(result1: Vec<Arc<String>>) -> Vec<Arc<String>> {
        println!("Dynamic Task2 started: processing {} items", result1.len());

        // Random sleep between 2s and 4s
        let millis = rand::thread_rng().gen_range(2000..=4000);
        sleep(Duration::from_millis(millis)).await;

        let results: Vec<Arc<String>> = result1
            .into_iter()
            .map(|item| Arc::new(format!("Dynamic Stage2-Final: {item}")))
            .collect();

        println!(
            "Dynamic Task2 finished after {} ms, produced {} results",
            millis,
            results.len()
        );

        results
    }

    #[tokio::test]
    async fn test_dynamic_pipeline_orchestrator() {
        dotenv::dotenv().ok();

        let tmp_dir = tempfile::tempdir().expect("Failed to create temp directory");

        /////////Parameters
        // Maximum number of payloads in the central processing queue
        const MAX_PAYLOADS: usize = 5;
        // Maximum number of concurrent tasks
        const MAX_CONCURRENT_TASKS: usize = 3;
        // Number of all payloads (just for the POC)
        const MAX_COMPLETED: usize = 10;

        ///////// Scheduler related resources
        let (signal_tx, mut signal_rx) = mpsc::unbounded_channel::<LoopSignal>();
        type PayloadType = DynamicPipelineTestPayload;
        type PayloadList = Arc<RwLock<Vec<Arc<PayloadType>>>>;
        let payloads: PayloadList = Arc::new(RwLock::new(Vec::new()));
        let mut payload_counters: HashMap<Uuid, usize> = HashMap::new();

        // List to keep track of active tasks
        let mut active_tasks: Vec<JoinHandle<()>> = Vec::new();

        // Counters
        let mut counter = 0;
        let mut completed_payloads = 0;

        loop {
            tokio::select! {
                signal = signal_rx.recv() => {
                    match signal {
                        Some(LoopSignal::TaskCompleted) => {
                            println!("Received dynamic task completion signal - checking for work...");

                        }
                        Some(LoopSignal::NewPayloadAdded) => {
                            println!("Received new dynamic payload signal - checking for work...");

                        }
                        Some(LoopSignal::Shutdown) => {
                            println!("Received shutdown signal");
                            break;
                        }
                        None => {
                            println!("Signal channel closed");
                            break;
                        }
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(10000)) => {
                    println!("Periodic check for dynamic work - no signals received for 10s");
                }
            }

            let current_size = payloads.read().unwrap().len();

            // Adds new payload to the queue if there is space left
            if current_size < MAX_PAYLOADS && counter < MAX_COMPLETED {
                counter += 1;

                let chunks = vec![
                    Arc::new(format!("Dynamic Chunk A from payload {counter}")),
                    Arc::new(format!("Dynamic Chunk B from payload {counter}")),
                ];

                let payload = Arc::new(DynamicPipelineTestPayload::new(chunks));
                let payload_id = payload.id();

                payloads.write().unwrap().push(Arc::clone(&payload));
                payload_counters.insert(payload_id, counter);

                println!(
                    "Added dynamic payload {} to list (size: {}/{})",
                    counter,
                    current_size + 1,
                    MAX_PAYLOADS
                );

                // Send signal that we added a payload
                let _ = signal_tx.send(LoopSignal::NewPayloadAdded);
            }

            let mut payloads_to_write = Vec::new();
            {
                let mut payload_list = payloads.write().unwrap();
                let mut payload_ids_to_remove = Vec::new();

                for (index, payload) in payload_list.iter().enumerate() {
                    let payload_id = payload.id();
                    let _chunks_status = payload.get_property_status("chunks");
                    let result1_status = payload.get_property_status("result1");
                    let result2_status = payload.get_property_status("result2");

                    // This is the case when the payload is fully completed
                    if let (Some(r1), Some(r2)) = (&result1_status, &result2_status)
                        && matches!(r1, PropertyStatus::Done)
                        && matches!(r2, PropertyStatus::Done)
                    {
                        let payload_counter =
                            payload_counters.get(&payload_id).copied().unwrap_or(0);
                        println!(
                            "Dynamic payload {} (ID: {}, counter: {}) fully completed!",
                            index + 1,
                            payload_id,
                            payload_counter
                        );

                        payload_ids_to_remove.push(payload_id);
                        completed_payloads += 1;
                        continue;
                    }

                    // If result1 is empty, create a new task that gets result1 and processes it
                    if let Some(status) = &result1_status
                        && matches!(status, PropertyStatus::Empty)
                        && active_tasks.len() < MAX_CONCURRENT_TASKS
                    {
                        println!(
                            "Creating Dynamic Stage1 async task for payload {} (ID: {}) - Tasks: {}/{}",
                            index + 1,
                            payload_id,
                            active_tasks.len() + 1,
                            MAX_CONCURRENT_TASKS
                        );

                        payload.set_property_status("result1", PropertyStatus::Processing);

                        let task_future = create_task(
                            "DynamicStage1_ChunksToProcessed".to_string(),
                            None,
                            *payload.get_arc("chunks").unwrap().downcast().unwrap(),
                            Some(*payload.get_arc("result1").unwrap().downcast().unwrap()),
                            *payload
                                .get_arc("property_status")
                                .unwrap()
                                .downcast()
                                .unwrap(),
                            "result1".to_string(),
                            stage1_transform_async,
                            Some(signal_tx.clone()),
                        );
                        let handle = tokio::spawn(task_future);
                        active_tasks.push(handle);
                    }

                    // if result1 is done and result2 is empty, create a new task that gets result1 and result2 and processes them
                    if let (Some(r1_status), Some(r2_status)) = (&result1_status, &result2_status)
                        && matches!(r1_status, PropertyStatus::Done)
                        && matches!(r2_status, PropertyStatus::Empty)
                        && active_tasks.len() < MAX_CONCURRENT_TASKS
                    {
                        println!(
                            "Creating Dynamic Stage2 task for payload {} (ID: {}) - Tasks: {}/{}",
                            index + 1,
                            payload_id,
                            active_tasks.len() + 1,
                            MAX_CONCURRENT_TASKS
                        );

                        payload.set_property_status("result2", PropertyStatus::Processing);

                        let task_future = create_task(
                            "DynamicStage2_ProcessedToFinal".to_string(),
                            None,
                            *payload.get_arc("result1").unwrap().downcast().unwrap(),
                            Some(*payload.get_arc("result2").unwrap().downcast().unwrap()),
                            *payload
                                .get_arc("property_status")
                                .unwrap()
                                .downcast()
                                .unwrap(),
                            "result2".to_string(),
                            stage2_transform_async,
                            Some(signal_tx.clone()),
                        );
                        let handle = tokio::spawn(task_future);
                        active_tasks.push(handle);
                    }
                }

                // Collect payloads to write to JSON before removing
                for payload_id in payload_ids_to_remove {
                    if let Some(pos) = payload_list.iter().position(|p| p.id() == payload_id) {
                        let payload = Arc::clone(&payload_list[pos]);
                        let counter = payload_counters.get(&payload_id).copied().unwrap_or(0);
                        payloads_to_write.push((payload, counter));

                        payload_list.remove(pos);
                        payload_counters.remove(&payload_id);
                        println!("Removed completed dynamic payload with ID: {payload_id}");
                    }
                }
            }

            // Write JSON files after releasing the lock
            for (payload, counter) in payloads_to_write {
                if let Err(e) =
                    write_dynamic_payload_to_json(&payload, counter, tmp_dir.path()).await
                {
                    eprintln!("Failed to write dynamic payload {counter} to JSON: {e}");
                }
            }

            let before_count = active_tasks.len();
            active_tasks.retain(|handle| !handle.is_finished());
            let after_count = active_tasks.len();

            // Show task status
            if before_count != after_count || !active_tasks.is_empty() {
                println!(
                    "Dynamic Status: {} active tasks, {} payloads in queue, {} completed",
                    active_tasks.len(),
                    payloads.read().unwrap().len(),
                    completed_payloads
                );
            }

            if completed_payloads >= MAX_COMPLETED {
                println!(
                    "Reached dynamic completion target: {completed_payloads} payloads processed"
                );
                break;
            }
        }

        // Let all tasks finish ()
        for handle in active_tasks {
            handle.await.unwrap();
        }
    }
}
