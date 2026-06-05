#!/usr/bin/env node
// Copy the freshly-built Neon cdylib to `cognee_neon.node`, picking the
// correct platform-specific artifact name. Fails loudly if no artifact is
// found, instead of silently leaving a stale `.node` in place.
//
// Usage: node scripts/copy-artifact.js [debug|release]
//   default profile: release (packaged build). For local iteration run
//   `cargo build` (debug) in cognee-neon and pass `debug` here.

const fs = require("fs");
const path = require("path");

const profile = process.argv[2] === "debug" ? "debug" : "release";
const jsDir = path.resolve(__dirname, "..");
const targetDir = path.join(jsDir, "cognee-neon", "target", profile);
const dest = path.join(jsDir, "cognee_neon.node");

// Platform-specific cdylib names produced by `cargo build`.
const candidates = [
  "libcognee_neon.so", // Linux
  "libcognee_neon.dylib", // macOS
  "cognee_neon.dll", // Windows
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
console.log(`copied ${path.relative(jsDir, found)} -> ${path.relative(jsDir, dest)}`);
