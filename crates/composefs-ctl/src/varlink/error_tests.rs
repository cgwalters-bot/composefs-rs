//! Root-free error-reply tests adapted from fork PR #22 (45dfa355).

use super::*;
use serde_json::{Value, json};
use zlink::{Service, futures_util::StreamExt as _, service::MethodReply};

struct Fixture {
    service: CfsctlService,
    conn: zlink::tokio::unix::Connection,
    _peer: tokio::net::UnixStream,
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let (sock, peer) = tokio::net::UnixStream::pair().unwrap();
        Self {
            service: CfsctlService::insecure_for_test(),
            conn: zlink::Connection::new(zlink::tokio::unix::Stream::try_from(sock).unwrap()),
            _peer: peer,
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn path(&self) -> PathBuf {
        self.dir.path().join("repo")
    }

    // Exercise generated dispatch, wire names, payloads and terminal streams.
    // No listener, server task, root privileges or network is needed.
    async fn call(&mut self, interface: &str, method: &str, params: Value, count: usize) -> Value {
        let fds = (0..count)
            .map(|_| std::fs::File::open(self.dir.path()).unwrap().into())
            .collect();
        let mut request = json!({"method": format!("org.composefs.{interface}.{method}"),
            "parameters": params, "more": true});
        if method == "GetInfo" {
            request.as_object_mut().unwrap().remove("parameters");
        }
        let request = request.to_string();
        let call = serde_json::from_str(&request).unwrap();
        let (reply, fds) = self.service.handle(&call, &mut self.conn, fds).await;
        assert!(fds.is_empty(), "unexpected reply fds for {method}");
        match reply {
            MethodReply::Single(params) => json!({"parameters": params}),
            MethodReply::Error(e) => serde_json::to_value(e).unwrap(),
            MethodReply::Multi(mut stream) => {
                let (result, fds) = stream.next().await.expect("one terminal error");
                assert!(fds.is_empty());
                let error = serde_json::to_value(result.unwrap_err()).unwrap();
                assert!(stream.next().await.is_none(), "extra frame for {method}");
                error
            }
        }
    }

    async fn repo(&mut self, method: &str, params: Value) -> Value {
        self.call("Repository", method, params, 0).await
    }

    async fn open(&mut self, algorithm: &str) -> u64 {
        let reply = self
            .repo(
                "InitRepository",
                json!({"path": self.path(), "algorithm": algorithm}),
            )
            .await;
        assert_eq!(reply["parameters"]["created"], true, "{reply}");
        let reply = self
            .repo("OpenRepository", json!({"path": self.path()}))
            .await;
        assert_eq!(
            reply["parameters"]["hash_algorithm"],
            if algorithm.contains("sha256") {
                "sha256"
            } else {
                "sha512"
            }
        );
        reply["parameters"]["handle"].as_u64().unwrap()
    }
}

pub(super) fn error(reply: Value, interface: &str, variant: &str, fields: Value) {
    assert_eq!(
        reply["error"],
        format!("org.composefs.{interface}.{variant}"),
        "{reply}"
    );
    if fields.is_null() {
        let params = reply["parameters"].as_object().expect("error parameters");
        assert_eq!(params.len(), 1, "{reply}");
        assert!(!params["message"].as_str().unwrap().is_empty(), "{reply}");
    } else {
        assert_eq!(reply["parameters"], fields);
    }
}

fn repository_calls(handle: u64) -> Vec<(&'static str, Value)> {
    vec![
        ("CloseRepository", json!({"handle": handle})),
        ("Fsck", json!({"handle": handle})),
        (
            "Gc",
            json!({"handle": handle, "dry_run": true, "roots": []}),
        ),
        ("ImageObjects", json!({"handle": handle, "name": "missing"})),
        ("ListImageRefs", json!({"handle": handle})),
        (
            "Mount",
            json!({"handle": handle, "name": "missing", "options": {}}),
        ),
    ]
}

#[tokio::test]
async fn repository_validation_and_handles() {
    assert_eq!(parse_algorithm(None).unwrap(), Algorithm::SHA512);
    assert_eq!(
        CfsctlService::resolve_selector(Some("path".into()), Some(false), None).unwrap(),
        PathBuf::from("path")
    );
    for algorithm in ["fsverity-sha256-12", "fsverity-sha512-12"] {
        let mut f = Fixture::new();
        for (path, user, system) in [
            (None, false, false),
            (None, true, true),
            (Some("unused"), true, false),
            (Some("unused"), false, true),
            (Some("unused"), true, true),
        ] {
            error(
                f.repo(
                    "OpenRepository",
                    json!({"path": path, "user": user, "system": system}),
                )
                .await,
                "Repository",
                "InvalidSpec",
                json!({"message": "exactly one of `path`, `user`, `system` must be set"}),
            );
        }
        for method in ["InitRepository", "EnsureRepository"] {
            for bad in ["sha256", "fsverity-md5-12", "fsverity-sha256-9"] {
                error(
                    f.repo(method, json!({"path": f.path(), "algorithm": bad}))
                        .await,
                    "Repository",
                    "InvalidSpec",
                    Value::Null,
                );
            }
        }
        error(
            f.repo("OpenRepository", json!({"path": f.path()})).await,
            "Repository",
            "RepoNotFound",
            Value::Null,
        );
        let handle = f.open(algorithm).await;
        assert_eq!(
            f.repo(
                "InitRepository",
                json!({"path": f.path(), "algorithm": algorithm})
            )
            .await["parameters"],
            json!({"created": false})
        );
        for status in ["Created", "Opened", "Opened"] {
            assert_eq!(
                f.repo(
                    "EnsureRepository",
                    json!({"path": f.dir.path().join("ensured"), "algorithm": algorithm})
                )
                .await["parameters"]["status"],
                status
            );
        }
        for method in ["ImageObjects", "Mount"] {
            error(
                f.repo(
                    method,
                    json!({"handle": handle, "name": "missing", "options": {}}),
                )
                .await,
                "Repository",
                "NoSuchRef",
                json!({"reference": "missing"}),
            );
        }
        for (overlay, count) in [(false, 1), (true, 0), (true, 1), (true, 3)] {
            error(
                f.call(
                    "Repository",
                    "Mount",
                    json!({"handle": handle, "name": "missing", "options": {"overlay": overlay}}),
                    count,
                )
                .await,
                "Repository",
                "InvalidSpec",
                json!({"message": format!("Mount expects {} fds for the requested options, got {count}", if overlay {2} else {0})}),
            );
        }
        assert_eq!(
            f.repo("CloseRepository", json!({"handle": handle})).await,
            json!({"parameters": null})
        );
        for invalid in [0, handle] {
            for (method, params) in repository_calls(invalid) {
                error(
                    f.repo(method, params).await,
                    "Repository",
                    "InvalidHandle",
                    json!({"handle": invalid}),
                );
            }
        }
        assert!(
            f.repo("OpenRepository", json!({"path": f.path()})).await["parameters"]["handle"]
                .as_u64()
                .unwrap()
                > handle
        );
    }
}

#[tokio::test]
async fn repository_helper_failures() {
    let mut f = Fixture::new();
    let handle = f.open("fsverity-sha256-12").await;
    error(
        f.repo(
            "InitRepository",
            json!({"path": f.path(), "algorithm": "fsverity-sha512-12"}),
        )
        .await,
        "Repository",
        "InternalError",
        Value::Null,
    );
    let blocker = f.dir.path().join("file");
    std::fs::write(&blocker, b"not a directory").unwrap();
    for method in ["InitRepository", "EnsureRepository"] {
        error(
            f.repo(method, json!({"path": blocker.join("child")})).await,
            "Repository",
            "InternalError",
            Value::Null,
        );
    }
    std::fs::create_dir_all(f.path().join("images/refs")).unwrap();
    std::fs::write(f.path().join("images/refs/bad"), b"not a symlink").unwrap();
    for method in ["ImageObjects", "Mount"] {
        error(
            f.repo(
                method,
                json!({"handle": handle, "name": "bad", "options": {}}),
            )
            .await,
            "Repository",
            "InternalError",
            Value::Null,
        );
    }
    std::fs::remove_dir_all(f.path().join("images/refs")).unwrap();
    std::fs::write(f.path().join("images/refs"), b"not a directory").unwrap();
    for (method, params) in [
        ("Fsck", json!({"handle": handle, "metadata_only": true})),
        ("Fsck", json!({"handle": handle, "metadata_only": false})),
        (
            "Gc",
            json!({"handle": handle, "dry_run": true, "roots": []}),
        ),
        (
            "Gc",
            json!({"handle": handle, "dry_run": false, "roots": []}),
        ),
        ("ListImageRefs", json!({"handle": handle})),
    ] {
        error(
            f.repo(method, params).await,
            "Repository",
            "InternalError",
            Value::Null,
        );
    }
}

#[cfg(feature = "oci")]
mod oci_errors {
    use super::*;
    const DIGEST: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";

