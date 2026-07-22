package ai.cognee;

/** Search strategy; enum constant names are the exact wire values. */
public enum SearchType {
    SUMMARIES,
    CHUNKS,
    RAG_COMPLETION,
    TRIPLET_COMPLETION,
    GRAPH_COMPLETION,
    GRAPH_SUMMARY_COMPLETION,
    CYPHER,
    NATURAL_LANGUAGE,
    GRAPH_COMPLETION_COT,
    GRAPH_COMPLETION_CONTEXT_EXTENSION,
    FEELING_LUCKY,
    FEEDBACK,
    TEMPORAL,
    CODING_RULES,
    CHUNKS_LEXICAL;

    /** Wire string (identical to {@link #name()}). */
    public String wire() {
        return name();
    }

    public static SearchType fromWire(String wire) {
        return valueOf(wire);
    }

    /**
     * Tolerant variant of {@link #fromWire}: returns {@code null} for a
     * {@code null}, empty, or unrecognized wire value instead of throwing.
     * This keeps deserialization forward-compatible when the core adds a new
     * search type this binding does not yet know about.
     */
    public static SearchType fromWireOrNull(String wire) {
        if (wire == null || wire.isEmpty()) {
            return null;
        }
        try {
            return valueOf(wire);
        } catch (IllegalArgumentException e) {
            return null;
        }
    }
}
