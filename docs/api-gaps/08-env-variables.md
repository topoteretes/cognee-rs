# Gap 8: Environment Variable Coverage

**Status: Implemented**

This document maps every environment variable used by the Python SDK to its Rust equivalent, identifying gaps.

For the implementation plan, see [impl/08-env-variables-plan.md](impl/08-env-variables-plan.md).

---

## Status Legend

| Status | Meaning |
|--------|---------|
| **Same** | Env var name matches Python; read in `crates/lib/src/config.rs` |
| **Renamed** | Functionally equivalent but different env var name in Rust |
| **Partial** | Read in a crate-local config (e.g. embedding, chunking, llm) but NOT in central `Settings` |
| **Missing** | Not read anywhere in Rust |

---

## Mapping Table

### Core LLM Configuration

| Python Env Var | Rust Equivalent | Status | Notes |
|----------------|-----------------|--------|-------|
| `LLM_PROVIDER` | `LLM_PROVIDER` | **Same** | In lib/config.rs |
| `LLM_MODEL` | `LLM_MODEL` (alias: `OPENAI_MODEL`) | **Same** | Rust has backward-compat alias |
| `LLM_API_KEY` | `LLM_API_KEY` (alias: `OPENAI_TOKEN`) | **Same** | Rust has backward-compat alias |
| `LLM_ENDPOINT` | `LLM_ENDPOINT` (alias: `OPENAI_URL`) | **Same** | Rust has backward-compat alias |
| `LLM_API_VERSION` | `LLM_API_VERSION` | **Same** | |
| `LLM_TEMPERATURE` | `LLM_TEMPERATURE` | **Same** | |
| `LLM_MAX_COMPLETION_TOKENS` | `LLM_MAX_TOKENS` | **Renamed** | Python field: `llm_max_completion_tokens`; Rust reads `LLM_MAX_TOKENS` |
| `LLM_STREAMING` | — | **Missing** | `Settings.llm_streaming` field exists but no env var read in `overlay_from_env()` |
| `LLM_RATE_LIMIT_ENABLED` | — | **Missing** | |
| `LLM_RATE_LIMIT_REQUESTS` | — | **Missing** | |
| `LLM_RATE_LIMIT_INTERVAL` | — | **Missing** | |
| `LLM_RATE_LIMIT_TOKENS` | — | **Missing** | |
| `LLM_INSTRUCTOR_MODE` | — | **Missing** | Python instructor framework config |
| `STRUCTURED_OUTPUT_FRAMEWORK` | — | **Missing** | instructor/baml selection (Python-specific) |

### BAML Framework (Python-specific)

| Python Env Var | Rust Equivalent | Status | Notes |
|----------------|-----------------|--------|-------|
| `BAML_LLM_PROVIDER` | — | **Missing** | BAML not planned for Rust |
| `BAML_LLM_MODEL` | — | **Missing** | |
| `BAML_LLM_ENDPOINT` | — | **Missing** | |
| `BAML_LLM_API_KEY` | — | **Missing** | |
| `BAML_LLM_TEMPERATURE` | — | **Missing** | |
| `BAML_LLM_API_VERSION` | — | **Missing** | |

### Embedding Configuration

| Python Env Var | Rust Equivalent | Status | Notes |
|----------------|-----------------|--------|-------|
| `EMBEDDING_PROVIDER` | `EMBEDDING_PROVIDER` | **Partial** | In embedding/config.rs and chunking/config.rs; NOT in lib/config.rs |
| `EMBEDDING_MODEL` | `EMBEDDING_MODEL` | **Same** | In lib/config.rs AND embedding/config.rs |
| `EMBEDDING_DIMENSIONS` | `EMBEDDING_DIMENSIONS` | **Same** | In lib/config.rs AND embedding/config.rs |
| `EMBEDDING_API_KEY` | `EMBEDDING_API_KEY` | **Partial** | In embedding/config.rs only (fallback: `LLM_API_KEY`) |
| `EMBEDDING_ENDPOINT` | `EMBEDDING_ENDPOINT` | **Partial** | In embedding/config.rs only |
| `EMBEDDING_API_VERSION` | `EMBEDDING_API_VERSION` | **Partial** | In embedding/config.rs only |
| `EMBEDDING_MAX_COMPLETION_TOKENS` | `EMBEDDING_MAX_COMPLETION_TOKENS` | **Partial** | In embedding/config.rs only |
| `EMBEDDING_BATCH_SIZE` | `EMBEDDING_BATCH_SIZE` | **Same** | In lib/config.rs AND embedding/config.rs |
| `EMBEDDING_RATE_LIMIT_ENABLED` | — | **Missing** | |
| `EMBEDDING_RATE_LIMIT_REQUESTS` | — | **Missing** | |
| `EMBEDDING_RATE_LIMIT_INTERVAL` | — | **Missing** | |
| `EMBEDDING_RATE_LIMIT_TOKENS` | — | **Missing** | |
| `HUGGINGFACE_TOKENIZER` | `HUGGINGFACE_TOKENIZER` | **Partial** | In embedding/config.rs and chunking/config.rs; NOT in lib/config.rs |