    fn calls(handle: u64) -> Vec<(&'static str, Value)> {
        vec![
            ("ListImages", json!({"handle": handle})),
            ("Check", json!({"handle": handle})),
            ("Inspect", json!({"handle": handle, "image": "missing"})),
            (
                "Tag",
                json!({"handle": handle, "manifest_digest": DIGEST, "name": "tag"}),
            ),
            ("Untag", json!({"handle": handle, "name": "tag"})),
            (
                "ComputeId",
                json!({"handle": handle, "image": "missing", "bootable": false}),
            ),
            (
                "Pull",
                json!({"handle": handle, "image": "unused", "local_fetch": "disabled", "bootable": false}),
            ),
            (
                "OciMount",
                json!({"handle": handle, "image": "missing", "bootable": false, "options": {}}),
            ),
            ("HasLayer", json!({"handle": handle, "diff_id": DIGEST})),
            (
                "GetLayer",
                json!({"handle": handle, "params": {"diff_id": DIGEST}}),
            ),
            (
                "PutLayer",
                json!({"handle": handle, "diff_id": DIGEST, "zerocopy": false}),
            ),
            (
                "FinalizeImage",
                json!({"handle": handle, "manifest_json": "{}", "config_json": "{}", "layers": []}),
            ),
        ]
    }

