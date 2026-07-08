/**
 * Memory ops — remember / rememberEntry / memify / improve.
 *
 * ─── Tier-A (no LLM, no network) ────────────────────────────────────────────
 *
 * Tier-A tests exercise argument-validation paths that return typed errors
 * without reaching the LLM: bad argument shapes, missing required args, and
 * the warm/init flow.
 *
 * ─── Tier-B (live LLM + embedding model) ────────────────────────────────────
 *
 * Tier-B tests run the full remember → memify → improve round-trip and only
 * activate when all required env vars are present. They skip cleanly in CI.
 *
 * Required env for Tier-B:
 *   OPENAI_URL                   OpenAI-compatible API base URL
 *   OPENAI_TOKEN                 API key
 *   OPENAI_MODEL                 (optional) model, default gpt-4o-mini
 *   COGNEE_E2E_EMBED_MODEL_PATH  path to a BGE-Small-v1.5 ONNX model
 *   COGNEE_E2E_TOKENIZER_PATH    path to the matching tokenizer.json
 */
import * as fs from "fs";
import * as os from "os";
import * as path from "path";

import { native, NativeBox } from "../src/native";

// ─── helpers ─────────────────────────────────────────────────────────────────

function makeTempHandle(email: string): { tmpDir: string; handle: NativeBox } {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "cognee-mem-"));
  const dbPath = path.join(tmpDir, "cognee.db");
  const handle = native.cogneeNew({
    system_root_directory: tmpDir,
    data_root_directory: path.join(tmpDir, "data"),
    relational_db_url: `sqlite:${dbPath}?mode=rwc`,
    embedding_provider: "mock",
    llm_api_key: "test-dummy-key",
    default_user_email: email,
  });
  return { tmpDir, handle };
}

function cleanup(dir: string) {
  try {
    fs.rmSync(dir, { recursive: true, force: true });
  } catch {
    /* best effort */
  }
}

// ─── Tier-A ──────────────────────────────────────────────────────────────────

describe("memory ops (Tier-A — no LLM)", () => {
  let tmpDir: string;
  let handle: NativeBox;

  beforeAll(async () => {
    process.env.MOCK_EMBEDDING = "true";
    ({ tmpDir, handle } = makeTempHandle("memory_tier_a@example.com"));
    await native.cogneeWarm(handle);
  });

  afterAll(() => {
    delete process.env.MOCK_EMBEDDING;
    cleanup(tmpDir);
  });

  // ── rememberEntry — deterministic path (writes a QA entry to session) ──────

  describe("rememberEntry", () => {
    it("stores a qa entry and returns a RememberResult", async () => {
      const sessionId = `mem-test-${Date.now()}`;
      const result = await native.cogneeRememberEntry(
        handle,
        {
          type: "qa",
          question: "What is the capital of France?",
          answer: "Paris",
        },
        "mem_test_ds",
        sessionId
      );
      expect(result).toBeDefined();
      // The result should have a status field.
      expect(typeof result.status).toBe("string");
    });

    it("stores a feedback entry and returns a RememberResult", async () => {
      const sessionId = `mem-test-fb-${Date.now()}`;
      const result = await native.cogneeRememberEntry(
        handle,
        {
          type: "feedback",
          feedbackText: "Good answer",
          feedbackScore: 5,
          qaId: "00000000-0000-0000-0000-000000000001",
        },
        "mem_fb_ds",
        sessionId
      );
      expect(result).toBeDefined();
      expect(typeof result.status).toBe("string");
    });
  });

  // ── memify — no existing graph → should return alreadyCompleted=false ───────

  describe("memify", () => {
    it("memify on an empty graph returns a MemifyResult without error", async () => {
      const h = makeTempHandle("memify_empty@example.com");
      await native.cogneeWarm(h.handle);
      try {
        const result = await native.cogneeMemify(h.handle);
        expect(typeof result).toBe("object");
        expect(typeof result.tripletCount).toBe("number");
        expect(typeof result.indexedCount).toBe("number");
        expect(typeof result.alreadyCompleted).toBe("boolean");
      } finally {
        cleanup(h.tmpDir);
      }
    });

    it("memify accepts optional options without error", async () => {
      const h = makeTempHandle("memify_opts@example.com");
      await native.cogneeWarm(h.handle);
      try {
        const result = await native.cogneeMemify(h.handle, {});
        expect(typeof result).toBe("object");
      } finally {
        cleanup(h.tmpDir);
      }
    });
  });

  // ── improve — runs on empty graph, returns structured result ─────────────────

  describe("improve", () => {
    it("improve on an empty graph returns an ImproveResult without error", async () => {
      const h = makeTempHandle("improve_empty@example.com");
      await native.cogneeWarm(h.handle);
      try {
        const result = await native.cogneeImprove(h.handle, {
          datasetName: "improve_test_ds",
        });
        expect(typeof result).toBe("object");
        expect(Array.isArray(result.stagesRun)).toBe(true);
        expect(typeof result.feedbackEntriesProcessed).toBe("number");
        expect(typeof result.sessionsPersisted).toBe("number");
      } finally {
        cleanup(h.tmpDir);
      }
    });
  });

  // ── argument-validation ──────────────────────────────────────────────────────

  describe("argument validation", () => {
    it("cogneeRemember throws synchronously when dataInput is missing", () => {
      expect(() => {
        // @ts-expect-error intentionally omitting required args
        native.cogneeRemember(handle);
      }).toThrow();
    });

    it("cogneeMemify throws synchronously when handle is missing", () => {
      expect(() => {
        // @ts-expect-error intentionally omitting required arg
        native.cogneeMemify();
      }).toThrow();
    });

    it("cogneeImprove throws synchronously when handle is missing", () => {
      expect(() => {
        // @ts-expect-error intentionally omitting required arg
        native.cogneeImprove();
      }).toThrow();
    });
  });
});

