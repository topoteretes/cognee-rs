package ai.cognee;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.nio.file.Path;
import java.util.concurrent.CompletionException;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

/**
 * {@code improve} with {@code runInBackground} returns the running result and
 * {@link Cognee#waitForBackgroundTasks} drains the run (SDK-1051).
 */
class BackgroundImproveTest {
    @Test
    void backgroundImproveReturnsRunningAndWaitDrains(@TempDir Path dir) {
        try (Cognee cognee = new Cognee(TestConfig.underTempDir(dir))) {
            ImproveResult running =
                    cognee.improve(new ImproveOptions("bg_ds").runInBackground(true)).join();
            assertEquals("running", running.status());
            assertTrue(running.background());
            assertFalse(running.finished());
            assertEquals("bg_ds", running.datasetName());
            assertNotNull(running.datasetId());
            assertTrue(running.stages().isEmpty());

            BackgroundTasksResult waited = cognee.waitForBackgroundTasks(
                    new WaitForBackgroundTasksOptions().timeoutSeconds(60)).join();
            assertTrue(waited.finished());
            assertEquals(0L, waited.pending());
        }
    }

    @Test
    void unknownDatasetIdFailsBeforeDispatch(@TempDir Path dir) {
        try (Cognee cognee = new Cognee(TestConfig.underTempDir(dir))) {
            CompletionException ex = assertThrows(CompletionException.class, () -> cognee
                    .improve(new ImproveOptions("bg_ds")
                            .datasetId("00000000-0000-0000-0000-0000000000aa")
                            .runInBackground(true))
                    .join());
            assertTrue(ex.getCause() instanceof CogneeException, String.valueOf(ex.getCause()));
            assertEquals("VALIDATION_ERROR", ((CogneeException) ex.getCause()).code());
        }
    }

    @Test
    void waitWithoutWorkAndBadTimeout(@TempDir Path dir) {
        try (Cognee cognee = new Cognee(TestConfig.underTempDir(dir))) {
            assertTrue(cognee.waitForBackgroundTasks().join().finished());
            CompletionException ex = assertThrows(CompletionException.class, () -> cognee
                    .waitForBackgroundTasks(new WaitForBackgroundTasksOptions().timeoutSeconds(-1))
                    .join());
            assertTrue(ex.getCause() instanceof CogneeException, String.valueOf(ex.getCause()));
            assertEquals("VALIDATION_ERROR", ((CogneeException) ex.getCause()).code());
        }
    }
}
