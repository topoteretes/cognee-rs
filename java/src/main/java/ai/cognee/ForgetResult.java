package ai.cognee;

import com.fasterxml.jackson.databind.JsonNode;

public final class ForgetResult {
    private final JsonNode root;

    ForgetResult(JsonNode root) {
        this.root = root;
    }

    public JsonNode raw() { return root; }
    public JsonNode target() { return root.path("target"); }
    public DeleteResult deleteResult() { return new DeleteResult(root.path("deleteResult")); }
}