### Graph Database Configuration

| Python Env Var | Rust Equivalent | Status | Notes |
|----------------|-----------------|--------|-------|
| `GRAPH_DATABASE_PROVIDER` | `GRAPH_DATABASE_PROVIDER` | **Same** | |
| `GRAPH_DATABASE_URL` | `GRAPH_DATABASE_URL` | **Same** | |
| `GRAPH_DATABASE_NAME` | `GRAPH_DATABASE_NAME` | **Same** | |
| `GRAPH_DATABASE_USERNAME` | `GRAPH_DATABASE_USERNAME` | **Same** | |
| `GRAPH_DATABASE_PASSWORD` | `GRAPH_DATABASE_PASSWORD` | **Same** | |
| `GRAPH_DATABASE_PORT` | `GRAPH_DATABASE_PORT` | **Same** | |
| `GRAPH_DATABASE_KEY` | `GRAPH_DATABASE_KEY` | **Same** | |
| `GRAPH_FILE_PATH` | `GRAPH_FILE_PATH` | **Same** | |
| `GRAPH_DATABASE_ALLOW_ANONYMOUS` | — | **Missing** | Neo4j anonymous auth toggle |

### Vector Database Configuration

| Python Env Var | Rust Equivalent | Status | Notes |
|----------------|-----------------|--------|-------|
| `VECTOR_DB_PROVIDER` | `VECTOR_DB_PROVIDER` | **Same** | |
| `VECTOR_DB_URL` | `VECTOR_DB_URL` | **Same** | |
| `VECTOR_DB_PORT` | `VECTOR_DB_PORT` | **Same** | |
| `VECTOR_DB_NAME` | `VECTOR_DB_NAME` | **Same** | |
| `VECTOR_DB_KEY` | `VECTOR_DB_KEY` | **Same** | |
| `VECTOR_DB_USERNAME` | — | **Missing** | |
| `VECTOR_DB_PASSWORD` | — | **Missing** | |
| `VECTOR_DB_HOST` | — | **Missing** | |

### Relational Database Configuration

| Python Env Var | Rust Equivalent | Status | Notes |
|----------------|-----------------|--------|-------|
| `DB_PROVIDER` | `DB_PROVIDER` | **Same** | |
| `DB_HOST` | `DB_HOST` | **Same** | |
| `DB_PORT` | `DB_PORT` | **Same** | |
| `DB_NAME` | `DB_NAME` | **Same** | |
| `DB_USERNAME` | `DB_USERNAME` | **Same** | |
| `DB_PASSWORD` | `DB_PASSWORD` | **Same** | |
| `DATABASE_URL` | `DATABASE_URL` | **Same** | |

### Session / Cache Configuration

| Python Env Var | Rust Equivalent | Status | Notes |
|----------------|-----------------|--------|-------|
| `CACHE_BACKEND` | — | **Missing** | Session backend selection (redis/fs) |
| `CACHING` | — | **Missing** | Enable caching toggle |
| `AUTO_FEEDBACK` | — | **Missing** | Auto-feedback detection |
| `CACHE_HOST` | — | **Missing** | Redis host |
| `CACHE_PORT` | — | **Missing** | Redis port |
| `CACHE_USERNAME` | — | **Missing** | Redis auth |
| `CACHE_PASSWORD` | — | **Missing** | Redis auth |
| `SESSION_TTL_SECONDS` | — | **Missing** | Session expiry (default 7 days) |
| `MAX_SESSION_CONTEXT_CHARS` | — | **Missing** | Max session context size |
| `USAGE_LOGGING` | — | **Missing** | Usage logging feature |
| `USAGE_LOGGING_TTL` | — | **Missing** | Usage log expiry |

