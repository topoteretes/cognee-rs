/**
 * Repro for the latent constructor bug surfaced while fixing config.get():
 * `new Cognee({ llmModel: ... })` (camelCase, as documented in the class
 * docstring and used in the example scripts) was silently ignored, because the
 * native `cogneeNew` only understood snake_case `Settings` keys.
 *
 * Runs fully offline: config is in-memory; no warm / LLM / network.
 */
import { Cognee } from "../src/cognee";

describe("repro: Cognee constructor accepts documented camelCase settings", () => {
  it("applies camelCase keys (readable back via config.get())", () => {
    const c = new Cognee({
      llmModel: "gpt-4o-mini",
      embeddingProvider: "mock",
      vectorDbProvider: "brute-force",
      chunkSize: 256,
      // Public name whose Settings field is `embedding_model_name`.
      embeddingModel: "text-embedding-3-small",
    });

    const cfg = c.config.get();
    expect(cfg.llmModel).toBe("gpt-4o-mini");
    expect(cfg.embeddingProvider).toBe("mock");
    expect(cfg.vectorDbProvider).toBe("brute-force");
    expect(cfg.chunkSize).toBe(256);
    expect(cfg.embeddingModel).toBe("text-embedding-3-small");
  });

  it("still accepts raw snake_case keys (back-compat)", () => {
    const c = new Cognee({ llm_model: "gpt-4o", chunk_size: 512 });
    const cfg = c.config.get();
    expect(cfg.llmModel).toBe("gpt-4o");
    expect(cfg.chunkSize).toBe(512);
  });

  it("accepts a JSON string of camelCase settings", () => {
    const c = new Cognee(JSON.stringify({ llmModel: "gpt-4.1", chunkSize: 128 }));
    const cfg = c.config.get();
    expect(cfg.llmModel).toBe("gpt-4.1");
    expect(cfg.chunkSize).toBe(128);
  });
});
