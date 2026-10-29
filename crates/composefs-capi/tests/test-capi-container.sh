#!/bin/bash
# Run C composefs tests against the Rust-built libcomposefs shared library
# inside a container: by default the C test suite (test-checksums,
# test-units) from Containerfile.test-capi.
#
# Usage: test-capi-container.sh [composefs-c-repo]
#
# Environment:
#   CAPI_CONTAINERFILE  Containerfile to build instead (in this directory)
#   COMPOSEFS_C_REF     composefs-c commit to fetch when no repo is given
#                       (default: the default branch)
#   PODMAN_BUILD_ARGS   extra arguments for podman build
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../../.." && pwd)"
C_REPO="${1:-}"
CONTAINERFILE="$SCRIPT_DIR/${CAPI_CONTAINERFILE:-Containerfile.test-capi}"

cargo build --release -p composefs-capi --manifest-path="$REPO_DIR/Cargo.toml"

BUILD_CTX=$(mktemp -d)
trap 'rm -rf "$BUILD_CTX"' EXIT

cp "$REPO_DIR/target/release/libcomposefs_capi.so" "$BUILD_CTX/"
cp -a "$SCRIPT_DIR" "$BUILD_CTX/tests"

if [ -n "$C_REPO" ]; then
    cp -a "$(cd "$C_REPO" && pwd)" "$BUILD_CTX/composefs-c"
else
    git init -q "$BUILD_CTX/composefs-c"
    git -C "$BUILD_CTX/composefs-c" fetch -q --depth=1 \
        https://github.com/composefs/composefs.git "${COMPOSEFS_C_REF:-HEAD}"
    git -C "$BUILD_CTX/composefs-c" checkout -q FETCH_HEAD
fi

# shellcheck disable=SC2086 # PODMAN_BUILD_ARGS is a list of arguments
podman build --no-cache ${PODMAN_BUILD_ARGS:-} -f "$CONTAINERFILE" "$BUILD_CTX"
