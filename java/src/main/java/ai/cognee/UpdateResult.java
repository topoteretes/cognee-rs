package ai.cognee;

import ai.cognee.internal.Json;
import com.fasterxml.jackson.databind.JsonNode;

/** Result of {@link Cognee#update}: the deleted item, the new data, and any re-cognify result. */
public final class UpdateResult {
    private final JsonNode root;

    UpdateResult(JsonNode root) {
        this.root = root;
    }

    public JsonNode raw() { return root; }
    public String deletedDataId() { return root.path("deletedDataId").asText(); }
    public DeleteResult deleteResult() { return new DeleteResult(root.path("deleteResult")); }
    public JsonNode newData() { return root.path("newData"); }

    /** The re-cognify result, or null when nothing was re-cognified. */
    public CognifyResult cognifyResult() {
        JsonNode n = root.path("cognifyResult");
        return n.isObject() ? Json.fromNode(n, CognifyResult.class) : null;
    }
}
