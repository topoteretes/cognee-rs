# Docker Configurations

This directory contains Docker setups for various services used by Cognee.

## Available Services

### [Ollama](./ollama/)

Local LLM inference with OpenAI-compatible API.

**Quick Start:**
```bash
cd ollama
./start.sh
```

**Features:**
- OpenAI-compatible API at `http://localhost:11435/v1/chat/completions`
- Automatic model downloading (default: llama3.2:3b)
- GPU support (NVIDIA/AMD)
- Simple single-container deployment

See [ollama/README.md](./ollama/README.md) for detailed documentation.

## General Requirements

- Docker Engine 20.10+
- Docker Compose 2.0+
- For GPU support: NVIDIA Container Toolkit or AMD ROCm

## Usage

Each service has its own directory with:
- `docker-compose.yml` - Service configuration
- `README.md` - Detailed documentation
- `start.sh` - Quick start script
- `.env.example` - Environment variable template

Navigate to the service directory and follow its README for setup instructions.