    #[tokio::test]
    async fn handles_and_validation() {
        for algorithm in ["fsverity-sha256-12", "fsverity-sha512-12"] {
            let mut f = Fixture::new();
            let handle = f.open(algorithm).await;
            f.repo("CloseRepository", json!({"handle": handle})).await;
            for invalid in [0, handle] {
                for (method, params) in calls(invalid) {
                    error(
                        f.call(
                            "Oci",
                            method,
                            params,
                            if method == "PutLayer" { 2 } else { 0 },
                        )
                        .await,
                        "Oci",
                        "InvalidHandle",
                        json!({"handle": invalid}),
                    );
                }
            }
            let handle =
                f.repo("OpenRepository", json!({"path": f.path()})).await["parameters"]["handle"]
                    .as_u64()
                    .unwrap();
            assert_eq!(
                f.call("Oci", "GetInfo", json!({}), 0).await["parameters"],
                json!({"features": ["splitdirfdstream-v0"]})
            );
            for (method, params) in [
                ("Inspect", json!({"image": "@sha256:bad"})),
                (
                    "ComputeId",
                    json!({"image": "@sha256:bad", "bootable": false}),
                ),
                (
                    "ComputeId",
                    json!({"image": "missing", "verity": "bad", "bootable": false}),
                ),
                ("Tag", json!({"manifest_digest": "bad", "name": "tag"})),
                (
                    "OciMount",
                    json!({"image": "sha256:bad", "bootable": false, "options": {}}),
                ),
                ("HasLayer", json!({"diff_id": "bad"})),
                ("GetLayer", json!({"params": {"diff_id": "bad"}})),
                ("PutLayer", json!({"diff_id": "bad", "zerocopy": false})),
                (
                    "FinalizeImage",
                    json!({"manifest_json": "{}", "config_json": "{}", "layers": [{"diff_id": "bad", "layer_verity": "bad"}]}),
                ),
                (
                    "FinalizeImage",
                    json!({"manifest_json": "{}", "config_json": "{}", "layers": [{"diff_id": DIGEST, "layer_verity": "bad"}]}),
                ),
            ] {
                let mut params = params;
                params["handle"] = json!(handle);
                error(
                    f.call(
                        "Oci",
                        method,
                        params,
                        if method == "PutLayer" { 2 } else { 0 },
                    )
                    .await,
                    "Oci",
                    "InvalidDigest",
                    Value::Null,
                );
            }
            for (method, params) in [
                ("GetLayer", json!({"params": {}})),
                ("PutLayer", json!({"diff_id": DIGEST, "zerocopy": false})),
            ] {
                let mut params = params;
                params["handle"] = json!(handle);
                error(
                    f.call("Oci", method, params, 0).await,
                    "Oci",
                    "InvalidRequest",
                    Value::Null,
                );
            }
            for (bootable, xattrs, variant) in [
                (false, None, "InvalidRequest"),
                (
                    true,
                    Some(composefs_oci::XattrFiltering::default()),
                    "InvalidRequest",
                ),
                (true, None, "InvalidDigest"),
            ] {
                error(f.call("Oci", "Pull", json!({"handle": handle, "image": "unused", "local_fetch": "disabled", "bootable": bootable, "xattrs": xattrs, "expected_digest": "bad"}), 0).await, "Oci", variant, Value::Null);
            }
            for method in ["Inspect", "ComputeId", "OciMount"] {
                error(f.call("Oci", method, json!({"handle": handle, "image": "missing", "bootable": false, "options": {}}), 0).await,
                    "Oci", "NoSuchImage", json!({"image": "missing"}));
            }
            // The library opens the manifest stream before its typed absence
            // check. Missing digests still produce untyped I/O errors.
            for (method, image) in [
                ("Inspect", format!("@{DIGEST}")),
                ("ComputeId", format!("@{DIGEST}")),
                ("OciMount", DIGEST.into()),
            ] {
                error(
                    f.call(
                        "Oci",
                        method,
                        json!({"handle": handle, "image": image, "bootable": false, "options": {}}),
                        0,
                    )
                    .await,
                    "Oci",
                    "InternalError",
                    Value::Null,
                );
            }
            let check = f
                .call(
                    "Oci",
                    "Check",
                    json!({"handle": handle, "image": "missing"}),
                    0,
                )
                .await;
            assert_eq!(check["parameters"]["ok"], false, "{check}");
            assert_eq!(check["parameters"]["images_corrupted"], 1);
            for method in ["Tag", "Untag", "FinalizeImage"] {
                let params = calls(handle)
                    .into_iter()
                    .find(|(m, _)| *m == method)
                    .unwrap()
                    .1;
                error(
                    f.call("Oci", method, params, 0).await,
                    "Oci",
                    "InternalError",
                    Value::Null,
                );
            }
        }
    }

