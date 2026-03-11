//! LLM-based Fact Extraction Example
//!
//! This example demonstrates using Qwen3-0.6B (quantized ONNX) to extract facts
//! from text in JSON format. This is a proof-of-concept for on-device entity
//! and relation extraction suitable for the cognee pipeline.
//!
//! The model extracts:
//! - Entities (people, locations, organizations, etc.)
//! - Facts/relations between entities
//!
//! Run with: cargo run --example llm_fact_extraction

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use ndarray::Array4;
use ort::logging::LogLevel;
use ort::session::{Session, builder::GraphOptimizationLevel};
use ort::value::{DynValue, Tensor};
use tokenizers::Tokenizer;

#[cfg(target_os = "android")]
use ort::execution_providers::CPUExecutionProvider;

const DEFAULT_MODEL_DIR: &str = "./target/models";
const QWEN_ONNX_FILENAME: &str = "qwen3-0.6b-q4.onnx";
const QWEN_TOKENIZER_FILENAME: &str = "qwen3-tokenizer.json";

// Generation parameters
const MAX_NEW_TOKENS: usize = 512;
const TEMPERATURE: f32 = 0.6;

// Qwen3-0.6B model architecture constants
const NUM_LAYERS: usize = 28;
const NUM_KV_HEADS: usize = 8;
const HEAD_DIM: usize = 128;

// Edge device optimization constants
const MAX_KV_LEN: usize = 2048; // Max cached tokens for edge devices
#[cfg(target_os = "android")]
const INTRA_THREADS: usize = 2; // Mobile-friendly thread count
#[cfg(not(target_os = "android"))]
const INTRA_THREADS: usize = 8; // Desktop can use more threads

/// Which execution provider configuration to use on Android
#[cfg(target_os = "android")]
#[derive(Debug, Clone, Copy)]
enum AndroidEP {
    CpuOnly,
    Xnnpack,
    ArmNN,
    ACL,
}

#[cfg(target_os = "android")]
impl AndroidEP {
    fn name(&self) -> &'static str {
        match self {
            AndroidEP::CpuOnly => "CPU-only",
            AndroidEP::Xnnpack => "XNNPACK + CPU",
            AndroidEP::ArmNN => "ArmNN + CPU",
            AndroidEP::ACL => "ACL + CPU",
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt::init();
    println!("Cognee-Rust: LLM Fact Extraction with Qwen3-0.6B\n");

    let model_dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_MODEL_DIR.to_string());
    let model_dir = PathBuf::from(model_dir);

    // Initialize ONNX Runtime
    #[cfg(feature = "onnx_dynamic_library")]
    {
        if let Ok(path) = std::env::var("ORT_DYLIB_PATH") {
            ort::init_from(path)?.commit();
        } else {
            ort::init().commit();
        }
    }

    #[cfg(not(feature = "onnx_dynamic_library"))]
    ort::init().commit();

    // Print ORT build info
    println!("ORT build info: {}", ort::info());

    // Check paths exist
    let model_path = model_dir.join(QWEN_ONNX_FILENAME);
    let tokenizer_path = model_dir.join(QWEN_TOKENIZER_FILENAME);

    if !model_path.exists() {
        return Err(format!(
            "Model not found at {:?}. Run `cargo build` first.",
            model_path
        )
        .into());
    }
    if !tokenizer_path.exists() {
        return Err(format!(
            "Tokenizer not found at {:?}. Run `cargo build` first.",
            tokenizer_path
        )
        .into());
    }

    let model_size = std::fs::metadata(&model_path)?.len();
    println!(
        "Model size: {:.2} MB",
        model_size as f64 / (1024.0 * 1024.0)
    );

    // Benchmark text — use only one text for fair comparison across EPs
    let benchmark_text = "Albert Einstein was born in Ulm, Germany in 1879. He developed the theory of relativity while working at the Swiss Patent Office in Bern.";

