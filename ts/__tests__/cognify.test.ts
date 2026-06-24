/**
 * Tier-B test for Phase-3 `cognify` / `addAndCognify` (LLM + embeddings).
 *
 * This exercises the live `add → cognify` round-trip from Node. It requires a
 * real OpenAI-compatible LLM and a locally-downloaded embedding model, so it
 * **skips cleanly** when the gating env vars are absent — which is the case in
 * the `ts-check` CI job (deterministic Tier-A only). It therefore never breaks
 * CI and only runs when an operator provides credentials.
 *
 * Required env (matches the Rust workspace test harness, see CLAUDE.md):
 *   - OPENAI_URL                    OpenAI-compatible API base URL
 *   - OPENAI_TOKEN                  API key
 *   - OPENAI_MODEL                  (optional) LLM model, default gpt-4o-mini
 *   - COGNEE_E2E_EMBED_MODEL_PATH   path to a BGE-Small-v1.5 ONNX model
 *   - COGNEE_E2E_TOKENIZER_PATH     path to the matching tokenizer.json
 */
import * as fs from "fs";
import * as os from "os";
import * as path from "path";

import { native } from "../src/native";

const OPENAI_URL = process.env.OPENAI_URL;
const OPENAI_TOKEN = process.env.OPENAI_TOKEN;
const EMBED_MODEL_PATH = process.env.COGNEE_E2E_EMBED_MODEL_PATH;
const TOKENIZER_PATH = process.env.COGNEE_E2E_TOKENIZER_PATH;

const haveCreds =
  !!OPENAI_URL && !!OPENAI_TOKEN && !!EMBED_MODEL_PATH && !!TOKENIZER_PATH;

// Skip the whole suite cleanly when creds/model env are missing (CI default).
const describeMaybe = haveCreds ? describe : describe.skip;

describeMaybe("Phase-3 cognify (Tier-B, live LLM + embeddings)", () => {
  let tmpDir: string;
  let dbPath: string;
  const email = "phase3_cognify_tier_b@example.com";

  beforeAll(() => {
    tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "cognee-cognify-"));
    dbPath = path.join(tmpDir, "cognee.db");
  });

  afterAll(() => {
    try {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    } catch {
      // best effort
    }
  });

  function makeSettings() {
    return {
      system_root_directory: tmpDir,
      data_root_directory: path.join(tmpDir, "data"),
      relational_db_url: `sqlite:${dbPath}?mode=rwc`,
      default_user_email: email,
      // LLM (OpenAI-compatible).
      llm_provider: "openai",
      llm_endpoint: OPENAI_URL,
      llm_api_key: OPENAI_TOKEN,
      llm_model: process.env.OPENAI_MODEL || "gpt-4o-mini",
      // Embeddings (local ONNX BGE-Small).
      embedding_provider: "onnx",
      embedding_model_path: EMBED_MODEL_PATH,
      embedding_tokenizer_path: TOKENIZER_PATH,
    };
  }

  it(
    "runs a live add → cognify round-trip producing a non-empty graph",
    async () => {
      const handle = native.cogneeNew(makeSettings());
      const text =
        "Alan Turing was a British mathematician. He worked at Bletchley Park " +
        "during World War II and is considered the father of computer science.";

      const added = await native.cogneeAdd(
        handle,
        { type: "text", text },
        "tier_b_kg"
      );
      expect(added.addedCount).toBe(1);

      const result = await native.cogneeCognify(handle, "tier_b_kg");
      // A real extraction should yield chunks and at least one entity/edge.
      expect(result.chunks).toBeGreaterThan(0);
      expect(result.entities + result.edges).toBeGreaterThan(0);
      expect(result.alreadyCompleted).toBe(false);
    },
    120_000
  );

  it(
    "addAndCognify returns both summaries in one call",
    async () => {
      const handle = native.cogneeNew(makeSettings());
      const text =
        "Marie Curie was a physicist and chemist who conducted pioneering " +
        "research on radioactivity. She won two Nobel Prizes.";

      const both = await native.cogneeAddAndCognify(
        handle,
        { type: "text", text },
        "tier_b_combined"
      );
      expect(both.add.addedCount).toBe(1);
      expect(both.cognify.chunks).toBeGreaterThan(0);
    },
    120_000
  );

  it(
    "addAndCognify on all-duplicate input zeroes the cognify summary",
    async () => {
      const handle = native.cogneeNew(makeSettings());
      const text = "Duplicate detection round-trip payload.";

      // Seed the content first.
      await native.cogneeAdd(handle, { type: "text", text }, "tier_b_dup");

      // Re-adding via addAndCognify should add nothing and skip cognify.
      const both = await native.cogneeAddAndCognify(
        handle,
        { type: "text", text },
        "tier_b_dup"
      );
      expect(both.add.addedCount).toBe(0);
      expect(both.cognify.chunks).toBe(0);
      expect(both.cognify.entities).toBe(0);
    },
    120_000
  );
});
