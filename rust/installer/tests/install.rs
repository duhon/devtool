use serde_json::{Value, json};
use services_installer::{
    copy_tree, extension_id, install, locations, uninstall, validate_payload,
};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::PathBuf,
};

fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, Value) {
    let directory = tempfile::Builder::new()
        .prefix("services-rust-'test-")
        .tempdir()
        .unwrap();
    let home = directory.path().join("user's home");
    let payload = directory.path().join("payload");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(payload.join("extension")).unwrap();
    fs::create_dir_all(payload.join("native/bin")).unwrap();
    let source: Value = serde_json::from_str(include_str!(
        "../../../adapters/extension/src/manifest.json"
    ))
    .unwrap();
    let key = source["key"].as_str().unwrap();
    let id = extension_id(key).unwrap();
    fs::write(
        payload.join("extension/manifest.json"),
        serde_json::to_vec(&json!({
            "manifest_version":3,"version":"1.0.0","key":key,"permissions":["nativeMessaging"]
        }))
        .unwrap(),
    )
    .unwrap();
    let metadata = json!({
        "application":"ServicesDevTools","formatVersion":1,"protocolVersion":1,
        "platform":"darwin","arch":"arm64","runtime":"rust","version":"1.0.0",
        "buildId":"test-release","extensionId":id,
        "repository":"owner/repository","branch":"devtool",
    });
    fs::write(
        payload.join("distribution.json"),
        serde_json::to_vec(&metadata).unwrap(),
    )
    .unwrap();
    let body =
        serde_json::to_vec(&json!({"id":1,"ok":true,"result":{"protocolVersion":1}})).unwrap();
    let mut frame = (body.len() as u32).to_le_bytes().to_vec();
    frame.extend(body);
    let octal = frame
        .iter()
        .map(|byte| format!("\\{byte:03o}"))
        .collect::<String>();
    let request_size = serde_json::to_vec(&json!({"id":1,"type":"health"}))
        .unwrap()
        .len()
        + 4;
    let script = format!(
        "#!/bin/sh\n/bin/dd bs=1 count={request_size} >/dev/null 2>/dev/null\nprintf '{octal}'\n/bin/cat >/dev/null\n"
    );
    for name in ["services-devtools", "services-install"] {
        let file = payload.join("native/bin").join(name);
        fs::write(&file, &script).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
    }
    (directory, home, payload, metadata)
}

fn archive(payload: &std::path::Path, path: &std::path::Path) -> String {
    use sha2::{Digest, Sha256};
    let output = flate2::write::GzEncoder::new(
        fs::File::create(path).unwrap(),
        flate2::Compression::default(),
    );
    let mut builder = tar::Builder::new(output);
    builder.append_dir_all("ServicesDevTools", payload).unwrap();
    builder.into_inner().unwrap().finish().unwrap();
    format!("{:x}", Sha256::digest(fs::read(path).unwrap()))
}

fn change_version(payload: &std::path::Path, version: &str) {
    for path in [
        payload.join("distribution.json"),
        payload.join("extension/manifest.json"),
    ] {
        let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        value["version"] = json!(version);
        fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
    }
}