    #[cfg(target_os = "android")]
    {
        // Run benchmarks with each execution provider
        let eps = [
            AndroidEP::CpuOnly,
            AndroidEP::Xnnpack,
            AndroidEP::ArmNN,
            AndroidEP::ACL,
        ];

        println!("\n{}", "=".repeat(60));
        println!("EXECUTION PROVIDER BENCHMARK");
        println!("Text: \"{}\"", benchmark_text);
        println!("{}\n", "=".repeat(60));

        let mut results: Vec<(&str, Option<ExtractionResult>)> = Vec::new();

        for ep in &eps {
            println!("\n>>> Benchmarking EP: {} <<<", ep.name());
            println!("{}", "-".repeat(60));

            match EdgeLLMGenerator::new_with_ep(&model_path, &tokenizer_path, *ep) {
                Ok(mut generator) => {
                    // Print model info on first run
                    if results.is_empty() {
                        println!(
                            "  Tokenizer: {} vocab size",
                            generator.tokenizer.get_vocab_size(true)
                        );
                        println!("  Model inputs:");
                        for input in generator.session.inputs() {
                            println!("    - {}", input.name());
                        }
                    }

                    match extract_facts(benchmark_text, &mut generator) {
                        Ok(result) => {
                            println!(
                                "  Result: {} input tokens, {} output tokens",
                                result.input_tokens, result.output_tokens
                            );
                            println!(
                                "  Prefill: {:.2} tok/s ({:.1?} for {} tokens)",
                                result.prefill_tok_per_sec(),
                                result.prefill_time,
                                result.input_tokens
                            );
                            println!(
                                "  Decode:  {:.2} tok/s ({:.1?} for {} tokens)",
                                result.decode_tok_per_sec(),
                                result.decode_time,
                                result.output_tokens.saturating_sub(1)
                            );
                            println!(
                                "  Total:   {:.2} tok/s, {:?}",
                                result.total_tok_per_sec(),
                                result.generation_time
                            );
                            results.push((ep.name(), Some(result)));
                        }
                        Err(e) => {
                            println!("  ERROR: {}", e);
                            results.push((ep.name(), None));
                        }
                    }

                    // End profiling for this session
                    match generator.session.end_profiling() {
                        Ok(f) => println!("  Profile: {}", f),
                        Err(e) => println!("  (no profile: {})", e),
                    }
                }
                Err(e) => {
                    println!("  FAILED to create session: {}", e);
                    results.push((ep.name(), None));
                }
            }
        }

        // Print summary table
        println!("\n{}", "=".repeat(78));
        println!("BENCHMARK SUMMARY");
        println!("{}", "=".repeat(78));
        println!(
            "{:<16} {:>8} {:>8} {:>14} {:>14} {:>14}",
            "EP", "In Tok", "Out Tok", "Prefill tok/s", "Decode tok/s", "Total Time"
        );
        println!("{}", "-".repeat(78));
        for (name, result) in &results {
            match result {
                Some(r) => {
                    println!(
                        "{:<16} {:>8} {:>8} {:>14.2} {:>14.2} {:>14.2?}",
                        name,
                        r.input_tokens,
                        r.output_tokens,
                        r.prefill_tok_per_sec(),
                        r.decode_tok_per_sec(),
                        r.generation_time
                    );
                }
                None => {
                    println!("{:<16} {:>8}", name, "FAILED");
                }
            }
        }
        println!("{}", "=".repeat(78));
    }

