#!/bin/bash
# Test script for Ollama OpenAI-compatible API

set -e

OLLAMA_URL="${OLLAMA_URL:-http://localhost:11435}"
MODEL="${MODEL:-llama3.2:3b}"

echo "Testing Ollama API at: $OLLAMA_URL"
echo "Using model: $MODEL"
echo ""

# Test 1: Check if Ollama is running
echo "=== Test 1: Health Check ==="
if curl -s "$OLLAMA_URL/api/tags" > /dev/null; then
  echo "✓ Ollama is running"
else
  echo "✗ Ollama is not responding"
  exit 1
fi
echo ""

# Test 2: List available models
echo "=== Test 2: List Models ==="
curl -s "$OLLAMA_URL/api/tags" | jq -r '.models[]?.name' || echo "No models found"
echo ""

# Test 2.5: Check if required model is available
echo "=== Test 2.5: Verify Model Available ==="
if curl -s "$OLLAMA_URL/api/tags" | jq -r '.models[]?.name' | grep -q "$MODEL"; then
  echo "✓ Model $MODEL is available"
else
  echo "✗ Model $MODEL is not found. Available models:"
  curl -s "$OLLAMA_URL/api/tags" | jq -r '.models[]?.name'
  echo ""
  echo "Please wait for the model to finish downloading. Check logs with: docker logs ollama -f"
  exit 1
fi
echo ""

# Test 3: OpenAI-compatible chat completion
echo "=== Test 3: Chat Completion (OpenAI-compatible) ==="
response=$(curl -s "$OLLAMA_URL/v1/chat/completions" \
  -H "Content-Type: application/json" \
  -d "{
    \"model\": \"$MODEL\",
    \"messages\": [
      {
        \"role\": \"system\",
        \"content\": \"You are a helpful assistant.\"
      },
      {
        \"role\": \"user\",
        \"content\": \"Say 'Hello from Ollama!' and nothing else.\"
      }
    ],
    \"temperature\": 0.0,
    \"max_tokens\": 50
  }")

if echo "$response" | jq -e '.choices[0].message.content' > /dev/null 2>&1; then
  echo "✓ Chat completion successful"
  echo "Response: $(echo "$response" | jq -r '.choices[0].message.content')"
else
  echo "✗ Chat completion failed"
  echo "$response" | jq .
  exit 1
fi
echo ""

# Test 4: Simple generation
echo "=== Test 4: Simple Generation ==="
response=$(curl -s "$OLLAMA_URL/api/generate" \
  -d "{
    \"model\": \"$MODEL\",
    \"prompt\": \"The capital of France is\",
    \"stream\": false
  }")

if echo "$response" | jq -e '.response' > /dev/null 2>&1; then
  echo "✓ Generation successful"
  echo "Response: $(echo "$response" | jq -r '.response')"
else
  echo "✗ Generation failed"
  echo "$response" | jq .
  exit 1
fi
echo ""

# Test 5: Structured output (JSON)
echo "=== Test 5: Structured Output ==="
response=$(curl -s "$OLLAMA_URL/v1/chat/completions" \
  -H "Content-Type: application/json" \
  -d "{
    \"model\": \"$MODEL\",
    \"messages\": [
      {
        \"role\": \"system\",
        \"content\": \"You are a helpful assistant that outputs valid JSON.\"
      },
      {
        \"role\": \"user\",
        \"content\": \"Extract the person and location from: 'Alice visited Paris.' Return as JSON with 'person' and 'location' fields.\"
      }
    ],
    \"temperature\": 0.0,
    \"format\": \"json\"
  }")

if echo "$response" | jq -e '.choices[0].message.content' > /dev/null 2>&1; then
  echo "✓ Structured output successful"
  content=$(echo "$response" | jq -r '.choices[0].message.content')
  echo "Response: $content"
  
  # Try to parse as JSON
  if echo "$content" | jq . > /dev/null 2>&1; then
    echo "✓ Valid JSON response"
  else
    echo "⚠ Response is not valid JSON (this is okay, model may need fine-tuning)"
  fi
else
  echo "✗ Structured output failed"
  echo "$response" | jq .
fi
echo ""

echo "=== All Tests Completed ==="
echo ""
echo "Ollama is ready to use!"
echo "Base URL: $OLLAMA_URL"
echo "OpenAI-compatible endpoint: $OLLAMA_URL/v1/chat/completions"
