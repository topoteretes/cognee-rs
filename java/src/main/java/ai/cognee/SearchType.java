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
}
