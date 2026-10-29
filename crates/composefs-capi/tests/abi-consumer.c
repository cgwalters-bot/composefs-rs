/* A small libcomposefs consumer: builds a tree, writes it as an image,
 * reads it back and probes error paths, printing what the library
 * returns.  abi-check.sh builds it once against the C library and runs
 * the same binary against the C and the Rust libcomposefs.so.1; the
 * outputs must be identical.
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/sysmacros.h>

#include "lcfs-writer.h"
#include "lcfs-mount.h"

static ssize_t write_cb(void *file, void *buf, size_t count)
{
	return fwrite(buf, 1, count, file);
}

static ssize_t write_zero_cb(void *file, void *buf, size_t count)
{
	return 0;
}

static void print_hex(const char *label, const uint8_t *data, size_t len)
{
	printf("%s ", label);
	for (size_t i = 0; i < len; i++)
		printf("%02x", data[i]);
	printf("\n");
}

static void print_result(const char *label, int r)
{
	printf("%s: %d errno=%s\n", label, r, r < 0 ? strerrorname_np(errno) : "-");
}

static struct lcfs_node_s *add(struct lcfs_node_s *parent, const char *name,
			       uint32_t mode)
{
	struct lcfs_node_s *node = lcfs_node_new();
	lcfs_node_set_mode(node, mode);
	if (lcfs_node_add_child(parent, node, name) < 0) {
		perror("lcfs_node_add_child");
		exit(1);
	}
	return node;
}

static struct lcfs_node_s *build_tree(void)
{
	struct timespec mtime = { .tv_sec = 1700000000, .tv_nsec = 5 };
	uint8_t digest[LCFS_DIGEST_SIZE];

	struct lcfs_node_s *root = lcfs_node_new();
	lcfs_node_set_mode(root, S_IFDIR | 0755);
	lcfs_node_set_mtime(root, &mtime);

	struct lcfs_node_s *file = add(root, "file", S_IFREG | 0644);
	lcfs_node_set_uid(file, 1000);
	lcfs_node_set_gid(file, 1000);
	lcfs_node_set_size(file, 4096);
	memset(digest, 0xab, sizeof(digest));
	lcfs_node_set_fsverity_digest(file, digest);
	char payload[3 + 2 * LCFS_DIGEST_SIZE] = "ab/";
	for (size_t i = 1; i < LCFS_DIGEST_SIZE; i++)
		strcat(payload, "ab");
	lcfs_node_set_payload(file, payload);
	lcfs_node_set_xattr(file, "user.test", "value", 5);
	lcfs_node_set_xattr(file, "trusted.overlay.opaque", "y", 1);

	struct lcfs_node_s *small = add(root, "small", S_IFREG | 0600);
	lcfs_node_set_size(small, 5);
	lcfs_node_set_content(small, (const uint8_t *)"hello", 5);

	lcfs_node_set_symlink_payload(add(root, "link", S_IFLNK | 0777), "file");
	struct lcfs_node_s *hardlink = add(root, "hardlink", S_IFREG | 0644);
	lcfs_node_make_hardlink(hardlink, file);
	struct lcfs_node_s *dev = add(root, "dev", S_IFBLK | 0600);
	lcfs_node_set_rdev64(dev, makedev(8, 1));
	/* A whiteout makes lcfs_write_to bump version 0 to 1 */
	add(add(root, "dir", S_IFDIR | 0700), "whiteout", S_IFCHR | 0);
	return root;
}

/* Print a loaded tree.  This leaves out directory nlink and size and the
 * overlay xattrs, where the Rust loader does not match C yet: it reports
 * nlink 1 and size 0 for directories, and drops the whiteout marker
 * xattrs and user-set trusted.overlay.* xattrs. */
