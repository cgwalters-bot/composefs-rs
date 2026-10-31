#!/bin/bash
# Run composefs-c's tests/ suites against the Rust mkcomposefs,
# composefs-info and mount.composefs inside a container; see
# Containerfile.test-c-cli and run-c-cli-suites.sh.
set -euo pipefail

# The composefs/composefs commit whose tests/ suites we run.
C_COMMIT=ec2573a0f68f548ae91f3e10acc990a08f9122dc

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../../.." && pwd)"

cargo build --release -p composefs-capi -p composefs-ctl --manifest-path="$REPO_DIR/Cargo.toml"

BUILD_CTX=$(mktemp -d)
trap 'rm -rf "$BUILD_CTX"' EXIT

cp "$REPO_DIR/target/release/libcomposefs_capi.so" "$REPO_DIR/target/release/cfsctl" \
    "$SCRIPT_DIR/run-c-cli-suites.sh" "$BUILD_CTX/"

git init -q "$BUILD_CTX/composefs-c"
git -C "$BUILD_CTX/composefs-c" fetch -q --depth=1 https://github.com/composefs/composefs.git "$C_COMMIT"
git -C "$BUILD_CTX/composefs-c" checkout -q FETCH_HEAD

podman build --no-cache --iidfile "$BUILD_CTX/image-id" \
    -f "$SCRIPT_DIR/Containerfile.test-c-cli" "$BUILD_CTX"
mkdir "$BUILD_CTX/tmp"
# Unlike a build RUN, this permits the mount tests. Probe fs-verity on
# the host filesystem rather than the container's overlay filesystem.
podman run --rm --privileged --env TMPDIR=/test-tmp \
    --volume "$BUILD_CTX/tmp:/test-tmp:Z" "$(<"$BUILD_CTX/image-id")"
