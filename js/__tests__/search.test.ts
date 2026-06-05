/**
 * Phase 4 — retrieval: `cogneeSearch` and `cogneeRecall`.
 *
 * ─── Tier-A (no LLM, no network) ────────────────────────────────────────────
 *
 * All tests in the top-level `describe` block run in CI (`js-check`).
 * They exercise the input-marshalling and type-parsing paths without requiring
 * a working LLM or cognified data:
 *
 *   1. SearchType wire names: every SCREAMING_SNAKE_CASE variant is accepted
 *      by the Rust parser (the call may subsequently fail with a LLM error, but
 *      it must NOT fail with a VALIDATION_ERROR). An unknown string must always
 *      produce a VALIDATION_ERROR.
 *   2. RecallScope wire strings: every valid scope string (including "all") is
 *      accepted by `normalize_scope`; an unknown scope produces a
 *      VALIDATION_ERROR before any LLM call.
 *   3. Response contract: `cogneeSearch` with `onlyContext: true` (skips LLM
 *      completion) against an empty graph returns a well-formed `SearchResponse`.
 *   4. Bad-arg rejection: missing required positional args throw synchronously.
 *
 * ─── Tier-B (live LLM + cognified data) ─────────────────────────────────────
 *
 * The nested `describe.skip` / env-gated suite exercises the actual
 * add → cognify → search/recall round-trip and only runs when all required
 * env vars are present. It skips cleanly in CI.
 *
 * Required env for Tier-B (mirrors Rust workspace test harness):
 *   OPENAI_URL                   OpenAI-compatible API base URL
 *   OPENAI_TOKEN                 API key
 *   OPENAI_MODEL                 (optional) model, default gpt-4o-mini
 *   COGNEE_E2E_EMBED_MODEL_PATH  path to a BGE-Small-v1.5 ONNX model
 *   COGNEE_E2E_TOKENIZER_PATH    path to the matching tokenizer.json
 */
import * as fs from "fs";
import * as os from "os";
import * as path from "path";

import { native, NativeBox, SearchTypeString } from "../src/native";

// ─── Tier-A setup ─────────────────────────────────────────────────────────

/** All 15 SCREAMING_SNAKE_CASE SearchType wire names. */
const ALL_SEARCH_TYPES: SearchTypeString[] = [
  "SUMMARIES",
  "CHUNKS",
  "RAG_COMPLETION",
  "TRIPLET_COMPLETION",
  "GRAPH_COMPLETION",
  "GRAPH_SUMMARY_COMPLETION",
  "CYPHER",
  "NATURAL_LANGUAGE",
  "GRAPH_COMPLETION_COT",
  "GRAPH_COMPLETION_CONTEXT_EXTENSION",
  "FEELING_LUCKY",
  "FEEDBACK",
  "TEMPORAL",
  "CODING_RULES",
  "CHUNKS_LEXICAL",
];

/** All valid RecallScope wire strings (including "all" sentinel). */
const ALL_RECALL_SCOPES = [
  "auto",
  "graph",
  "session",
  "trace",
  "graph_context",
  "all",
] as const;

/**
 * Extract the `code` property from a thrown error, or return `null` if the
 * thrown value has no `code`.
 */
function errorCode(err: unknown): string | null {
  if (err && typeof err === "object" && "code" in err) {
    return (err as { code: string }).code;
  }
  return null;
}

