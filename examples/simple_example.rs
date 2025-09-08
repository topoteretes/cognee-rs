use cognee_rust::data::payload_types::cognee_payload::{CogneePayload, PropertyStatus};
use cognee_rust::infrastructure::task::create_task;
use serde::{Deserialize, Serialize};
use serde_json;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::fs;
use tokio::task::JoinHandle;
use tokio::time::sleep;

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

async fn write_payload_to_json(
    payload: &CogneePayload<String, String, String>,
    counter: usize,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let chunks = {
        let chunks_arc = payload.chunks_arc();
        let chunks_guard = chunks_arc.read().unwrap();
        chunks_guard
            .iter()
            .map(|chunk| chunk.as_str().to_string())
            .collect::<Vec<String>>()
    };

    let stage1_results = {
        let result1_arc = payload.result1_arc();
        let result1_guard = result1_arc.read().unwrap();
        result1_guard
            .iter()
            .map(|result| result.as_str().to_string())
            .collect::<Vec<String>>()
    };

    let stage2_results = {
        let result2_arc = payload.result2_arc();
        let result2_guard = result2_arc.read().unwrap();
        result2_guard
            .iter()
            .map(|result| result.as_str().to_string())
            .collect::<Vec<String>>()
    };

    let completed_payload = CompletedPayload {
        counter,
        original_chunks: chunks.clone(),
        stage1_results,
        stage2_results,
        metadata: PayloadMetadata {
            total_chunks: chunks.len(),
            completion_timestamp: chrono::Utc::now().to_rfc3339(),
        },
    };

    let json_content = serde_json::to_string_pretty(&completed_payload)?;

    let filename = format!("payload-{}.json", counter);
    fs::write(&filename, json_content).await?;

    println!("Written payload {} to {}", counter, filename);
    Ok(())
}

async fn stage1_transform_async(chunks: Vec<Arc<String>>) -> Vec<Arc<String>> {
    sleep(Duration::from_millis(1000)).await;
    chunks
        .into_iter()
        .map(|chunk| Arc::new(format!("Stage1-Processed: {}", chunk)))
        .collect()
}

async fn stage2_transform(result1: Vec<Arc<String>>) -> Vec<Arc<String>> {
    sleep(Duration::from_millis(5000)).await;
    result1
        .into_iter()
        .map(|item| Arc::new(format!("Stage2-Final: {}", item)))
        .collect()
}

#[tokio::main]
async fn main() {
    dotenv::dotenv().ok();
    println!("Pipeline with create_task and payloads");

    let payloads: Arc<RwLock<Vec<Arc<CogneePayload<String, String, String>>>>> =
        Arc::new(RwLock::new(Vec::new()));

    let mut payload_counters: HashMap<usize, usize> = HashMap::new();

    println!("Starting dynamic task scheduling...");

    const MAX_PAYLOADS: usize = 5;
    let mut counter = 0;
    let mut active_tasks: Vec<JoinHandle<()>> = Vec::new();

    let mut completed_payloads = 0;
    const MAX_COMPLETED: usize = 1000;

    loop {
        let current_size = payloads.read().unwrap().len();

        if current_size < MAX_PAYLOADS && completed_payloads < MAX_COMPLETED {
            counter += 1;

            let chunks = vec![
                Arc::new(format!("Chunk A from payload {}", counter)),
                Arc::new(format!("Chunk B from payload {}", counter)),
            ];

            let payload = CogneePayload::new(chunks);

            let payload_index = current_size;
            payloads.write().unwrap().push(Arc::new(payload));
            payload_counters.insert(payload_index, counter);

            println!(
                "Added payload {} to list (size: {}/{})",
                counter,
                current_size + 1,
                MAX_PAYLOADS
            );
        }

        let mut completed_payloads_data = Vec::new();
        {
            let mut payload_list = payloads.write().unwrap();
            let mut indices_to_remove = Vec::new();

            for (index, payload) in payload_list.iter().enumerate() {
                let _chunks_status = payload.get_property_status("chunks");
                let result1_status = payload.get_property_status("result1");
                let result2_status = payload.get_property_status("result2");

                if let (Some(r1), Some(r2)) = (&result1_status, &result2_status) {
                    if matches!(r1, PropertyStatus::Done) && matches!(r2, PropertyStatus::Done) {
                        let payload_counter = payload_counters.get(&index).copied().unwrap_or(0);
                        println!(
                            "Payload {} (counter: {}) fully completed!",
                            index + 1,
                            payload_counter
                        );

                        completed_payloads_data.push((Arc::clone(payload), payload_counter));

                        indices_to_remove.push(index);
                        completed_payloads += 1;
                        continue;
                    }
                }

                if let Some(status) = &result1_status {
                    if matches!(status, PropertyStatus::Empty) {
                        println!("Creating Stage1 async task for payload {}", index + 1);

                        payload.set_property_status("result1", PropertyStatus::Processing);

                        let handle = create_task(
                            "Stage1_ChunksToProcessed",
                            None,
                            payload.chunks_arc(),
                            Some(payload.result1_arc()),
                            payload.property_status_arc(),
                            "result1",
                            stage1_transform_async,
                        );
                        active_tasks.push(handle);
                    }
                }

                if let (Some(r1_status), Some(r2_status)) = (&result1_status, &result2_status) {
                    if matches!(r1_status, PropertyStatus::Done)
                        && matches!(r2_status, PropertyStatus::Empty)
                    {
                        println!("Creating Stage2 task for payload {}", index + 1);

                        payload.set_property_status("result2", PropertyStatus::Processing);

                        let handle = create_task(
                            "Stage2_ProcessedToFinal",
                            None,
                            payload.result1_arc(),
                            Some(payload.result2_arc()),
                            payload.property_status_arc(),
                            "result2",
                            stage2_transform,
                        );
                        active_tasks.push(handle);
                    }
                }
            }

            for &index in indices_to_remove.iter().rev() {
                payload_list.remove(index);
                payload_counters.remove(&index);

                let mut updated_counters = HashMap::new();
                for (idx, counter_val) in payload_counters.iter() {
                    if *idx > index {
                        updated_counters.insert(idx - 1, *counter_val);
                    } else {
                        updated_counters.insert(*idx, *counter_val);
                    }
                }
                payload_counters = updated_counters;

                println!("Removed completed payload from list");
            }
        }

        for (payload, counter) in completed_payloads_data {
            if let Err(e) = write_payload_to_json(&payload, counter).await {
                eprintln!("Failed to write payload {} to JSON: {}", counter, e);
            }
        }

        active_tasks.retain(|handle| !handle.is_finished());

        if completed_payloads >= MAX_COMPLETED {
            println!(
                "Reached completion target: {} payloads processed",
                completed_payloads
            );
            break;
        }
    }

    println!(
        "Waiting for {} remaining tasks to complete...",
        active_tasks.len()
    );
    for handle in active_tasks {
        handle.await.unwrap();
    }

    println!("Pipeline completed successfully!");
    println!("Total payloads processed: {}", completed_payloads);
}
