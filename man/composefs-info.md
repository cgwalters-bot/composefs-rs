---
title: composefs-info
section: 1
footer: composefs
header: User Commands
---

# NAME

composefs-info - print information about a composefs image

# SYNOPSIS
**composefs-info** [*OPTIONS*] ls|objects|missing-objects|dump *IMAGE* [*IMAGE2* *IMAGE3* ...]

**composefs-info** measure-file *FILE* [*FILE2* ...]

# DESCRIPTION

The composefs-info command lets you inspect a composefs image. It has
several sub-commands:

**ls**
:   Prints a simple list of the files and directorie in the images as
    well as their backing file or symlink target.

**objects**
:   Prints a list of all the backing files referenced by the images,
    in sorted order.

**missing-objects**
:   Prints a list of all the missing backing files referenced by the
    images, in sorted order, given a backing file store passed in
    using the --basedir option.

**dump**
:   Prints a full dump of the images in a line based textual format.
    See **composefs-dump(5)** for more details. This format is also
    accepted as input to mkcomposefs if the --from-file
    option is used.

**measure-file**
:    Interpret the provided paths as generic files, and print their fsverity digest.

# OPTIONS

The provided *IMAGE* argument must be a composefs file. At least one
image (or, for **measure-file**, one file) is required, and multiple
can be specified.

**composefs-info** accepts the following options, before or after the
sub-command:


**\-\-basedir**=*PATH*
:   This should point to a directory of backing files, and will be used
    by the **missing-objects** command to know what files are available.
    It is required by **missing-objects**.

**\-\-filter**=*NAME*
:   Only load the top-level entries whose name matches one of these,
    for every sub-command that reads images. Can be specified
    multiple times. Cannot be a full path (e.g. cannot contain `/`).
    This is intended to be used for tooling to efficiently lookup metadata
    files embedded in the image without loading and printing the entire
    image.

# EXIT STATUS

**composefs-info** exits 0 on success and 1 on any error, including
invalid command-line arguments and a missing *IMAGE* or *FILE*.

# SEE ALSO
**composefs-info(1)**, **composefs-dump(5)**

[composefs upstream](https://github.com/composefs/composefs-rs)
