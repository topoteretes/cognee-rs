//! OpenAI API adapter with JSON schema support for structured outputs.
//!
//! This adapter uses OpenAI's function calling or JSON mode to generate
//! structured outputs based on JSON schemas derived from Rust types.

use async_trait::async_trait;
use reqwest::Client;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use crate::error::{LlmError, LlmResult};
use crate::llm_trait::Llm;
use crate::schema::generate_json_schema;
use crate::types::{GenerationOptions, GenerationResponse, Message, MessageRole, TokenUsage};

/// OpenAI API adapter.
///
/// Supports structured output generation via:
/// - Function calling (for GPT-4 and GPT-3.5-turbo)
/// - JSON mode (response_format with type: "json_object")
/// - JSON schema validation (via function parameters)
///
/// # Example
/// ```ignore
/// use cognee_llm::adapters::OpenAIAdapter;
/// use cognee_llm::Llm;
///
/// let adapter = OpenAIAdapter::new(
///     "gpt-4-turbo-preview",
///     "sk-...",
///     None, // Use default base URL
/// )?;
///
/// let result: MyStruct = adapter.create_structured_output(
///     "Extract information from this text",
///     "You are a helpful assistant",
///     None,
/// ).await?;
/// ```
#[derive(Clone)]
pub struct OpenAIAdapter {
    model: String,
    api_key: String,
    base_url: String,
    client: Client,
}

impl OpenAIAdapter {
    /// Default OpenAI API base URL
    pub const DEFAULT_BASE_URL: &'static str = "https://api.openai.com/v1";

    /// Create a new OpenAI adapter.
    ///
    /// # Arguments
    /// * `model` - Model identifier (e.g., "gpt-4", "gpt-3.5-turbo")
    /// * `api_key` - OpenAI API key
    /// * `base_url` - Optional custom base URL (defaults to OpenAI's API)
    ///
    /// # Returns
    /// A new OpenAI adapter instance
    pub fn new(
        model: impl Into<String>,
        api_key: impl Into<String>,
        base_url: Option<String>,
    ) -> LlmResult<Self> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .map_err(|e| LlmError::ConfigError(format!("Failed to create HTTP client: {}", e)))?;

        Ok(Self {
            model: model.into(),
            api_key: api_key.into(),
            base_url: base_url.unwrap_or_else(|| Self::DEFAULT_BASE_URL.to_string()),
            client,
        })
    }

    /// Build the authorization header value
    fn auth_header(&self) -> String {
        format!("Bearer {}", self.api_key)
    }

    /// Call the OpenAI chat completions API
    async fn call_api(&self, request_body: Value) -> LlmResult<OpenAIResponse> {
        let url = format!("{}/chat/completions", self.base_url);

        let response = self
            .client
            .post(&url)
            .header("Authorization", self.auth_header())
            .header("Content-Type", "application/json")
            .json(&request_body)
            .send()
            .await
            .map_err(|e| LlmError::NetworkError(e.to_string()))?;

        let status = response.status();

        if !status.is_success() {
            let error_body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());

            return Err(match status.as_u16() {
                401 => LlmError::AuthenticationError(error_body),
                429 => LlmError::RateLimitExceeded(error_body),
                400 => LlmError::InvalidResponse(format!("Bad request: {}", error_body)),
                _ => LlmError::ApiError(format!("HTTP {}: {}", status, error_body)),
            });
        }

        response
            .json::<OpenAIResponse>()
            .await
            .map_err(|e| LlmError::DeserializationError(format!("Failed to parse response: {}", e)))
    }

    /// Convert our Message type to OpenAI's format
    fn convert_messages(messages: &[Message]) -> Vec<Value> {
        messages
            .iter()
            .map(|msg| {
                json!({
                    "role": match msg.role {
                        MessageRole::System => "system",
                        MessageRole::User => "user",
                        MessageRole::Assistant => "assistant",
                    },
                    "content": msg.content
                })
            })
            .collect()
    }

    /// Convert JSON Schema to an example JSON with placeholder values
    /// This is clearer for LLMs than showing the full schema
    fn schema_to_example(schema: &Value) -> String {
        fn create_example(value: &Value, definitions: Option<&Value>) -> Value {
            match value {
                Value::Object(obj) => {
                    // Handle $ref references
                    if let Some(ref_str) = obj.get("$ref").and_then(|v| v.as_str())
                        && let Some(def_name) = ref_str.strip_prefix("#/definitions/")
                        && let Some(defs) = definitions
                        && let Some(def) = defs.get(def_name)
                    {
                        return create_example(def, definitions);
                    }

                    // Get the type of this field
                    let type_val = obj.get("type");

                    // Handle arrays
                    if let Some(Value::String(t)) = type_val
                        && t == "array"
                    {
                        if let Some(items) = obj.get("items") {
                            // Return array with one example item
                            return json!([create_example(items, definitions)]);
                        }
                        return json!([]);
                    }

                    // Handle objects with properties
                    if let Some(props) = obj.get("properties")
                        && let Value::Object(props_obj) = props
                    {
                        let mut result = serde_json::Map::new();
                        for (key, val) in props_obj {
                            result.insert(key.clone(), create_example(val, definitions));
                        }
                        return Value::Object(result);
                    }

                    // Handle primitive types
                    if let Some(Value::String(t)) = type_val {
                        return match t.as_str() {
                            "string" => json!("example"),
                            "number" | "integer" => json!(0),
                            "boolean" => json!(false),
                            _ => json!(null),
                        };
                    }

                    // Handle union types (e.g., ["string", "null"])
                    if let Some(Value::Array(types)) = type_val {
                        for t in types {
                            if let Value::String(type_str) = t
                                && type_str != "null"
                            {
                                return match type_str.as_str() {
                                    "string" => json!("example"),
                                    "number" | "integer" => json!(0),
                                    "boolean" => json!(false),
                                    _ => json!(null),
                                };
                            }
                        }
                    }

                    json!(null)
                }
                _ => value.clone(),
            }
        }

        let definitions = schema.get("definitions");
        let example = create_example(schema, definitions);

        serde_json::to_string_pretty(&example).unwrap_or_else(|_| "{}".to_string())
    }
}

