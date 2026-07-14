package ai.cognee;

import static org.junit.jupiter.api.Assertions.assertEquals;

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
    void improveOptionsRequiresDatasetName() {
        ImproveOptions o = new ImproveOptions("ds").feedbackAlpha(0.2);
        JsonNode n = Json.tree(o.toJson());
        assertEquals("ds", n.path("datasetName").asText());
    }
}