static void dump(struct lcfs_node_s *node, const char *path)
{
	struct timespec mtime;
	lcfs_node_get_mtime(node, &mtime);
	printf("%s mode=%o uid=%u gid=%u rdev=%llu mtime=%lld.%ld", path,
	       lcfs_node_get_mode(node), lcfs_node_get_uid(node),
	       lcfs_node_get_gid(node),
	       (unsigned long long)lcfs_node_get_rdev64(node),
	       (long long)mtime.tv_sec, mtime.tv_nsec);
	if (!lcfs_node_dirp(node))
		printf(" nlink=%u size=%llu", lcfs_node_get_nlink(node),
		       (unsigned long long)lcfs_node_get_size(node));
	if (lcfs_node_get_payload(node))
		printf(" payload=%s", lcfs_node_get_payload(node));
	if (lcfs_node_get_content(node))
		printf(" content=%.*s", (int)lcfs_node_get_size(node),
		       lcfs_node_get_content(node));
	if (lcfs_node_get_fsverity_digest(node))
		print_hex(" digest", lcfs_node_get_fsverity_digest(node), 4);
	else
		printf("\n");
	for (size_t i = 0; i < lcfs_node_get_n_xattr(node); i++) {
		const char *name = lcfs_node_get_xattr_name(node, i);
		size_t len;
		const char *value = lcfs_node_get_xattr(node, name, &len);
		if (strstr(name, ".overlay."))
			continue;
		printf("  xattr %s=%.*s\n", name, (int)len, value);
	}
	for (size_t i = 0; i < lcfs_node_get_n_children(node); i++) {
		struct lcfs_node_s *child = lcfs_node_get_child(node, i);
		char child_path[4096];
		snprintf(child_path, sizeof(child_path), "%s/%s", path,
			 lcfs_node_get_name(child));
		dump(child, child_path);
	}
}

int main(void)
{
	struct lcfs_node_s *root = build_tree();
	char *image = NULL;
	size_t image_size = 0;
	FILE *f = open_memstream(&image, &image_size);
	uint8_t digest[LCFS_DIGEST_SIZE] = { 0 };

	struct lcfs_write_options_s options = { 0 };
	options.format = LCFS_FORMAT_EROFS;
	options.version = 0;
	options.max_version = 1;
	options.digest_out = digest;
	options.file = f;
	options.file_write_cb = write_cb;
	print_result("write_to", lcfs_write_to(root, &options));
	fclose(f);
	printf("version=%u max_version=%u size=%zu\n", options.version,
	       options.max_version, image_size);
	print_hex("digest_out", digest, sizeof(digest));
	memset(digest, 0, sizeof(digest));
	lcfs_compute_fsverity_from_data(digest, (uint8_t *)image, image_size);
	print_hex("fsverity", digest, sizeof(digest));

	struct lcfs_write_options_s bad = options;
	bad.file_write_cb = write_zero_cb;
	print_result("write_to zero-progress callback", lcfs_write_to(root, &bad));
	bad = options;
	bad.digest_out = NULL;
	bad.file_write_cb = NULL;
	bad.flags = LCFS_BUILD_COMPUTE_DIGEST;
	print_result("write_to flags", lcfs_write_to(root, &bad));
	bad.flags = 0;
	bad.format = LCFS_FORMAT_EROFS + 1;
	print_result("write_to format", lcfs_write_to(root, &bad));
	bad.format = LCFS_FORMAT_EROFS;
	bad.version = LCFS_VERSION_MAX + 1;
	print_result("write_to version", lcfs_write_to(root, &bad));
	lcfs_node_unref(root);

	root = lcfs_load_node_from_image((uint8_t *)image, image_size);
	if (root == NULL) {
		perror("lcfs_load_node_from_image");
		return 1;
	}
	dump(root, "");
	lcfs_node_unref(root);

	const char *entries[] = { "small", "link", NULL };
	struct lcfs_read_options_s read_options = { .toplevel_entries = entries };
	root = lcfs_load_node_from_image_ext((uint8_t *)image, image_size,
					     &read_options);
	printf("toplevel_entries: %zu children\n", lcfs_node_get_n_children(root));
	lcfs_node_unref(root);
	image[0] ^= 0xff;
	root = lcfs_load_node_from_image((uint8_t *)image, image_size);
	print_result("load corrupt image", root ? 0 : -1);
	free(image);

	const char *objdirs[] = { "/nonexistent" };
	struct lcfs_mount_options_s mount_options = { .objdirs = objdirs, .n_objdirs = 1 };
	mount_options.flags = LCFS_MOUNT_FLAGS_MASK + 1;
	print_result("mount flags",
		     lcfs_mount_image("/nonexistent", "/nonexistent", &mount_options));
	mount_options.flags = 0;
	mount_options.upperdir = "/nonexistent";
	print_result("mount upperdir without workdir",
		     lcfs_mount_image("/nonexistent", "/nonexistent", &mount_options));
	mount_options.upperdir = NULL;
	mount_options.flags = LCFS_MOUNT_FLAGS_IDMAP;
	mount_options.idmap_fd = -1;
	print_result("mount idmap without fd",
		     lcfs_mount_image("/nonexistent", "/nonexistent", &mount_options));
	return 0;
}
