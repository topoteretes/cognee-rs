#!/bin/bash
# Example: Using Ollama with Rust's reqwest

cat << 'EOF'
Example Rust code to use Ollama's OpenAI-compatible API:

```rust
use serde::{Deserialize, Serialize};
use reqwest::Client;

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,
    temperature: f32,
}

#[derive(Serialize, Deserialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: Message,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new();
    
    let request = ChatRequest {
        model: "llama3.2:3b".to_string(),
        messages: vec![
            Message {
                role: "system".to_string(),
                content: "You are a helpful assistant.".to_string(),
            },
            Message {
                role: "user".to_string(),
                content: "What is the capital of France?".to_string(),
            },
        ],
        temperature: 0.0,
    };
    
    let response = client
        .post("http://localhost:11435/v1/chat/completions")
        .json(&request)
        .send()
        .await?
        .json::<ChatResponse>()
        .await?;
    
    println!("Assistant: {}", response.choices[0].message.content);
    
    Ok(())
}
```

Dependencies in Cargo.toml:
```toml
[dependencies]
reqwest = { version = "0.12", features = ["json"] }
serde = { version = "1", features = ["derive"] }
tokio = { version = "1", features = ["full"] }
```
EOF

echo ""
echo "To test the API right now, run:"
echo "  curl http://localhost:11435/v1/chat/completions \\"
echo "    -H 'Content-Type: application/json' \\"
echo "    -d '{"
echo "      \"model\": \"llama3.2:3b\","
echo "      \"messages\": ["
echo "        {\"role\": \"user\", \"content\": \"Hello!\"}"
echo "      ]"
echo "    }'"
