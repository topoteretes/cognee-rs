#!/bin/bash
# Quick start script for Ollama

set -e

# Configuration
CONTAINER_NAME="${CONTAINER_NAME:-ollama}"
MODEL_NAME="${MODEL_NAME:-llama3.2:3b}"
MODEL_NAMES="${MODEL_NAMES:-${MODEL_NAME},llama3.1:8b}"
PORT="${PORT:-11435}"
VOLUME_NAME="${VOLUME_NAME:-ollama_data}"
DNS_SERVERS="${DNS_SERVERS:-1.1.1.1,8.8.8.8}"
RECREATE_CONTAINER="${RECREATE_CONTAINER:-0}"

IFS=',' read -ra DNS_ARRAY <<< "$DNS_SERVERS"
DNS_FLAGS=()
for dns in "${DNS_ARRAY[@]}"; do
  dns_trimmed="$(echo "$dns" | xargs)"
  [[ -z "$dns_trimmed" ]] && continue
  DNS_FLAGS+=(--dns "$dns_trimmed")
done

echo "🚀 Starting Ollama with OpenAI-compatible API"
echo ""

# Check if Docker is running
if ! docker info > /dev/null 2>&1; then
  echo "❌ Docker is not running. Please start Docker and try again."
  exit 1
fi

# Check if container already exists
if docker ps -a --format '{{.Names}}' | grep -q "^${CONTAINER_NAME}$"; then
  if [[ "$RECREATE_CONTAINER" == "1" ]]; then
    echo "♻️  Recreating container '$CONTAINER_NAME'..."
    docker rm -f "$CONTAINER_NAME" > /dev/null 2>&1 || true
  fi
fi

if docker ps -a --format '{{.Names}}' | grep -q "^${CONTAINER_NAME}$"; then
  echo "📦 Container '$CONTAINER_NAME' already exists."
  if docker ps --format '{{.Names}}' | grep -q "^${CONTAINER_NAME}$"; then
    echo "✓ Container is already running"
  else
    echo "▶️  Starting existing container..."
    docker start "$CONTAINER_NAME"
  fi
else
  # Build the image if it doesn't exist
  if ! docker images | grep -q "ollama-cognee"; then
    echo "🔨 Building Ollama image..."
    docker build -t ollama-cognee .
  fi

  # Check for GPU support
  GPU_FLAGS=""
  if command -v nvidia-smi &> /dev/null; then
    echo "🎮 NVIDIA GPU detected! Enabling GPU support..."
    GPU_FLAGS="--gpus all"
  fi

  # Start container
  echo "🐳 Starting Ollama container..."
  docker run -d \
    $GPU_FLAGS \
    --name "$CONTAINER_NAME" \
    -p "$PORT:11434" \
    -v "$VOLUME_NAME:/root/.ollama" \
    "${DNS_FLAGS[@]}" \
    -e MODEL_NAME="$MODEL_NAME" \
    -e MODEL_NAMES="$MODEL_NAMES" \
    ollama-cognee
fi

echo ""
echo "⏳ Waiting for Ollama to be ready (this includes model download)..."
for i in {1..180}; do
  if curl -s http://localhost:$PORT/api/tags > /dev/null 2>&1; then
    # Check if model is available
    if docker exec "$CONTAINER_NAME" ollama list 2>/dev/null | grep -q "$MODEL_NAME"; then
      echo "✓ Ollama is ready with model $MODEL_NAME!"
      break
    fi
  fi
  if [ $i -eq 180 ]; then
    echo "❌ Timeout waiting for Ollama. Check logs with: docker logs $CONTAINER_NAME"
    exit 1
  fi
  # Show progress every 10 seconds
  if [ $((i % 5)) -eq 0 ]; then
    echo "   Still initializing... ($i/180 - may be downloading model)"
  fi
  sleep 2
done

echo ""
echo "🎉 Ollama is running!"
echo ""
echo "📍 Endpoints:"
echo "   - Ollama API:        http://localhost:$PORT"
echo "   - OpenAI-compatible: http://localhost:$PORT/v1/chat/completions"
echo "   - Primary model:     $MODEL_NAME"
echo "   - Model set:         $MODEL_NAMES"
echo ""
echo "📚 Useful commands:"
echo "   - View logs:         docker logs -f $CONTAINER_NAME"
echo "   - Stop container:    docker stop $CONTAINER_NAME"
echo "   - Remove container:  docker rm $CONTAINER_NAME"
echo "   - List models:       docker exec $CONTAINER_NAME ollama list"
echo "   - Pull model:        docker exec $CONTAINER_NAME ollama pull <model-name>"
echo "   - Test API:          ./test-api.sh"
echo ""
echo "🧪 Running API tests..."
OLLAMA_URL="http://localhost:$PORT" MODEL="$MODEL_NAME" ./test-api.sh
