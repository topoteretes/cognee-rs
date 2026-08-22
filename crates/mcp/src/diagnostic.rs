#[cfg(any(feature = "engine", test))]
use serde_json::{Value, json};

#[cfg(any(feature = "engine", test))]
use crate::error::AgentError;
#[cfg(any(feature = "engine", test))]
use crate::redact::redact_json;

#[cfg(any(feature = "engine", test))]
fn diagnostic_success(result: Value) -> String {
    diagnostic_json(json!({"ok": true, "result": result}))
}

#[cfg(any(feature = "engine", test))]
fn diagnostic_failure(code: &str, message: &str) -> AgentError {
    AgentError::Diagnostic(diagnostic_json(json!({
        "ok": false,
        "code": code,
        "error": message,
    })))
}

#[cfg(any(feature = "engine", test))]
fn diagnostic_json(value: Value) -> String {
    serde_json::to_string(&redact_json(&value).value)
        .unwrap_or_else(|_| r#"{"ok":false,"code":"SERIALIZATION_ERROR"}"#.to_owned())
}

#[cfg(any(feature = "engine", test))]
fn recall_options(
    dataset: &str,
    session_id: Option<&str>,
    search_type: &str,
    top_k: usize,
) -> Value {
    let mut options = json!({
        "datasets": [dataset],
        "searchType": search_type,
        "topK": top_k,
        "autoRoute": false,
    });
    if let Some(session_id) = session_id {
        options["sessionId"] = Value::String(session_id.to_owned());
    }
    options
}

#[cfg(feature = "engine")]
pub fn run_recall_from_env(
    env: &impl crate::config::EnvSource,
    query: &str,
    session_id: Option<&str>,
    search_type: &str,
    top_k: usize,
) -> Result<(), AgentError> {
    use crate::config::AgentConfig;
    use crate::embedding_generation::{EmbeddingFingerprint, EmbeddingGeneration};

    let config = AgentConfig::from_env(env)
        .map_err(|error| diagnostic_failure("CONFIGURATION_ERROR", &error.to_string()))?;
    let embedding = config.embedding.as_ref().ok_or_else(|| {
        diagnostic_failure(
            "CONFIGURATION_ERROR",
            "APEX_COGNEE_EMBEDDING_PROVIDER is required",
        )
    })?;
    let generation_id = EmbeddingFingerprint::from_config(embedding).stable_id();
    let generation = EmbeddingGeneration::new(&config.layout, generation_id, embedding)
        .map_err(|error| diagnostic_failure("CONFIGURATION_ERROR", &error.to_string()))?;
    let settings = config
        .cognee_settings(&generation)
        .map_err(|error| diagnostic_failure("CONFIGURATION_ERROR", &error.to_string()))?;
    let options = recall_options(&config.dataset, session_id, search_type, top_k);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| diagnostic_failure("RUNTIME_ERROR", &error.to_string()))?;

    let result = runtime.block_on(async {
        let state = cognee_bindings_common::HandleState::from_settings(settings);
        let result = cognee_bindings_common::ops::retrieval::recall(&state, query, &options).await;
        state.close().await;
        result
    });

    match result {
        Ok(result) => {
            println!("{}", diagnostic_success(result));
            Ok(())
        }
        Err(error) => Err(diagnostic_failure(error.code(), &error.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{diagnostic_failure, diagnostic_success, recall_options};
    use crate::error::AgentError;

    #[test]
    fn diagnostic_success_is_json_and_redacts_recalled_credentials() {
        let output = diagnostic_success(json!({
            "items": [{"content": "Authorization: Bearer sk-secret-value-123456"}],
            "searchTypeUsed": "CHUNKS",
        }));

        let parsed: serde_json::Value = match serde_json::from_str(&output) {
            Ok(parsed) => parsed,
            Err(error) => panic!("diagnostic JSON: {error}"),
        };
        assert_eq!(parsed["ok"], true);
        assert_eq!(parsed["result"]["searchTypeUsed"], "CHUNKS");
        assert!(output.contains("[REDACTED]"));
        assert!(!output.contains("sk-secret-value-123456"));
    }

    #[test]
    fn diagnostic_failure_retains_code_but_redacts_sdk_message() {
        let error = diagnostic_failure(
            "RUNTIME_ERROR",
            "request failed with Authorization: Bearer sk-secret-value-123456",
        );

        let AgentError::Diagnostic(output) = error else {
            panic!("expected diagnostic error")
        };
        let parsed: serde_json::Value = match serde_json::from_str(&output) {
            Ok(parsed) => parsed,
            Err(error) => panic!("diagnostic JSON: {error}"),
        };
        assert_eq!(parsed["ok"], false);
        assert_eq!(parsed["code"], "RUNTIME_ERROR");
        assert!(output.contains("[REDACTED]"));
        assert!(!output.contains("sk-secret-value-123456"));
    }

    #[test]
    fn recall_options_target_the_existing_dataset_and_optional_session() {
        assert_eq!(
            recall_options("agent_sessions", Some("session-123"), "CHUNKS", 7),
            json!({
                "datasets": ["agent_sessions"],
                "sessionId": "session-123",
                "searchType": "CHUNKS",
                "topK": 7,
                "autoRoute": false,
            })
        );
    }
}