    #[cfg(not(target_os = "android"))]
    {
        // Desktop: single run with default EPs
        println!("Loading EdgeLLMGenerator...");
        let mut generator = EdgeLLMGenerator::new(&model_path, &tokenizer_path)?;
        println!(
            "  Tokenizer loaded: {} vocab size",
            generator.tokenizer.get_vocab_size(true)
        );

        println!("  Model inputs:");
        for input in generator.session.inputs() {
            println!("    - {}", input.name());
        }
        println!("  Model outputs:");
        for output in generator.session.outputs() {
            println!("    - {}", output.name());
        }

        let texts = [
            benchmark_text,
            "Apple Inc. was founded by Steve Jobs, Steve Wozniak, and Ronald Wayne in Cupertino, California in 1976.",
            "The Amazon River flows through Brazil and is the largest river by water volume in the world.",
        ];

        println!("\n{}", "=".repeat(60));
        println!("Extracting facts from {} texts...", texts.len());
        println!("{}\n", "=".repeat(60));

        for (idx, text) in texts.iter().enumerate() {
            println!("Text {}: \"{}\"", idx + 1, text);
            println!("{}", "-".repeat(60));

            match extract_facts(text, &mut generator) {
                Ok(result) => {
                    println!("Extracted knowledge graph:\n{}", result.json_output);
                    println!(
                        "Stats: {} in, {} out | prefill {:.2} tok/s ({:.1?}) | decode {:.2} tok/s ({:.1?}) | total {:?}\n",
                        result.input_tokens,
                        result.output_tokens,
                        result.prefill_tok_per_sec(),
                        result.prefill_time,
                        result.decode_tok_per_sec(),
                        result.decode_time,
                        result.generation_time
                    );
                }
                Err(e) => {
                    println!("Error extracting facts: {}\n", e);
                }
            }
        }

        match generator.session.end_profiling() {
            Ok(profile_file) => println!("ORT profiling data written to: {}", profile_file),
            Err(e) => println!("Could not end profiling: {}", e),
        }
    }

    println!("\nBenchmark completed!");
    Ok(())
}

/// Result of fact extraction including timing info
struct ExtractionResult {
    json_output: String,
    input_tokens: usize,
    output_tokens: usize,
    generation_time: std::time::Duration,
    prefill_time: std::time::Duration,
    decode_time: std::time::Duration,
}

impl ExtractionResult {
    fn total_tok_per_sec(&self) -> f64 {
        self.output_tokens as f64 / self.generation_time.as_secs_f64()
    }

    /// Prefill speed: input tokens processed per second (first inference call)
    fn prefill_tok_per_sec(&self) -> f64 {
        self.input_tokens as f64 / self.prefill_time.as_secs_f64()
    }

    /// Decode speed: output tokens generated per second (excluding prefill)
    fn decode_tok_per_sec(&self) -> f64 {
        if self.output_tokens <= 1 {
            return 0.0;
        }
        // First decode token is produced during prefill, remaining are pure decode
        (self.output_tokens - 1) as f64 / self.decode_time.as_secs_f64()
    }
}

/// Extract facts from text using the LLM
fn extract_facts(
    text: &str,
    generator: &mut EdgeLLMGenerator,
) -> Result<ExtractionResult, Box<dyn Error>> {
    // Create the extraction prompt
    let prompt = create_extraction_prompt(text);

    // Count input tokens
    let encoding = generator
        .tokenizer
        .encode(prompt.as_str(), false)
        .map_err(|e| format!("Tokenization failed: {}", e))?;
    let input_tokens = encoding.get_ids().len();

    println!("  Input tokens: {}", input_tokens);

    // Generate response
    let start = Instant::now();
    let (output, output_tokens, timings) =
        generator.generate(&prompt, MAX_NEW_TOKENS, TEMPERATURE)?;
    let generation_time = start.elapsed();

    // Extract JSON from the response
    let json_output = extract_json_from_response(&output)?;

    Ok(ExtractionResult {
        json_output,
        input_tokens,
        output_tokens,
        generation_time,
        prefill_time: timings.prefill_time,
        decode_time: timings.decode_time,
    })
}