### System / Storage

| Python Env Var | Rust Equivalent | Status | Notes |
|----------------|-----------------|--------|-------|
| `COGNEE_SYSTEM_ROOT_DIRECTORY` | `COGNEE_SYSTEM_ROOT_DIRECTORY` | **Same** | |
| `COGNEE_DATA_ROOT_DIRECTORY` | `COGNEE_DATA_ROOT_DIRECTORY` | **Same** | |
| `COGNEE_DEFAULT_DATASET_NAME` | `COGNEE_DEFAULT_DATASET_NAME` | **Same** | |
| `COGNEE_DEFAULT_USER_ID` | `COGNEE_DEFAULT_USER_ID` | **Same** | |
| `CACHE_ROOT_DIRECTORY` | — | **Missing** | `Settings.cache_root_directory` field exists but no env var read |
| `STORAGE_BACKEND` | — | **Missing** | local/s3 selection |
| `STORAGE_BUCKET_NAME` | — | **Missing** | S3 bucket name |

### AWS / S3

| Python Env Var | Rust Equivalent | Status | Notes |
|----------------|-----------------|--------|-------|
| `AWS_REGION` | — | **Missing** | AWS region |
| `AWS_ENDPOINT_URL` | — | **Missing** | Custom S3 endpoint |
| `AWS_ACCESS_KEY_ID` | — | **Missing** | AWS auth |
| `AWS_SECRET_ACCESS_KEY` | — | **Missing** | AWS auth |
| `AWS_SESSION_TOKEN` | — | **Missing** | AWS session |
| `AWS_PROFILE_NAME` | — | **Missing** | AWS profile |
| `AWS_BEDROCK_RUNTIME_ENDPOINT` | — | **Missing** | Bedrock endpoint |

### Ontology

| Python Env Var | Rust Equivalent | Status | Notes |
|----------------|-----------------|--------|-------|
| `ONTOLOGY_FILE_PATH` | `ONTOLOGY_FILE_PATH` | **Same** | |
| `ONTOLOGY_RESOLVER` | `ONTOLOGY_RESOLVER` | **Same** | |
| `ONTOLOGY_MATCHING_STRATEGY` | `ONTOLOGY_MATCHING_STRATEGY` | **Same** | |

### Observability / Tracing

| Python Env Var | Rust Equivalent | Status | Notes |
|----------------|-----------------|--------|-------|
| `COGNEE_TRACING_ENABLED` | — | **Missing** | OpenTelemetry toggle |
| `OTEL_SERVICE_NAME` | — | **Missing** | OTEL service name |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | — | **Missing** | OTLP endpoint |
| `OTEL_EXPORTER_OTLP_HEADERS` | — | **Missing** | OTLP headers |
| `LANGFUSE_PUBLIC_KEY` | — | **Missing** | Langfuse integration |
| `LANGFUSE_SECRET_KEY` | — | **Missing** | |
| `LANGFUSE_HOST` | — | **Missing** | |

### Authentication (Library-Level)

| Python Env Var | Rust Equivalent | Status | Notes |
|----------------|-----------------|--------|-------|
| `DEFAULT_USER_EMAIL` | — | **Missing** | Default user email |
| `DEFAULT_USER_PASSWORD` | — | **Missing** | Default user password |
| `ENABLE_BACKEND_ACCESS_CONTROL` | — | **Missing** | ACL toggle |

### Logging

| Python Env Var | Rust Equivalent | Status | Notes |
|----------------|-----------------|--------|-------|
| `LOG_LEVEL` | — | **Missing** | Rust uses `tracing` env filter instead |
| `COGNEE_LOGS_DIR` | — | **Missing** | `Settings.logs_root_directory` field exists but no env var read |
| `COGNEE_LOG_FILE` | — | **Missing** | File logging toggle |
| `COGNEE_LOG_MAX_BYTES` | — | **Missing** | Log rotation config |
| `COGNEE_LOG_BACKUP_COUNT` | — | **Missing** | Log rotation config |

### Web Scraper

| Python Env Var | Rust Equivalent | Status | Notes |
|----------------|-----------------|--------|-------|
| `TAVILY_API_KEY` | — | **Missing** | Tavily web search API |
| `WEB_SCRAPER_TIMEOUT` | — | **Missing** | Scraper timeout |
| `WEB_SCRAPER_MAX_DELAY` | — | **Missing** | Max crawl delay |

