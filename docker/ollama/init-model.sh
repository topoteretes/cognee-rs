#!/bin/bash

# Primary model to use for test/api examples
MODEL_NAME="${MODEL_NAME:-llama3.2:3b}"

# Comma-separated models to ensure are available.
# Includes a stronger default model in addition to the primary one.
MODEL_NAMES="${MODEL_NAMES:-${MODEL_NAME},llama3.1:8b}"

IFS=',' read -ra MODEL_ARRAY <<< "$MODEL_NAMES"

trim() {
  local value="$1"
  value="${value#${value%%[![:space:]]*}}"
  value="${value%${value##*[![:space:]]}}"
  echo "$value"
}

model_available() {
  local model="$1"
  ollama list 2>/dev/null | grep -Fq "$model"
}

echo "Starting Ollama service..."
# Start Ollama in the background
ollama serve &
OLLAMA_PID=$!

# Wait for Ollama to be ready
echo "Waiting for Ollama to start..."
sleep 5

# Try to reach the API
for i in {1..20}; do
  if ollama list > /dev/null 2>&1; then
    echo "Ollama is ready!"
    break
  fi
  sleep 1
done

# Check/pull all requested models
echo "Ensuring requested models are available: $MODEL_NAMES"
for raw_model in "${MODEL_ARRAY[@]}"; do
  model="$(trim "$raw_model")"
  [[ -z "$model" ]] && continue

  if model_available "$model"; then
    echo "✓ Model $model is already available"
    continue
  fi

  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "📦 Downloading model: $model"
  echo "   This is a one-time download and may take several minutes..."
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo ""

  if ollama pull "$model"; then
    echo ""
    echo "✓ Model $model downloaded successfully!"
  else
    echo ""
    echo "❌ ERROR: Failed to download model $model"
    echo "   Please check your internet connection and try again."
    exit 1
  fi
done

# Verify primary model is available
echo ""
echo "Verifying primary model availability..."
if model_available "$MODEL_NAME"; then
  echo "✓ Primary model $MODEL_NAME is ready to use"
else
  echo "❌ ERROR: Primary model $MODEL_NAME is not available"
  echo "Available models:"
  ollama list
  exit 1
fi

# Keep the container running by waiting for the Ollama process
echo ""
echo "✓ Ollama is ready with primary model: $MODEL_NAME"
echo "✓ Available model set: $MODEL_NAMES"
echo "✓ OpenAI-compatible API available at: http://localhost:11434/v1/chat/completions"
echo ""
wait $OLLAMA_PID
