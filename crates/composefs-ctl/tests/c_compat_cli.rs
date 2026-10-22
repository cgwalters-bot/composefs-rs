//! Differential tests of the composefs-c compatible command-line contract.
//!
//! Each expected exit status and output below was recorded from the C tools
//! (`mkcomposefs`, `composefs-info`, `mount.composefs`) built from
//! composefs/composefs at ec2573a0f68f548ae91f3e10acc990a08f9122dc, run with
//! the same arguments.  The Rust tools are reached through the hidden
//! `cfsctl <tool>` subcommands, which share their parsing with the multi-call
//! entry points.

use std::path::Path;
use std::process::{Command, Output};

const OBJ_B: &str = "0a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f9";
const OBJ_E: &str = "ffeeddccbbaa99887766554433221100ffeeddccbbaa99887766554433221100";

fn run(tool: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cfsctl"))
        .arg(tool)
        .args(args)
        .output()
        .expect("spawn cfsctl")
}

fn stdout(out: &Output) -> String {
    String::from_utf8(out.stdout.clone()).unwrap()
}

/// Assert the exit status, showing stderr on mismatch.
fn assert_status(out: &Output, code: i32, what: &str) {
    assert_eq!(
        out.status.code(),
        Some(code),
        "{what}: stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn object_path(id: &str) -> String {
    format!("{}/{}", &id[..2], &id[2..])
}

/// Build a small image with an inline file, a directory holding an external
/// file, a symlink and a second external file.
fn fixture_image(dir: &Path) -> String {
    let dump = format!(
        "/ 4096 40755 3 0 0 0 0.0 - - -\n\
         /a 5 100644 1 0 0 0 0.0 - hello -\n\
         /b 4096 40755 2 0 0 0 0.0 - - -\n\
         /b/c 8192 100644 1 0 0 0 0.0 {} - {OBJ_B}\n\
         /d 1 120777 1 0 0 0 0.0 a - -\n\
         /e 8192 100644 1 0 0 0 0.0 {} - {OBJ_E}\n",
        object_path(OBJ_B),
        object_path(OBJ_E),
    );
    let dumpfile = dir.join("fixture.dump");
    std::fs::write(&dumpfile, dump).unwrap();
    let image = dir.join("fixture.cfs");
    let out = run(
        "mkcomposefs",
        &[
            "--from-file",
            dumpfile.to_str().unwrap(),
            image.to_str().unwrap(),
        ],
    );
    assert_status(&out, 0, "mkcomposefs fixture");
    image.to_str().unwrap().to_owned()
}

#[test]
fn info_filter_is_global() {
    let dir = tempfile::tempdir().unwrap();
    let image = fixture_image(dir.path());
    let ls_b = format!("/b/\t\n/b/c\t@ {}\n", object_path(OBJ_B));

    // Before or after the subcommand, as getopt permutes arguments in C.
    for args in [["--filter=b", "ls", &image], ["ls", "--filter=b", &image]] {
        let out = run("composefs-info", &args);
        assert_status(&out, 0, "ls --filter");
        assert_eq!(stdout(&out), ls_b, "{args:?}");
    }

    let out = run(
        "composefs-info",
        &["--filter=b", "--filter=d", "ls", &image],
    );
    assert_status(&out, 0, "ls two filters");
    assert_eq!(stdout(&out), format!("{ls_b}/d\t-> a\n"));

    let out = run("composefs-info", &["--filter=nonexistent", "ls", &image]);
    assert_status(&out, 0, "ls unmatched filter");
    assert_eq!(stdout(&out), "");

    // The filter applies when loading the image, so objects honours it too.
    let out = run("composefs-info", &["--filter=b", "objects", &image]);
    assert_status(&out, 0, "objects --filter");
    assert_eq!(stdout(&out), format!("{}\n", object_path(OBJ_B)));

    let basedir = dir.path().join("objects");
    std::fs::create_dir(&basedir).unwrap();
    let out = run(
        "composefs-info",
        &[
            "--filter=e",
            "--basedir",
            basedir.to_str().unwrap(),
            "missing-objects",
            &image,
        ],
    );
    assert_status(&out, 0, "missing-objects --filter");
    assert_eq!(stdout(&out), format!("{}\n", object_path(OBJ_E)));
}

#[test]
fn info_dump_applies_filter() {
    let dir = tempfile::tempdir().unwrap();
    let image = fixture_image(dir.path());

    let out = run("composefs-info", &["--filter=b", "dump", &image]);
    assert_status(&out, 0, "dump --filter");
    // C prints the on-disk size of directories (4096 and 40 here) where the
    // Rust dumpfile writer prints 0, a known divergence outside the
    // command-line contract; compare everything else.
    let without_size = |s: &str| -> Vec<String> {
        s.lines()
            .map(|l| {
                let mut f: Vec<&str> = l.split(' ').collect();
                f.remove(1);
                f.join(" ")
            })
            .collect()
    };
    let c_output = format!(
        "/ 4096 40755 3 0 0 0 0.0 - - -\n\
         /b 40 40755 2 0 0 0 0.0 - - -\n\
         /b/c 8192 100644 1 0 0 0 0.0 {} - {OBJ_B}\n",
        object_path(OBJ_B)
    );
    assert_eq!(without_size(&stdout(&out)), without_size(&c_output));
}

#[test]
fn info_usage_errors_exit_1() {
    let dir = tempfile::tempdir().unwrap();
    let image = fixture_image(dir.path());

    for args in [
        &[][..],
        &["ls"],
        &["dump"],
        &["objects"],
        &["--basedir=/", "missing-objects"],
        &["measure-file"],
        &["bogus", &image],
        &["--bogus", "dump", &image],
        &["--filter=a/b", "dump", &image],
    ] {
        let out = run("composefs-info", args);
        assert_status(&out, 1, &format!("composefs-info {args:?}"));
        assert_eq!(stdout(&out), "", "{args:?}");
    }

    // Deliberately stricter than C, which runs without --basedir and stats
    // object paths relative to fd 0.
    let out = run("composefs-info", &["missing-objects", &image]);
    assert_status(&out, 1, "missing-objects without --basedir");

    let out = run("composefs-info", &["--help"]);
    assert_status(&out, 0, "composefs-info --help");
}

#[test]
fn mount_rejects_empty_basedir_entries() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path().to_str().unwrap();
    // C rejects these before opening the image or any basedir.
    for basedir in [
        "basedir=".to_owned(),
        format!("basedir={d}::{d}"),
        format!("basedir={d}:"),
        format!("basedir=:{d}"),
    ] {
        let out = run("mount.composefs", &["-o", &basedir, "image", "mnt"]);
        assert_status(&out, 1, &basedir);
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("incorrect format"),
            "{basedir}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn mount_help_and_usage_errors() {
    assert_status(&run("mount.composefs", &["-h"]), 0, "mount.composefs -h");
    assert_status(&run("mount.composefs", &[]), 1, "no operands");
    assert_status(&run("mount.composefs", &["image"]), 1, "no mountpoint");
    assert_status(&run("mount.composefs", &["-x", "a", "b"]), 1, "bad flag");
}

/// Write a dumpfile containing a whiteout (chr 0,0) to `dir`.
fn whiteout_dump(dir: &Path) -> String {
    let path = dir.join("whiteout.dump");
    std::fs::write(
        &path,
        "/ 4096 40755 2 0 0 0 0.0 - - -\n/wh 0 20644 1 0 0 0 0.0 - - -\n",
    )
    .unwrap();
    path.to_str().unwrap().to_owned()
}

#[test]
fn mkcomposefs_version_range() {
    // composefs_version 1 and 0 images of the whiteout dumpfile, from C.
    const V1: &str = "7470c92de2d37d9315817ca2988d4fafe04b235401018faf302c127eb2486685";
    const V0: &str = "8a872494dd9d7cb5202fc0238fb321e23e54a88d4b832b3205178497e9d79d74";

    let dir = tempfile::tempdir().unwrap();
    let dump = whiteout_dump(dir.path());
    for (opts, expected) in [
        // The default range is 0..=1, and the whiteout forces an upgrade.
        (&[][..], V1),
        (&["--max-version=1"], V1),
        (&["--min-version=1"], V1),
        // Giving either option leaves the other at 0, so no upgrade...
        (&["--min-version=0"], V0),
        (&["--max-version=0"], V0),
        // ...and max_version below min_version is raised to it.
        (&["--min-version=1", "--max-version=0"], V1),
    ] {
        let mut args = opts.to_vec();
        args.extend(["--print-digest-only", "--from-file", &dump]);
        let out = run("mkcomposefs", &args);
        assert_status(&out, 0, &format!("{opts:?}"));
        assert_eq!(stdout(&out), format!("{expected}\n"), "{opts:?}");
    }

    for opts in [["--max-version=2"], ["--min-version=2"]] {
        let mut args = opts.to_vec();
        args.extend(["--print-digest-only", "--from-file", &dump]);
        assert_status(&run("mkcomposefs", &args), 1, &format!("{opts:?}"));
    }
}

#[test]
fn mkcomposefs_threads_and_usage_errors() {
    let dir = tempfile::tempdir().unwrap();
    let dump = whiteout_dump(dir.path());

    let out = run(
        "mkcomposefs",
        &["--threads=0", "--print-digest-only", "--from-file", &dump],
    );
    assert_status(&out, 1, "--threads=0");

    let out = run(
        "mkcomposefs",
        &["--threads=1", "--print-digest-only", "--from-file", &dump],
    );
    assert_status(&out, 0, "--threads=1");

    for args in [&[][..], &["--bogus"], &["--threads=x", "src", "dst"]] {
        assert_status(&run("mkcomposefs", args), 1, &format!("{args:?}"));
    }
    assert_status(&run("mkcomposefs", &["--help"]), 0, "--help");
}
