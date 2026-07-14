package ai.cognee;

import static org.junit.jupiter.api.Assertions.assertDoesNotThrow;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;

import org.junit.jupiter.api.Test;

class StaticsTest {
    @Test
    void staticsAreIdempotentAndSafe() {
        assertNotNull(Cognee.version());
        assertDoesNotThrow(Cognee::setupLogging);
        assertDoesNotThrow(Cognee::setupLogging); // idempotent
        assertDoesNotThrow(Cognee::initOtlp);
        // initTelemetry reports whether analytics are effective for this process;
        // the decision is stable, so repeated calls must agree.
        boolean effective = Cognee.initTelemetry();
        assertEquals(effective, Cognee.initTelemetry());
    }
}
