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
