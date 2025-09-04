use crate::data::payload_base::PayloadBase;
use crate::data::payloadbehavior::PayloadBehavior;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct CogneePayload<T1, T2, T3, T4>
where
    T1: Clone + Send + Sync,
    T2: Clone + Send + Sync,
    T3: Clone + Send + Sync,
    T4: Clone + Send + Sync,
{
    base: Arc<RwLock<PayloadBase>>,
    chunks: Arc<RwLock<Vec<String>>>, // Mutable multiple tasks can read it but only one can write it when there is no lock
    result1: Arc<RwLock<Vec<T1>>>,    // Generic type T1
    result2: Arc<RwLock<Vec<T2>>>,    // Generic type T2
    result3: Arc<RwLock<Vec<T3>>>,    // Generic type T3
    result4: Arc<RwLock<Vec<T4>>>,    // Generic type T4
}

impl<T1, T2, T3, T4> CogneePayload<T1, T2, T3, T4>
where
    T1: Clone + Send + Sync,
    T2: Clone + Send + Sync,
    T3: Clone + Send + Sync,
    T4: Clone + Send + Sync,
{
    pub fn new(chunks: Vec<String>) -> Self {
        Self {
            base: Arc::new(RwLock::new(PayloadBase::new())),
            chunks: Arc::new(RwLock::new(chunks)),
            result1: Arc::new(RwLock::new(Vec::new())),
            result2: Arc::new(RwLock::new(Vec::new())),
            result3: Arc::new(RwLock::new(Vec::new())),
            result4: Arc::new(RwLock::new(Vec::new())),
        }
    }

    // Get Arc references to chunks for direct access
    pub fn chunks_arc(&self) -> Arc<RwLock<Vec<String>>> {
        Arc::clone(&self.chunks)
    }

    // Chunks operations with RwLock (same pattern as results)
    pub fn add_chunk(&self, item: String) {
        let mut chunks = self.chunks.write().unwrap();
        chunks.push(item);
    }

    pub fn add_chunks_batch(&self, items: Vec<String>) {
        let mut chunks = self.chunks.write().unwrap();
        chunks.extend(items);
    }

    // avoid this because this copies the whole property
    pub fn get_chunks_copy(&self) -> Vec<String> {
        let chunks = self.chunks.read().unwrap();
        chunks.clone()
    }

    pub fn chunks_len(&self) -> usize {
        let chunks = self.chunks.read().unwrap();
        chunks.len()
    }

    pub fn clear_chunks(&self) {
        let mut chunks = self.chunks.write().unwrap();
        chunks.clear()
    }

    // Get Arc references to result collections for direct access
    pub fn result1_arc(&self) -> Arc<RwLock<Vec<T1>>> {
        Arc::clone(&self.result1)
    }

    pub fn result2_arc(&self) -> Arc<RwLock<Vec<T2>>> {
        Arc::clone(&self.result2)
    }

    pub fn result3_arc(&self) -> Arc<RwLock<Vec<T3>>> {
        Arc::clone(&self.result3)
    }

    pub fn result4_arc(&self) -> Arc<RwLock<Vec<T4>>> {
        Arc::clone(&self.result4)
    }

    // Result1 operations with RwLock
    pub fn add_result1(&self, item: T1) {
        let mut result1 = self.result1.write().unwrap();
        result1.push(item);
    }

    pub fn add_result1_batch(&self, items: Vec<T1>) {
        let mut result1 = self.result1.write().unwrap();
        result1.extend(items);
    }
    // avoid this because this copies the whole property
    pub fn get_result1_copy(&self) -> Vec<T1> {
        let result1 = self.result1.read().unwrap();
        result1.clone()
    }

    pub fn result1_len(&self) -> usize {
        let result1 = self.result1.read().unwrap();
        result1.len()
    }

    pub fn clear_result1(&self) {
        let mut result1 = self.result1.write().unwrap();
        result1.clear();
    }

    // Result2 operations with RwLock
    pub fn add_result2(&self, item: T2) {
        let mut result2 = self.result2.write().unwrap();
        result2.push(item);
    }

    pub fn add_result2_batch(&self, items: Vec<T2>) {
        let mut result2 = self.result2.write().unwrap();
        result2.extend(items);
    }

    pub fn get_result2_copy(&self) -> Vec<T2> {
        let result2 = self.result2.read().unwrap();
        result2.clone()
    }

    pub fn result2_len(&self) -> usize {
        let result2 = self.result2.read().unwrap();
        result2.len()
    }

    pub fn clear_result2(&self) {
        let mut result2 = self.result2.write().unwrap();
        result2.clear();
    }

    // Result3 operations with RwLock
    pub fn add_result3(&self, item: T3) {
        let mut result3 = self.result3.write().unwrap();
        result3.push(item);
    }

    pub fn add_result3_batch(&self, items: Vec<T3>) {
        let mut result3 = self.result3.write().unwrap();
        result3.extend(items);
    }

    pub fn get_result3_copy(&self) -> Vec<T3> {
        let result3 = self.result3.read().unwrap();
        result3.clone()
    }

    pub fn result3_len(&self) -> usize {
        let result3 = self.result3.read().unwrap();
        result3.len()
    }

    pub fn clear_result3(&self) {
        let mut result3 = self.result3.write().unwrap();
        result3.clear();
    }

    // Result4 operations with RwLock
    pub fn add_result4(&self, item: T4) {
        let mut result4 = self.result4.write().unwrap();
        result4.push(item);
    }

    pub fn add_result4_batch(&self, items: Vec<T4>) {
        let mut result4 = self.result4.write().unwrap();
        result4.extend(items);
    }

    pub fn get_result4_copy(&self) -> Vec<T4> {
        let result4 = self.result4.read().unwrap();
        result4.clone()
    }

    pub fn result4_len(&self) -> usize {
        let result4 = self.result4.read().unwrap();
        result4.len()
    }

    pub fn clear_result4(&self) {
        let mut result4 = self.result4.write().unwrap();
        result4.clear();
    }

    // Base access with RwLock for optimal read/write patterns
    pub fn read_base<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&PayloadBase) -> R,
    {
        let base = self.base.read().unwrap();
        f(&base)
    }

    pub fn write_base<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut PayloadBase) -> R,
    {
        let mut base = self.base.write().unwrap();
        f(&mut base)
    }
}

