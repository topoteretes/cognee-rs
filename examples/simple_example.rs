use cognee_rust::create_cognee_payload;
use cognee_rust::data::payload_base::PayloadBase;
use cognee_rust::data::payload_types::cognee_payload::PropertyStatus;
use cognee_rust::infrastructure::task::LoopSignal;
use cognee_rust::infrastructure::task::create_task;
use cognee_rust::{PayloadConstructor, PayloadTrait};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::sleep;
use uuid::Uuid;


// Create a dynamic payload type for testing
create_cognee_payload!(
    DynamicPipelinePayload,
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
async fn write_dynamic_payload_to_json<T>(
    payload: &T,
    counter: usize,
) -> Result<(), Box<dyn std::error::Error>>
where
    T: PayloadTrait,
{
    let chunks: Vec<Arc<String>> = *payload
        .payload_get_copy("chunks")
        .unwrap()
        .downcast()
        .unwrap();
    let result1: Vec<Arc<String>> = *payload
        .payload_get_copy("result1")
        .unwrap()
        .downcast()
        .unwrap();
    let result2: Vec<Arc<String>> = *payload
        .payload_get_copy("result2")
        .unwrap()
        .downcast()
        .unwrap();

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

    let filename = format!("dynamic_result_{counter}.json");
    let json_content = serde_json::to_string_pretty(&completed_payload)?;

    tokio::fs::write(&filename, json_content).await?;
    println!("Written dynamic payload #{counter} to {filename}");

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

async fn stage2_transform(result1: Vec<Arc<String>>) -> Vec<Arc<String>> {
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

async fn run_pipeline<T>(
    max_payloads: usize,
    max_concurrent_tasks: usize,
    max_completed: usize,
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
            _ = tokio::time::sleep(Duration::from_millis(10000)) => {
                println!("Periodic check for dynamic work - no signals received for 10s");
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

                let _chunks_status = payload.payload_get_property_status("chunks");
                let result1_status = payload.payload_get_property_status("result1");
                let result2_status = payload.payload_get_property_status("result2");

                // This is the case when the payload is fully completed
                if let (Some(r1), Some(r2)) = (&result1_status, &result2_status)
                    && matches!(r1, PropertyStatus::Done)
                    && matches!(r2, PropertyStatus::Done)
                {
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

                // If result1 is empty, create a new task that gets result1 and processes it
                if let Some(status) = &result1_status
                    && matches!(status, PropertyStatus::Empty)
                    && active_tasks.len() < max_concurrent_tasks
                {
                    println!(
                        "Creating Dynamic Stage1 async task for payload {} (ID: {}) - Tasks: {}/{}",
                        index + 1,
                        payload_id,
                        active_tasks.len() + 1,
                        max_concurrent_tasks
                    );

                    payload.payload_set_property_status("result1", PropertyStatus::Processing);

                    let task_future = create_task(
                        "DynamicStage1_ChunksToProcessed",
                        None,
                        *payload.payload_get_arc("chunks").unwrap().downcast().unwrap(),
                        Some(*payload.payload_get_arc("result1").unwrap().downcast().unwrap()),
                        *payload
                            .payload_get_arc("property_status")
                            .unwrap()
                            .downcast()
                            .unwrap(),
                        "result1",
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
                    && active_tasks.len() < max_concurrent_tasks
                {
                    println!(
                        "Creating Dynamic Stage2 task for payload {} (ID: {}) - Tasks: {}/{}",
                        index + 1,
                        payload_id,
                        active_tasks.len() + 1,
                        max_concurrent_tasks
                    );

                    payload.payload_set_property_status("result2", PropertyStatus::Processing);

                    let task_future = create_task(
                        "DynamicStage2_ProcessedToFinal",
                        None,
                        *payload.payload_get_arc("result1").unwrap().downcast().unwrap(),
                        Some(*payload.payload_get_arc("result2").unwrap().downcast().unwrap()),
                        *payload
                            .payload_get_arc("property_status")
                            .unwrap()
                            .downcast()
                            .unwrap(),
                        "result2",
                        stage2_transform,
                        Some(signal_tx.clone()),
                    );
                    let handle = tokio::spawn(task_future);
                    active_tasks.push(handle);
                }
            }

            // Collect payloads to write to JSON before removing
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
        for (payload, counter) in payloads_to_write {
            if let Err(e) = write_dynamic_payload_to_json(&*payload, counter).await {
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

#[tokio::main]
async fn main() {
    dotenv::dotenv().ok();

    // Pipeline parameters
    let max_payloads = 5; // Maximum number of payloads in the central processing queue
    let max_concurrent_tasks = 3; // Maximum number of concurrent tasks
    let max_completed = 10; // Number of all payloads (just for the POC)



    run_pipeline(
        max_payloads,
        max_concurrent_tasks,
        max_completed,
        std::marker::PhantomData::<DynamicPipelinePayload>,
    )
    .await;
}
