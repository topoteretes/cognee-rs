package ai.cognee;

import com.fasterxml.jackson.databind.JsonNode;

/** A recall result. Top-level keys are camelCase; {@link #searchResponse()} is
 *  the nested (open-ended) search response, or null. */
public final class RecallResult {
    private final JsonNode root;

    RecallResult(JsonNode root) {
        this.root = root;
    }

    public JsonNode raw() {
        return root;
    }

    /** The recalled memory items (open-ended array). */
    public JsonNode items() {
        return root.path("items");
    }

    /** The effective search type, or null when unset or not recognized by this binding. */
    public SearchType searchTypeUsed() {
        return SearchType.fromWireOrNull(root.path("searchTypeUsed").asText(null));
    }

    public boolean autoRouted() {
        return root.path("autoRouted").asBoolean(false);
    }

    /** The nested search response, or null if absent. */
    public SearchResponse searchResponse() {
        JsonNode n = root.path("searchResponse");
        return n.isObject() ? new SearchResponse(n) : null;
    }
}