/// Create the prompt for fact extraction
fn create_extraction_prompt(text: &str) -> String {
    format!(
        r#"<|im_start|>system
You are a top-tier algorithm designed for extracting information in structured formats to build a knowledge graph.
**Nodes** represent entities and concepts. They're akin to Wikipedia nodes.
**Edges** represent relationships between concepts. They're akin to Wikipedia links.

The aim is to achieve simplicity and clarity in the knowledge graph.
# 1. Labeling Nodes
**Consistency**: Ensure you use basic or elementary types for node labels.
  - For example, when you identify an entity representing a person, always label it as **"Person"**.
  - Avoid using more specific terms like "Mathematician" or "Scientist", keep those as "profession" property.
  - Don't use too generic terms like "Entity".
**Node IDs**: Never utilize integers as node IDs.
  - Node IDs should be names or human-readable identifiers found in the text.
# 2. Handling Numerical Data and Dates
  - For example, when you identify an entity representing a date, make sure it has type **"Date"**.
  - Extract the date in the format "YYYY-MM-DD"
  - If not possible to extract the whole date, extract month or year, or both if available.
  - **Property Format**: Properties must be in a key-value format.
  - **Quotation Marks**: Never use escaped single or double quotes within property values.
  - **Naming Convention**: Use snake_case for relationship names, e.g., `acted_in`.
# 3. Coreference Resolution
  - **Maintain Entity Consistency**: When extracting entities, it's vital to ensure consistency.
  If an entity, such as "John Doe", is mentioned multiple times in the text but is referred to by different names or pronouns (e.g., "Joe", "he"),
  always use the most complete identifier for that entity throughout the knowledge graph. In this example, use "John Doe" as the Persons ID.
Remember, the knowledge graph should be coherent and easily understandable, so maintaining consistency in entity references is crucial.
# 4. Strict Compliance
Adhere to the rules strictly. Non-compliance will result in termination.
/no_think
<|im_end|>
<|im_start|>user
Extract all nodes (entities) and edges (relationships) from: "{}"

Output JSON format:
{{"nodes": [{{"id": "...", "type": "Person|Location|Organization|Date|...", "properties": {{...}}}}], "edges": [{{"source": "...", "target": "...", "relationship": "..."}}]}}
<|im_end|>
<|im_start|>assistant
```json
{{"#,
        text
    )
}

/// Create an empty KV cache tensor with 0 sequence length
fn empty_kv_tensor() -> Result<DynValue, Box<dyn Error>> {
    let array: Array4<f32> = Array4::zeros((1, NUM_KV_HEADS, 0, HEAD_DIM));
    Ok(Tensor::from_array(array)?.into())
}

/// Timing data returned from generate
struct GenerateTimings {
    prefill_time: std::time::Duration,
    decode_time: std::time::Duration,
}

/// Edge-optimized LLM generator with KV cache and optional NNAPI acceleration
struct EdgeLLMGenerator {
    session: Session,
    tokenizer: Tokenizer,
    kv_cache: Vec<DynValue>,
    past_seq_len: usize,
    max_kv_len: usize,
}

impl EdgeLLMGenerator {
    /// Create a new generator with default EPs (desktop path)
    #[cfg(not(target_os = "android"))]
    fn new(model_path: &Path, tokenizer_path: &Path) -> Result<Self, Box<dyn Error>> {
        let tokenizer = Tokenizer::from_file(tokenizer_path)
            .map_err(|e| format!("Failed to load tokenizer: {}", e))?;

        let session = Self::create_session(model_path)?;

        let kv_cache = (0..NUM_LAYERS * 2)
            .map(|_| empty_kv_tensor())
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self {
            session,
            tokenizer,
            kv_cache,
            past_seq_len: 0,
            max_kv_len: MAX_KV_LEN,
        })
    }

    /// Create a new generator with a specific execution provider (Android benchmark path)
    #[cfg(target_os = "android")]
    fn new_with_ep(
        model_path: &Path,
        tokenizer_path: &Path,
        ep: AndroidEP,
    ) -> Result<Self, Box<dyn Error>> {
        let tokenizer = Tokenizer::from_file(tokenizer_path)
            .map_err(|e| format!("Failed to load tokenizer: {}", e))?;

        let session = Self::create_session_with_ep(model_path, ep)?;

        let kv_cache = (0..NUM_LAYERS * 2)
            .map(|_| empty_kv_tensor())
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self {
            session,
            tokenizer,
            kv_cache,
            past_seq_len: 0,
            max_kv_len: MAX_KV_LEN,
        })
    }

    /// Create session with a specific EP on Android for benchmarking
    #[cfg(target_os = "android")]
    fn create_session_with_ep(model_path: &Path, ep: AndroidEP) -> Result<Session, Box<dyn Error>> {
        use core::num::NonZeroUsize;

        println!("  Configuring EP: {:?}...", ep);

        // Only log warnings and errors to reduce noise during benchmarking
        let ort_logger: Arc<dyn Fn(LogLevel, &str, &str, &str, &str) + Send + Sync> = Arc::new(
            |level: LogLevel, category: &str, _id: &str, _code_location: &str, message: &str| {
                if matches!(level, LogLevel::Warning | LogLevel::Error | LogLevel::Fatal) {
                    println!("  [ORT {:?}] [{}] {}", level, category, message);
                }
            },
        );

        let mut builder = Session::builder()?;
        builder = builder
            .with_optimization_level(GraphOptimizationLevel::Level3)?
            .with_intra_threads(INTRA_THREADS)?
            .with_log_level(LogLevel::Warning)?
            .with_logger(ort_logger)?
            .with_profiling(format!(
                "/data/local/tmp/cognee/ort_profile_{}",
                match ep {
                    AndroidEP::CpuOnly => "cpu",
                    AndroidEP::Xnnpack => "xnnpack",
                    AndroidEP::ArmNN => "armnn",
                    AndroidEP::ACL => "acl",
                }
            ))?;

        match ep {
            AndroidEP::CpuOnly => {
                builder =
                    builder.with_execution_providers([CPUExecutionProvider::default().build()])?;
            }
            AndroidEP::Xnnpack => {
                builder = builder.with_execution_providers([
                    ort::ep::XNNPACK::default()
                        .with_intra_op_num_threads(NonZeroUsize::new(INTRA_THREADS).unwrap())
                        .build(),
                    CPUExecutionProvider::default().build(),
                ])?;
            }
            AndroidEP::ArmNN => {
                builder = builder.with_execution_providers([
                    ort::ep::ArmNN::default().build(),
                    CPUExecutionProvider::default().build(),
                ])?;
            }
            AndroidEP::ACL => {
                builder = builder.with_execution_providers([
                    ort::ep::ACL::default().with_fast_math(true).build(),
                    CPUExecutionProvider::default().build(),
                ])?;
            }
        }

        let session = builder.commit_from_file(model_path)?;

        println!("  Session created successfully with EP: {:?}", ep);
        if let Ok(meta) = session.metadata() {
            println!(
                "  Model metadata: producer={:?}, description={:?}, domain={:?}, version={:?}",
                meta.producer(),
                meta.description(),
                meta.domain(),
                meta.version()
            );
        }

        Ok(session)
    }

    #[cfg(not(target_os = "android"))]
    fn create_session(model_path: &Path) -> Result<Session, Box<dyn Error>> {
        println!(
            "  Using CPU execution provider ({} threads)...",
            INTRA_THREADS
        );

        // Custom logger to capture all ORT internal messages (including EP registration)
        let ort_logger: Arc<dyn Fn(LogLevel, &str, &str, &str, &str) + Send + Sync> = Arc::new(
            |level: LogLevel, category: &str, _id: &str, code_location: &str, message: &str| {
                println!(
                    "  [ORT {:?}] [{}] {} (at {})",
                    level, category, message, code_location
                );
            },
        );

        let session = Session::builder()?
            .with_optimization_level(GraphOptimizationLevel::Level3)?
            .with_intra_threads(INTRA_THREADS)?
            .with_log_level(LogLevel::Verbose)?
            .with_log_verbosity(10)?
            .with_logger(ort_logger)?
            .with_execution_providers([
                // Prefer TensorRT over CUDA.
                ort::ep::TensorRT::default().build(),
                ort::ep::CUDA::default().build(),
                // Or use ANE on Apple platforms
                ort::ep::CoreML::default().build(),
                ort::ep::CPU::default().build(),
            ])?
            .commit_from_file(model_path)?;

        println!("  Session created successfully.");
        if let Ok(meta) = session.metadata() {
            println!(
                "  Model metadata: producer={:?}, description={:?}, domain={:?}, version={:?}",
                meta.producer(),
                meta.description(),
                meta.domain(),
                meta.version()
            );
        }

        Ok(session)
    }

    /// Reset KV cache for a new conversation
    fn reset_cache(&mut self) -> Result<(), Box<dyn Error>> {
        self.kv_cache = (0..NUM_LAYERS * 2)
            .map(|_| empty_kv_tensor())
            .collect::<Result<Vec<_>, _>>()?;
        self.past_seq_len = 0;
        Ok(())
    }

    /// Check and evict cache if it exceeds max length
    fn maybe_evict_cache(&mut self) -> Result<(), Box<dyn Error>> {
        if self.past_seq_len > self.max_kv_len {
            println!(
                "  KV cache exceeded {} tokens, resetting...",
                self.max_kv_len
            );
            self.reset_cache()?;
        }
        Ok(())
    }

    /// Generate text from a prompt
    fn generate(
        &mut self,
        prompt: &str,
        max_new_tokens: usize,
        temperature: f32,
    ) -> Result<(String, usize, GenerateTimings), Box<dyn Error>> {
        // Reset cache for each new prompt (stateless generation)
        self.reset_cache()?;

        // Tokenize the prompt
        let encoding = self
            .tokenizer
            .encode(prompt, false)
            .map_err(|e| format!("Tokenization failed: {}", e))?;

        let input_ids: Vec<i64> = encoding.get_ids().iter().map(|&id| id as i64).collect();
        let mut generated_tokens = Vec::new();

        // Get special token IDs
        let eos_token_id = self
            .tokenizer
            .token_to_id("<|im_end|>")
            .or_else(|| self.tokenizer.token_to_id("<|endoftext|>"))
            .unwrap_or(151643) as i64;

        let mut current_input_ids = input_ids;
        let mut prefill_time = std::time::Duration::ZERO;
        let mut decode_time = std::time::Duration::ZERO;

        for step in 0..max_new_tokens {
            // Check memory limits
            self.maybe_evict_cache()?;

            let seq_len = current_input_ids.len();
            let total_seq_len = self.past_seq_len + seq_len;

            // Create attention mask for total sequence length
            let attention_mask: Vec<i64> = vec![1i64; total_seq_len];

            // Create position IDs starting from past_seq_len
            let position_ids: Vec<i64> =
                (self.past_seq_len as i64..(total_seq_len as i64)).collect();

            // Create input tensors
            let input_ids_tensor =
                Tensor::from_array((vec![1usize, seq_len], current_input_ids.clone()))?;
            let attention_mask_tensor =
                Tensor::from_array((vec![1usize, total_seq_len], attention_mask))?;
            let position_ids_tensor = Tensor::from_array((vec![1usize, seq_len], position_ids))?;

            // Build inputs with KV cache
            let mut inputs: Vec<(std::borrow::Cow<str>, DynValue)> = vec![
                ("input_ids".into(), input_ids_tensor.into()),
                ("attention_mask".into(), attention_mask_tensor.into()),
                ("position_ids".into(), position_ids_tensor.into()),
            ];

            // Add KV cache inputs - move DynValues directly (Arc-backed, cheap)
            // Replace with empty tensors so we can move the cached values into inputs
            for layer in 0..NUM_LAYERS {
                let key_name = format!("past_key_values.{}.key", layer);
                let value_name = format!("past_key_values.{}.value", layer);
                let cached_key =
                    std::mem::replace(&mut self.kv_cache[layer * 2], empty_kv_tensor()?);
                let cached_value =
                    std::mem::replace(&mut self.kv_cache[layer * 2 + 1], empty_kv_tensor()?);
                inputs.push((key_name.into(), cached_key));
                inputs.push((value_name.into(), cached_value));
            }

            // Run inference with timing
            let step_start = Instant::now();
            let mut outputs = self.session.run(inputs)?;
            let step_elapsed = step_start.elapsed();

            // step 0 = prefill (processes all input tokens), rest = decode (1 token each)
            if step == 0 {
                prefill_time = step_elapsed;
            } else {
                decode_time += step_elapsed;
            }

            // Get logits from first output
            let (shape, logits) = outputs[0].try_extract_tensor::<f32>()?;

            // Get logits for the last token
            let vocab_size = shape[shape.len() - 1] as usize;
            let last_token_logits_start = (seq_len - 1) * vocab_size;
            let last_token_logits =
                &logits[last_token_logits_start..last_token_logits_start + vocab_size];

            // Sample next token
            let next_token = sample_token(last_token_logits, temperature);

            // Check for end of sequence
            if next_token == eos_token_id && step > 0 {
                break;
            }

            generated_tokens.push(next_token as u32);

            // Update KV cache from outputs - take ownership of DynValues directly
            for layer in 0..NUM_LAYERS {
                let key_name = format!("present.{}.key", layer);
                let value_name = format!("present.{}.value", layer);
                self.kv_cache[layer * 2] = outputs
                    .remove(&key_name)
                    .unwrap_or_else(|| panic!("missing output {}", key_name));
                self.kv_cache[layer * 2 + 1] = outputs
                    .remove(&value_name)
                    .unwrap_or_else(|| panic!("missing output {}", value_name));
            }

            // Update for next iteration
            self.past_seq_len = total_seq_len;
            current_input_ids = vec![next_token];

            // Print progress
            if (step + 1) % 20 == 0 {
                println!("  Generated {} tokens...", step + 1);
            }
        }

        // Decode generated tokens
        let token_count = generated_tokens.len();
        let output_text = self
            .tokenizer
            .decode(&generated_tokens, true)
            .map_err(|e| format!("Decoding failed: {}", e))?;

        let timings = GenerateTimings {
            prefill_time,
            decode_time,
        };

        Ok((output_text, token_count, timings))
    }
}

/// Sample a token from logits using temperature sampling
fn sample_token(logits: &[f32], temperature: f32) -> i64 {
    if temperature <= 0.0 {
        // Greedy decoding
        return logits
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap())
            .map(|(idx, _)| idx as i64)
            .unwrap_or(0);
    }

    // Apply temperature
    let scaled_logits: Vec<f32> = logits.iter().map(|&l| l / temperature).collect();

    // Softmax
    let max_logit = scaled_logits
        .iter()
        .cloned()
        .fold(f32::NEG_INFINITY, f32::max);
    let exp_logits: Vec<f32> = scaled_logits
        .iter()
        .map(|&l| (l - max_logit).exp())
        .collect();
    let sum_exp: f32 = exp_logits.iter().sum();
    let probs: Vec<f32> = exp_logits.iter().map(|&e| e / sum_exp).collect();

    // Sample from distribution
    let r: f32 = rand::random();
    let mut cumsum = 0.0;
    for (idx, &p) in probs.iter().enumerate() {
        cumsum += p;
        if r < cumsum {
            return idx as i64;
        }
    }

    probs.len() as i64 - 1
}

/// Extract JSON from the model's response
fn extract_json_from_response(response: &str) -> Result<String, Box<dyn Error>> {
    // Try to find JSON in the response
    let trimmed = response.trim();

    // Look for JSON object boundaries
    if let Some(start) = trimmed.find('{') {
        // Find matching closing brace
        let mut depth = 0;
        let mut end = start;
        for (i, c) in trimmed[start..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = start + i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }

        let json_str = &trimmed[start..end];

        // Validate it's valid JSON
        match serde_json::from_str::<serde_json::Value>(json_str) {
            Ok(json) => {
                return Ok(serde_json::to_string_pretty(&json)?);
            }
            Err(_) => {
                // Return raw if parsing fails
                return Ok(format!("Raw output (invalid JSON):\n{}", json_str));
            }
        }
    }

    // No JSON found, return raw response
    Ok(format!("Raw output (no JSON found):\n{}", trimmed))
}
