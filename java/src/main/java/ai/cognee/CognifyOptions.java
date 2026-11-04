package ai.cognee;

import java.util.Map;

/** Per-call options for {@link Cognee#cognify} / {@link Cognee#addAndCognify}. */
public final class CognifyOptions extends Options {
    public CognifyOptions tenant(String tenant) { put("tenant", tenant); return this; }
    public CognifyOptions chunkSize(int n) { put("chunkSize", n); return this; }
    /** Accepted but inert on the default chunking path: no chunker reads it, so it changes no chunk boundary (Python's DefaultChunkEngine does consume it, but that engine is unreachable). Only validation reads it, rejecting overlap >= chunk size. Kept for API compatibility; see CognifyConfig::chunk_overlap in crates/cognify/src/config.rs. */
    public CognifyOptions chunkOverlap(int n) { put("chunkOverlap", n); return this; }
    public CognifyOptions summarization(boolean b) { put("summarization", b); return this; }

    /**
     * Graph extractor for this run (Python's {@code extractor}): {@code "auto"} (the LLM when one
     * is usable, else the local GLiNER extractor), {@code "llm"}, {@code "gliner"} (alias
     * {@code "gliner_demo"}) or another registered kind. Overrides {@code GRAPH_EXTRACTOR}.
     */
    public CognifyOptions extractor(String kind) { put("extractor", kind); return this; }

    /**
     * A custom graph model's JSON Schema, given as JSON text, extracted instead of the default
     * {@code KnowledgeGraph} (Python's {@code graph_model}). {@code "{}"} means the default model.
     * The text is parsed and the schema validated natively: invalid JSON or an invalid schema fails
     * the call with a validation error.
     * See docs/guides/custom-graph-schema.md for the schema contract.
     */
    public CognifyOptions graphSchema(String schemaJson) { put("graphSchema", schemaJson); return this; }

    /** {@link #graphSchema(String)} for a schema already held as a JSON-shaped map. */
    public CognifyOptions graphSchema(Map<String, ?> schema) { put("graphSchema", schema); return this; }

    /**
     * The same custom graph model written in the spec DSL (Python's {@code GraphSchemaSpec}:
     * entities with primitive, enum and relation fields), given as JSON text and compiled natively
     * into a graph schema. An invalid spec, or one passed together with {@link #graphSchema(String)},
     * fails the call with a validation error.
     * See docs/guides/custom-graph-schema.md for the spec format.
     */
    public CognifyOptions graphSpec(String specJson) { put("graphSpec", specJson); return this; }

    /** {@link #graphSpec(String)} for a spec already held as a JSON-shaped map. */
    public CognifyOptions graphSpec(Map<String, ?> spec) { put("graphSpec", spec); return this; }

    /**
     * How each chunk links to the custom graph extracted from it: {@code "direct"} (the root, the
     * default) or {@code "all"} (every stored node). Requires {@link #graphSchema(String)} or
     * {@link #graphSpec(String)}; any other value fails the call with a validation error.
     */
    public CognifyOptions chunkAttachment(String attachment) {
        put("chunkAttachment", attachment);
        return this;
    }
}