impl<T1, T2, T3, T4> PayloadBehavior for CogneePayload<T1, T2, T3, T4>
where
    T1: Clone + Send + Sync,
    T2: Clone + Send + Sync,
    T3: Clone + Send + Sync,
    T4: Clone + Send + Sync,
{
    fn id(&self) -> Uuid {
        self.read_base(|base| base.metainfo.id)
    }

    fn task_done(&mut self) {
        self.write_base(|base| base.metainfo.task_done());
    }
}

#[test]
fn parallel_readers_no_copy() {
    ////// Payload creation and initializing chunks
    let initial_chunks: Vec<String> = (0..1023)
        .map(|i| {
            // Create different content for each chunk
            match i % 5 {
                0 => format!("document_text_{:04}_analysis_ready", i),
                1 => format!("embedding_vector_{:04}_processed", i),
                2 => format!("memory_fragment_{:04}_indexed", i),
                3 => format!("knowledge_node_{:04}_connected", i),
                _ => format!("data_segment_{:04}_transformed", i),
            }
        })
        .collect();

    let payload = Arc::new(CogneePayload::<String, String, String, String>::new(
        initial_chunks,
    ));

    ////// Get Arc references for direct manipulation of the properties
    let chunks_arc = payload.chunks_arc();
    let result1_arc = payload.result1_arc();
    let result2_arc = payload.result2_arc();
    let result3_arc = payload.result3_arc();
    let result4_arc = payload.result4_arc();

    // List for the threads
    let mut threads = Vec::new();

    // ---- Task 1: process chunks in batches (length analysis) ----
    let result1 = Arc::clone(&result1_arc);
    let chunks_ref = Arc::clone(&chunks_arc);
    let t1 = thread::spawn(move || {
        let total_chunks = {
            let chunks_guard = chunks_ref.read().unwrap();
            chunks_guard.len()
        };
        println!(
            "Task 1 starting - processing {} chunks for length analysis...",
            total_chunks
        );

        const BATCH_SIZE: usize = 100;
        let mut total_processed = 0;

        // Process chunks in batches of 100 (zero-copy references)
        for batch_start in (0..total_chunks).step_by(BATCH_SIZE) {
            let batch_end = (batch_start + BATCH_SIZE).min(total_chunks);

            // Hold lock and work with references directly
            let mut batch_results = Vec::with_capacity(batch_end - batch_start);
            {
                let chunks_guard = chunks_ref.read().unwrap();

                // Random sleep between 2-4 seconds per batch (while holding lock)
                let sleep_ms = 2000 + (rand::random::<u64>() % 2001); // 2000-4000ms
                thread::sleep(Duration::from_millis(sleep_ms));

                // Process elements directly with references (no copying!)
                for i in batch_start..batch_end {
                    let chunk = &chunks_guard[i]; // ← Reference, no copy!
                    let length = chunk.len();
                    batch_results.push(format!("chunk_{}_length_{}", i, length));
                }

                // Write batch results
                {
                    let mut result1_guard = result1.write().unwrap();
                    result1_guard.extend(batch_results);
                }
            } // Lock released here

            total_processed += batch_end - batch_start;
            println!(
                "Task 1: processed {}/{} chunks (batch size: {}, zero-copy refs)",
                total_processed,
                total_chunks,
                batch_end - batch_start
            );
        }

        println!("Task 1 completed - processed length analysis");
    });
    threads.push(t1);

    // ---- Task 2: process chunks in batches (character analysis) ----
    let result2 = Arc::clone(&result2_arc);
    let chunks_ref = Arc::clone(&chunks_arc);
    let t2 = thread::spawn(move || {
        let total_chunks = {
            let chunks_guard = chunks_ref.read().unwrap();
            chunks_guard.len()
        };
        println!(
            "Task 2 starting - processing {} chunks for character analysis...",
            total_chunks
        );

        const BATCH_SIZE: usize = 50;
        let mut total_processed = 0;

        // Process chunks in batches of 50 (zero-copy references)
        for batch_start in (0..total_chunks).step_by(BATCH_SIZE) {
            let batch_end = (batch_start + BATCH_SIZE).min(total_chunks);

            // Hold lock and work with references directly
            let mut batch_results = Vec::with_capacity(batch_end - batch_start);
            {
                let chunks_guard = chunks_ref.read().unwrap();

                // Random sleep between 2-4 seconds per batch (while holding lock)
                let sleep_ms = 2000 + (rand::random::<u64>() % 2001); // 2000-4000ms
                thread::sleep(Duration::from_millis(sleep_ms));

                // Process elements directly with references (no copying!)
                for i in batch_start..batch_end {
                    let chunk = &chunks_guard[i]; // ← Reference, no copy!
                    let first_char = chunk.chars().next().unwrap_or('-');
                    let char_count = chunk.chars().count();
                    batch_results.push(format!(
                        "chunk_{}_first_char_{}_count_{}",
                        i, first_char, char_count
                    ));
                }

                // This is output writing
                {
                    let mut result2_guard = result2.write().unwrap();
                    result2_guard.extend(batch_results);
                }
            } // Lock released here

            total_processed += batch_end - batch_start;
            println!(
                "Task 2: processed {}/{} chunks (batch size: {}, zero-copy refs)",
                total_processed,
                total_chunks,
                batch_end - batch_start
            );
        }

        println!("Task 2 completed - processed character analysis");
    });
    threads.push(t2);

    // First phase: Wait for initial processing threads (1 & 2) to complete
    println!(
        "Phase 1: Waiting for {} initial threads to complete...",
        threads.len()
    );
    for (i, thread) in threads.into_iter().enumerate() {
        thread.join().unwrap();
        println!("Thread {} completed!", i + 1);
    }
    println!("Phase 1 completed! Now starting phase 2...");

    // Second phase: Process results from phase 1
    let mut phase2_threads = Vec::new();

    // ---- Thread 3: Process result1 data and write to result3 ----
    let result1_input = Arc::clone(&result1_arc);
    let result3_output = Arc::clone(&result3_arc);
    let t3 = thread::spawn(move || {
        println!("Thread 3 starting - processing result1 data...");

        // Get length of result1 without copying
        let total_entries = {
            let result1_guard = result1_input.read().unwrap();
            result1_guard.len()
        };

        println!(
            "Thread 3: processing {} length result strings in batches (zero-copy references)",
            total_entries
        );

        const BATCH_SIZE: usize = 200;
        let mut total_processed = 0;

        // Process in batches with direct references (zero copy!)
        for batch_start in (0..total_entries).step_by(BATCH_SIZE) {
            let batch_end = (batch_start + BATCH_SIZE).min(total_entries);

            // Hold lock and work with references directly
            let mut batch_results = Vec::with_capacity(batch_end - batch_start);
            {
                let result1_guard = result1_input.read().unwrap();

                // Random sleep between 2-4 seconds per batch (while holding lock)
                let sleep_ms = 2000 + (rand::random::<u64>() % 2001); // 2000-4000ms
                thread::sleep(Duration::from_millis(sleep_ms));

                // Process elements directly with references (no copying!)
                for i in batch_start..batch_end {
                    let result_string = &result1_guard[i]; // ← Reference, no copy!
                    // Parse the string format: "chunk_{}_length_{}"
                    if let Some(length_str) = result_string.split('_').nth(3) {
                        if let Ok(length) = length_str.parse::<usize>() {
                            let classification = match length {
                                0..=10 => "tiny",
                                11..=20 => "small",
                                21..=30 => "medium",
                                31..=50 => "large",
                                _ => "huge",
                            };
                            batch_results.push(format!(
                                "{}_classified_as_{}",
                                result_string, classification
                            ));
                        } else {
                            batch_results.push(format!("{}_classification_failed", result_string));
                        }
                    } else {
                        batch_results.push(format!("{}_parse_failed", result_string));
                    }
                }

                {
                    let mut result3_guard = result3_output.write().unwrap();
                    result3_guard.extend(batch_results);
                }
            } // Lock released here

            total_processed += batch_end - batch_start;
            println!(
                "Thread 3: processed {}/{} length strings (batch size: {}, zero-copy refs)",
                total_processed,
                total_entries,
                batch_end - batch_start
            );
        }

        println!("Thread 3 completed - processed length classifications");
    });
    phase2_threads.push(t3);

    // ---- Thread 4: Process result2 data and write to result4 ----
    let result2_input = Arc::clone(&result2_arc);
    let result4_output = Arc::clone(&result4_arc);
    let t4 = thread::spawn(move || {
        println!("Thread 4 starting - processing result2 data...");

        // Get length of result2 without copying
        let total_entries = {
            let result2_guard = result2_input.read().unwrap();
            result2_guard.len()
        };

        println!(
            "Thread 4: processing {} character result strings in batches (zero-copy references)",
            total_entries
        );

        const BATCH_SIZE: usize = 150;
        let mut total_processed = 0;

        // Process in batches with direct references (zero copy!)
        for batch_start in (0..total_entries).step_by(BATCH_SIZE) {
            let batch_end = (batch_start + BATCH_SIZE).min(total_entries);

            // Hold lock and work with references directly
            let mut batch_results = Vec::with_capacity(batch_end - batch_start);
            {
                let result2_guard = result2_input.read().unwrap();

                // Random sleep between 2-4 seconds per batch (while holding lock)
                let sleep_ms = 2000 + (rand::random::<u64>() % 2001); // 2000-4000ms
                thread::sleep(Duration::from_millis(sleep_ms));

                // Process elements directly with references (no copying!)
                for i in batch_start..batch_end {
                    let result_string = &result2_guard[i]; // ← Reference, no copy!
                    // Parse the string format: "chunk_{}_first_char_{}_count_{}"
                    let parts: Vec<&str> = result_string.split('_').collect();
                    if parts.len() >= 6 {
                        if let Ok(_char_count) = parts[5].parse::<usize>() {
                            let first_char = parts[4].chars().next().unwrap_or('-');
                            let char_category = match first_char {
                                'a'..='z' => "lowercase",
                                'A'..='Z' => "uppercase",
                                '0'..='9' => "numeric",
                                _ => "special",
                            };
                            batch_results.push(format!(
                                "{}_categorized_as_{}",
                                result_string, char_category
                            ));
                        } else {
                            batch_results.push(format!("{}_count_parse_failed", result_string));
                        }
                    } else {
                        batch_results.push(format!("{}_format_parse_failed", result_string));
                    }
                }
                {
                    let mut result4_guard = result4_output.write().unwrap();
                    result4_guard.extend(batch_results);
                }
            } // Lock released here

            // Write batch to result4

            total_processed += batch_end - batch_start;
            println!(
                "Thread 4: processed {}/{} character strings (batch size: {}, zero-copy refs)",
                total_processed,
                total_entries,
                batch_end - batch_start
            );
        }

        println!("Thread 4 completed - processed character analysis");
    });
    phase2_threads.push(t4);

    // Wait for phase 2 threads to complete
    println!(
        "Phase 2: Waiting for {} processing threads to complete...",
        phase2_threads.len()
    );
    for (i, thread) in phase2_threads.into_iter().enumerate() {
        thread.join().unwrap();
        println!("Thread {} completed!", i + 1);
    }

    println!("\nAll phases completed!");
}