#[async_trait]
impl Llm for OpenAIAdapter {
    async fn generate(
        &self,
        messages: Vec<Message>,
        options: Option<GenerationOptions>,
    ) -> LlmResult<GenerationResponse> {
        let opts = options.unwrap_or_default();

        let mut request_body = json!({
            "model": self.model,
            "messages": Self::convert_messages(&messages),
        });

        // Add optional parameters
        if let Some(temp) = opts.temperature {
            request_body["temperature"] = json!(temp);
        }
        if let Some(max_tokens) = opts.max_tokens {
            request_body["max_tokens"] = json!(max_tokens);
        }
        if let Some(top_p) = opts.top_p {
            request_body["top_p"] = json!(top_p);
        }
        if let Some(freq_penalty) = opts.frequency_penalty {
            request_body["frequency_penalty"] = json!(freq_penalty);
        }
        if let Some(pres_penalty) = opts.presence_penalty {
            request_body["presence_penalty"] = json!(pres_penalty);
        }
        if let Some(stop) = opts.stop
            && !stop.is_empty()
        {
            request_body["stop"] = json!(stop);
        }

        let response = self.call_api(request_body).await?;

        // Extract the first choice
        let choice = response
            .choices
            .first()
            .ok_or_else(|| LlmError::InvalidResponse("No choices in response".to_string()))?;

        Ok(GenerationResponse {
            content: choice.message.content.clone().unwrap_or_default(),
            model: response.model,
            finish_reason: choice.finish_reason.clone(),
            usage: response.usage.map(|u| TokenUsage {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
                total_tokens: u.total_tokens,
            }),
        })
    }

    async fn create_structured_output<T>(
        &self,
        text_input: &str,
        system_prompt: &str,
        options: Option<GenerationOptions>,
    ) -> LlmResult<T>
    where
        T: Serialize + DeserializeOwned + JsonSchema + Send,
    {
        let messages = vec![
            Message {
                role: MessageRole::System,
                content: system_prompt.to_string(),
            },
            Message {
                role: MessageRole::User,
                content: text_input.to_string(),
            },
        ];

        self.create_structured_output_with_messages(messages, options)
            .await
    }

