package ai.cognee;

import com.fasterxml.jackson.databind.JsonNode;
import java.util.ArrayList;
import java.util.List;

/** Result of a delete/empty/forget op. Underlying keys are snake_case. */
public final class DeleteResult {
    private final JsonNode root;

    DeleteResult(JsonNode root) {
        this.root = root;
    }

    public JsonNode raw() {
        return root;
    }

    public int deletedData() { return root.path("deleted_data").asInt(); }
    public int deletedDatasets() { return root.path("deleted_datasets").asInt(); }
    public int deletedGraphNodes() { return root.path("deleted_graph_nodes").asInt(); }
    public int deletedVectorPoints() { return root.path("deleted_vector_points").asInt(); }
    public boolean prunedSessions() { return root.path("pruned_sessions").asBoolean(false); }

    public List<String> warnings() {
        List<String> out = new ArrayList<>();
        root.path("warnings").forEach(n -> out.add(n.asText()));
        return out;
    }
}
