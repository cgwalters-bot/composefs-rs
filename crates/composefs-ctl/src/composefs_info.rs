//! composefs-info - Query information from composefs images.
//!
//! This is a Rust reimplementation of the C composefs-info tool, providing
//! commands to inspect EROFS images, list objects, and compute fs-verity digests.
//!
//! ## Compatibility status
//!
//! Implemented subcommands:
//! - `ls` — lists files with type suffixes, skips whiteout entries
//! - `dump` — outputs composefs-dump(5) text format (image → tree → dumpfile)
//! - `objects` — lists all backing file object paths (XX/XXXX...)
//! - `missing-objects` — lists objects not present in `--basedir`
//! - `measure-file` — computes fs-verity digest of files
//!
//! Compatibility notes:
//! - `measure-file` tries `FS_IOC_MEASURE_VERITY` first; falls back to
//!   in-process computation when the kernel reports verity is absent or the
//!   filesystem doesn't support it, matching the C `lcfs_fd_get_fsverity()`
//!   behaviour.

use std::collections::HashSet;
use std::io::Write;
use std::path::Path;
use std::{fs::File, io::Read, path::PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use composefs::{
    dumpfile::write_dumpfile,
    erofs::reader::erofs_to_filesystem,
    fsverity::{FsVerityHashValue, Sha256HashValue, measure_verity_with_fallback},
    generic_tree::{Inode, LeafContent, LeafId},
    tree::{FileSystem, RegularFile},
};

/// Query information from composefs images.
#[derive(Parser, Debug)]
#[command(
    name = "composefs-info",
    version,
    about = "Query information from composefs images"
)]
struct Cli {
    /// Only load the top-level entries with this name (can be repeated).
    ///
    /// Applies to every command that reads images, as in C composefs-info.
    #[arg(long, global = true, value_name = "NAME", value_parser = parse_filter)]
    filter: Vec<String>,

    /// Base directory for object lookups (used by missing-objects).
    #[arg(long, global = true, value_name = "PATH")]
    basedir: Option<PathBuf>,

    /// The subcommand to run.
    #[command(subcommand)]
    command: Command,
}

/// Available subcommands.
#[derive(Subcommand, Debug)]
enum Command {
    /// Simple listing of files and directories in the image.
    Ls {
        /// Composefs image files to inspect.
        #[arg(required = true)]
        images: Vec<PathBuf>,
    },

    /// Full dump in composefs-dump(5) format.
    Dump {
        /// Composefs image files to dump.
        #[arg(required = true)]
        images: Vec<PathBuf>,
    },

    /// List all backing file object paths.
    Objects {
        /// Composefs image files to inspect.
        #[arg(required = true)]
        images: Vec<PathBuf>,
    },

    /// List backing files not present in basedir.
    MissingObjects {
        /// Composefs image files to inspect.
        #[arg(required = true)]
        images: Vec<PathBuf>,
    },

    /// Print the fs-verity digest of files.
    MeasureFile {
        /// Files to measure.
        #[arg(required = true)]
        files: Vec<PathBuf>,
    },
}

/// A filter names a single top-level entry, so it may not contain `/`.
fn parse_filter(s: &str) -> Result<String, String> {
    if s.contains('/') {
        return Err(format!("Filter must be a single name: {s}"));
    }
    Ok(s.to_owned())
}

/// Entry point for the composefs-info multi-call mode.
pub fn run() -> Result<()> {
    let cli = crate::parse_c_compat_args(std::env::args_os());
    run_with_cli(cli)
}

/// Entry point when invoked as a hidden `cfsctl composefs-info` subcommand.
///
/// `extra_args` contains everything after the `composefs-info` token as
/// captured by clap's trailing-var-arg mechanism.  A synthetic `argv[0]` is
/// prepended so that parsing produces the same help text / error
/// messages as the standalone binary.
pub fn run_from_args(extra_args: Vec<std::ffi::OsString>) -> Result<()> {
    let mut argv = vec![std::ffi::OsString::from("composefs-info")];
    argv.extend(extra_args);
    let cli = crate::parse_c_compat_args(argv);
    run_with_cli(cli)
}

fn run_with_cli(cli: Cli) -> Result<()> {
    let filter = &cli.filter;
    match &cli.command {
        Command::Ls { images } => cmd_ls(filter, images),
        Command::Dump { images } => cmd_dump(filter, images),
        Command::Objects { images } => cmd_objects(filter, images),
        Command::MissingObjects { images } => {
            // C composefs-info treats a missing --basedir as "every object is
            // missing" only by accident (it stats against fd 0); require it.
            let basedir = cli
                .basedir
                .as_deref()
                .context("missing-objects requires --basedir")?;
            cmd_missing_objects(basedir, filter, images)
        }
        Command::MeasureFile { files } => cmd_measure_file(files),
    }
}

/// Print escaped path (matches C implementation behavior).
fn print_escaped<W: Write>(out: &mut W, s: &[u8]) -> std::io::Result<()> {
    for &c in s {
        match c {
            b'\\' => write!(out, "\\\\")?,
            b'\n' => write!(out, "\\n")?,
            b'\r' => write!(out, "\\r")?,
            b'\t' => write!(out, "\\t")?,
            // Non-printable or non-ASCII characters are hex-escaped
            c if !c.is_ascii_graphic() && c != b' ' => write!(out, "\\x{c:02x}")?,
            c => out.write_all(&[c])?,
        }
    }
    Ok(())
}

