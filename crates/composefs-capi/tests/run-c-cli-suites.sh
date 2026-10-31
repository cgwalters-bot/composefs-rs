#!/bin/bash
# Run composefs-c's tests/ suites against the Rust CLIs in TOOLS and print
# pass, skip and fail counts per suite.  The C scripts are run unmodified;
# this wrapper runs them one case at a time so each case gets a result,
# and turns the cases they pass silently without testing into SKIPs.
#
# Usage: run-c-cli-suites.sh TOOLS C_TESTS_DIR RUST_LIBCOMPOSEFS
set -uo pipefail

TOOLS=$1
C_TESTS=$2
RUST_LIB=$3
ASSETS=$C_TESTS/assets

# test-units.sh defaults to /var/tmp because fs-verity is more likely to
# work there; probe the same place.
export TMPDIR=${TMPDIR:-/var/tmp}

# Asset lists copied from tests/meson.build at the pinned commit.
TEST_ASSETS="honggfuzz-write-inode-data.dump config.dump.gz
config-with-hard-link.dump.gz special.dump longlink.dump bigfile.dump
bigfile-xattr.dump special_v1.dump honggfuzz-long-symlink.dump
honggfuzz-longlink-unterminated.dump honggfuzz-bigfile-with-acl.dump
no-newline.dump honggfuzz-chardev-nonzero-size.dump
cs9-x86_64-developer.dump.gz cs9-x86_64-minimal.dump.gz
f36-x86_64-silverblue.dump.gz"
TEST_ASSETS_SHOULD_FAIL="should-fail-long-link.dump
should-fail-empty-xattr-key.dump should-fail-long-xattr-key.dump
should-fail-long-xattr-value.dump should-fail-honggfuzz-long-xattr.dump
should-fail-too-big.dump should-fail-no-ftype.dump
should-fail-empty-name.dump should-fail-empty-link-name.dump
should-fail-dot-name.dump should-fail-dotdot-name.dump
should-fail-dir-hardlink.dump should-fail-self-hardlink.dump
should-fail-big-inline.dump"

SUITES=(check-checksums check-units check-dump-filtered check-should-fail)
declare -A GATING=([check-checksums]=1 [check-units]=1)

# Cases of gating suites that fail because of bugs in the Rust CLIs.  They
# still run and are reported, but do not gate; drop a case once it passes.
# Observed with the pinned C corpus on main (f5aa0a2); values quote the
# failing check or mkcomposefs diagnostic, not an inferred diagnosis.
declare -A KNOWN_FAIL=(
    [check-checksums/config.dump.gz]="rebuilt images differ: char 1061, line 1"
    [check-checksums/config-with-hard-link.dump.gz]="rebuilt images differ: char 1061, line 1"
    [check-checksums/special.dump]="Input exceeded maximum length 512"
    [check-checksums/special_v1.dump]="Input exceeded maximum length 512"
    [check-checksums/longlink.dump]="Symlink target length 4070 exceeds limit 1024"
    [check-checksums/honggfuzz-long-symlink.dump]="Symlink target length 3914 exceeds limit 1024"
    [check-checksums/honggfuzz-longlink-unterminated.dump]="Missing content"
    [check-checksums/no-newline.dump]="entry cannot have device (rdev) 10"
    [check-checksums/honggfuzz-chardev-nonzero-size.dump]="Invalid erofs checksum of file generated"
    [check-checksums/cs9-x86_64-developer.dump.gz]="invalid object pathname: Invalid character '2' at position 2"
    [check-checksums/cs9-x86_64-minimal.dump.gz]="invalid object pathname: Invalid character '6' at position 2"
    [check-checksums/f36-x86_64-silverblue.dump.gz]="invalid object pathname: Invalid character '7' at position 2"
)

declare -A npass nskip nfail nknown
for s in "${SUITES[@]}"; do npass[$s]=0 nskip[$s]=0 nfail[$s]=0 nknown[$s]=0; done

log=$(mktemp)
trap 'rm -f -- "$log"' EXIT

# record SUITE PASS|SKIP|FAIL CASE [REASON]
record() {
    local reason=${4:-}
    if [ -n "${KNOWN_FAIL[$1/$3]:-}" ]; then
        case $2 in
            FAIL) reason="known Rust bug, not gating: ${KNOWN_FAIL[$1/$3]}" nknown[$1]=$((nknown[$1] + 1)) ;;
            PASS) reason="listed as a known failure; remove it from KNOWN_FAIL" ;;
        esac
    fi
    echo "$2: $1 $3${reason:+ ($reason)}"
    case $2 in
        PASS) npass[$1]=$((npass[$1] + 1)) ;;
        SKIP) nskip[$1]=$((nskip[$1] + 1)) ;;
        FAIL) nfail[$1]=$((nfail[$1] + 1)) ;;
    esac
}

