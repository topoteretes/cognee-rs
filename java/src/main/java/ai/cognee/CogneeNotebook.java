package ai.cognee;

import com.fasterxml.jackson.databind.JsonNode;

/** A notebook. {@code cells} are open-ended, exposed via the tree. */
public final class CogneeNotebook {
    private final JsonNode root;

    CogneeNotebook(JsonNode root) {
        this.root = root;
    }

    public JsonNode raw() { return root; }
    public String id() { return root.path("id").asText(); }
    public String name() { return root.path("name").asText(); }
    public JsonNode cells() { return root.path("cells"); }
}
