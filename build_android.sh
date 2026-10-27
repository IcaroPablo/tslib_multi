#!/bin/bash
set -euo pipefail

# Build tslib-jni for Android (arm64 + x86_64)
# Requires: cargo-ndk, Android NDK ($ANDROID_NDK_HOME)
#
# Usage: ./build_android.sh [--debug]

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
OUTPUT_DIR="${SCRIPT_DIR}/../TS6_Droid_CN/app/src/main/jniLibs"
PROFILE="--release"

if [[ "${1:-}" == "--debug" ]]; then
    PROFILE=""
fi

# Android NDK setup
export ANDROID_NDK_HOME="${ANDROID_NDK_HOME:-$HOME/Android/Sdk/ndk/27.2.12479018}"
export ANDROID_NDK="$ANDROID_NDK_HOME"

echo "Building tslib-jni for Android..."
echo "NDK: ${ANDROID_NDK_HOME}"
echo "Output: ${OUTPUT_DIR}"

cd "${SCRIPT_DIR}"
cargo ndk \
    -t arm64-v8a \
    -t x86_64 \
    -o "${OUTPUT_DIR}" \
    build ${PROFILE} -p tslib-jni -j10

echo "Done. Libraries:"
find "${OUTPUT_DIR}" -name "*.so" -type f -exec ls -lh {} \; 2>/dev/null || echo "(no .so files found)"