// ─── Tier-B (live LLM) ───────────────────────────────────────────────────────

const OPENAI_URL = process.env.OPENAI_URL;
const OPENAI_TOKEN = process.env.OPENAI_TOKEN;
const EMBED_MODEL_PATH = process.env.COGNEE_E2E_EMBED_MODEL_PATH;
const TOKENIZER_PATH = process.env.COGNEE_E2E_TOKENIZER_PATH;

const haveCreds =
  !!OPENAI_URL && !!OPENAI_TOKEN && !!EMBED_MODEL_PATH && !!TOKENIZER_PATH;

const describeMaybe = haveCreds ? describe : describe.skip;

describeMaybe("memory ops (Tier-B — live LLM)", () => {
  let tmpDir: string;
  let handle: NativeBox;

  beforeAll(async () => {
    tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "cognee-mem-b-"));
    const dbPath = path.join(tmpDir, "cognee.db");
    handle = native.cogneeNew({
      system_root_directory: tmpDir,
      data_root_directory: path.join(tmpDir, "data"),
      relational_db_url: `sqlite:${dbPath}?mode=rwc`,
      default_user_email: "mem_tier_b@example.com",
      llm_provider: "openai",
      llm_endpoint: OPENAI_URL,
      llm_api_key: OPENAI_TOKEN,
      llm_model: process.env.OPENAI_MODEL || "gpt-4o-mini",
      embedding_provider: "onnx",
      embedding_model_path: EMBED_MODEL_PATH,
      embedding_tokenizer_path: TOKENIZER_PATH,
    });
    await native.cogneeWarm(handle);
  }, 120_000);

  afterAll(() => {
    cleanup(tmpDir);
  });

  it(
    "cogneeRemember stores text and returns PipelineRunCompleted",
    async () => {
      const result = await native.cogneeRemember(
        handle,
        { type: "text", text: "Nikola Tesla invented the AC motor." },
        "mem_b_ds"
      );
      expect(result).toBeDefined();
      expect(result.status).toBe("PipelineRunCompleted");
      // `remember` results are snake_case (Python-parity wire shape); see #46.
      expect(result.dataset_name).toBe("mem_b_ds");
    },
    300_000
  );

  it(
    "cogneeMemify indexes triplets and returns MemifyResult",
    async () => {
      const result = await native.cogneeMemify(handle);
      expect(typeof result.tripletCount).toBe("number");
      expect(typeof result.indexedCount).toBe("number");
      expect(typeof result.alreadyCompleted).toBe("boolean");
    },
    120_000
  );

  it(
    "cogneeImprove returns ImproveResult with stagesRun array",
    async () => {
      const result = await native.cogneeImprove(handle, {
        datasetName: "mem_b_ds",
      });
      expect(Array.isArray(result.stagesRun)).toBe(true);
      expect(typeof result.feedbackEntriesProcessed).toBe("number");
      expect(typeof result.sessionsPersisted).toBe("number");
    },
    120_000
  );
});