describe("Phase-4 search (Tier-A, no LLM)", () => {
  let tmpDir: string;
  let dbPath: string;
  let handle: NativeBox;
  const email = "phase4_search_tier_a@example.com";

  beforeAll(async () => {
    process.env.MOCK_EMBEDDING = "true";
    tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "cognee-search-"));
    dbPath = path.join(tmpDir, "cognee.db");
    handle = native.cogneeNew({
      system_root_directory: tmpDir,
      data_root_directory: path.join(tmpDir, "data"),
      relational_db_url: `sqlite:${dbPath}?mode=rwc`,
      embedding_provider: "mock",
      llm_api_key: "test-dummy-key",
      default_user_email: email,
    });
    await native.cogneeWarm(handle);
  });

  afterAll(() => {
    delete process.env.MOCK_EMBEDDING;
    try {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    } catch {
      // best effort
    }
  });

  // ── 1. SearchType wire-name parsing ─────────────────────────────────────
  //
  // Strategy: pass each type and assert the rejection (if any) is NOT a
  // VALIDATION_ERROR.  A VALIDATION_ERROR means the type string was rejected
  // at the parsing stage, which would be a bug. A RUNTIME_ERROR (LLM auth
  // failure, empty graph, etc.) is expected with a dummy key and empty DB.

  describe("SearchType wire-name parsing", () => {
    it("has exactly 15 variants", () => {
      expect(ALL_SEARCH_TYPES).toHaveLength(15);
    });

    it.each(ALL_SEARCH_TYPES)(
      "accepts search type %s without a VALIDATION_ERROR",
      async (searchType) => {
        try {
          await native.cogneeSearch(handle, "test query", { searchType });
          // If it succeeded (e.g. empty-graph fast-path), all good.
        } catch (err) {
          // Any failure must NOT be a VALIDATION_ERROR — that would mean the
          // Rust parser rejected the SearchType string.
          expect(errorCode(err)).not.toBe("VALIDATION_ERROR");
        }
      },
      15_000
    );

    it("rejects an unknown SearchType string with VALIDATION_ERROR", async () => {
      let caught: unknown;
      try {
        await native.cogneeSearch(handle, "test query", {
          searchType: "NOT_A_REAL_TYPE" as SearchTypeString,
        });
      } catch (err) {
        caught = err;
      }
      expect(caught).toBeDefined();
      expect(errorCode(caught)).toBe("VALIDATION_ERROR");
    }, 10_000);
  });

  // ── 2. RecallScope wire-string parsing ──────────────────────────────────
  //
  // Same strategy: a valid scope must not produce a VALIDATION_ERROR.

  describe("RecallScope wire-string parsing", () => {
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
          // @ts-expect-error: intentionally passing an invalid scope
          scope: "not_a_valid_scope",
        });
      } catch (err) {
        caught = err;
      }
      expect(caught).toBeDefined();
      expect(errorCode(caught)).toBe("VALIDATION_ERROR");
    }, 10_000);
  });

  // ── 3. Response contract (onlyContext=true bypasses LLM) ─────────────────
  //
  // With `onlyContext: true` the orchestrator returns raw context items
  // without running an LLM completion. On an empty graph the vector
  // collection may not exist yet, which yields a RUNTIME_ERROR (not a
  // VALIDATION_ERROR). Either success or a RUNTIME_ERROR is acceptable here.

  describe("SearchResponse shape (onlyContext, empty graph)", () => {
    it(
      "cogneeSearch with CHUNKS + onlyContext either returns a SearchResponse or a RUNTIME_ERROR",
      async () => {
        try {
          const result = await native.cogneeSearch(handle, "test query", {
            searchType: "CHUNKS",
            onlyContext: true,
          });
          expect(typeof result).toBe("object");
          expect(result).toHaveProperty("search_type");
          expect(result).toHaveProperty("result");
        } catch (err) {
          // On an empty graph the vector collection may not exist yet,
          // producing a RUNTIME_ERROR. That is acceptable — it is NOT a
          // VALIDATION_ERROR (which would indicate a bug in type parsing).
          expect(errorCode(err)).not.toBe("VALIDATION_ERROR");
        }
      },
      30_000
    );
  });

  // ── 4. RecallResult shape (empty graph, no session) ──────────────────────

  describe("RecallResult shape (empty graph)", () => {
    it("cogneeRecall returns a valid RecallResult even on an empty graph", async () => {
      let result: ReturnType<(typeof native)["cogneeRecall"]> extends Promise<
        infer R
      >
        ? R
        : never;
      try {
        result = await native.cogneeRecall(handle, "test query", {
          scope: "graph",
          topK: 3,
        });
        // If it succeeds, validate the shape.
        expect(Array.isArray(result.items)).toBe(true);
        expect(typeof result.autoRouted).toBe("boolean");
        expect(
          result.searchTypeUsed === null ||
            ALL_SEARCH_TYPES.includes(result.searchTypeUsed as SearchTypeString)
        ).toBe(true);
      } catch (err) {
        // LLM / embedding failure with dummy key is expected; NOT a validation error.
        expect(errorCode(err)).not.toBe("VALIDATION_ERROR");
      }
    }, 30_000);
  });

  // ── 5. Bad-arg rejection (sync) ──────────────────────────────────────────

  describe("argument validation", () => {
    it("cogneeSearch throws synchronously when query arg is missing", () => {
      expect(() => {
        // @ts-expect-error: intentionally omitting required arg
        native.cogneeSearch(handle);
      }).toThrow();
    });

    it("cogneeRecall throws synchronously when query arg is missing", () => {
      expect(() => {
        // @ts-expect-error: intentionally omitting required arg
        native.cogneeRecall(handle);
      }).toThrow();
    });
  });
});

