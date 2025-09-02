//! On-device AI memory (Rust library)
//! Core data shapes and a stub pipeline-friendly API.

pub mod data;  // <- our payload + types live here

/// Say hello from the library.
pub async fn hello() -> &'static str {
    "Hello from cognee!"
}

/// Placeholder for an on-device embedding step.
pub async fn embed_stub(text: &str) {
    println!("Embedding text: {text}");
}

/// Placeholder for storing a memory item.
pub async fn store_stub(id: &str, content: &str) {
    println!("Storing memory: id={id}, content={content}");
}

/// Placeholder for a retrieval call.
pub async fn retrieve_stub(query: &str) {
    println!("Retrieving with query: {query}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hello_works() {
        assert_eq!(hello().await, "Hello from cognee!");
    }

    #[tokio::test]
    async fn retrieve_runs() {
        retrieve_stub("hello").await;
    }
}
