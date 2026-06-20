# Custom Extraction Prompts

## What it does

Overrides the LLM prompt that the cognify pipeline uses for the
entity/relationship (graph) extraction stage. Mirrors Python's cognify
`custom_prompt` parameter. When set, the [`FactExtractor`] uses your prompt
instead of the built-in default.

## When to use it

- Steer extraction toward a domain vocabulary ("treat each section header as a
  Topic node…").
- Tighten or loosen what counts as an entity/relationship for your corpus.

## How it is wired

This knob **is consumed** by the standalone pipeline. `CognifyConfig` carries
`custom_extraction_prompt: Option<String>`, and the graph-extraction task passes
it straight into `FactExtractor::extract_facts(text, prompt)`
([`crates/cognify/src/tasks.rs`](../../crates/cognify/src/tasks.rs), the
`extract_graph_from_data` task). `None` falls back to the default prompt.

## Example (programmatic)

```rust
use cognee_cognify::CognifyConfig;

let config = CognifyConfig::default()
    .with_custom_prompt(
        "Extract people, organizations, and the roles connecting them.".to_string(),
    );
// pass `config` to the cognify pipeline
```

The builder is [`CognifyConfig::with_custom_prompt`]; the field is
`custom_extraction_prompt`.

## CLI

There is currently **no CLI flag** for the extraction prompt — `cognee-cli
cognify` does not expose `custom_extraction_prompt`. Use it via the
`CognifyConfig` builder in Rust (or the HTTP/binding surfaces that accept a
cognify config). Note this is distinct from `cognee-cli search
--system-prompt[-path]`, which sets the *search* answer-generation prompt, not
the cognify extraction prompt.

## Pointers

- [`CognifyConfig`](../../crates/cognify/src/config.rs) — `custom_extraction_prompt`, `with_custom_prompt`.
- [`crates/cognify/src/tasks.rs`](../../crates/cognify/src/tasks.rs) — where the prompt reaches `FactExtractor`.
- [operations.md](../operations.md) — the cognify stage in context.

[`FactExtractor`]: ../../crates/cognify/src/
[`CognifyConfig::with_custom_prompt`]: ../../crates/cognify/src/config.rs
