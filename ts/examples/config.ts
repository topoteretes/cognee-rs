/**
 * config.ts — Runnable example: programmatic configuration of LLM, embedding,
 * vector-DB, and graph-DB backends.
 *
 * Prerequisites
 * -------------
 * 1. Build the native addon:
 *      cd ts/cognee-ts-neon && npm run build
 * 2. Set the following environment variables:
 *      OPENAI_URL=https://api.openai.com/v1
 *      OPENAI_TOKEN=sk-...
 *
 * Running
 * -------
 *   npm run example:config          (from the ts/ directory)
 *   npx ts-node ts/examples/config.ts
 *
 * What it does
 * ------------
 * 1. Shows three ways to configure a Cognee handle:
 *    a. Pass a settings object at construction time.
 *    b. Use granular config setters (setLlmModel, setLlmApiKey, …).
 *    c. Use bulk setters (setLlmConfig, setEmbeddingConfig, …).
 * 2. Reads the config back (secret fields are redacted) and prints it.
 *
 * No LLM or embedding calls are made — this example exits without warming.
 */

import { Cognee } from "../src";

function main(): void {
  const llmEndpoint = process.env.OPENAI_URL;
  const llmApiKey = process.env.OPENAI_TOKEN;
  if (!llmEndpoint || !llmApiKey) {
    console.error(
      "ERROR: OPENAI_URL and OPENAI_TOKEN must be set.\n" +
      "Example:\n" +
      "  export OPENAI_URL=https://api.openai.com/v1\n" +
      "  export OPENAI_TOKEN=sk-...\n"
    );
    process.exit(1);
  }

  // ── Method A: settings object at construction ──────────────────────────────
  //
  // Keys are camelCase Settings field names (e.g. llmModel, embeddingProvider).
  // Both camelCase and snake_case are accepted by the Rust layer.
  console.log("=== Method A: settings object at construction ===");
  const cognee = new Cognee({
    llmEndpoint,
    llmApiKey,
    llmModel: "gpt-4o-mini",
    embeddingProvider: "mock",
  });

  const cfgA = cognee.config.get();
  console.log(`  llm_model     = ${cfgA["llm_model"]}`);
  console.log(`  llm_api_key   = ${cfgA["llm_api_key"]}  (redacted)`);
  console.log(`  emb_provider  = ${cfgA["embedding_provider"]}`);

  // ── Method B: granular setters ─────────────────────────────────────────────
  //
  // Each setter targets a specific field.  Changes take effect immediately.
  console.log("\n=== Method B: granular setters ===");
  cognee.config.setLlmModel("gpt-4o");
  cognee.config.setLlmTemperature(0.1);

  const cfgB = cognee.config.get();
  console.log(`  llm_model after setLlmModel: ${cfgB["llm_model"]}`);
  console.log(`  llm_temperature after setLlmTemperature: ${cfgB["llm_temperature"]}`);

  // The generic set() raises for unknown keys:
  try {
    cognee.config.set("definitely_not_a_real_key", 42);
  } catch (err: unknown) {
    const msg = err instanceof Error ? err.message : String(err);
    console.log(`  Expected error for unknown key: ${msg}`);
  }

  // ── Method C: bulk setters ─────────────────────────────────────────────────
  //
  // Each bulk setter atomically applies a group of related settings.
  console.log("\n=== Method C: bulk setters ===");

  cognee.config.setLlmConfig({
    llm_model: "gpt-4o-mini",
    llm_temperature: 0.0,
    llm_max_retries: 3,
  });

  cognee.config.setEmbeddingConfig({
    embedding_provider: "mock",
    embedding_dimensions: 128,
  });

  cognee.config.setVectorDbConfig({
    vector_db_provider: "brute-force",
  });

  cognee.config.setGraphDbConfig({
    graph_database_provider: "ladybug",
  });

  // ── Read-back ──────────────────────────────────────────────────────────────
  console.log("\n=== Final config read-back (secrets redacted) ===");
  const finalCfg = cognee.config.get();
  const keysToShow = [
    "llm_model",
    "llm_api_key",
    "llm_temperature",
    "llm_max_retries",
    "embedding_provider",
    "embedding_dimensions",
    "vector_db_provider",
    "graph_database_provider",
  ];
  for (const key of keysToShow) {
    if (key in finalCfg) {
      console.log(`  ${key} = ${JSON.stringify(finalCfg[key])}`);
    }
  }

  console.log("\nConfig example complete.");
}

main();
