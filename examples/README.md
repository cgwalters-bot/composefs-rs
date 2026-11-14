# composefs examples

> This text is LLM generated.

This directory contains a few different approaches to using `cfsctl` to produce
a verified operating system image.

 - `uki`: an OS built around a [Unified Kernel Image](https://github.com/uapi-group/specifications/blob/main/specs/unified_kernel_image.md).
   If this image is signed then the signature effectively covers every single file in the filesystem.
   This works with a special form of multi-stage `Containerfile` which builds a base image, measures it using `cfsctl` and then uses that measurement to inject the composefs image fs-verity hash into the second stage of the build which actually builds the UKI (and embeds the hash into the `.cmdline`).
   We avoid a circular hash dependency by removing the UKI from the final image via a white-out (but `cfsctl` still knows how to find it).
 - `bls`: an OS built around a separate kernel and initramfs installed with a [Type #1 Boot Loader Specification Entries](https://uapi-group.org/specifications/specs/boot_loader_specification/#type-1-boot-loader-specification-entries).
   In this case we simply hack the bootloader entry to refer to the correct composefs hash at install type.
 - `unified`: similar to the `uki` example, but avoiding the intermediate `cfsctl` step by running `cfsctl` inside a build stage from the `Containerfile` itself.
   This involves bind-mounting the earlier build stage of the base image so that we can measure it from inside the stage that builds the UKI.
 - `unified-secureboot`: based on the `unified` example, adding signing for Secure Boot.

## Using the examples

The main use of the examples is to act as a scratch space for feature
development (like initramfs integration) and to show how you can build a system
image in various configurations using composefs.  They are also run from CI and
are very useful for local testing, however.

You can build the various images using the `build` script found in each
subdirectory.  It takes a single argument: the OS to build the image from
(`fedora`, `rawhide`, `arch`, `ubuntu`, `rhel9`, etc.).  You should not build
multiple images in parallel due to conflicting feature flags and shared use of
the tmp/ directory.  After the image is built, you can run tests against it by
saying something like:

```
TEST_IMAGE=examples/bls/arch-bls-efi.qcow2 pytest examples/test
```

Building and running tests on a particular image is supported via the
`examples/test/run` script, which you can use like:

```
examples/test/run bls rhel9
```

The tests are run using [`test.thing`](https://codeberg.org/lis/test.thing). We
keep a copy of it in-tree.  You you can also use it to run the VM images for
manual inspection:

```
examples/testthing.py examples/bls/fedora-bls-efi.qcow2
```

In that case, you should add this fragment to your ssh configuration:

```
Host tt.*
        ControlPath ${XDG_RUNTIME_DIR}/test.thing/%h/ssh
```

So you can access the test machine via `ssh tt.0` and so on.

## Apt setup transport checks

The [G3 evidence](https://github.com/cgwalters-forge/tracker/blob/main/docs/composefs-rs-ci-2026-11-03.md#g3--new-issue-candidate-no-exact-covering-issue-found)
records archive EOF, timeout and connection failures before example tests in
`sudo apt install -y crun/plucky podman/plucky`. The concrete threat is an
archive connection closing or stalling during setup, not a test flake or a
solver failure. Use apt's native acquisition recovery rather than retrying
the command or job.

Run `just test-apt-acquire` explicitly on Linux with apt-get, dpkg-deb and
Python 3. It uses a runtime-built package and loopback HTTP archive, isolated
apt configuration (including preferences/auth), status, indexes and cache.
It never installs packages or accesses an external mirror. It checks recovery
after two EOFs, persistent EOF/idle-timeout exhaustion, missing packages,
dependency conflicts and a failed update with cached indexes. A negative
control disables acquisition retries and must fail on the same transient EOF.
Raw apt output is withheld; success reports exits, requests and elapsed times.

The candidate invocation-local settings are:

```sh
-o Acquire::Retries=2 -o Acquire::http::Timeout=15 -o Acquire::https::Timeout=15
```

This allows two acquisition retries, not retries of solving or installation.
The tested single-URI HTTP stall consumes at most six requests (one internal
reconnect per acquisition attempt), six 15-second waits and 1+2 seconds
backoff. The harness enforces a 108-second per-command deadline including
15 seconds scheduling slack. These are **idle** timeouts, not a whole-setup
deadline: multiple packages, address selection or a continuously sending peer
can take longer. HTTPS has a corresponding timeout but is not fault-tested;
the recorded archive URLs use HTTP.

Also set `-o APT::Update::Error-Mode=any` on each update so failed refreshes
cannot succeed using cached indexes. Keep final failures, packages, suites,
mirrors and test commands unchanged; do not use `--fix-missing` or global apt
configuration.

**Not wired into CI:** `.github/` is protected in this run, so production
behavior is unchanged. In `.github/workflows/ci.yml`, the pending callers are
the examples job's `Install dependencies`, `Get a newer podman for heredoc
support (from plucky)` and `Ensure patched tools are installed` apt invocations.
Add the acquisition options to each update/install/build-dep and the strict
update option to each update. This is required before claiming #62 is fixed.
After wiring, record three authorized setup attempts on the exact resulting
revision on Ubuntu 24.04, with exits and elapsed times, then validate VM example
tests separately. Local fault injection is neither live setup nor VM evidence;
do not enable or dispatch Actions implicitly.