    #[tokio::test]
    async fn helper_io_errors() {
        let mut f = Fixture::new();
        let handle = f.open("fsverity-sha256-12").await;
        std::fs::create_dir_all(f.path().join("streams")).unwrap();
        std::fs::write(f.path().join("streams/refs"), b"not a directory").unwrap();
        for (method, params) in calls(handle) {
            if matches!(
                method,
                "ListImages" | "Check" | "Inspect" | "ComputeId" | "OciMount"
            ) {
                error(
                    f.call("Oci", method, params, 0).await,
                    "Oci",
                    "InternalError",
                    Value::Null,
                );
            }
        }
        let layer_id = composefs_oci::layer_content_id(&DIGEST.parse().unwrap());
        std::fs::write(f.path().join("streams").join(layer_id), b"not a symlink").unwrap();
        for (method, params) in calls(handle) {
            if matches!(method, "HasLayer" | "GetLayer" | "PutLayer") {
                error(
                    f.call(
                        "Oci",
                        method,
                        params,
                        if method == "PutLayer" { 2 } else { 0 },
                    )
                    .await,
                    "Oci",
                    "InternalError",
                    Value::Null,
                );
            }
        }
        tokio::task::LocalSet::new().run_until(async {
            error(f.call("Oci", "Pull", json!({"handle": handle, "image": format!("oci:{}", f.dir.path().join("absent").display()), "local_fetch": "disabled", "bootable": false}), 0).await, "Oci", "InternalError", Value::Null);
        }).await;
    }

