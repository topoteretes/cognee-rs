package ai.cognee;

import ai.cognee.internal.Json;
import com.fasterxml.jackson.databind.JsonNode;

/** Result of {@link Cognee#update}: the document's id (unchanged by the update) and any re-cognify result. */
public final class UpdateResult {
    private final JsonNode root;

    UpdateResult(JsonNode root) {
        this.root = root;
    }

    public JsonNode raw() { return root; }
    /** The document's id: the row the requested id resolved to, kept by the update. */
    public String dataId() { return root.path("dataId").asText(); }
    public DeleteResult deleteResult() { return new DeleteResult(root.path("deleteResult")); }

    /** The re-cognify result, or null when nothing was re-cognified. */
    public CognifyResult cognifyResult() {
        JsonNode n = root.path("cognifyResult");
        return n.isObject() ? Json.fromNode(n, CognifyResult.class) : null;
    }
}
