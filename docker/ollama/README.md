# Ollama Docker Setup

Docker configuration for running Ollama with OpenAI-compatible API for local LLM inference.

## Features

- **Ollama service** with OpenAI-compatible API endpoints
- **Pre-configured model set** automatically pulled on first start
  - Primary model (`MODEL_NAME`): `llama3.2:3b`
  - Additional stronger model (`MODEL_NAMES`): `llama3.1:8b`
- **GPU support** for NVIDIA and AMD (optional)
- **Persistent storage** for models and data
- **Simple single-container deployment**

## Quick Start

### Using the start script (Recommended)

```bash
./start.sh
```

This will build the image (if needed), start the container, and verify it's working.

### Manual Docker Commands

1. Build the image:
   ```bash
   docker build -t ollama-cognee .
   ```

2. Run the container:
   ```bash
   docker run -d \
     --name ollama \
     -p 11435:11434 \
     -v ollama_data:/root/.ollama \
     -e MODEL_NAME=llama3.2:3b \
    -e MODEL_NAMES=llama3.2:3b,llama3.1:8b \
     ollama-cognee
   ```

3. With GPU support (NVIDIA):
   ```bash
   docker run -d \
     --gpus all \
     --name ollama \
     -p 11435:11434 \
     -v ollama_data:/root/.ollama \
     -e MODEL_NAME=llama3.2:3b \
    -e MODEL_NAMES=llama3.2:3b,llama3.1:8b \
     ollama-cognee
   ```

## API Endpoints

### Ollama Native API

- **Base URL**: `http://localhost:11435`
- **List models**: `GET /api/tags`
- **Generate**: `POST /api/generate`
- **Chat**: `POST /api/chat`

### OpenAI-Compatible API

Ollama provides OpenAI-compatible endpoints at:

- **Base URL**: `http://localhost:11435/v1`
- **Chat Completions**: `POST /v1/chat/completions`
- **Completions**: `POST /v1/completions`
- **Embeddings**: `POST /v1/embeddings`

### Example: Using OpenAI-Compatible API

```bash
curl http://localhost:11435/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "llama3.2:3b",
    "messages": [
      {
        "role": "system",
        "content": "You are a helpful assistant."
      },
      {
        "role": "user",
        "content": "What is the capital of France?"
      }
    ],
    "temperature": 0.7
  }'
```

### Example: Using with cognee-llm

```rust
use cognee_llm::{LlmConfig, LlmProvider};

let config = LlmConfig::new(LlmProvider::Ollama, "llama3.2:3b")
    .with_endpoint("http://localhost:11435/v1")
    .with_temperature(0.0)
    .with_max_tokens(2048);
```

## Available Models

Common models you can use:
Stop the container and restart with a new `MODEL_NAME` or `MODEL_NAMES`:

```bash
docker stop ollama
docker rm ollama
MODEL_NAME=mistral:7b ./start.sh
```

Or pull multiple models (with a stronger model included):

```bash
docker stop ollama
docker rm ollama
MODEL_NAME=llama3.2:3b MODEL_NAMES=llama3.2:3b,llama3.1:8b,qwen2.5:7b ./start.sh
```

Or manually:
```bash
docker run -d \
  --name ollama \
  -p 11434:11434 \
  -v ollama_data:/root/.ollama \
  -e MODEL_NAME=mistral:7b \
  ollama-cognee
```bash
docker-compose down
MODEL_NAME=mistral:7b docker-compose up -d
```

Or edit `docker-compose.yml`:
```yaml
services:
  ollama:
    environment:
      - MODEL_NAME=mistral:7b
```

### Pull Additional Models

```bash
docker exec -it ollama ollama pull llama3.1:8b
docker exec -it ollama ollama list  # List all models
```

## GPU Support

### NVIDIA GPU
Run with GPU support:
   ```bash
   docker run -d \
     --gpus all \
     --name ollama \
     -p 11435:11434 \
     -v ollama_data:/root/.ollama \
     ollama-cognee
   ```

   The `start.sh` script automatically detects NVIDIA GPUs and enables support.

### AMD GPU

Use the base Ollama image with ROCm tag:
```bash
docker run -d \
  --gpus all \
  --name ollama \
  -p 11435:11434 \
  -v ollama_data:/root/.ollama \
  ollama/ollama:rocm
```
- Model management
- Prompt library
- Multi-user support

## Management Commands

```bash
# Start services
docker-compose up -d

# Stop services
docker-compose down

# View logs
docker-compose logs -f ollama

# Restart services
docker-compose restart

# List running models
docker exec ollama ollama list

# Pull a new model
docker econtainer
./start.sh

# Stop container
docker stop ollama

# View logs
docker logs -f ollama

# Restart container
docker restart ollama

# List running models
docker exec ollama ollama list

# Pull a new model
docker exec ollama ollama pull llama3.1:8b

# Run a model directly
docker exec -it ollama ollama run llama3.2:3b

# Remove container
docker rm -f ollama

# Remove container and volume
docker rm -f ollama
docker volume rm ollama_data
- Use a smaller model (e.g., `llama3.2:1b`)
- Increase Docker memory limits
- Enable GPU support

### Check Ollama health
```bash
curl http://localhost:11434/api/tags
```

## Integration with Cognee
 restart ollama
docker running, you can use it with Cognee's LLM abstraction:

```rust
use cognee_llm::{Llm, LlmConfig, LlmProvider, Message};

// Configure Ollama client
let config = LlmConfig::new(LlmProvider::Ollama, "llama3.2:3b")
    .with_endpoint("http://localhost:11435/v1")
    .with_temperature(0.0);

// Create LLM client (implement OllamaAdapter)
let llm = OllamaAdapter::new(config)?;

// Use for structured output
let graph = llm.create_structured_output(
    "Extract entities from: Alice told Bob about the meeting.",
    "Extract a knowledge graph with nodes and edges.",
    None,
).await?;
```

## Performance Tips

1. **Use appropriate model size** for your hardware
2. **Enable GPU** if available (10-100x faster)
3. **Keep models loaded** (first request is slower)
4. **Adjust context length** with `num_ctx` parameter
5. **Use quantized models** for better performance

## Resources

- [Ollama Documentation](https://github.com/ollama/ollama)
- [Ollama Model Library](https://ollama.com/library)
- [OpenAI API Compatibility](https://github.com/ollama/ollama/blob/main/docs/openai.md)
- [Docker Hub: ollama/ollama](https://hub.docker.com/r/ollama/ollama)