    // PR #22's minimal local-layout fixture, using an uncompressed tar layer.
    fn bootable_layout(parent: &Path) -> PathBuf {
        let layout = parent.join("bootable-oci-image");
        let blobs = layout.join("blobs/sha256");
        std::fs::create_dir_all(&blobs).unwrap();
        let mut tar = tar::Builder::new(Vec::new());
        for path in ["usr/", "boot/", "sysroot/"] {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Directory);
            header.set_size(0);
            header.set_mode(0o755);
            header.set_cksum();
            tar.append_data(&mut header, path, &[] as &[u8]).unwrap();
        }
        let blob = |data: &[u8], media_type: &str| {
            let digest = composefs_oci::sha256_content_digest(data).to_string();
            std::fs::write(blobs.join(digest.strip_prefix("sha256:").unwrap()), data).unwrap();
            json!({"mediaType": media_type, "digest": digest, "size": data.len()})
        };
        let layer = blob(
            &tar.into_inner().unwrap(),
            "application/vnd.oci.image.layer.v1.tar",
        );
        let config = blob(&serde_json::to_vec(&json!({"architecture": "amd64", "os": "linux", "rootfs": {"type": "layers", "diff_ids": [layer["digest"]]}})).unwrap(), "application/vnd.oci.image.config.v1+json");
        let manifest = blob(
            &serde_json::to_vec(&json!({"schemaVersion": 2, "config": config, "layers": [layer]}))
                .unwrap(),
            "application/vnd.oci.image.manifest.v1+json",
        );
        std::fs::write(
            layout.join("oci-layout"),
            br#"{"imageLayoutVersion":"1.0.0"}"#,
        )
        .unwrap();
        std::fs::write(
            layout.join("index.json"),
            serde_json::to_vec(&json!({"schemaVersion": 2, "manifests": [manifest]})).unwrap(),
        )
        .unwrap();
        layout
    }

    #[tokio::test]
    async fn pull_boot_image_mismatch() {
        tokio::task::LocalSet::new().run_until(async {
            let mut f = Fixture::new();
            let handle = f.open("fsverity-sha512-12").await;
            let layout = bootable_layout(f.dir.path());
            let expected = "00".repeat(64);
            let tried = composefs_oci::XattrFiltering::VARIANTS.len() * composefs::erofs::format::FormatVersion::BOOT_VERSIONS.len();
            let request = json!({"method": "org.composefs.Oci.Pull", "more": true, "parameters": {"handle": handle, "image": format!("oci:{}", layout.display()), "local_fetch": "disabled", "bootable": true, "expected_digest": expected}}).to_string();
            let call = serde_json::from_str(&request).unwrap();
            let (reply, fds) = f.service.handle(&call, &mut f.conn, vec![]).await;
            assert!(fds.is_empty());
            let MethodReply::Multi(mut stream) = reply else { panic!("expected pull stream") };
            let (mut progress, mut mismatch) = (0, false);
            while let Some((reply, fds)) = stream.next().await {
                assert!(!mismatch, "frame after terminal BootImageMismatch");
                assert!(fds.is_empty());
                match reply {
                    Ok(frame) => {
                        let frame = serde_json::to_value(frame).unwrap();
                        assert_eq!(frame["continues"], true, "{frame}");
                        assert!(frame["parameters"]["completed"].is_null());
                        progress += 1;
                    }
                    Err(err) => {
                        error(serde_json::to_value(err).unwrap(), "Oci", "BootImageMismatch", json!({"expected": expected, "tried": tried}));
                        mismatch = true;
                    }
                }
            }
            assert!(progress > 0);
            assert!(mismatch);
        }).await;
    }
}
