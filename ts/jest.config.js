module.exports = {
  preset: "ts-jest",
  testEnvironment: "node",
  testMatch: ["**/__tests__/**/*.test.ts"],
  modulePathIgnorePatterns: ["cognee-ts-neon/target", "\\.node$"],
  watchPathIgnorePatterns: ["\\.node$"],
  // The pipeline-op suites (add/cognify) warm the native engines (qdrant,
  // ladybug, ONNX) on first use, which can exceed jest's 5s default under
  // parallel CPU contention. Give every test a generous budget so the cold
  // engine build never trips the timeout.
  testTimeout: 30000,
};
