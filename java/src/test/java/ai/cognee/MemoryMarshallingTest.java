package ai.cognee;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;

import ai.cognee.internal.Json;
import com.fasterxml.jackson.databind.JsonNode;
import org.junit.jupiter.api.Test;

class MemoryMarshallingTest {
    @Test
    void memoryEntryQaSerializes() {
        MemoryEntry e = MemoryEntry.qa("q?", "a.").context("ctx").feedbackScore(3);
        JsonNode n = Json.tree(Json.toJson(e));
        assertEquals("qa", n.path("type").asText());
        assertEquals("q?", n.path("question").asText());
        assertEquals(3, n.path("feedbackScore").asInt());
    }

    @Test
    void memifyResultDeserializes() {
        String canned = "{\"tripletCount\":5,\"indexedCount\":5,\"batchCount\":1,"
                + "\"alreadyCompleted\":false,\"priorPipelineRunId\":null}";
        MemifyResult r = Json.fromJson(canned, MemifyResult.class);
        assertEquals(5, r.tripletCount());
        assertEquals(1, r.batchCount());
    }

    @Test
    void improveResultDeserializes() {
        String canned = "{\"datasetId\":null,\"datasetName\":\"ds\",\"sessionIds\":[\"s1\"],"
                + "\"stages\":[{\"stage\":\"feedback_weights\",\"status\":\"completed\","
                + "\"reason\":null,\"error\":null,\"counts\":{\"sessions\":1},\"durationMs\":3}],"
                + "\"background\":false,\"finished\":true,\"error\":null,\"status\":\"completed\"}";
        ImproveResult r = Json.fromJson(canned, ImproveResult.class);
        assertEquals("ds", r.datasetName());
        assertEquals("completed", r.status());
        assertEquals("feedback_weights", r.stages().get(0).stage());
        assertEquals(1L, r.stages().get(0).counts().get("sessions"));
    }

    @Test
    void improveOptionsRequiresDatasetName() {
        // Happy path: a supplied datasetName serializes.
        JsonNode n = Json.tree(new ImproveOptions("ds").feedbackAlpha(0.2).toJson());
        assertEquals("ds", n.path("datasetName").asText());
        // datasetName is required — a null/empty name is rejected fast at the API
        // boundary (clearer than a surprising later core-side VALIDATION_ERROR).
        assertThrows(IllegalArgumentException.class, () -> new ImproveOptions(null));
        assertThrows(IllegalArgumentException.class, () -> new ImproveOptions(""));
    }
}
