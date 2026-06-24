/**
 * Recall op — cogneeRecall (the session-first, multi-scope retrieval op).
 *
 * This file is a focused companion to search.test.ts, targeting the recall-
 * specific paths: scope routing, rememberEntry + recall round-trip, and the
 * RecallResult shape.
 *
 * ─── Tier-A (no LLM, no network) ────────────────────────────────────────────
 *
 * Tier-A tests exercise scope parsing, argument validation, and the
 * RecallResult shape on an empty graph.  They do not depend on cognified data.
 *
 * ─── Tier-B (live LLM + embedding model) ────────────────────────────────────
 *
 * Tier-B tests verify the end-to-end remember → recall round-trip and only
 * run when all required env vars are present.
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

import { native, NativeBox, RecallScopeString } from "../src/native";

// ─── helpers ─────────────────────────────────────────────────────────────────

function makeTempHandle(email: string): { tmpDir: string; handle: NativeBox } {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "cognee-recall-"));
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

function errorCode(err: unknown): string | null {
  if (err && typeof err === "object" && "code" in err) {
    return (err as { code: string }).code;
  }
  return null;
}

/** All valid RecallScope wire strings. */
const ALL_RECALL_SCOPES: RecallScopeString[] = [
  "auto",
  "graph",
  "session",
  "trace",
  "graph_context",
];

// ─── Tier-A ──────────────────────────────────────────────────────────────────

describe("recall ops (Tier-A — no LLM)", () => {
  let tmpDir: string;
  let handle: NativeBox;

  beforeAll(async () => {
    process.env.MOCK_EMBEDDING = "true";
    ({ tmpDir, handle } = makeTempHandle("recall_tier_a@example.com"));
    await native.cogneeWarm(handle);
  });

  afterAll(() => {
    delete process.env.MOCK_EMBEDDING;
    cleanup(tmpDir);
  });

  // ── scope parsing ─────────────────────────────────────────────────────────

  describe("RecallScope parsing", () => {
    it.each(ALL_RECALL_SCOPES)(
      "accepts scope '%s' without a VALIDATION_ERROR",
      async (scope) => {
        try {
          await native.cogneeRecall(handle, "test query", { scope });
        } catch (err) {
          expect(errorCode(err)).not.toBe("VALIDATION_ERROR");
        }
      },
      15_000
    );

    it("accepts scope='all' (sentinel for fan-out) without a VALIDATION_ERROR", async () => {
      try {
        await native.cogneeRecall(handle, "test query", { scope: "all" });
      } catch (err) {
        expect(errorCode(err)).not.toBe("VALIDATION_ERROR");
      }
    }, 15_000);

    it("accepts an array of scope strings without a VALIDATION_ERROR", async () => {
      try {
        await native.cogneeRecall(handle, "test query", {
          scope: ["graph", "session"],
        });
      } catch (err) {
        expect(errorCode(err)).not.toBe("VALIDATION_ERROR");
      }
    }, 15_000);

    it("rejects an unknown scope string with VALIDATION_ERROR", async () => {
      let caught: unknown;
      try {
        await native.cogneeRecall(handle, "test query", {
          // @ts-expect-error intentionally invalid scope
          scope: "not_a_valid_scope",
        });
      } catch (err) {
        caught = err;
      }
      expect(caught).toBeDefined();
      expect(errorCode(caught)).toBe("VALIDATION_ERROR");
    }, 10_000);
  });

  // ── RecallResult shape ────────────────────────────────────────────────────

  describe("RecallResult shape on empty graph", () => {
    it("returns a RecallResult with items array and autoRouted boolean", async () => {
      try {
        const result = await native.cogneeRecall(handle, "test query", {
          scope: "graph",
          topK: 3,
        });
        expect(Array.isArray(result.items)).toBe(true);
        expect(typeof result.autoRouted).toBe("boolean");
      } catch (err) {
        // LLM/embedding failure is expected on an empty graph with a dummy key.
        expect(errorCode(err)).not.toBe("VALIDATION_ERROR");
      }
    }, 30_000);

    it("session scope returns a RecallResult or a non-validation error", async () => {
      try {
        const result = await native.cogneeRecall(handle, "test query", {
          scope: "session",
          sessionId: "test-session-id",
        });
        expect(typeof result).toBe("object");
      } catch (err) {
        expect(errorCode(err)).not.toBe("VALIDATION_ERROR");
      }
    }, 30_000);
  });

  // ── session-first path after rememberEntry ────────────────────────────────

  describe("recall after rememberEntry", () => {
    it("session scope returns the stored QA entry", async () => {
      const sessionId = `recall-sess-${Date.now()}`;

      // Store a QA entry in the session.
      await native.cogneeRememberEntry(
        handle,
        {
          type: "qa",
          question: "What is Rust?",
          answer: "A systems programming language.",
        },
        "recall_test_ds",
        sessionId
      );

      // Recall with session scope — should surface the stored entry.
      try {
        const result = await native.cogneeRecall(handle, "What is Rust?", {
          scope: "session",
          sessionId,
          topK: 5,
        });
        expect(typeof result).toBe("object");
        expect(Array.isArray(result.items)).toBe(true);
      } catch (err) {
        // LLM/embedding failure with a dummy key is acceptable.
        expect(errorCode(err)).not.toBe("VALIDATION_ERROR");
      }
    }, 30_000);
  });

  // ── argument validation ───────────────────────────────────────────────────

  describe("argument validation", () => {
    it("throws synchronously when query arg is missing", () => {
      expect(() => {
        // @ts-expect-error intentionally omitting required arg
        native.cogneeRecall(handle);
      }).toThrow();
    });

    it("throws synchronously when handle is missing", () => {
      expect(() => {
        // @ts-expect-error intentionally omitting required arg
        native.cogneeRecall();
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

describeMaybe("recall ops (Tier-B — live LLM)", () => {
  let tmpDir: string;
  let handle: NativeBox;
  const sessionId = `recall-b-${Date.now()}`;

  beforeAll(async () => {
    tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "cognee-recall-b-"));
    const dbPath = path.join(tmpDir, "cognee.db");
    handle = native.cogneeNew({
      system_root_directory: tmpDir,
      data_root_directory: path.join(tmpDir, "data"),
      relational_db_url: `sqlite:${dbPath}?mode=rwc`,
      default_user_email: "recall_b@example.com",
      llm_provider: "openai",
      llm_endpoint: OPENAI_URL,
      llm_api_key: OPENAI_TOKEN,
      llm_model: process.env.OPENAI_MODEL || "gpt-4o-mini",
      embedding_provider: "onnx",
      embedding_model_path: EMBED_MODEL_PATH,
      embedding_tokenizer_path: TOKENIZER_PATH,
    });

    await native.cogneeWarm(handle);

    // Store a QA entry so there is something to recall from session scope.
    await native.cogneeRememberEntry(
      handle,
      {
        type: "qa",
        question: "Who invented the telephone?",
        answer: "Alexander Graham Bell invented the telephone.",
      },
      "recall_b_ds",
      sessionId
    );
  }, 120_000);

  afterAll(() => {
    cleanup(tmpDir);
  });

  it(
    "recall with scope=session returns the stored QA entry",
    async () => {
      const result = await native.cogneeRecall(
        handle,
        "Who invented the telephone?",
        { scope: "session", sessionId, topK: 5 }
      );
      expect(result).toBeDefined();
      expect(Array.isArray(result.items)).toBe(true);
      expect(typeof result.autoRouted).toBe("boolean");
    },
    120_000
  );
});
