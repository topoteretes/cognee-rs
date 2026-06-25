# Cognee Utils

Shared utilities for the Cognee Rust codebase.

## Features

### Retry Logic

Generic retry implementation with exponential backoff that allows custom error-based retry decisions.

```rust
use cognee_utils::retry::{retry_with_backoff, RetryConfig, RetryDecision};

#[derive(Debug)]
enum MyError {
    Transient(String),
    Permanent(String),
}

let config = RetryConfig::new(3, 100, 5000);

let result = retry_with_backoff(
    config,
    || async {
        // Your async operation here
        fetch_data().await
    },
    |error| {
        match error {
            MyError::Transient(_) => RetryDecision::Retry,
            MyError::Permanent(_) => RetryDecision::Abort,
        }
    },
).await;
```

### Configuration Options

- `max_retries`: Maximum number of retry attempts
- `initial_delay_ms`: Initial delay before first retry
- `max_delay_ms`: Maximum delay cap
- `backoff_multiplier`: Exponential backoff multiplier (default: 2.0)
- `jitter_factor`: Optional randomization factor (0.0 to 1.0)

### ID Generation

Deterministic UUID v5 generation (content-addressed) and name normalization,
shared across the codebase via the `NAMESPACE_OID` constant.

```rust
use cognee_utils::{generate_node_id, generate_edge_name, generate_node_name};

// Same normalized input → same UUID (lowercase, spaces → underscores, drop apostrophes)
let id = generate_node_id("Alice Smith"); // UUID v5 of "alice_smith"

// Edge names normalize like node IDs but return the string label
assert_eq!(generate_edge_name("Works At"), "works_at");

// Node names normalize for display (lowercase, drop apostrophes, keep spaces)
assert_eq!(generate_node_name("Alice Smith"), "alice smith");
```

### Secret Redaction

`redact` mirrors Python's `redact_secrets`: it masks OpenAI-style keys,
`api_key=`/`api-key=`, `Bearer <token>`, and `password=` values, keeping the
first 6 characters and replacing the rest with `***REDACTED***`. Returns a
`Cow<str>` to avoid allocating when there is nothing to redact.

```rust
use cognee_utils::redact;

let safe = redact("Authorization: Bearer sk-secret-token");
```

### Tracing Attribute Keys

`tracing_keys` exposes shared `cognee.*` span-attribute key constants (e.g.
`COGNEE_LLM_MODEL`, `COGNEE_SEARCH_TYPE`, `COGNEE_PIPELINE_NAME`) so
instrumentation across crates uses consistent attribute names.
