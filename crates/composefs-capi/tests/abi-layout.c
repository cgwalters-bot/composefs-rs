/* Print the sizes and field offsets of the public libcomposefs structs and
 * the values of its public constants, one "name value" line each, as the
 * C headers define them.  abi-check.sh compares the output with
 * abi-layout.txt, which the Rust unit test test_abi_layout_matches_c_headers
 * checks the Rust definitions against.
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */
#include <errno.h>
#include <stddef.h>
#include <stdio.h>

#include "lcfs-writer.h"
#include "lcfs-mount.h"
#include "lcfs-erofs.h"

#define SIZE(s) printf("%s.size %zu\n", #s, sizeof(struct s))
#define FIELD(s, f) printf("%s.%s %zu\n", #s, #f, offsetof(struct s, f))
#define CONST(c) printf("%s %lld\n", #c, (long long)(c))

int main(void)
{
	SIZE(lcfs_write_options_s);
	FIELD(lcfs_write_options_s, format);
	FIELD(lcfs_write_options_s, version);
	FIELD(lcfs_write_options_s, flags);
	FIELD(lcfs_write_options_s, digest_out);
	FIELD(lcfs_write_options_s, file);
	FIELD(lcfs_write_options_s, file_write_cb);
	FIELD(lcfs_write_options_s, max_version);
	FIELD(lcfs_write_options_s, reserved);
	FIELD(lcfs_write_options_s, reserved2);

	SIZE(lcfs_read_options_s);
	FIELD(lcfs_read_options_s, toplevel_entries);
	FIELD(lcfs_read_options_s, reserved);
	FIELD(lcfs_read_options_s, reserved2);

	SIZE(lcfs_mount_options_s);
	FIELD(lcfs_mount_options_s, objdirs);
	FIELD(lcfs_mount_options_s, n_objdirs);
	FIELD(lcfs_mount_options_s, workdir);
	FIELD(lcfs_mount_options_s, upperdir);
	FIELD(lcfs_mount_options_s, expected_fsverity_digest);
	FIELD(lcfs_mount_options_s, flags);
	FIELD(lcfs_mount_options_s, idmap_fd);
	FIELD(lcfs_mount_options_s, image_mountdir);
	FIELD(lcfs_mount_options_s, reserved);
	FIELD(lcfs_mount_options_s, reserved2);

	SIZE(lcfs_erofs_header_s);
	FIELD(lcfs_erofs_header_s, magic);
	FIELD(lcfs_erofs_header_s, version);
	FIELD(lcfs_erofs_header_s, flags);
	FIELD(lcfs_erofs_header_s, composefs_version);
	FIELD(lcfs_erofs_header_s, unused);

	CONST(LCFS_DIGEST_SIZE);
	CONST(LCFS_BUILD_SKIP_XATTRS);
	CONST(LCFS_BUILD_USE_EPOCH);
	CONST(LCFS_BUILD_SKIP_DEVICES);
	CONST(LCFS_BUILD_COMPUTE_DIGEST);
	CONST(LCFS_BUILD_NO_INLINE);
	CONST(LCFS_BUILD_USER_XATTRS);
	CONST(LCFS_BUILD_BY_DIGEST);
	CONST(LCFS_FORMAT_EROFS);
	CONST(LCFS_FLAGS_NONE);
	CONST(LCFS_FLAGS_MASK);
	CONST(LCFS_VERSION_MAX);
	CONST(LCFS_DEFAULT_VERSION_MIN);
	CONST(LCFS_DEFAULT_VERSION_MAX);
	CONST(LCFS_INODE_XATTRMETA_SIZE);
	CONST(LCFS_INODE_EXTERNAL_XATTR_MAX);
	CONST(LCFS_INLINE_CONTENT_MAX);
	CONST(LCFS_RECOMMENDED_INLINE_CONTENT_MAX);

	CONST(ENOVERITY);
	CONST(EWRONGVERITY);
	CONST(ENOSIGNATURE);
	CONST(LCFS_MOUNT_FLAGS_NONE);
	CONST(LCFS_MOUNT_FLAGS_REQUIRE_VERITY);
	CONST(LCFS_MOUNT_FLAGS_READONLY);
	CONST(LCFS_MOUNT_FLAGS_IDMAP);
	CONST(LCFS_MOUNT_FLAGS_TRY_VERITY);
	CONST(LCFS_MOUNT_FLAGS_MASK);

	CONST(LCFS_EROFS_VERSION);
	CONST(LCFS_EROFS_MAGIC);
	CONST(LCFS_EROFS_FLAGS_HAS_ACL);
	return 0;
}
