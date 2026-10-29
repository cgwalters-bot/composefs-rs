#!/bin/bash
# Check that the Rust libcomposefs.so.1 is ABI-compatible with the C one,
# printing a report and failing if any check fails.  Runs inside the
# container built from Containerfile.abi-check.
#
# Usage: abi-check.sh <composefs-c-source-dir> <dir-with-rust-libcomposefs.so.1>
set -euo pipefail

TESTS_DIR="$(cd "$(dirname "$0")" && pwd)"
C_SRC=$1
C_LIB=$1/build/libcomposefs
RUST_LIB=$2
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
failed=0

# Run a check in a subshell that stops at its first failing command; its
# output goes to the report as detail.  (An `if` or `||` would turn off
# `set -e` inside the check.)
check() {
    local name=$1 status
    shift
    set +e
    (set -e; "$@") > "$WORK/out" 2>&1
    status=$?
    set -e
    if [ $status = 0 ]; then
        echo "PASS  $name"
    else
        echo "FAIL  $name"
        failed=1
    fi
    sed 's/^/      /' "$WORK/out"
}

# Defined dynamic symbols as "TYPE NAME[@VERSION]".
symbols() {
    readelf --dyn-syms -W "$1" |
        awk '$7 != "UND" && ($5 == "GLOBAL" || $5 == "WEAK") { print $4, $8 }' | sort
}

soname() {
    readelf -d "$1" | sed -n 's/.*Library soname: \[\(.*\)\]/\1/p'
}

# Fail unless $1, run with LD_LIBRARY_PATH=$2, loads libcomposefs.so.1 from $2.
uses_lib() {
    LD_LIBRARY_PATH=$2 ldd "$1" | grep -q "libcomposefs.so.1 => $2/libcomposefs.so.1 " ||
        { echo "$1 does not load $2/libcomposefs.so.1"; return 1; }
}

check_soname() {
    echo "C: $(soname "$C_LIB/libcomposefs.so.1"), Rust: $(soname "$RUST_LIB/libcomposefs.so.1")"
    [ "$(soname "$C_LIB/libcomposefs.so.1")" = "$(soname "$RUST_LIB/libcomposefs.so.1")" ]
}

check_symbols() {
    symbols "$C_LIB/libcomposefs.so.1" > "$WORK/c.syms"
    symbols "$RUST_LIB/libcomposefs.so.1" > "$WORK/rust.syms"
    echo "C exports $(wc -l < "$WORK/c.syms"), Rust $(wc -l < "$WORK/rust.syms");" \
        "versioned: C $(grep -c @ "$WORK/c.syms" || true), Rust $(grep -c @ "$WORK/rust.syms" || true)"
    diff -U0 "$WORK/c.syms" "$WORK/rust.syms" | sed -n 's/^-\([^-]\)/C only:    \1/p; s/^+\([^+]\)/Rust only: \1/p'
    cmp -s "$WORK/c.syms" "$WORK/rust.syms"
}

check_layout() {
    gcc -Wall -Werror -I"$C_SRC/libcomposefs" -o "$WORK/abi-layout" "$TESTS_DIR/abi-layout.c"
    "$WORK/abi-layout" > "$WORK/layout"
    echo "$(wc -l < "$WORK/layout") struct sizes, field offsets and constants vs abi-layout.txt"
    grep -v '^#' "$TESTS_DIR/abi-layout.txt" | diff -u - "$WORK/layout"
}

check_test_lcfs() {
    uses_lib "$C_SRC/build/tests/test-lcfs" "$RUST_LIB"
    LD_LIBRARY_PATH=$RUST_LIB "$C_SRC/build/tests/test-lcfs"
}

check_consumer() {
    gcc -Wall -I"$C_SRC/libcomposefs" -o "$WORK/consumer" "$TESTS_DIR/abi-consumer.c" \
        -L"$C_LIB" -lcomposefs
    uses_lib "$WORK/consumer" "$C_LIB"
    uses_lib "$WORK/consumer" "$RUST_LIB"
    LD_LIBRARY_PATH=$C_LIB "$WORK/consumer" > "$WORK/consumer.c.out"
    LD_LIBRARY_PATH=$RUST_LIB "$WORK/consumer" > "$WORK/consumer.rust.out" || true
    echo "$(wc -l < "$WORK/consumer.c.out") lines of output compared"
    diff -u "$WORK/consumer.c.out" "$WORK/consumer.rust.out"
}

echo "=== libcomposefs.so.1 ABI report: Rust vs C ($(git -C "$C_SRC" rev-parse --short HEAD 2>/dev/null || echo unknown)) ==="
check "SONAME" check_soname
check "exported symbols and versions" check_symbols
check "struct layouts and constants" check_layout
check "C tests/test-lcfs against Rust" check_test_lcfs
check "C consumer binary against Rust" check_consumer
echo "KNOWN DIFFERENCES (not compared): images loaded by Rust have directory nlink 1"
echo "      and size 0, and lack overlay whiteout markers and trusted.overlay.* xattrs"
exit $failed
