module.exports = {
  preset: "ts-jest",
  testEnvironment: "node",
  testMatch: ["**/__tests__/**/*.test.ts"],
  modulePathIgnorePatterns: ["cognee-neon/target", "\\.node$"],
  watchPathIgnorePatterns: ["\\.node$"],
};
