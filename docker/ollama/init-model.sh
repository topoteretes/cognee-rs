#!/bin/bash

# Default model to pull (can be overridden via MODEL_NAME env var)
MODEL_NAME="${MODEL_NAME:-llama3.2:3b}"

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

# Check if model is already pulled
echo "Checking for model: $MODEL_NAME"
if ollama list 2>/dev/null | grep -q "$MODEL_NAME"; then
  echo "✓ Model $MODEL_NAME is already available"
else
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "📦 Downloading model: $MODEL_NAME"
  echo "   This is a one-time download and may take several minutes..."
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo ""
  
  # Pull the model (this is synchronous and will block until complete)
  if ollama pull "$MODEL_NAME"; then
    echo ""
    echo "✓ Model $MODEL_NAME downloaded successfully!"
  else
    echo ""
    echo "❌ ERROR: Failed to download model $MODEL_NAME"
    echo "   Please check your internet connection and try again."
    exit 1
  fi
fi

# Verify model is available
echo ""
echo "Verifying model availability..."
if ollama list 2>/dev/null | grep -q "$MODEL_NAME"; then
  echo "✓ Model $MODEL_NAME is ready to use"
else
  echo "❌ ERROR: Model $MODEL_NAME is not available"
  echo "Available models:"
  ollama list
  exit 1
fi

# Keep the container running by waiting for the Ollama process
echo ""
echo "✓ Ollama is ready with model: $MODEL_NAME"
echo "✓ OpenAI-compatible API available at: http://localhost:11434/v1/chat/completions"
echo ""
wait $OLLAMA_PID
