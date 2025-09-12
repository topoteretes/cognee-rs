use crate::data::payload_trait::{PayloadConstructor, PayloadTrait};
use crate::data::payload_types::cognee_payload::PropertyStatus;
use crate::infrastructure::task::LoopSignal;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use uuid::Uuid;

/// Pipeline executor that manages the execution of multiple payloads through configured tasks
pub async fn run_pipeline<T>(
    max_payloads: usize,
    max_concurrent_tasks: usize,
    max_completed: usize,
    pipeline_tasks: Vec<Arc<dyn crate::infrastructure::task::TaskConfigTrait>>,
    _payload_type: std::marker::PhantomData<T>,
) where
    T: PayloadTrait + PayloadConstructor + Clone + Send + Sync + 'static,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::create_cognee_payload;
    use crate::data::payload_base::PayloadBase;
    use crate::infrastructure::task::{TaskConfig, TaskConfigTrait};
    use rand::Rng;
    use std::sync::Mutex;
    use std::time::Duration;

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
    struct ProcessedData {
        id: usize,
        content: String,
        word_count: usize,
        processed_at: String,
    }

    #[derive(Debug, Clone)]
    struct FinalResult {
        chunk_id: usize,
        analysis: String,
        score: f64,
        metadata: Vec<String>,
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
}