### Feature Flags

| Python Env Var | Rust Equivalent | Status | Notes |
|----------------|-----------------|--------|-------|
| `ENABLE_LAST_ACCESSED` | — | **Missing** | Track last accessed timestamps |
| `MOCK_CODE_SUMMARY` | — | **Missing** | Testing flag |

### Rust-Only Env Vars (not in Python)

These env vars are read in Rust but have no Python equivalent:

| Rust Env Var | Crate | Notes |
|---|---|---|
| `OPENAI_TOKEN` | lib/config.rs | Legacy alias for `LLM_API_KEY` |
| `OPENAI_URL` | lib/config.rs | Legacy alias for `LLM_ENDPOINT` |
| `OPENAI_MODEL` | lib/config.rs | Legacy alias for `LLM_MODEL` |
| `LLM_MAX_TOKENS` | lib/config.rs | Legacy alias for `LLM_MAX_COMPLETION_TOKENS` |
| `LLM_MAX_RETRIES` | lib/config.rs | Not in Python config |
| `LLM_MAX_PARALLEL_REQUESTS` | lib/config.rs | Not in Python config |
| `EMBEDDING_MAX_SEQUENCE_LENGTH` | lib/config.rs | Not in Python config |
| `EMBEDDING_MODEL_PATH` | lib/config.rs | ONNX model path (Rust ONNX-specific) |
| `EMBEDDING_TOKENIZER_PATH` | lib/config.rs | ONNX tokenizer path (Rust ONNX-specific) |
| `COGNEE_E2E_EMBED_MODEL_PATH` | lib/config.rs | Test alias for `EMBEDDING_MODEL_PATH` |
| `COGNEE_E2E_TOKENIZER_PATH` | lib/config.rs | Test alias for `EMBEDDING_TOKENIZER_PATH` |
| `MOCK_EMBEDDING` | embedding/config.rs | Force zero-vector embeddings for testing |
| `COGNEE_TOKEN_COUNTER` | chunking/config.rs | Explicit tokenizer selection override |
| `COGNEE_DEBUG_LLM_REQUEST` | llm/adapters/openai.rs | Debug toggle for HTTP request logging |
| `TRANSCRIPTION_MODEL` | llm/adapters/openai.rs | Whisper model name override |
| `LLM_VISION_MODEL` | llm/adapters/openai.rs | Vision model name override |

---

## Summary Statistics

| Category | Total Python Vars | Covered in Rust | Missing | Coverage |
|----------|-------------------|-----------------|---------|----------|
| **Core LLM** | 14 | 8 | 6 | 57% |
| **BAML** | 6 | 0 | 6 | 0% |
| **Embedding** | 13 | 8 | 5 | 62% |
| **Graph DB** | 9 | 8 | 1 | 89% |
| **Vector DB** | 8 | 5 | 3 | 63% |
| **Relational DB** | 7 | 7 | 0 | 100% |
| **Session/Cache** | 11 | 0 | 11 | 0% |
| **System/Storage** | 7 | 4 | 3 | 57% |
| **AWS/S3** | 7 | 0 | 7 | 0% |
| **Ontology** | 3 | 3 | 0 | 100% |
| **Observability** | 7 | 0 | 7 | 0% |
| **Authentication** | 3 | 0 | 3 | 0% |
| **Logging** | 5 | 0 | 5 | 0% |
| **Web Scraper** | 3 | 0 | 3 | 0% |
| **Feature Flags** | 2 | 0 | 2 | 0% |
| **Total** | **105** | **43** | **62** | **41%** |

Notes on counting:
- "Covered" includes Same, Renamed, and Partial statuses (env var is read somewhere in Rust)
- Embedding section has 13 vars (was 12; added `EMBEDDING_RATE_LIMIT_TOKENS` per Python source)
- Core LLM coverage increased from 7 to 8 (counting `LLM_MAX_COMPLETION_TOKENS` as Renamed/covered)
- Embedding coverage increased from 5 to 8 (corrected: `EMBEDDING_API_VERSION`, `EMBEDDING_MAX_COMPLETION_TOKENS`, `HUGGINGFACE_TOKENIZER` are Partial, not Missing)
