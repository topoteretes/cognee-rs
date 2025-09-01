//! On-device AI memory (Rust library)
//!
//! This is just a stub version to get the project compiling and usable.

/// Say hello from the library.
pub fn hello() {
    println!("Hello from your Rust library!");
}

/// Placeholder for an on-device embedding step.
pub fn embed_stub(text: &str) {
    println!("Embedding text: {text}");
}

/// Placeholder for storing a memory item.
pub fn store_stub(id: &str, content: &str) {
    println!("Storing memory: id={id}, content={content}");
}

/// Placeholder for a retrieval call.
pub fn retrieve_stub(query: &str) {
    println!("Retrieving with query: {query}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_prints() {
        hello();
    }

    #[test]
    fn retrieve_prints() {
        retrieve_stub("hello");
    }
}
