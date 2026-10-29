package ai.cognee;

import static org.junit.jupiter.api.Assertions.assertDoesNotThrow;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.nio.file.Path;
import java.util.List;
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
    void notebookCrud(@TempDir Path dir) {
        try (Cognee cognee = handle(dir)) {
            CogneeNotebook nb = cognee.notebooks().create("nb1").join();
            assertEquals("nb1", nb.name());
            assertTrue(cognee.notebooks().delete(nb.id()).join());
        }
    }

    @Test
    void pipelineRunStatusResetsAreNoOpSafe(@TempDir Path dir) {
        try (Cognee cognee = handle(dir)) {
            cognee.add(List.of(DataInput.text("reset me")), "ds").join();
            CogneeDataset ds = cognee.datasets().list().join().stream()
                    .filter(d -> "ds".equals(d.name())).findFirst().orElseThrow();
            // Resetting a dataset with no recorded runs is a deterministic no-op.
            assertDoesNotThrow(() ->
                    cognee.users().resetPipelineRunStatus(ds.id(), "cognify_pipeline").join());
            assertDoesNotThrow(() ->
                    cognee.users().resetDatasetPipelineRunStatus(ds.id()).join());
        }
    }
}
