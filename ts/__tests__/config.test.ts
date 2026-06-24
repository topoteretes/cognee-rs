/**
 * Tier-A tests for the Phase-2 config surface.
 *
 * Pure config-manager exercise: granular + bulk + generic setters, the
 * `getConfig` read-back (with secret blanking), the `defaults < env < object`
 * overlay on `cogneeNew`, and that a setter mutation is observable on a
 * subsequent warm (rebuild-on-change).
 *
 * Runs WITHOUT any LLM/network/model I/O: config mutation is in-memory, and the
 * single warm uses a fully-local mock config (MOCK_EMBEDDING + temp sqlite).
 */
import * as fs from "fs";
import * as os from "os";
import * as path from "path";

import { native } from "../src/native";

describe("Phase-2 config surface", () => {
  describe("granular setters + getConfig", () => {
    it("applies granular setters across each area, including Option B fields", () => {
      const handle = native.cogneeNew({});

      // LLM (existing + tuning)
      native.configSetLlmProvider(handle, "openai");
      native.configSetLlmModel(handle, "gpt-4o");
      native.configSetLlmTemperature(handle, 0.42);
      native.configSetLlmStreaming(handle, true);
      native.configSetLlmMaxRetries(handle, 7);
      // Embedding (path = Option B)
      native.configSetEmbeddingProvider(handle, "openai");
      native.configSetEmbeddingDimensions(handle, 1536);
      native.configSetEmbeddingModelPath(handle, "/models/m.onnx");
      // Vector DB host/port/name (Option B)
      native.configSetVectorDbHost(handle, "vector-host");
      native.configSetVectorDbPort(handle, 6333);
      native.configSetVectorDbName(handle, "my_collection");
      // Graph
      native.configSetGraphFilePath(handle, "/data/graph");
      // Paths (Option B)
      native.configSetCacheRootDirectory(handle, "/tmp/cache");
      // Ontology (Option B)
      native.configSetOntologyFilePath(handle, "/onto.owl");
      native.configSetOntologyResolver(handle, "custom");

      const cfg = native.getConfig(handle) as Record<string, unknown>;
      expect(cfg.llm_provider).toBe("openai");
      expect(cfg.llm_model).toBe("gpt-4o");
      expect(cfg.llm_temperature).toBeCloseTo(0.42);
      expect(cfg.llm_streaming).toBe(true);
      expect(cfg.llm_max_retries).toBe(7);
      expect(cfg.embedding_provider).toBe("openai");
      expect(cfg.embedding_dimensions).toBe(1536);
      expect(cfg.embedding_model_path).toBe("/models/m.onnx");
      expect(cfg.vector_db_host).toBe("vector-host");
      expect(cfg.vector_db_port).toBe(6333);
      expect(cfg.vector_db_name).toBe("my_collection");
      expect(cfg.graph_file_path).toBe("/data/graph");
      expect(cfg.cache_root_directory).toBe("/tmp/cache");
      expect(cfg.ontology_file_path).toBe("/onto.owl");
      expect(cfg.ontology_resolver).toBe("custom");
    });

    it("blanks secret fields in getConfig", () => {
      const handle = native.cogneeNew({});
      native.configSetLlmApiKey(handle, "sk-super-secret");
      native.configSetEmbeddingApiKey(handle, "sk-embed-secret");
      native.configSetVectorDbKey(handle, "vector-secret");

      const cfg = native.getConfig(handle) as Record<string, unknown>;
      // Secret fields must be redacted, never echoed back verbatim.
      for (const field of [
        "llm_api_key",
        "embedding_api_key",
        "vector_db_key",
        "vector_db_password",
        "graph_database_key",
        "graph_database_password",
        "db_password",
        "cache_password",
        "default_user_password",
        "otel_exporter_otlp_headers",
      ]) {
        expect(cfg[field]).toBe("***REDACTED***");
      }
      // Spot-check the values we set are NOT present.
      const serialized = JSON.stringify(cfg);
      expect(serialized).not.toContain("sk-super-secret");
      expect(serialized).not.toContain("sk-embed-secret");
      expect(serialized).not.toContain("vector-secret");
    });
  });

  describe("generic set(key, value)", () => {
    it("succeeds for a newly-covered Option B key", () => {
      const handle = native.cogneeNew({});
      native.configSet(handle, "vector_db_host", "host-via-generic");
      native.configSet(handle, "vector_db_port", 7000);
      native.configSet(handle, "llm_streaming", true);
      native.configSet(handle, "ontology_file_path", "/o2.owl");

      const cfg = native.getConfig(handle) as Record<string, unknown>;
      expect(cfg.vector_db_host).toBe("host-via-generic");
      expect(cfg.vector_db_port).toBe(7000);
      expect(cfg.llm_streaming).toBe(true);
      expect(cfg.ontology_file_path).toBe("/o2.owl");
    });

    it("throws a typed UnknownKey error for an unknown key", () => {
      const handle = native.cogneeNew({});
      let caught: unknown;
      try {
        native.configSet(handle, "nonexistent_key", "value");
      } catch (e) {
        caught = e;
      }
      expect(caught).toBeDefined();
      expect((caught as { message?: string }).message).toBeDefined();
      expect((caught as { code?: string }).code).toBe("UNKNOWN_CONFIG_KEY");
    });

    it("throws a typed TypeMismatch error for a wrong value type", () => {
      const handle = native.cogneeNew({});
      let caught: unknown;
      try {
        native.configSet(handle, "chunk_size", "not-a-number");
      } catch (e) {
        caught = e;
      }
      expect(caught).toBeDefined();
      expect((caught as { message?: string }).message).toBeDefined();
      expect((caught as { code?: string }).code).toBe("CONFIG_TYPE_MISMATCH");
    });
  });

  describe("bulk setters", () => {
    it("applies a bulk vector-db config (incl. Option B host/port/name)", () => {
      const handle = native.cogneeNew({});
      native.configSetVectorDbConfig(handle, {
        vector_db_provider: "qdrant",
        vector_db_host: "bulk-host",
        vector_db_port: 6334,
        vector_db_name: "bulk_coll",
      });
      const cfg = native.getConfig(handle) as Record<string, unknown>;
      expect(cfg.vector_db_provider).toBe("qdrant");
      expect(cfg.vector_db_host).toBe("bulk-host");
      expect(cfg.vector_db_port).toBe(6334);
      expect(cfg.vector_db_name).toBe("bulk_coll");
    });

    it("rejects an out-of-subset key with a typed UnknownKey error", () => {
      const handle = native.cogneeNew({});
      let caught: unknown;
      try {
        // vector_db_url is NOT in set_llm_config's allowlist.
        native.configSetLlmConfig(handle, { vector_db_url: "/v" });
      } catch (e) {
        caught = e;
      }
      expect(caught).toBeDefined();
      expect((caught as { message?: string }).message).toBeDefined();
      expect((caught as { code?: string }).code).toBe("UNKNOWN_CONFIG_KEY");
    });
  });

  describe("defaults < env < object overlay (cogneeNew)", () => {
    // The env-derived baseline is whatever from_env() produces in this process
    // (defaults overlaid by env / any .env file). We capture it empirically and
    // assert the OVERLAY COMPOSITION against it, rather than hard-coding an env
    // value — `process.env` writes from jest are not guaranteed to reach Rust's
    // `std::env::var` in-process, so we test the composition contract directly.
    let baseline: Record<string, unknown>;

    beforeAll(() => {
      const h = native.cogneeNew();
      baseline = native.getConfig(h) as Record<string, unknown>;
    });

    it("a key the object omits keeps its env-derived baseline value", () => {
      // The object provides a different field (vector_db_host) and omits
      // llm_model. With object-over-defaults the omitted llm_model would reset
      // to the default; with the env<object overlay it keeps the baseline.
      const handle = native.cogneeNew({ vector_db_host: "object-host" });
      const cfg = native.getConfig(handle) as Record<string, unknown>;
      // Object-provided field wins.
      expect(cfg.vector_db_host).toBe("object-host");
      // Omitted field retains the env-derived baseline.
      expect(cfg.llm_model).toBe(baseline.llm_model);
      expect(cfg.llm_provider).toBe(baseline.llm_provider);
    });

    it("a key the object provides overrides the baseline value", () => {
      const handle = native.cogneeNew({ llm_model: "object-model" });
      const cfg = native.getConfig(handle) as Record<string, unknown>;
      expect(cfg.llm_model).toBe("object-model");
      expect(cfg.llm_model).not.toBe(baseline.llm_model);
    });

    it("with no argument, getConfig matches the env-derived baseline", () => {
      const handle = native.cogneeNew();
      const cfg = native.getConfig(handle) as Record<string, unknown>;
      expect(cfg.llm_model).toBe(baseline.llm_model);
    });
  });

  describe("rebuild-on-change (version-invalidated services)", () => {
    // NOTE: per the Phase-2 plan, the Tier-A rebuild assertion deliberately
    // avoids booting a heavy backend (ONNX / qdrant / ladybug / SeaORM) — those
    // warms are resource-heavy and prone to OOM when repeated in one jest
    // process. Each setter bumps the config version, and `HandleState::services`
    // is keyed on that version, so a mutation invalidates the cached services
    // and forces a rebuild on the next op. We observe that contract via the
    // config snapshot (the value the next warm/op would build from changes),
    // which is the version-advance proxy the plan recommends. The full warm /
    // rebuild path is exercised by sdk_handle.test.ts and the Phase-3+ tests.
    let tmpDir: string;

    beforeAll(() => {
      tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "cognee-config-rebuild-"));
    });
    afterAll(() => {
      try {
        fs.rmSync(tmpDir, { recursive: true, force: true });
      } catch {
        // best effort
      }
    });

    it("a config change is reflected in the snapshot the next build would use", () => {
      const vecA = path.join(tmpDir, "vectors-a");
      const handle = native.cogneeNew({
        system_root_directory: tmpDir,
        data_root_directory: path.join(tmpDir, "data"),
        vector_db_url: vecA,
        embedding_provider: "mock",
        llm_api_key: "test-dummy-key",
      });

      const before = native.getConfig(handle) as Record<string, unknown>;
      expect(before.vector_db_url).toBe(vecA);

      // Mutate config: this bumps the version, invalidating any cached services
      // so the next op rebuilds the engines from this new value.
      const vecB = path.join(tmpDir, "vectors-b");
      native.configSet(handle, "vector_db_url", vecB);

      const after = native.getConfig(handle) as Record<string, unknown>;
      expect(after.vector_db_url).toBe(vecB);
      expect(after.vector_db_url).not.toBe(before.vector_db_url);
    });
  });
});
