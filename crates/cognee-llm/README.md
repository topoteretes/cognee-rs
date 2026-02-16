# Cognee LLM

LLM abstraction layer for Cognee with support for structured output generation.

## Features

- **Async-first**: All operations are async, supporting both API calls and local inference
- **Structured outputs**: Generate type-safe structured data (e.g., knowledge graphs) from text
- **Provider-agnostic**: Trait-based design supports OpenAI, Anthropic, Ollama, local models, etc.
- **Configuration**: Flexible configuration with sensible defaults

## Usage

### Basic Trait Definition

```rust
use cognee_llm::{Llm, Message, GenerationOptions};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct KnowledgeGraph {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
}

// Implement the Llm trait for your provider
let llm: Box<dyn Llm> = ...;

// Generate structured output
let graph: KnowledgeGraph = llm.create_structured_output(
    "Alice told Bob to bring documents.",
    "Extract a knowledge graph with nodes and edges.",
    None,
).await?;
```

### With Retry Logic

```rust
use cognee_llm::{Llm, LlmError};
use cognee_utils::retry::{retry_with_backoff, RetryConfig, RetryDecision};

let retry_config = RetryConfig::new(3, 100, 5000);

let graph: KnowledgeGraph = retry_with_backoff(
    retry_config,
    || llm.create_structured_output(
        "Alice told Bob to bring the documents.",
        "Extract entities and relationships.",
        None,
    ),
    |error| match error {
        LlmError::NetworkError(_) | LlmError::RateLimitExceeded(_) => RetryDecision::Retry,
        LlmError::ContentPolicyViolation(_) => RetryDecision::Abort,
        _ => RetryDecision::Retry,
    },
).await?;
```

## Architecture

The crate provides:

- **`Llm` trait**: Core async trait with structured output generation
- **Configuration types**: `LlmConfig`, `LlmProvider`, `GenerationOptions`
- **Type-safe responses**: Generic over `T: Serialize + DeserializeOwned`
- **Comprehensive errors**: `LlmError` covers API, network, serialization, rate limit errors

## Next Steps

Implement concrete providers:
- OpenAI adapter
- Anthropic adapter
- Local ONNX Runtime adapter
- Ollama adapter