#[test]
fn archive_update_preserves_data_and_refuses_repeat_or_downgrade() {
    let (directory, home, payload, _) = fixture();
    let paths = install(&home, &payload).unwrap();
    assert!(!paths.data.join("update-local-signing.json").exists());
    assert!(!paths.data.join("update-installer-signing.json").exists());
    fs::write(paths.data.join("saved-request"), "keep this request").unwrap();
    change_version(&payload, "2.0.0");
    let path = directory.path().join("update.tar.gz");
    let sha = archive(&payload, &path);
    services_installer::updates::install_archive(&home, &path, &sha, "2.0.0").unwrap();
    let installed: Value = serde_json::from_slice(
        &fs::read(paths.base.join("extension/native/distribution.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(installed["version"], "2.0.0");
    let signing: Value =
        serde_json::from_slice(&fs::read(paths.data.join("update-local-signing.json")).unwrap())
            .unwrap();
    assert_eq!(signing["phase"], "ready-for-health-check");
    assert_eq!(signing["signing"]["success"], true);
    assert_eq!(signing["verification"]["success"], true);
    assert_eq!(signing["quarantineAbsent"], true);
    assert!(signing["after"]["quarantine"].is_null());
    assert!(
        signing["stagedExecutable"]
            .as_str()
            .unwrap()
            .contains(".extension-install-")
    );
    let installer_signing: Value = serde_json::from_slice(
        &fs::read(paths.data.join("update-installer-signing.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(installer_signing["phase"], "ready-for-health-check");
    assert_eq!(installer_signing["signing"]["success"], true);
    assert_eq!(installer_signing["verification"]["success"], true);
    assert_eq!(installer_signing["quarantineAbsent"], true);
    assert!(
        installer_signing["stagedExecutable"]
            .as_str()
            .unwrap()
            .ends_with("/native/bin/services-install")
    );
    assert_eq!(
        fs::read_to_string(paths.data.join("saved-request")).unwrap(),
        "keep this request"
    );
    let error =
        services_installer::updates::install_archive(&home, &path, &sha, "2.0.0").unwrap_err();
    assert!(error.to_string().contains("newer"), "{error:#}");
    change_version(&payload, "1.0.0");
    let sha = archive(&payload, &path);
    assert!(services_installer::updates::install_archive(&home, &path, &sha, "1.0.0").is_err());
}

#[test]
fn quarantined_archive_update_clears_only_the_two_staged_executable_markers() {
    let (directory, home, payload, _) = fixture();
    let paths = install(&home, &payload).unwrap();
    change_version(&payload, "2.0.0");
    let path = directory.path().join("update.tar.gz");
    let sha = archive(&payload, &path);
    let marked = std::process::Command::new("/usr/bin/xattr")
        .args([
            "-w",
            "com.apple.quarantine",
            "0081;6ae242f3;Google Chrome for Testing;",
        ])
        .arg(&path)
        .output()
        .unwrap();
    assert!(marked.status.success());
    services_installer::updates::install_archive(&home, &path, &sha, "2.0.0").unwrap();
    let signing: Value =
        serde_json::from_slice(&fs::read(paths.data.join("update-local-signing.json")).unwrap())
            .unwrap();
    assert!(signing["before"]["quarantine"].is_array());
    assert_eq!(signing["quarantineRemoval"]["attempted"], true);
    assert_eq!(signing["quarantineRemoval"]["success"], true);
    assert_eq!(signing["quarantineAbsent"], true);
    assert_eq!(signing["phase"], "ready-for-health-check");
    let installer_signing: Value = serde_json::from_slice(
        &fs::read(paths.data.join("update-installer-signing.json")).unwrap(),
    )
    .unwrap();
    assert!(installer_signing["before"]["quarantine"].is_array());
    assert_eq!(installer_signing["quarantineRemoval"]["success"], true);
    assert_eq!(installer_signing["quarantineAbsent"], true);
    assert_eq!(installer_signing["phase"], "ready-for-health-check");
    for name in ["services-devtools", "services-install"] {
        let executable = paths.base.join("extension/native/bin").join(name);
        let attributes = std::process::Command::new("/usr/bin/xattr")
            .arg(&executable)
            .output()
            .unwrap();
        assert!(attributes.status.success());
        assert!(!String::from_utf8_lossy(&attributes.stdout).contains("com.apple.quarantine"));
    }
    for unchanged in [path, paths.base.join("extension/manifest.json")] {
        let quarantine = std::process::Command::new("/usr/bin/xattr")
            .args(["-p", "com.apple.quarantine"])
            .arg(&unchanged)
            .output()
            .unwrap();
        assert!(quarantine.status.success(), "{}", unchanged.display());
    }
}

#[test]
fn failed_local_signing_preserves_the_installed_version_and_diagnostics() {
    for (name, diagnostics) in [
        ("services-devtools", "update-local-signing.json"),
        ("services-install", "update-installer-signing.json"),
    ] {
        let (directory, home, payload, _) = fixture();
        let paths = install(&home, &payload).unwrap();
        fs::write(paths.data.join("saved-request"), "preserve").unwrap();
        change_version(&payload, "2.0.0");
        let executable = fs::read(std::env::current_exe().unwrap()).unwrap();
        fs::write(payload.join("native/bin").join(name), &executable[..64]).unwrap();
        let path = directory.path().join("update.tar.gz");
        let sha = archive(&payload, &path);
        let error =
            services_installer::updates::install_archive(&home, &path, &sha, "2.0.0").unwrap_err();
        assert!(
            format!("{error:#}").contains("Local ad-hoc signing failed"),
            "{name}: {error:#}"
        );
        let installed: Value = serde_json::from_slice(
            &fs::read(paths.base.join("extension/native/distribution.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(installed["version"], "1.0.0");
        assert_eq!(
            fs::read_to_string(paths.data.join("saved-request")).unwrap(),
            "preserve"
        );
        let signing: Value =
            serde_json::from_slice(&fs::read(paths.data.join(diagnostics)).unwrap()).unwrap();
        assert_eq!(signing["signing"]["success"], false);
        assert_eq!(signing["phase"], "after-signing");
    }
}

#[test]
fn archive_rejections_leave_the_installed_application_and_data_unchanged() {
    for case in ["checksum", "version", "source", "permissions", "health"] {
        let (directory, home, payload, _) = fixture();
        let paths = install(&home, &payload).unwrap();
        let manifest_path = paths.base.join("extension/manifest.json");
        let original = fs::read(&manifest_path).unwrap();
        fs::write(paths.data.join("saved-request"), "preserve").unwrap();
        change_version(&payload, "2.0.0");
        if case == "source" {
            let path = payload.join("distribution.json");
            let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            value["repository"] = json!("foreign/repository");
            fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
        }
        if case == "permissions" {
            let path = payload.join("extension/manifest.json");
            let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            value["permissions"] = json!(["nativeMessaging", "tabs"]);
            fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
        }
        if case == "health" {
            fs::write(
                payload.join("native/bin/services-devtools"),
                "#!/bin/sh\nexit 1\n",
            )
            .unwrap();
        }
        let path = directory.path().join("update.tar.gz");
        let sha = archive(&payload, &path);
        let sha = if case == "checksum" {
            "0".repeat(64)
        } else {
            sha
        };
        let version = if case == "version" { "2.0.1" } else { "2.0.0" };
        assert!(
            services_installer::updates::install_archive(&home, &path, &sha, version).is_err(),
            "{case}"
        );
        assert_eq!(fs::read(&manifest_path).unwrap(), original, "{case}");
        assert_eq!(
            fs::read_to_string(paths.data.join("saved-request")).unwrap(),
            "preserve",
            "{case}"
        );
    }
}

#[test]
fn archive_rejects_links_and_parent_directory_paths_before_installation() {
    use sha2::{Digest, Sha256};
    for case in ["symlink", "hardlink", "traversal"] {
        let (directory, home, payload, _) = fixture();
        let paths = install(&home, &payload).unwrap();
        let original = fs::read(paths.base.join("extension/manifest.json")).unwrap();
        let path = directory.path().join("unsafe.tar.gz");
        let output = flate2::write::GzEncoder::new(
            fs::File::create(&path).unwrap(),
            flate2::Compression::default(),
        );
        let mut builder = tar::Builder::new(output);
        let mut header = tar::Header::new_gnu();
        header.set_size(0);
        header.set_mode(0o644);
        header.set_path("ServicesDevTools/unsafe").unwrap();
        if case == "traversal" {
            let name = b"ServicesDevTools/../../escaped";
            header.as_mut_bytes()[..100].fill(0);
            header.as_mut_bytes()[..name.len()].copy_from_slice(name);
        } else {
            header.set_entry_type(if case == "symlink" {
                tar::EntryType::Symlink
            } else {
                tar::EntryType::Link
            });
            header.set_link_name("../../escaped").unwrap();
        }
        header.set_cksum();
        builder.append(&header, std::io::empty()).unwrap();
        builder.into_inner().unwrap().finish().unwrap();
        let sha = format!("{:x}", Sha256::digest(fs::read(&path).unwrap()));
        assert!(
            services_installer::updates::install_archive(&home, &path, &sha, "2.0.0").is_err(),
            "{case}"
        );
        assert_eq!(
            fs::read(paths.base.join("extension/manifest.json")).unwrap(),
            original,
            "{case}"
        );
        assert!(!directory.path().join("escaped").exists());
    }
}

fn legacy_layout(paths: &services_installer::Locations, payload: &std::path::Path, linked: bool) {
    let release = paths.releases.join("test-release");
    copy_tree(payload, &release).unwrap();
    copy_tree(
        &paths.base.join("extension/native"),
        &release.join("native"),
    )
    .unwrap();
    symlink("releases/test-release", &paths.current).unwrap();
    symlink("current/native", paths.base.join("native")).unwrap();
    if linked {
        fs::remove_dir_all(paths.base.join("extension")).unwrap();
        symlink("current/extension", paths.base.join("extension")).unwrap();
    } else {
        fs::remove_dir_all(paths.base.join("extension/native")).unwrap();
    }
    let mut registration: Value =
        serde_json::from_slice(&fs::read(&paths.registration).unwrap()).unwrap();
    registration["path"] = json!(paths.base.join("native/host"));
    fs::write(
        &paths.registration,
        serde_json::to_vec(&registration).unwrap(),
    )
    .unwrap();
}

fn assert_single_copy(paths: &services_installer::Locations) {
    for path in [&paths.current, &paths.releases, &paths.base.join("native")] {
        assert_eq!(
            fs::symlink_metadata(path).unwrap_err().kind(),
            std::io::ErrorKind::NotFound,
            "{}",
            path.display()
        );
    }
    assert!(
        paths
            .base
            .join("extension/native/bin/services-devtools")
            .is_file()
    );
    assert!(fs::read_dir(&paths.base).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".extension-")
    }));
}

#[test]
fn rejects_invalid_build_paths_and_wrong_extension_identity() {
    let (_directory, _home, payload, mut metadata) = fixture();
    metadata["buildId"] = json!("../../outside");
    fs::write(
        payload.join("distribution.json"),
        serde_json::to_vec(&metadata).unwrap(),
    )
    .unwrap();
    assert!(
        validate_payload(&payload)
            .unwrap_err()
            .to_string()
            .contains("metadata")
    );
    metadata["buildId"] = json!("valid-release");
    metadata["extensionId"] = json!("a".repeat(32));
    fs::write(
        payload.join("distribution.json"),
        serde_json::to_vec(&metadata).unwrap(),
    )
    .unwrap();
    assert!(
        validate_payload(&payload)
            .unwrap_err()
            .to_string()
            .contains("identity")
    );
}

#[test]
fn node_payload_is_not_accepted_as_a_rust_distribution() {
    let (_directory, _home, payload, mut metadata) = fixture();
    metadata["runtime"] = json!("node");
    fs::write(
        payload.join("distribution.json"),
        serde_json::to_vec(&metadata).unwrap(),
    )
    .unwrap();
    assert!(validate_payload(&payload).is_err());
}

#[test]
fn rejects_invalid_public_key_bytes() {
    assert!(extension_id("bm90IGEgcHVibGljIGtleQ==").is_err());
}

#[test]
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn installer_prints_installed_version_and_chrome_steps_for_install_update_and_migration() {
    let (_directory, home, payload, _metadata) = fixture();
    for scenario in ["install", "update", "migration"] {
        if scenario == "migration" {
            legacy_layout(&locations(&home), &payload, true);
        }
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_services-install"))
            .env("HOME", &home)
            .arg("install")
            .arg(&payload)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{scenario}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!(
                "Services DevTools 1.0.0: installation completed.\n\
                 chrome://extensions -> Developer mode -> Load unpacked\n\
                 Cmd+Shift+G, paste:\n\
                 {}\n\
                 Enter -> Select\n",
                locations(&home).base.join("extension").display()
            ),
            "{scenario}"
        );
    }
}

#[test]
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn refuses_concurrent_installation_and_unmanaged_directories() {
    let (_directory, home, payload, _metadata) = fixture();
    let paths = locations(&home);
    fs::create_dir_all(paths.base.join("install.lock")).unwrap();
    assert!(
        install(&home, &payload)
            .unwrap_err()
            .to_string()
            .contains("Another installation")
    );
    fs::remove_dir(paths.base.join("install.lock")).unwrap();
    fs::create_dir(paths.base.join("extension")).unwrap();
    assert!(
        install(&home, &payload)
            .unwrap_err()
            .to_string()
            .contains("unmanaged path")
    );
}

#[test]
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn update_and_uninstall_keep_data_and_restore_previous_registration() {
    let (_directory, home, payload, mut metadata) = fixture();
    let paths = locations(&home);
    fs::create_dir_all(paths.registration.parent().unwrap()).unwrap();
    let original = "{\"path\":\"/previous/dev/host\"}";
    fs::write(&paths.registration, original).unwrap();
    install(&home, &payload).unwrap();
    let profile = paths.data.join(".variable-profiles.json");
    fs::write(&profile, "{\"preserve\":true}").unwrap();
    assert_single_copy(&paths);
    let manifest: Value = serde_json::from_slice(&fs::read(&paths.registration).unwrap()).unwrap();
    assert_eq!(
        manifest["path"],
        paths
            .base
            .join("extension/native/host")
            .to_string_lossy()
            .as_ref()
    );
    metadata["buildId"] = json!("updated-release");
    fs::write(
        payload.join("distribution.json"),
        serde_json::to_vec(&metadata).unwrap(),
    )
    .unwrap();
    install(&home, &payload).unwrap();
    install(&home, &payload).unwrap();
    assert_eq!(fs::read_to_string(&profile).unwrap(), "{\"preserve\":true}");
    assert_single_copy(&paths);
    uninstall(&home).unwrap();
    assert!(!paths.releases.exists());
    assert!(!paths.base.join("extension").exists());
    assert_eq!(fs::read_to_string(&paths.registration).unwrap(), original);
    assert_eq!(fs::read_to_string(&profile).unwrap(), "{\"preserve\":true}");
}

#[test]
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn extension_updates_use_a_permanent_directory_and_remove_stale_files() {
    let (_directory, home, payload, mut metadata) = fixture();
    fs::write(payload.join("extension/probe.txt"), "old").unwrap();
    fs::write(payload.join("extension/obsolete.js"), "old").unwrap();
    let paths = install(&home, &payload).unwrap();
    let extension = paths.base.join("extension");
    assert!(fs::symlink_metadata(&extension).unwrap().is_dir());
    let permanent = fs::canonicalize(&extension).unwrap();
    assert_eq!(
        permanent,
        fs::canonicalize(&paths.base).unwrap().join("extension")
    );
    metadata["buildId"] = json!("new-extension");
    fs::write(
        payload.join("distribution.json"),
        serde_json::to_vec(&metadata).unwrap(),
    )
    .unwrap();
    fs::write(payload.join("extension/probe.txt"), "new").unwrap();
    fs::remove_file(payload.join("extension/obsolete.js")).unwrap();
    install(&home, &payload).unwrap();
    assert_eq!(fs::canonicalize(&extension).unwrap(), permanent);
    assert_eq!(
        fs::read_to_string(extension.join("probe.txt")).unwrap(),
        "new"
    );
    assert!(!extension.join("obsolete.js").exists());
    assert_single_copy(&paths);
    assert!(fs::read_dir(&paths.base).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".extension-")
    }));
}

#[test]
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn legacy_extension_link_migration_rolls_back_and_then_updates_in_place() {
    let (_directory, home, payload, mut metadata) = fixture();
    fs::write(payload.join("extension/probe.txt"), "old").unwrap();
    let paths = install(&home, &payload).unwrap();
    legacy_layout(&paths, &payload, true);
    let extension = paths.base.join("extension");
    fs::remove_dir_all(&extension).unwrap();
    symlink("current/extension", &extension).unwrap();
    let old_uninstall = fs::read(paths.base.join("uninstall")).unwrap();
    fs::remove_file(paths.base.join("uninstall")).unwrap();
    fs::create_dir(paths.base.join("uninstall")).unwrap();
    metadata["buildId"] = json!("migrated-extension");
    fs::write(
        payload.join("distribution.json"),
        serde_json::to_vec(&metadata).unwrap(),
    )
    .unwrap();
    fs::write(payload.join("extension/probe.txt"), "new").unwrap();
    assert!(install(&home, &payload).is_err());
    assert_eq!(
        fs::read_link(&extension).unwrap(),
        PathBuf::from("current/extension")
    );
    assert_eq!(
        fs::read_to_string(extension.join("probe.txt")).unwrap(),
        "old"
    );
    fs::remove_dir(paths.base.join("uninstall")).unwrap();
    fs::write(paths.base.join("uninstall"), old_uninstall).unwrap();
    install(&home, &payload).unwrap();
    assert!(fs::symlink_metadata(&extension).unwrap().is_dir());
    assert_eq!(
        fs::read_to_string(extension.join("probe.txt")).unwrap(),
        "new"
    );
    assert_single_copy(&paths);
    uninstall(&home).unwrap();
    assert!(!extension.exists());
}

#[test]
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn an_unrecognized_extension_directory_is_not_deleted_on_uninstall() {
    let (_directory, home, payload, _metadata) = fixture();
    let paths = install(&home, &payload).unwrap();
    let extension = paths.base.join("extension");
    fs::write(extension.join("manifest.json"), "{}").unwrap();
    fs::write(extension.join("keep.txt"), "foreign").unwrap();
    let registration = fs::read(&paths.registration).unwrap();
    assert!(uninstall(&home).is_err());
    assert_eq!(
        fs::read_to_string(extension.join("keep.txt")).unwrap(),
        "foreign"
    );
    assert_eq!(fs::read(&paths.registration).unwrap(), registration);
}

#[test]
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn migration_works_after_old_releases_have_been_deleted() {
    for linked in [false, true] {
        let (_directory, home, payload, _metadata) = fixture();
        let paths = install(&home, &payload).unwrap();
        let profile = paths.data.join(".variable-profiles.json");
        fs::write(&profile, "{\"keep\":true}").unwrap();
        legacy_layout(&paths, &payload, linked);
        fs::remove_dir_all(&paths.releases).unwrap();
        install(&home, &payload).unwrap();
        assert_single_copy(&paths);
        let registration: Value =
            serde_json::from_slice(&fs::read(&paths.registration).unwrap()).unwrap();
        assert_eq!(
            registration["path"],
            json!(paths.base.join("extension/native/host"))
        );
        assert_eq!(fs::read_to_string(&profile).unwrap(), "{\"keep\":true}");
    }
}

#[test]
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn failed_host_check_preserves_the_installed_copy() {
    let (_directory, home, payload, mut metadata) = fixture();
    let paths = install(&home, &payload).unwrap();
    let registration = fs::read(&paths.registration).unwrap();
    metadata["buildId"] = json!("broken-release");
    fs::write(
        payload.join("distribution.json"),
        serde_json::to_vec(&metadata).unwrap(),
    )
    .unwrap();
    fs::write(
        payload.join("native/bin/services-devtools"),
        "#!/bin/sh\nexit 7\n",
    )
    .unwrap();
    assert!(install(&home, &payload).is_err());
    let state: Value = serde_json::from_slice(&fs::read(&paths.state).unwrap()).unwrap();
    assert_eq!(state["buildId"], "test-release");
    assert_eq!(fs::read(&paths.registration).unwrap(), registration);
    assert_single_copy(&paths);
}

#[test]
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn invalid_managed_file_preserves_registration_and_installed_files() {
    let (_directory, home, payload, mut metadata) = fixture();
    fs::write(payload.join("extension/probe.txt"), "old").unwrap();
    let paths = install(&home, &payload).unwrap();
    let registration = fs::read(&paths.registration).unwrap();
    let state = fs::read(&paths.state).unwrap();
    fs::remove_file(paths.base.join("uninstall")).unwrap();
    fs::create_dir(paths.base.join("uninstall")).unwrap();
    metadata["buildId"] = json!("failed-activation");
    fs::write(payload.join("extension/probe.txt"), "new").unwrap();
    fs::write(
        payload.join("distribution.json"),
        serde_json::to_vec(&metadata).unwrap(),
    )
    .unwrap();
    assert!(install(&home, &payload).is_err());
    assert_single_copy(&paths);
    assert_eq!(fs::read(&paths.registration).unwrap(), registration);
    assert_eq!(fs::read(&paths.state).unwrap(), state);
    assert!(
        fs::symlink_metadata(paths.base.join("extension"))
            .unwrap()
            .is_dir()
    );
    assert_eq!(
        fs::read_to_string(paths.base.join("extension/probe.txt")).unwrap(),
        "old"
    );
}

fn remember_custom_profile(paths: &services_installer::Locations) -> PathBuf {
    let file = paths
        .home
        .join("Chrome's custom profile/NativeMessagingHosts/com.chrome_extentions.host.json");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::copy(&paths.registration, &file).unwrap();
    let mut state: Value = serde_json::from_slice(&fs::read(&paths.state).unwrap()).unwrap();
    state["registrations"].as_array_mut().unwrap().push(json!({
        "path":file,"previousRegistration":null,
    }));
    fs::write(&paths.state, serde_json::to_vec_pretty(&state).unwrap()).unwrap();
    file
}

#[test]
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn update_repairs_closed_custom_profiles_and_uninstall_cleans_all_registrations() {
    let (_directory, home, payload, _metadata) = fixture();
    let paths = install(&home, &payload).unwrap();
    let custom = remember_custom_profile(&paths);
    fs::remove_file(&custom).unwrap();
    let data = paths.data.join(".variable-profiles.json");
    fs::write(&data, "preserved fixture").unwrap();
    install(&home, &payload).unwrap();
    let restored: Value = serde_json::from_slice(&fs::read(&custom).unwrap()).unwrap();
    assert_eq!(
        restored["allowed_origins"],
        json!(["chrome-extension://ckicpgdfkgcmgnclbfbcbkillcpaffnb/"])
    );
    assert_eq!(
        restored["path"],
        paths
            .base
            .join("extension/native/host")
            .to_string_lossy()
            .as_ref()
    );
    uninstall(&home).unwrap();
    assert!(!custom.exists());
    assert!(!paths.registration.exists());
    assert_eq!(fs::read_to_string(&data).unwrap(), "preserved fixture");
}

#[test]
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn modified_custom_registration_is_preserved_on_update_and_uninstall() {
    let (_directory, home, payload, _metadata) = fixture();
    let paths = install(&home, &payload).unwrap();
    let custom = remember_custom_profile(&paths);
    let foreign = "{\"path\":\"/other/installation/host\"}";
    fs::write(&custom, foreign).unwrap();
    install(&home, &payload).unwrap();
    assert_eq!(fs::read_to_string(&custom).unwrap(), foreign);
    uninstall(&home).unwrap();
    assert_eq!(fs::read_to_string(&custom).unwrap(), foreign);
    assert!(!paths.registration.exists());
}

#[test]
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn existing_manual_registration_is_adopted_without_opening_the_browser() {
    let (_directory, home, payload, _metadata) = fixture();
    let paths = install(&home, &payload).unwrap();
    let google = home.join("Library/Application Support/Google");
    let custom =
        google.join("Chrome-Incognito/NativeMessagingHosts/com.chrome_extentions.host.json");
    let foreign = google.join("Chrome-Other/NativeMessagingHosts/com.chrome_extentions.host.json");
    fs::create_dir_all(custom.parent().unwrap()).unwrap();
    fs::create_dir_all(foreign.parent().unwrap()).unwrap();
    fs::copy(&paths.registration, &custom).unwrap();
    fs::write(&foreign, "{\"path\":\"/other/host\"}").unwrap();
    install(&home, &payload).unwrap();
    let state: Value = serde_json::from_slice(&fs::read(&paths.state).unwrap()).unwrap();
    assert_eq!(state["registrations"].as_array().unwrap().len(), 2);
    assert!(
        state["registrations"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["previousRegistration"].is_null())
    );
    uninstall(&home).unwrap();
    assert!(!custom.exists());
    assert!(!paths.registration.exists());
    assert_eq!(
        fs::read_to_string(&foreign).unwrap(),
        "{\"path\":\"/other/host\"}"
    );
}

#[test]
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn failed_activation_rolls_back_every_registration_and_its_permissions() {
    let (_directory, home, payload, mut metadata) = fixture();
    let paths = install(&home, &payload).unwrap();
    let custom = remember_custom_profile(&paths);
    let mut manifest: Value = serde_json::from_slice(&fs::read(&custom).unwrap()).unwrap();
    manifest["description"] = json!("previous custom registration");
    fs::write(&custom, serde_json::to_vec(&manifest).unwrap()).unwrap();
    fs::set_permissions(&custom, fs::Permissions::from_mode(0o640)).unwrap();
    let old_custom = fs::read(&custom).unwrap();
    let old_default = fs::read(&paths.registration).unwrap();
    let old_state = fs::read(&paths.state).unwrap();
    fs::remove_file(paths.base.join("uninstall")).unwrap();
    fs::create_dir(paths.base.join("uninstall")).unwrap();
    metadata["buildId"] = json!("multi-profile-activation-failure");
    fs::write(
        payload.join("distribution.json"),
        serde_json::to_vec(&metadata).unwrap(),
    )
    .unwrap();
    let failed = std::process::Command::new("/bin/sh")
        .args([
            "-c",
            "umask 077; exec \"$1\" install \"$2\"",
            "installer-test",
        ])
        .arg(env!("CARGO_BIN_EXE_services-install"))
        .arg(&payload)
        .env("HOME", &home)
        .output()
        .unwrap();
    assert!(!failed.status.success());
    assert_eq!(fs::read(&custom).unwrap(), old_custom);
    assert_eq!(
        fs::metadata(&custom).unwrap().permissions().mode() & 0o777,
        0o640
    );
    assert_eq!(fs::read(&paths.registration).unwrap(), old_default);
    assert_eq!(fs::read(&paths.state).unwrap(), old_state);
    assert_single_copy(&paths);
}

#[test]
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn legacy_registration_backup_migrates_and_is_restored_on_uninstall() {
    let (_directory, home, payload, _metadata) = fixture();
    let paths = locations(&home);
    let original = "{\"path\":\"/previous/source/host\"}";
    fs::create_dir_all(paths.registration.parent().unwrap()).unwrap();
    fs::write(&paths.registration, original).unwrap();
    install(&home, &payload).unwrap();
    let mut state: Value = serde_json::from_slice(&fs::read(&paths.state).unwrap()).unwrap();
    state.as_object_mut().unwrap().remove("registrations");
    state["previousRegistration"] = json!(original);
    fs::write(&paths.state, serde_json::to_vec(&state).unwrap()).unwrap();
    install(&home, &payload).unwrap();
    let migrated: Value = serde_json::from_slice(&fs::read(&paths.state).unwrap()).unwrap();
    assert_eq!(
        migrated["registrations"][0]["previousRegistration"],
        original
    );
    assert_eq!(migrated["registrations"].as_array().unwrap().len(), 1);
    uninstall(&home).unwrap();
    assert_eq!(fs::read_to_string(&paths.registration).unwrap(), original);
}

#[test]
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn invalid_recorded_path_cannot_be_removed_as_a_registration() {
    let (_directory, home, payload, _metadata) = fixture();
    let paths = install(&home, &payload).unwrap();
    let mut state: Value = serde_json::from_slice(&fs::read(&paths.state).unwrap()).unwrap();
    state["registrations"][0]["path"] = json!(paths.data.join("not-a-native-manifest"));
    fs::write(&paths.state, serde_json::to_vec(&state).unwrap()).unwrap();
    assert!(uninstall(&home).is_err());
    assert!(paths.base.join("extension").exists());
    assert!(paths.registration.exists());
}
