/**
 * Verify ``setupTelemetry()`` is a no-op without OTLP configuration
 * and idempotent on repeat calls (gap 07 decisions 2 + 12).
 *
 * The function is exposed on the Neon module; this test exercises the
 * happy path only — we do not stand up a mock collector.
 */
import { setupTelemetry } from "../src";

describe("setupTelemetry", () => {
  beforeEach(() => {
    delete process.env.OTEL_EXPORTER_OTLP_ENDPOINT;
    delete process.env.COGNEE_TRACING_ENABLED;
  });

  it("does not throw when no OTLP endpoint is configured", () => {
    expect(() => setupTelemetry()).not.toThrow();
  });

  it("is idempotent on subsequent calls", () => {
    setupTelemetry();
    expect(() => setupTelemetry()).not.toThrow();
    expect(() => setupTelemetry()).not.toThrow();
  });
});
