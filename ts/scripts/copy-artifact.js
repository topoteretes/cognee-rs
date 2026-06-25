#!/usr/bin/env node
// Copy the freshly-built Neon cdylib to `cognee_ts_neon.node`, picking the
// correct platform-specific artifact name. Fails loudly if no artifact is
// found, instead of silently leaving a stale `.node` in place.
//
// Usage: node scripts/copy-artifact.js [debug|release]
//   default profile: release (packaged build). For local iteration run
//   `cargo build` (debug) in cognee-ts-neon and pass `debug` here.

const fs = require("fs");
const path = require("path");

const profile = process.argv[2] === "debug" ? "debug" : "release";
const tsDir = path.resolve(__dirname, "..");
const targetDir = path.join(tsDir, "cognee-ts-neon", "target", profile);
const dest = path.join(tsDir, "cognee_ts_neon.node");

// Platform-specific cdylib names produced by `cargo build`.
const candidates = [
  "libcognee_ts_neon.so", // Linux
  "libcognee_ts_neon.dylib", // macOS
  "cognee_ts_neon.dll", // Windows
];

const found = candidates
  .map((name) => path.join(targetDir, name))
  .find((p) => fs.existsSync(p));

if (!found) {
  console.error(
    `error: no Neon cdylib found in ${targetDir}\n` +
      `       looked for: ${candidates.join(", ")}\n` +
      `       did the '${profile}' build succeed?`,
  );
  process.exit(1);
}

fs.copyFileSync(found, dest);
console.log(`copied ${path.relative(tsDir, found)} -> ${path.relative(tsDir, dest)}`);