// ─── Tier-B (live LLM + cognified data) ───────────────────────────────────

const OPENAI_URL = process.env.OPENAI_URL;
const OPENAI_TOKEN = process.env.OPENAI_TOKEN;
const EMBED_MODEL_PATH = process.env.COGNEE_E2E_EMBED_MODEL_PATH;
const TOKENIZER_PATH = process.env.COGNEE_E2E_TOKENIZER_PATH;

const haveCreds =
  !!OPENAI_URL && !!OPENAI_TOKEN && !!EMBED_MODEL_PATH && !!TOKENIZER_PATH;

// Skip the entire Tier-B suite cleanly when creds/model env are missing (CI default).
const describeMaybe = haveCreds ? describe : describe.skip;

describeMaybe(
  "Phase-4 search (Tier-B, live LLM + cognified data)",
  () => {
    let tmpDir: string;
    let dbPath: string;
    let handle: NativeBox;
    const email = "phase4_search_tier_b@example.com";

    beforeAll(async () => {
      tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "cognee-search-b-"));
      dbPath = path.join(tmpDir, "cognee.db");

      handle = native.cogneeNew({
        system_root_directory: tmpDir,
        data_root_directory: path.join(tmpDir, "data"),
        relational_db_url: `sqlite:${dbPath}?mode=rwc`,
        default_user_email: email,
        llm_provider: "openai",
        llm_endpoint: OPENAI_URL,
        llm_api_key: OPENAI_TOKEN,
        llm_model: process.env.OPENAI_MODEL || "gpt-4o-mini",
        embedding_provider: "onnx",
        embedding_model_path: EMBED_MODEL_PATH,
        embedding_tokenizer_path: TOKENIZER_PATH,
      });

      // Ingest and cognify a small text so the graph is non-empty.
      const text =
        "Marie Curie was a Polish physicist and chemist. She won two Nobel Prizes, " +
        "one in Physics (1903) and one in Chemistry (1911).";

      await native.cogneeAddAndCognify(
        handle,
        { type: "text", text },
        "tier_b_search"
      );
    }, 300_000);

    afterAll(() => {
      try {
        fs.rmSync(tmpDir, { recursive: true, force: true });
      } catch {
        // best effort
      }
    });

    it(
      "cogneeSearch GRAPH_COMPLETION returns a SearchResponse",
      async () => {
        const result = await native.cogneeSearch(
          handle,
          "Who is Marie Curie?",
          { searchType: "GRAPH_COMPLETION" }
        );
        expect(result).toBeDefined();
        expect(result).toHaveProperty("result");
      },
      120_000
    );

    it(
      "cogneeSearch CHUNKS returns items",
      async () => {
        const result = await native.cogneeSearch(handle, "Marie Curie", {
          searchType: "CHUNKS",
          topK: 3,
        });
        expect(result).toBeDefined();
        expect(result.result).toBeDefined();
      },
      60_000
    );

    it(
      "cogneeRecall returns a valid RecallResult",
      async () => {
        const result = await native.cogneeRecall(
          handle,
          "What did Marie Curie win?",
          { scope: "graph", topK: 5 }
        );
        expect(result).toBeDefined();
        expect(Array.isArray(result.items)).toBe(true);
        expect(typeof result.autoRouted).toBe("boolean");
      },
      120_000
    );

    it(
      "cogneeRecall with scope='all' fans out across sources",
      async () => {
        const result = await native.cogneeRecall(
          handle,
          "Nobel Prize chemistry",
          { scope: "all", topK: 5 }
        );
        expect(result).toBeDefined();
        expect(Array.isArray(result.items)).toBe(true);
      },
      120_000
    );
  }
);
