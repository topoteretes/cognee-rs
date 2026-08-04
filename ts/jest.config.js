module.exports = {
  preset: "ts-jest",
  testEnvironment: "node",
  testMatch: ["**/__tests__/**/*.test.ts"],
  // bindings/target, not cognee-ts-neon/target: the Neon crate is a member of
  // the workspace rooted at <repo>/bindings (see bindings/Cargo.toml), so the
  // build output now lands outside this rootDir. The pattern is kept rather
  // than dropped so the guard still holds if the target dir ever moves back
  // under ts/.
  modulePathIgnorePatterns: ["bindings/target", "\\.node$"],
  watchPathIgnorePatterns: ["\\.node$"],
  // The pipeline-op suites (add/cognify) warm the native engines (qdrant,
  // ladybug, ONNX) on first use, which can exceed jest's 5s default under
  // parallel CPU contention. Give every test a generous budget so the cold
  // engine build never trips the timeout.
  testTimeout: 30000,
};
