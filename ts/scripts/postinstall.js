#!/usr/bin/env node
// postinstall.js — source-build fallback for the cognee native addon.
//
// This script runs automatically after `npm install`.  It checks whether a
// matching prebuilt binary was already resolved via the optional dependencies
// (e.g. `@cognee/neon-linux-x64-gnu`).  If one was found, nothing further is
// needed.
//
// The check-first approach ensures that `npm install` on a supported platform
// that has a matching prebuilt package completes without compiling Rust —
// building Rust on every install is not acceptable.
//
// Source-build fallback: a source build requires the Rust addon crate, which is
// only present in a git checkout of the repository — the published npm tarball
// ships JS only (the `cognee-ts-neon/` Rust source and the wider workspace are not
// included, as bundling the whole Rust workspace in npm is impractical).  So:
//   • From a published install on a platform WITHOUT a prebuilt binary, this
//     script cannot build from source; it prints how to proceed and exits 0.
//   • From a git checkout (the `cognee-ts-neon/` crate is present), it falls back
//     to `npm run build:rust`, which requires a Rust toolchain.
//
// Environment:
//   COGNEE_SKIP_POSTINSTALL=1   — bypass the entire script (useful in CI steps
//                                 that build the addon themselves).

"use strict";

const fs = require("fs");
const path = require("path");
const { execSync } = require("child_process");

// Allow CI to skip the postinstall entirely.
if (process.env.COGNEE_SKIP_POSTINSTALL) {
  process.exit(0);
}

// The platform names produced by @neon-rs/load, mapped to our optional dep
// package names (must stay in sync with native.ts and package.json).
const PLATFORM_PACKAGES = {
  "linux-x64-gnu":    "@cognee/neon-linux-x64-gnu",
  "linux-arm64-gnu":  "@cognee/neon-linux-arm64-gnu",
  "darwin-x64":       "@cognee/neon-darwin-x64",
  "darwin-arm64":     "@cognee/neon-darwin-arm64",
  "win32-x64-msvc":   "@cognee/neon-win32-x64-msvc",
};

/**
 * Detect the current platform key using the same logic as @neon-rs/load.
 * Returns null when the platform/arch combination is not in our matrix.
 */
function detectPlatformKey() {
  const { platform, arch } = process;
  if (platform === "linux") {
    // Detect glibc vs musl via process.report (same technique as @neon-rs/load).
    const report = process.report && process.report.getReport();
    const isGlibc =
      report &&
      typeof report === "object" &&
      "header" in report &&
      report.header &&
      "glibcVersionRuntime" in report.header;
    const libc = isGlibc ? "gnu" : "musl";
    if (arch === "x64" || arch === "arm64") {
      return `linux-${arch}-${libc}`;
    }
  } else if (platform === "darwin") {
    if (arch === "x64" || arch === "arm64") {
      return `darwin-${arch}`;
    }
  } else if (platform === "win32") {
    if (arch === "x64") {
      return "win32-x64-msvc";
    }
  }
  return null;
}

/**
 * Returns true when the optional platform package is installed and loadable.
 */
function prebuildInstalled(pkgName) {
  try {
    require.resolve(pkgName);
    return true;
  } catch (_e) {
    return false;
  }
}

/**
 * Returns true when a locally-built `cognee_ts_neon.node` exists next to this
 * package (produced by a previous `npm run build:rust`).
 */
function localBuildExists() {
  const nodeFile = path.join(__dirname, "..", "cognee_ts_neon.node");
  return fs.existsSync(nodeFile);
}

// --- Main ---

const platformKey = detectPlatformKey();
const platformPkg = platformKey ? PLATFORM_PACKAGES[platformKey] : null;

// 1. If a matching prebuilt optional dep was installed, we're done.
if (platformPkg && prebuildInstalled(platformPkg)) {
  console.log(`cognee: found prebuilt binary (${platformPkg}) — skipping source build.`);
  process.exit(0);
}

// 2. If a locally-built artifact already exists (e.g. developer workflow),
//    we're done.
if (localBuildExists()) {
  console.log("cognee: found local cognee_ts_neon.node — skipping source build.");
  process.exit(0);
}

// 3. No prebuilt and no local artifact.
const platformInfo = platformKey || `${process.platform}-${process.arch}`;
const pkgRoot = path.join(__dirname, "..");

// A source build is only possible when the Rust addon crate is present, i.e. in
// a git checkout.  The published npm tarball ships JS only, so there is nothing
// to build from — surface that honestly instead of failing to `cd` into a
// missing directory.
const hasRustSource = fs.existsSync(path.join(pkgRoot, "cognee-ts-neon", "Cargo.toml"));
if (!hasRustSource) {
  console.error(
    `\ncognee: no prebuilt binary for ${platformInfo}, and this install has no ` +
    "Rust source to build from (the npm package ships JS only).\n" +
    "  The native addon is unavailable.  To use it on this platform:\n" +
    `  • Use a platform with a prebuilt binary (${Object.keys(PLATFORM_PACKAGES).join(", ")}), or\n` +
    "  • Build from a git checkout of the repository: clone it, then\n" +
    "    `cd ts && npm run build` (requires a Rust toolchain).\n",
  );
  // Exit 0 so `npm install` does not fail hard; the error surfaces at require().
  process.exit(0);
}

// Source build from a git checkout.
console.log(`cognee: no prebuilt binary for ${platformInfo}; attempting source build (requires Rust toolchain).`);
console.log("cognee: if you do not want a source build, set COGNEE_SKIP_POSTINSTALL=1.");

try {
  execSync("npm run build:rust", {
    cwd: pkgRoot,
    stdio: "inherit",
  });
  console.log("cognee: source build succeeded.");
} catch (err) {
  console.error(
    "\ncognee: source build FAILED. The native addon is unavailable.\n" +
    "  • Install Rust (https://rustup.rs) and retry, or\n" +
    `  • Use a platform with a prebuilt binary (${Object.keys(PLATFORM_PACKAGES).join(", ")}), or\n` +
    "  • Set COGNEE_SKIP_POSTINSTALL=1 to suppress this step.\n",
  );
  // Exit 0 so npm install does not fail hard when Rust is absent — consumers
  // who only need the JS types or are on an unsupported platform should still
  // be able to install the package.  The error is surfaced at `require()` time.
  process.exit(0);
}
