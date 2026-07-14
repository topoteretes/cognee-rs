package ai.cognee;

import com.fasterxml.jackson.databind.JsonNode;

/**
 * A search result. The underlying payload is open-ended (the {@code result}
 * field is a tagged union and item {@code payload}s are arbitrary), so this
 * exposes the parsed tree via {@link #raw()} plus typed accessors for the
 * stable top-level fields.
 */
public final class SearchResponse {
    private final JsonNode root;

    SearchResponse(JsonNode root) {
        this.root = root;
    }

    /** The full parsed response tree (snake_case keys, as produced by the core). */
    public JsonNode raw() {
        return root;
    }

    /** The search strategy, or {@code null} if absent or not recognized by this binding. */
    public SearchType searchType() {
        return SearchType.fromWireOrNull(root.path("search_type").asText(null));
    }

    /** The `{kind, data}` result union node. */
    public JsonNode result() {
        return root.path("result");
    }

    public boolean onlyContext() {
        return root.path("only_context").asBoolean(false);
    }

    public boolean useCombinedContext() {
        return root.path("use_combined_context").asBoolean(false);
    }

    public boolean verbose() {
        return root.path("verbose").asBoolean(false);
    }
}