    async fn create_structured_output_with_messages<T>(
        &self,
        messages: Vec<Message>,
        options: Option<GenerationOptions>,
    ) -> LlmResult<T>
    where
        T: Serialize + DeserializeOwned + JsonSchema + Send,
    {
        let opts = options.unwrap_or_default();

        // Generate JSON schema for the response type
        let schema = generate_json_schema::<T>();

        // Try function calling first (works with OpenAI)
        let mut request_body = json!({
            "model": self.model,
            "messages": Self::convert_messages(&messages),
            "functions": [{
                "name": "extract_structured_data",
                "description": "Extract structured data from the input",
                "parameters": schema
            }],
            "function_call": {"name": "extract_structured_data"}
        });

        // Add optional parameters
        if let Some(temp) = opts.temperature {
            request_body["temperature"] = json!(temp);
        }
        if let Some(max_tokens) = opts.max_tokens {
            request_body["max_tokens"] = json!(max_tokens);
        }

        let response = self.call_api(request_body.clone()).await?;

        let choice = response
            .choices
            .first()
            .ok_or_else(|| LlmError::InvalidResponse("No choices in response".to_string()))?;

        // Try to extract from function call (OpenAI style)
        if let Some(function_call) = &choice.message.function_call {
            return serde_json::from_str::<T>(&function_call.arguments).map_err(|e| {
                LlmError::DeserializationError(format!(
                    "Failed to deserialize function call arguments: {}. Raw: {}",
                    e, function_call.arguments
                ))
            });
        }

        // Fallback to JSON mode (works with Ollama and other providers)
        // Rebuild the request with JSON mode and example-based prompt
        let mut json_messages = Self::convert_messages(&messages);

        // Create an example JSON structure based on the schema
        let example = Self::schema_to_example(&schema);

        // Inject clearer instructions into the last user message
        if let Some(last_msg) = json_messages.last_mut()
            && last_msg["role"] == "user"
        {
            let original_content = last_msg["content"].as_str().unwrap_or("");
            last_msg["content"] = json!(format!(
                "{}\n\n\
                    Extract the information from the text above and return it as JSON.\n\
                    Use this structure as your template (but with actual data from the text):\n\
                    {}",
                original_content, example
            ));
        }

        let mut json_request = json!({
            "model": self.model,
            "messages": json_messages,
            "response_format": {"type": "json_object"}
        });

        // Add optional parameters
        if let Some(temp) = opts.temperature {
            json_request["temperature"] = json!(temp);
        }
        if let Some(max_tokens) = opts.max_tokens {
            json_request["max_tokens"] = json!(max_tokens);
        }

        let json_response = self.call_api(json_request).await?;

        let json_choice = json_response.choices.first().ok_or_else(|| {
            LlmError::InvalidResponse("No choices in JSON mode response".to_string())
        })?;

        // Parse JSON from content
        let content = json_choice.message.content.as_ref().ok_or_else(|| {
            LlmError::InvalidResponse("No content in JSON mode response".to_string())
        })?;

        serde_json::from_str::<T>(content).map_err(|e| {
            LlmError::DeserializationError(format!(
                "Failed to deserialize JSON content: {}. Raw: {}",
                e, content
            ))
        })
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_function_calling(&self) -> bool {
        true
    }

    fn max_context_length(&self) -> u32 {
        // Context lengths for common OpenAI models
        match self.model.as_str() {
            m if m.starts_with("gpt-4-turbo") => 128_000,
            m if m.starts_with("gpt-4-32k") => 32_768,
            m if m.starts_with("gpt-4") => 8_192,
            m if m.starts_with("gpt-3.5-turbo-16k") => 16_384,
            m if m.starts_with("gpt-3.5-turbo") => 4_096,
            _ => 4_096, // Conservative default
        }
    }
}

// OpenAI API response types
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OpenAIResponse {
    id: String,
    object: String,
    created: i64,
    model: String,
    choices: Vec<OpenAIChoice>,
    usage: Option<OpenAIUsage>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OpenAIChoice {
    index: u32,
    message: OpenAIMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OpenAIMessage {
    role: String,
    content: Option<String>,
    function_call: Option<OpenAIFunctionCall>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OpenAIFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct OpenAIUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_openai_adapter_creation() {
        let adapter = OpenAIAdapter::new("gpt-4", "test-key", None);
        assert!(adapter.is_ok());

        let adapter = adapter.unwrap();
        assert_eq!(adapter.model(), "gpt-4");
        assert_eq!(adapter.base_url, OpenAIAdapter::DEFAULT_BASE_URL);
    }

    #[test]
    fn test_openai_adapter_custom_base_url() {
        let adapter = OpenAIAdapter::new(
            "gpt-4",
            "test-key",
            Some("https://custom.api.com/v1".to_string()),
        );
        assert!(adapter.is_ok());

        let adapter = adapter.unwrap();
        assert_eq!(adapter.base_url, "https://custom.api.com/v1");
    }

    #[test]
    fn test_message_conversion() {
        let messages = vec![
            Message {
                role: MessageRole::System,
                content: "You are helpful".to_string(),
            },
            Message {
                role: MessageRole::User,
                content: "Hello".to_string(),
            },
        ];

        let converted = OpenAIAdapter::convert_messages(&messages);
        assert_eq!(converted.len(), 2);
        assert_eq!(converted[0]["role"], "system");
        assert_eq!(converted[0]["content"], "You are helpful");
        assert_eq!(converted[1]["role"], "user");
        assert_eq!(converted[1]["content"], "Hello");
    }

    #[test]
    fn test_context_length() {
        let adapter = OpenAIAdapter::new("gpt-4-turbo-preview", "key", None).unwrap();
        assert_eq!(adapter.max_context_length(), 128_000);

        let adapter = OpenAIAdapter::new("gpt-4", "key", None).unwrap();
        assert_eq!(adapter.max_context_length(), 8_192);

        let adapter = OpenAIAdapter::new("gpt-3.5-turbo-16k", "key", None).unwrap();
        assert_eq!(adapter.max_context_length(), 16_384);
    }
}
