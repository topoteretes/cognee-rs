package ai.cognee;

import com.fasterxml.jackson.databind.JsonNode;

/** Result of {@code remember}/{@code rememberEntry}. Keys are snake_case
 *  (Python-SDK parity); exposed via {@link #raw()}. */
public final class RememberResult {
    private final JsonNode root;

    RememberResult(JsonNode root) {
        this.root = root;
    }

    public JsonNode raw() {
        return root;
    }
}