# run_case SUITE CASE COMMAND...: one C script invocation is one case;
# its output is shown only when it fails.
run_case() {
    local suite=$1 name=$2
    shift 2
    if "$@" >"$log" 2>&1; then
        record "$suite" PASS "$name"
    else
        sed 's/^/    /' "$log"
        record "$suite" FAIL "$name"
    fi
}

echo "NOTE: composefs-dump is C code (composefs-c tools/composefs-dump.c) running on the Rust libcomposefs"
dump_lib=$(ldd "$TOOLS/composefs-dump" | awk '$1 == "libcomposefs.so.1" { print $3 }')
echo "composefs-dump loads libcomposefs.so.1 from ${dump_lib:-nowhere}"
if [ -z "$dump_lib" ] || ! cmp -s "$dump_lib" "$RUST_LIB"; then
    echo "error: composefs-dump does not load the Rust libcomposefs" >&2
    exit 1
fi

# test-units' test_mount_digest passes without checking anything when
# fs-verity is missing, and accepts a refused final mount; probe both
# here so those passes are reported as skips.
has_fsverity=n
probe=$(mktemp)
echo foo >"$probe"
fsverity enable "$probe" >/dev/null 2>&1 && has_fsverity=y
rm -f -- "$probe"
can_mount=n
probe=$(mktemp -d)
if [ -w /dev/loop-control ] && mount -t tmpfs none "$probe" 2>/dev/null; then
    umount "$probe" && can_mount=y
fi
rmdir -- "$probe"
echo "Probes: fs-verity in $TMPDIR=$has_fsverity, mount permitted=$can_mount"

for f in $TEST_ASSETS; do
    # test-checksums.sh silently skips a listed asset that is missing.
    if [ ! -f "$ASSETS/$f" ]; then
        record check-checksums SKIP "$f" "asset not in the composefs-c tree"
        continue
    fi
    run_case check-checksums "$f" "$C_TESTS/test-checksums.sh" "$TOOLS" "$ASSETS" "$f"
done

# test-units.sh runs all its tests in one invocation and prints a line
# per test.
"$C_TESTS/test-units.sh" "$TOOLS" >"$log" 2>&1
units_rc=$?
units_seen=0
while read -r line; do
    case $line in
        "Test "*": OK") result=PASS name=${line#Test } name=${name%: OK} ;;
        "Test "*" Failed") result=FAIL name=${line#Test } name=${name% Failed} ;;
        *) continue ;;
    esac
    units_seen=$((units_seen + 1))
    reason=
    if [ "$result" = PASS ] && [ "$name" = test_mount_digest ]; then
        if [ $has_fsverity = n ]; then
            result=SKIP reason="no fs-verity in $TMPDIR, nothing ran"
        elif [ $can_mount = n ]; then
            result=SKIP reason="mount not permitted, only the fs-verity rejections ran"
        fi
    elif [ "$result" = PASS ] && [ "$name" = test_composefs_info_measure_files ] && [ $has_fsverity = n ]; then
        reason="fs-verity half did not run"
    fi
    record check-units "$result" "$name" "$reason"
done <"$log"
if [ $units_rc != 0 ] || [ $units_seen = 0 ]; then
    sed 's/^/    /' "$log"
    [ "${nfail[check-units]}" != 0 ] || record check-units FAIL test-units.sh "exit status $units_rc"
fi

# check-dump-filtered and check-should-fail are reported but do not gate:
# they are known to fail until the slice 1 issue fixes composefs-info
# --filter and mkcomposefs' exit statuses and input checks.
run_case check-dump-filtered special.dump "$C_TESTS/test-dump-filtered.sh" "$TOOLS" "$ASSETS"
for f in $TEST_ASSETS_SHOULD_FAIL; do
    run_case check-should-fail "$f" "$C_TESTS/test-should-fail.sh" "$TOOLS" "$ASSETS/$f"
done

echo "=== Summary ==="
rc=0
for s in "${SUITES[@]}"; do
    if [ -n "${GATING[$s]:-}" ]; then
        kind=gating
        [ "${nfail[$s]}" = "${nknown[$s]}" ] || rc=1
    else
        kind="not gating"
    fi
    known=
    [ "${nknown[$s]}" = 0 ] || known=", ${nknown[$s]} of them known"
    echo "$s: ${npass[$s]} pass, ${nskip[$s]} skip, ${nfail[$s]} fail$known ($kind)"
done
exit $rc
