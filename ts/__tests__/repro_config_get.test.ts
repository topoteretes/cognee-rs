/**
 * Repro for the v0.1.1 bug: high-level `Cognee.config.get()` returns an object
 * whose documented camelCase fields (`llmModel`, `embeddingProvider`,
 * `chunkSize`, ...) are all `undefined`.
 *
 * Mirrors the user's repro (/tmp/archive_extract/test.mjs section 2). Runs fully
 * offline: config mutation is in-memory; no warm / LLM / network / model I/O.
 */
import { Cognee } from "../src/cognee";

describe("repro: high-level config.get() camelCase read-back", () => {
  it("returns the values set via the setters, keyed by the camelCase API", () => {
    const c = new Cognee({ llm_model: "gpt-4o-mini" });
    c.config.setLlmModel("gpt-4o");
    c.config.setEmbeddingProvider("openai");
    c.config.setEmbeddingModel("text-embedding-3-small");
    c.config.setVectorDbProvider("brute-force");
    c.config.setChunkSize(512);

    const cfg = c.config.get();

    // Log the raw returned shape so the failure mode is visible in test output.
    // eslint-disable-next-line no-console
    console.log("raw config.get() keys:", Object.keys(cfg).slice(0, 12));
    // eslint-disable-next-line no-console
    console.log(
      "camelCase reads:",
      JSON.stringify({
        llmModel: cfg.llmModel,
        embeddingProvider: cfg.embeddingProvider,
        embeddingModel: cfg.embeddingModel,
        vectorDbProvider: cfg.vectorDbProvider,
        chunkSize: cfg.chunkSize,
      })
    );

    expect(cfg.llmModel).toBe("gpt-4o");
    expect(cfg.embeddingProvider).toBe("openai");
    expect(cfg.embeddingModel).toBe("text-embedding-3-small");
    expect(cfg.vectorDbProvider).toBe("brute-force");
    expect(cfg.chunkSize).toBe(512);
  });
});
