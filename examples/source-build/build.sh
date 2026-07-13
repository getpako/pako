#!/usr/bin/env bash
set -euo pipefail

cmake \
    -S "$PAKO_SOURCE_DIR" \
    -B "$PAKO_BUILD_DIR" \
    -G Ninja \
    -DCMAKE_INSTALL_PREFIX=/
cmake --build "$PAKO_BUILD_DIR" --parallel "$PAKO_JOBS"
ctest --test-dir "$PAKO_BUILD_DIR" --output-on-failure
DESTDIR="$PAKO_DESTDIR" cmake --install "$PAKO_BUILD_DIR"
