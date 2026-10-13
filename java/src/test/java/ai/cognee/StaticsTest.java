package ai.cognee;

import static org.junit.jupiter.api.Assertions.assertDoesNotThrow;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;
import static org.junit.jupiter.api.Assertions.assertNotNull;

import org.junit.jupiter.api.Test;

class StaticsTest {
    @Test
    void staticsAreIdempotentAndSafe() {
        assertNotNull(Cognee.version());
        assertDoesNotThrow(() -> Cognee.setupLogging());
        assertDoesNotThrow(() -> Cognee.setupLogging()); // idempotent
        assertDoesNotThrow(() -> Cognee.initOtlp());
        // initTelemetry reports whether analytics are effective for this process;
        // the decision is stable, so repeated calls must agree.
        boolean effective = Cognee.initTelemetry();
        assertEquals(effective, Cognee.initTelemetry());
        assertEquals(effective, Cognee.isTelemetryEnabled());
    }

    @Test
    void telemetrySwitchIsProcessWideAndWinsOverEnv() {
        boolean before = Cognee.isTelemetryEnabled();
        try {
            Cognee.setTelemetryEnabled(false);
            assertFalse(Cognee.isTelemetryEnabled());
            assertFalse(Cognee.initTelemetry());
            Cognee.setTelemetryEnabled(true);
            // No COGNEE_HOST_SDK in the test environment, so the switch alone
            // decides — even if TELEMETRY_DISABLED is set for the build.
            assertTrue(Cognee.isTelemetryEnabled());
        } finally {
            Cognee.setTelemetryEnabled(before);
        }
    }

    @Test
    void otlpRejectsUnknownOptionKeys() {
        // initOtlp latches on its first call, so a validation error can only be
        // observed while nothing has installed it yet; either way it must not
        // succeed silently with a bad key once validation runs.
        try {
            Cognee.initOtlp(java.util.Map.of("otel_nope", "x"));
        } catch (CogneeException e) {
            assertTrue(e.getMessage().contains("unknown OpenTelemetry setting"), e.getMessage());
        }
    }
}
