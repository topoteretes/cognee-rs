#!/bin/sh
# CMake compiler launcher used via CMAKE_C/CXX_COMPILER_LAUNCHER (see
# .cargo/config.toml [env]). Routes the bundled C/C++ builds of native
# dependencies (lbug's Ladybug tree, etc.) through ccache when it is
# installed, and is a transparent pass-through when it is not — so machines
# without ccache build exactly as before.
#
# Why: cargo puts lbug's CMake build in target/.../build/lbug-<unit-hash>/out,
# and the hash shifts whenever lbug's build-dependency closure (cc, cmake,
# cxx-build, …) changes in the resolved dependency graph. Each shift restarts
# the ~2.5 min C++ build from an empty OUT_DIR. ccache makes those restarts
# near-instant cache hits. See docs/build/lbug-rebuilds.md.
if command -v ccache >/dev/null 2>&1; then
    exec ccache "$@"
fi
exec "$@"
