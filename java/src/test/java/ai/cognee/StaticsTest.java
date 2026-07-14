package ai.cognee;

import static org.junit.jupiter.api.Assertions.assertDoesNotThrow;
import static org.junit.jupiter.api.Assertions.assertNotNull;

import org.junit.jupiter.api.Test;

class StaticsTest {
    @Test
    void staticsAreIdempotentAndSafe() {
        assertNotNull(Cognee.version());
        assertDoesNotThrow(Cognee::setupLogging);
        assertDoesNotThrow(Cognee::setupLogging); // idempotent
        assertDoesNotThrow(Cognee::initOtlp);
        assertDoesNotThrow(Cognee::initTelemetry);
    }
}