/// Walk and print entries: directory line first, then recurse into children.
fn ls_print<W: Write>(
    out: &mut W,
    fs: &FileSystem<Sha256HashValue>,
    dir: &composefs::tree::Directory<Sha256HashValue>,
    path: &[u8],
    seen_leaf_ids: &mut HashSet<LeafId>,
) -> Result<()> {
    for (name, child) in dir.sorted_entries() {
        let name_bytes = name.as_encoded_bytes();

        let mut child_path = path.to_vec();
        child_path.push(b'/');
        child_path.extend_from_slice(name_bytes);

        match child {
            Inode::Directory(child_dir) => {
                // Print the directory entry with trailing slash.
                print_escaped(out, &child_path)?;
                write!(out, "/\t")?;
                writeln!(out)?;
                // Recurse into the directory.
                ls_print(out, fs, child_dir, &child_path, seen_leaf_ids)?;
            }
            Inode::Leaf(leaf_id, _) => {
                let leaf = fs.leaf(*leaf_id);

                print_escaped(out, &child_path)?;

                match &leaf.content {
                    LeafContent::Regular(regular) => {
                        let is_hardlink = !seen_leaf_ids.insert(*leaf_id);
                        if !is_hardlink && let RegularFile::External(id, _) = regular {
                            write!(out, "\t@ ")?;
                            print_escaped(out, id.to_object_pathname().as_bytes())?;
                        }
                    }
                    LeafContent::Symlink(target) => {
                        write!(out, "\t-> ")?;
                        print_escaped(out, target.as_encoded_bytes())?;
                    }
                    _ => {}
                }

                writeln!(out)?;
            }
        }
    }
    Ok(())
}

/// Load an image, keeping only the top-level entries named in `filter`.
///
/// An empty filter keeps everything.  This mirrors C's `toplevel_entries`
/// load option, so the filter applies equally to every image command.
fn load_image(image_path: &PathBuf, filter: &[String]) -> Result<FileSystem<Sha256HashValue>> {
    let image_data = read_image(image_path)?;
    let mut fs = erofs_to_filesystem::<Sha256HashValue>(&image_data)
        .with_context(|| format!("Failed to parse image: {image_path:?}"))?;
    if !filter.is_empty() {
        fs.root
            .retain_top_level(|name| filter.iter().any(|f| f == name));
        fs.compact();
    }
    Ok(fs)
}

/// List files and directories in the image.
fn cmd_ls(filter: &[String], images: &[PathBuf]) -> Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    for image_path in images {
        let fs = load_image(image_path, filter)?;
        let mut seen_leaf_ids = HashSet::new();
        ls_print(&mut out, &fs, &fs.root, b"", &mut seen_leaf_ids)?;
    }

    Ok(())
}

/// Dump the image in composefs-dump(5) text format.
///
/// This matches the C composefs-info dump output: the EROFS image is parsed
/// back into a filesystem tree which is then serialized as a dumpfile.
fn cmd_dump(filter: &[String], images: &[PathBuf]) -> Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    for image_path in images {
        let fs = load_image(image_path, filter)?;
        write_dumpfile(&mut out, &fs)
            .with_context(|| format!("Failed to dump image: {image_path:?}"))?;
    }

    Ok(())
}

/// Collect all external object IDs from a parsed filesystem.
///
/// Iterates the leaves table directly — each `RegularFile::External` entry
/// is a unique content-addressed object.  Because `erofs_to_filesystem`
/// deduplicates hard-linked inodes into a single leaf, each object appears
/// exactly once even if it is referenced by multiple paths.
fn collect_objects_from_fs(fs: &FileSystem<Sha256HashValue>) -> HashSet<Sha256HashValue> {
    fs.leaves
        .iter()
        .filter_map(|leaf| match &leaf.content {
            LeafContent::Regular(RegularFile::External(id, _)) => Some(id.clone()),
            _ => None,
        })
        .collect()
}

/// List all object paths from the images.
fn cmd_objects(filter: &[String], images: &[PathBuf]) -> Result<()> {
    for image_path in images {
        let fs = load_image(image_path, filter)?;
        let mut objects: Vec<Sha256HashValue> = collect_objects_from_fs(&fs).into_iter().collect();
        objects.sort_by_key(|id| id.to_hex());

        for obj in objects {
            println!("{}", obj.to_object_pathname());
        }
    }
    Ok(())
}

/// List objects not present in basedir.
fn cmd_missing_objects(basedir: &Path, filter: &[String], images: &[PathBuf]) -> Result<()> {
    let mut all_objects: HashSet<Sha256HashValue> = HashSet::new();

    for image_path in images {
        let fs = load_image(image_path, filter)?;
        all_objects.extend(collect_objects_from_fs(&fs));
    }

    let mut missing: Vec<Sha256HashValue> = all_objects
        .into_iter()
        .filter(|obj| !basedir.join(obj.to_object_pathname()).exists())
        .collect();

    missing.sort_by_key(|a| a.to_hex());

    for obj in missing {
        println!("{}", obj.to_object_pathname());
    }

    Ok(())
}

/// Compute and print the fs-verity digest of each file.
fn cmd_measure_file(files: &[PathBuf]) -> Result<()> {
    for path in files {
        let file = File::open(path).with_context(|| format!("Failed to open file: {path:?}"))?;
        let digest = measure_verity_with_fallback::<Sha256HashValue>(file)
            .with_context(|| format!("Failed to measure verity for {path:?}"))?;
        println!("{}", digest.to_hex());
    }
    Ok(())
}

/// Read an entire image file into memory.
fn read_image(path: &PathBuf) -> Result<Vec<u8>> {
    let mut file = File::open(path).with_context(|| format!("Failed to open image: {path:?}"))?;
    let mut data = Vec::new();
    file.read_to_end(&mut data)
        .with_context(|| format!("Failed to read image: {path:?}"))?;
    Ok(data)
}
