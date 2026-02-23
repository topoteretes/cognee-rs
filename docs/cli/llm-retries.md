# LLM Retries in CLI

The Rust CLI supports configuring structured-output retry attempts for OpenAI-compatible LLM calls.

## Commands

You can override retries per command run with:

```bash
cognee-cli cognify --llm-max-retries 4
cognee-cli search "What is TechCorp?" --llm-max-retries 4
```

## Config key

You can persist the default with:

```bash
cognee-cli config set llm_max_retries 4
```

Inspect/reset it with:

```bash
cognee-cli config get llm_max_retries
cognee-cli config unset llm_max_retries
```

## Behavior

- Config key: `llm_max_retries`
- Default value: `2`
- Minimum value: `1`
- Precedence: CLI flag (`--llm-max-retries`) overrides config value

This setting is passed to `OpenAIAdapter` and controls retries in structured output paths (strict schema, function-call, and JSON fallback parsing).
