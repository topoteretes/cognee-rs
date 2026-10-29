package ai.cognee;

import com.fasterxml.jackson.databind.JsonNode;

/** Result of {@code remember}/{@code rememberEntry}. Keys are snake_case
 *  (Python-SDK parity); exposed via {@link #raw()}. When the automatic improve ran, {@code raw()}
 *  has an {@code improve} object (Python's {@code ImproveResult}, snake_case); {@code
 *  improve_error} and {@code improve_skipped} are present only when set. */
public final class RememberResult {
    private final JsonNode root;

    RememberResult(JsonNode root) {
        this.root = root;
    }

    public JsonNode raw() {
        return root;
    }
}
