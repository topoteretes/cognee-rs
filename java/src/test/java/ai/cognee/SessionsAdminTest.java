package ai.cognee;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;

import java.nio.file.Path;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

class SessionsAdminTest {
    private Cognee handle(Path dir) {
        return new Cognee(TestConfig.underTempDir(dir));
    }

    @Test
    void defaultUserResolves(@TempDir Path dir) {
        try (Cognee cognee = handle(dir)) {
            CogneeUser u = cognee.users().getOrCreateDefault().join();
            assertNotNull(u.id());
        }
    }

    @Test
    void graphContextRoundTrips(@TempDir Path dir) {
        try (Cognee cognee = handle(dir)) {
            String sessionId = "sess-round-trip";
            // No context stored yet.
            assertNull(cognee.sessions().getGraphContext(sessionId).join());
            // Store then read back the exact payload.
            cognee.sessions().setGraphContext(sessionId, "graph-ctx-payload").join();
            assertEquals("graph-ctx-payload",
                    cognee.sessions().getGraphContext(sessionId).join());
            // Overwrite is observed on the next read.
            cognee.sessions().setGraphContext(sessionId, "updated").join();
            assertEquals("updated", cognee.sessions().getGraphContext(sessionId).join());
        }
    }
}
