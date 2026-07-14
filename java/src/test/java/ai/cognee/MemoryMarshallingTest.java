package ai.cognee;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import ai.cognee.internal.Json;
import com.fasterxml.jackson.databind.JsonNode;
import java.nio.file.Path;
import java.util.Map;
import java.util.concurrent.CompletionException;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

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
    void improveOptionsRequiresDatasetName(@TempDir Path dir) {
        // Happy path: a supplied datasetName serializes.
        JsonNode n = Json.tree(new ImproveOptions("ds").feedbackAlpha(0.2).toJson());
        assertEquals("ds", n.path("datasetName").asText());
        // A null datasetName is omitted from the wire payload...
        assertFalse(Json.tree(new ImproveOptions(null).toJson()).has("datasetName"));
        // ...and the core rejects the missing name deterministically (validation
        // runs before any LLM call), surfacing a VALIDATION_ERROR.
        try (Cognee cognee = new Cognee(Map.of(
                "data_root_directory", dir.resolve("data").toString(),
                "system_root_directory", dir.resolve("sys").toString()))) {
            CompletionException ex = assertThrows(CompletionException.class,
                    () -> cognee.improve(new ImproveOptions(null)).join());
            assertTrue(ex.getCause() instanceof CogneeException,
                    "cause should be CogneeException, was: " + ex.getCause());
            assertEquals("VALIDATION_ERROR", ((CogneeException) ex.getCause()).code());
        }
    }
}
