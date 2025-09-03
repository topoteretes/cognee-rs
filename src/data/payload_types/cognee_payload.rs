use crate::data::payload_base::PayloadBase;
use crate::data::traits::PayloadBehavior;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct CogneePayload {
    base: Arc<RwLock<PayloadBase>>,
    pub chunks: Arc<RwLock<Vec<String>>>,
    pub results: Arc<RwLock<Vec<String>>>,
}

impl CogneePayload {
    pub fn new(chunks: Vec<String>) -> Self {
        Self {
            base: Arc::new(RwLock::new(PayloadBase::new())),
            chunks: Arc::new(RwLock::new(chunks)),
            results: Arc::new(RwLock::new(Vec::new())),
        }
    }

    // Safe access methods for concurrent use
    pub fn read_chunks<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&Vec<String>) -> R,
    {
        let chunks = self.chunks.read().unwrap();

        f(&chunks)
    }

    pub fn write_chunks<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut Vec<String>) -> R,
    {
        let mut chunks = self.chunks.write().unwrap();
        f(&mut chunks)
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

    // Safe access methods for results
    pub fn read_results<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&Vec<String>) -> R,
    {
        let results = self.results.read().unwrap();
        f(&results)
    }

    pub fn write_results<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut Vec<String>) -> R,
    {
        let mut results = self.results.write().unwrap();
        f(&mut results)
    }
}

impl PayloadBehavior for CogneePayload {
    fn id(&self) -> Uuid {
        self.read_base(|base| base.metainfo.id)
    }

    fn task_done(&mut self) {
        self.write_base(|base| base.metainfo.task_done());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_cognee_payload() {
        let chunks = vec![
            "This is the first chunk of text".to_string(),
            "Here is another chunk with different content".to_string(),
            "And a third chunk to test with".to_string(),
        ];

        let payload = CogneePayload::new(chunks.clone());

        assert!(!payload.id().is_nil(), "Payload should have a valid UUID");

        payload.read_chunks(|payload_chunks| {
            assert_eq!(payload_chunks.len(), 3, "Should have 3 chunks");
            assert_eq!(payload_chunks[0], "This is the first chunk of text");
            assert_eq!(
                payload_chunks[1],
                "Here is another chunk with different content"
            );
            assert_eq!(payload_chunks[2], "And a third chunk to test with");
        });

        payload.read_base(|base| {
            assert_eq!(
                base.metainfo.task_counter, 0,
                "Initial task counter should be 0"
            );
            assert!(!base.metainfo.id.is_nil(), "Should have a valid UUID");
        });
    }

    #[test]
    fn test_cognee_payload_task_done() {
        let chunks = vec!["test chunk".to_string()];
        let mut payload = CogneePayload::new(chunks);

        payload.read_base(|base| {
            assert_eq!(base.metainfo.task_counter, 0);
        });

        payload.task_done();

        payload.read_base(|base| {
            assert_eq!(base.metainfo.task_counter, 1);
        });
    }

    #[test]
    fn test_cognee_payload_modify_chunks() {
        let initial_chunks = vec!["chunk1".to_string(), "chunk2".to_string()];
        let payload = CogneePayload::new(initial_chunks);

        payload.write_chunks(|chunks| {
            chunks.push("chunk3".to_string());
        });

        payload.read_chunks(|chunks| {
            assert_eq!(chunks.len(), 3);
            assert_eq!(chunks[2], "chunk3");
        });

        payload.write_chunks(|chunks| {
            chunks[0] = "modified_chunk1".to_string();
        });

        payload.read_chunks(|chunks| {
            assert_eq!(chunks[0], "modified_chunk1");
        });
    }

    #[test]
    fn test_cognee_payload_multithreaded_copy() {
        use rand::Rng;
        use std::thread;
        use std::time::Duration;
        let initial_chunks: Vec<String> = (0..5)
            .map(|i| {
                // make a 1 MB string (1_000_000 bytes) by repeating
                let base = format!("chunk_{i}_");
                base.repeat(1_000_000 / base.len())
            })
            .collect();
        let payload = Arc::new(CogneePayload::new(initial_chunks));
        let mut handles = Vec::new();

        for worker_id in 0..10 {
            let p = Arc::clone(&payload);
            handles.push(thread::spawn(move || {
                println!("Task started worker {}", worker_id);
                // Copying the whole property
                let chunks: Vec<String> = p.read_chunks(|v| v.clone());
                println!("Chunks cloned {}", worker_id);
                for ch in chunks {
                    let mut rng = rand::thread_rng();
                    let secs = rng.gen_range(2..=4); // random int in [2, 8]
                    thread::sleep(Duration::from_secs(secs));
                    let out = format!("worker_{worker_id} processed {ch}");

                    p.write_results(|r| r.push(out));
                }
                println!("Task started ended {}", worker_id);
            }));
        }

        for h in handles {
            h.join().unwrap();
        }
    }

    #[test]
    fn test_cognee_payload_results() {
        let chunks = vec!["test chunk".to_string()];
        let payload = CogneePayload::new(chunks);

        payload.read_results(|results| {
            assert_eq!(results.len(), 0, "Results should start empty");
        });

        payload.write_results(|results| {
            results.push("result1".to_string());
            results.push("result2".to_string());
        });

        payload.read_results(|results| {
            assert_eq!(results.len(), 2);
            assert_eq!(results[0], "result1");
            assert_eq!(results[1], "result2");
        });

        payload.write_results(|results| {
            results[0] = "modified_result1".to_string();
        });

        payload.read_results(|results| {
            assert_eq!(results[0], "modified_result1");
            assert_eq!(results[1], "result2");
        });
    }
}
