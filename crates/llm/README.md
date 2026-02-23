# Cognee LLM

LLM abstraction layer for Cognee with support for structured output generation.

## Features

- **Async-first**: All operations are async, supporting both API calls and local inference
- **Structured outputs**: Generate type-safe structured data (e.g., knowledge graphs) from text
- **JSON Schema generation**: Automatic schema generation from Rust types using `schemars`
- **Provider-agnostic**: Trait-based design supports OpenAI, Anthropic, Ollama, local models, etc.
- **Configuration**: Flexible configuration with sensible defaults

## Usage

### OpenAI Adapter

```rust
use cognee_llm::{Llm, OpenAIAdapter, GenerationOptions};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, JsonSchema)]
struct KnowledgeGraph {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
}

// Create OpenAI adapter
let llm = OpenAIAdapter::new(
    "gpt-4",
    "sk-...",  // Your API key
    None,      // Use default OpenAI base URL
)?;

// Generate structured output
let graph: KnowledgeGraph = llm.create_structured_output(
    "Alice told Bob to bring documents.",
    "Extract a knowledge graph with nodes and edges.",
    Some(GenerationOptions {
        temperature: Some(0.0),
        max_tokens: Some(1000),
        ..Default::default()
    }),
).await?;
```

### Custom Base URL (for OpenAI-compatible APIs)

```rust
// Use with Ollama, LocalAI, or other OpenAI-compatible services
let llm = OpenAIAdapter::new(
    "llama3.2:3b",
    "not-needed",  // Some services don't require API key
    Some("http://localhost:11435/v1".to_string()),
)?;
```

**Note:** The adapter automatically detects API capabilities:
- **OpenAI/Azure**: Uses function calling for structured outputs (more reliable)
- **Ollama/LocalAI**: Automatically falls back to JSON mode with example-based prompts
- No configuration needed - it just works with both!

### Basic Trait Definition

```rust
use cognee_llm::{Llm, Message, GenerationOptions};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, JsonSchema)]
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

### JSON Schema Generation

The crate automatically generates JSON schemas from your Rust types to guide the LLM:

```rust
use cognee_llm::schema::{generate_json_schema, build_schema_prompt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, JsonSchema)]
struct Person {
    name: String,
    age: u32,
    email: Option<String>,
}

// Generate schema as JSON value
let schema = generate_json_schema::<Person>();

// Or build a complete prompt with schema embedded
let prompt = build_schema_prompt::<Person>(
    "Extract the person's information from the text."
);
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
- **OpenAI adapter**: Production-ready implementation using OpenAI's function calling API
- **JSON Schema generation**: `schemars`-based schema generation from Rust types
- **Schema utilities**: Helper functions to generate schemas and build prompts
- **Configuration types**: `LlmConfig`, `LlmProvider`, `GenerationOptions`
- **Type-safe responses**: Generic over `T: Serialize + DeserializeOwned + JsonSchema`
- **Comprehensive errors**: `LlmError` covers API, network, serialization, rate limit errors

## Implementation Details

### OpenAI Adapter

The `OpenAIAdapter` uses a dual-strategy approach for structured outputs:

**Primary (Function Calling):**
1. **Schema Generation**: Automatically generates JSON schema from your Rust type using `schemars`
2. **Function Definition**: Creates an OpenAI function with the schema as parameters
3. **Forced Execution**: Sets `function_call: {name: "extract_structured_data"}` to force the model to use the function
4. **Validation**: Parses and validates the function call arguments into your type

**Fallback (JSON Mode):**
1. **Automatic Detection**: If function calling isn't supported, automatically switches to JSON mode
2. **Example Generation**: Creates example JSON from the schema (clearer than full schema for LLMs)
3. **Response Format**: Sets `response_format: {"type": "json_object"}` for JSON-only responses
4. **Content Parsing**: Parses the JSON from the response content

This dual approach provides:
- **Universal compatibility**: Works with OpenAI, Azure OpenAI, Ollama, LocalAI, and others
- **High reliability**: Function calling for best results, JSON mode for broad compatibility
- **Type safety**: Compile-time guarantees about response structure
- **Zero configuration**: Automatic detection and fallback

### Adding New Adapters

To add support for other providers:

1. Create a new module in `src/adapters/`
2. Implement the `Llm` trait
3. Use `generate_json_schema::<T>()` to get the schema
4. Adapt the schema to the provider's format (function calling, JSON mode, etc.)
5. Parse the response into type `T`

See `src/adapters/openai.rs` as a reference implementation.

## Testing

### Unit Tests

Run the unit tests:

```bash
cargo test --package cognee-llm --lib
```

### Integration Tests with Ollama

The crate includes integration tests that use a local Ollama instance for realistic testing:

```bash
# Run all integration tests (requires Ollama)
./scripts/run_tests_with_local_env.sh

# Run a specific test
./scripts/run_tests_with_local_env.sh test_entity_extraction
```

The test script automatically:
- Starts the Ollama Docker container if not running
- Waits for the model to be ready
- Sets environment variables (`OPENAI_URL`, `OPENAI_TOKEN`, `OPENAI_MODEL`)
- Runs the tests
- Shows cleanup instructions

**Manual testing:**

```bash
# Set environment variables
export OPENAI_URL="http://localhost:11435/v1"
export OPENAI_TOKEN="not-needed"
export OPENAI_MODEL="llama3.2:3b"

# Run integration tests
cargo test --package cognee-llm --test integration_openai -- --nocapture
```

Tests will automatically skip if environment variables are not set.

## Next Steps

- **Anthropic adapter**: Claude support with prompt caching
- **Ollama native adapter**: Direct HTTP/gRPC without OpenAI compatibility layer
- **ONNX adapter**: Local inference with quantized models
- **Streaming support**: Real-time token streaming for all adapters

## Next Steps

Planned adapters:
- Anthropic adapter (Claude with tool use)
- Ollama adapter (OpenAI-compatible API)
- Local ONNX Runtime adapter (on-device inference)
