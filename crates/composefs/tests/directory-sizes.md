# Directory dump parity evidence
> This text is LLM generated.

## Representation

`composefs-info dump` now reads directory sizes from the source EROFS inodes,
without changing the writer or storing layout state in the mutable logical tree.
Tree-only dump APIs retain zero sizes: there is no serialized layout to describe.
At composefs-c revision `ec2573a0f68f548ae91f3e10acc990a08f9122dc`,
[`lcfs-writer-erofs.c:1724–1750`](https://github.com/containers/composefs/blob/ec2573a0f68f548ae91f3e10acc990a08f9122dc/libcomposefs/lcfs-writer-erofs.c#L1724-L1750)
loads `i_size` from either inode header, and
[`composefs-info.c:192–202`](https://github.com/containers/composefs/blob/ec2573a0f68f548ae91f3e10acc990a08f9122dc/tools/composefs-info.c#L192-L202)
prints that size. This includes block padding and root whiteout stubs, even
though the latter are omitted from the logical tree.

`dumpfile::tests::test_image_directory_sizes` covers V0/V1 compact inodes and V2
extended inodes, root sizes 4096/60, an empty child of size 27, and directories
with 0/118/119/240/500 five-byte filenames. Their sizes are respectively
27/2033/4096/4113/8549, covering tail promotion and multiple blocks. Rebuilding
each dumped image must reproduce the original bytes, not just its text.

## Positive corpus run

Run on 2026-11-14 against the above C revision and Rust base `f5aa0a2` plus this
patch. No corpus files are vendored. Built C with Meson (`-Dman=disabled
-Dfuse=disabled`) and Rust with `cargo build --ignore-rust-version -p
composefs-ctl --bin cfsctl`. For each existing `tests/assets/*.sha256` input,
decompressed `.gz` where needed and invoked both implementations' `mkcomposefs
--from-file - IMAGE`, passing `--min-version=N --max-version=N` only when the
asset has a `.version` file, exactly as C's `tests/test-checksums.sh` does.
Compared image SHA-256 with the published sidecar, compared both implementations'
`composefs-info dump` of the **C image**, and rebuilt successful Rust images
from their Rust dumps. Each subprocess had a 120-second timeout; none timed out.

All 15 existing inputs build with C (exit 0) and match their published hashes.
Rust dumps all 15 C images successfully (exit 0); **every directory path/size
pair matches C**. Full text comparison below is byte-for-byte, without xattr
sorting or other normalization. Parse failures exit 1 and produce no Rust image.

| Asset | Rust parse | Full dump vs C | Rust image SHA-256 vs published | Rust dump/rebuild |
| --- | --- | --- | --- | --- |
| bigfile-xattr.dump | 0 | equal | equal | identical |
| bigfile.dump | 0 | equal | equal | identical |
| config-with-hard-link.dump.gz | 0 | differs: digest field | equal | differs |
| config.dump.gz | 0 | differs: digest field | equal | differs |
| cs9-x86_64-developer.dump.gz | 1: object pathname | differs: payload lost | unavailable | unavailable |
| cs9-x86_64-minimal.dump.gz | 1: object pathname | differs: payload lost | unavailable | unavailable |
| f36-x86_64-silverblue.dump.gz | 1: object pathname | differs: payload lost | unavailable | unavailable |
| honggfuzz-bigfile-with-acl.dump | 0 | equal | equal | identical |
| honggfuzz-chardev-nonzero-size.dump | 0 | differs: opaque xattrs | differs: default version | identical |
| honggfuzz-long-symlink.dump | 1: 3914-byte symlink | equal | unavailable | unavailable |
| honggfuzz-longlink-unterminated.dump | 1: missing content | equal | unavailable | unavailable |
| longlink.dump | 1: 4070-byte symlink | equal | unavailable | unavailable |
| no-newline.dump | 1: directory rdev 10 | equal | unavailable | unavailable |
| special.dump (V0) | 1: inline limit 512 | differs: xattrs | unavailable | unavailable |
| special_v1.dump (V1) | 1: inline limit 512 | differs: opaque xattrs | unavailable | unavailable |

`honggfuzz-long-xattr.dump.gz`, `honggfuzz-reread-what-we-wrote.dump`, and
`honggfuzz-write-inode-data.dump` have hash sidecars but no input in the pinned
tree; no parse, dump, or digest result is claimed for them.

C reproduces `special.dump` hash
`d344782ff962ab7d1c5533755c19c5bf3c3485f55bfaf228fc89524c7e2ba61e`
and `special_v1.dump` hash
`2b1dfb9ffe6938cb3134c339059572e38cbd8cc515d54758fdff55cad5a37427`.
Rust cannot yet build either: #71's inline-content sibling was still pending
with no open implementation PR when this run began. Digest parity for those
two inputs is **not established**; rerun after stacking on that sibling.

The chardev image's Rust default hash is
`b7d4dc72c273e9813d8bc09eb080837ea4a6705737c0eacc0f42eb7a4c748e3d`,
instead of published
`3e7ca8195f29cfd0583ed761c8eead21892ef0ec61d1402aa7f7d84ef8316947`.
Explicit V1 makes both implementations match the published hash; explicit V0
makes both produce the former hash. This is the #31/#34 CLI whiteout-version
upgrade dependency, not directory metadata corruption. Filtered dumping and
global CLI options also remain #31/#34 work; this run tests unfiltered dumps.

Other residual defects are independent of directory sizes: no-verity external
files gain a digest in Rust dump output (changing config-image rebuilds),
noncanonical redirect paths are rejected on input or lost on image decoding,
long symlinks and permissive C input are rejected, and C/Rust differ in xattr
ordering and visibility of generated opaque xattrs. This is evidence for #32
and #57, not new CI wiring or a parity-gate promotion.

## Checks

`cargo fmt --check` passed. Exact requested `cargo clippy --all-targets` and
`cargo test --workspace --exclude composefs-integration-tests` exited 101:
the runner has Rust 1.93.1 and `test-with` 0.17 requires 1.95. An ignored local
lockfile downgrade from 0.17.2 to 0.17.0 did not remove that requirement.

With `--ignore-rust-version`, clippy passed with existing warnings, and the
focused `cargo test -p composefs dumpfile` run passed 28 tests (2 C-dependent
tests ignored). The workspace run exited 101: 220 passed, 10 failed, 8 ignored
in composefs, stopping before the other test binaries. Nine failures require
fs-verity unsupported by the temporary filesystem; the tenth could not locate
`mkcomposefs` on PATH. Retrying that test with the locally built C tools on PATH
passed. An earlier workspace attempt exceeded the tool's 200-second deadline;
the completed retry used a 600-second deadline. No mount or fs-verity success
is claimed, and the ordinary required checks still need a compatible runner.
