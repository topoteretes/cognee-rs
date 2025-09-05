use crate::data::payload_base::PayloadBase;
use crate::data::payloadbehavior::PayloadBehavior;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct CogneePayload<TC, T1, T2>
where
    TC: Clone + Send + Sync,
    T1: Clone + Send + Sync,
    T2: Clone + Send + Sync,
{
    base: Arc<RwLock<PayloadBase>>,
    chunks: Arc<RwLock<Vec<Arc<TC>>>>,
    result1: Arc<RwLock<Vec<Arc<T1>>>>,
    result2: Arc<RwLock<Vec<Arc<T2>>>>,
}

impl<TC, T1, T2> CogneePayload<TC, T1, T2>
where
    TC: Clone + Send + Sync,
    T1: Clone + Send + Sync,
    T2: Clone + Send + Sync,
{
    pub fn new(chunks: Vec<Arc<TC>>) -> Self {
        Self {
            base: Arc::new(RwLock::new(PayloadBase::new())),
            chunks: Arc::new(RwLock::new(chunks)),
            result1: Arc::new(RwLock::new(Vec::new())),
            result2: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub fn chunks_arc(&self) -> Arc<RwLock<Vec<Arc<TC>>>> {
        Arc::clone(&self.chunks)
    }
    pub fn add_chunk(&self, item: Arc<TC>) {
        let mut chunks = self.chunks.write().unwrap();
        chunks.push(item);
    }

    pub fn add_chunks_batch(&self, items: Vec<Arc<TC>>) {
        let mut chunks = self.chunks.write().unwrap();
        chunks.extend(items);
    }

    pub fn get_chunks_copy(&self) -> Vec<Arc<TC>> {
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

    pub fn result1_arc(&self) -> Arc<RwLock<Vec<Arc<T1>>>> {
        Arc::clone(&self.result1)
    }

    pub fn result2_arc(&self) -> Arc<RwLock<Vec<Arc<T2>>>> {
        Arc::clone(&self.result2)
    }

    pub fn add_result1(&self, item: Arc<T1>) {
        let mut result1 = self.result1.write().unwrap();
        result1.push(item);
    }

    pub fn add_result1_batch(&self, items: Vec<Arc<T1>>) {
        let mut result1 = self.result1.write().unwrap();
        result1.extend(items);
    }
    pub fn get_result1_copy(&self) -> Vec<Arc<T1>> {
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

    pub fn add_result2(&self, item: Arc<T2>) {
        let mut result2 = self.result2.write().unwrap();
        result2.push(item);
    }

    pub fn add_result2_batch(&self, items: Vec<Arc<T2>>) {
        let mut result2 = self.result2.write().unwrap();
        result2.extend(items);
    }

    pub fn get_result2_copy(&self) -> Vec<Arc<T2>> {
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

impl<TC, T1, T2> PayloadBehavior for CogneePayload<TC, T1, T2>
where
    TC: Clone + Send + Sync,
    T1: Clone + Send + Sync,
    T2: Clone + Send + Sync,
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
    use std::thread;
    use std::time::Duration;
    let initial_chunks: Vec<Arc<String>> = (0..1023)
        .map(|i| {
            let content = match i % 5 {
                0 => format!("document_text_{:04}_analysis_ready", i),
                1 => format!("embedding_vector_{:04}_processed", i),
                2 => format!("memory_fragment_{:04}_indexed", i),
                3 => format!("knowledge_node_{:04}_connected", i),
                _ => format!("data_segment_{:04}_transformed", i),
            };
            Arc::new(content)
        })
        .collect();

    let payload = Arc::new(CogneePayload::<String, String, String>::new(initial_chunks));

    let chunks_arc = payload.chunks_arc();
    let result1_arc = payload.result1_arc();
    let result2_arc = payload.result2_arc();

    let mut threads = Vec::new();

    // ---- Task 1: process chunks in batches and move to result1 ----
    let result1 = Arc::clone(&result1_arc);
    let chunks_ref = Arc::clone(&chunks_arc);
    let t1 = thread::spawn(move || {
        let total_chunks = {
            let chunks_guard = chunks_ref.read().unwrap();
            chunks_guard.len()
        };
        println!(
            "Task 1 starting - moving {} chunks to result1...",
            total_chunks
        );

        const BATCH_SIZE: usize = 100;
        let mut total_processed = 0;

        for batch_start in (0..total_chunks).step_by(BATCH_SIZE) {
            let batch_end = (batch_start + BATCH_SIZE).min(total_chunks);

            let mut batch_results = Vec::with_capacity(batch_end - batch_start);
            {
                {
                    let chunks_guard = chunks_ref.read().unwrap();
                    for i in batch_start..batch_end {
                        let chunk = Arc::clone(&chunks_guard[i]);
                        batch_results.push(chunk);
                    }
                }
                println!("Batch processing starts");
                let sleep_ms = 1000 + (rand::random::<u64>() % 1001);
                thread::sleep(Duration::from_millis(sleep_ms));
                println!("Batch processing ends");

                {
                    let mut result1_guard = result1.write().unwrap();
                    result1_guard.extend(batch_results);
                }
            }

            total_processed += batch_end - batch_start;
            println!(
                "Task 1: processed {}/{} chunks (batch size: {})",
                total_processed,
                total_chunks,
                batch_end - batch_start
            );
        }

        println!("Task 1 completed - moved chunks to result1");
    });
    threads.push(t1);

    // ---- Task 2: process chunks in batches and move to result2 ----
    let result2 = Arc::clone(&result2_arc);
    let chunks_ref = Arc::clone(&chunks_arc);
    let t2 = thread::spawn(move || {
        let total_chunks = {
            let chunks_guard = chunks_ref.read().unwrap();
            chunks_guard.len()
        };
        println!(
            "Task 2 starting - moving {} chunks to result2...",
            total_chunks
        );

        const BATCH_SIZE: usize = 50;
        let mut total_processed = 0;

        for batch_start in (0..total_chunks).step_by(BATCH_SIZE) {
            let batch_end = (batch_start + BATCH_SIZE).min(total_chunks);

            let mut batch_results = Vec::with_capacity(batch_end - batch_start);
            {
                {
                    let chunks_guard = chunks_ref.read().unwrap();
                    for i in batch_start..batch_end {
                        let chunk = Arc::clone(&chunks_guard[i]);
                        batch_results.push(chunk);
                    }
                }

                println!("Batch processing starts");
                let sleep_ms = 1000 + (rand::random::<u64>() % 1001);
                thread::sleep(Duration::from_millis(sleep_ms));
                println!("Batch processing ends");

                {
                    let mut result2_guard = result2.write().unwrap();
                    result2_guard.extend(batch_results);
                }
            }

            total_processed += batch_end - batch_start;
            println!(
                "Task 2: processed {}/{} chunks (batch size: {})",
                total_processed,
                total_chunks,
                batch_end - batch_start
            );
        }

        println!("Task 2 completed - moved chunks to result2");
    });
    threads.push(t2);

    println!(
        "Phase 1: Waiting for {} initial threads to complete...",
        threads.len()
    );
    for (i, thread) in threads.into_iter().enumerate() {
        thread.join().unwrap();
        println!("Thread {} completed!", i + 1);
    }
    println!("All processing completed!");
}
