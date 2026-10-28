mod browser;
mod copilot_fixture;

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use services_installer::{copy_tree, extension_id, unique_id};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn run(root: &Path, program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .current_dir(root)
        .env("MACOSX_DEPLOYMENT_TARGET", "13.5")
        .status()
        .with_context(|| format!("Run {program}"))?;
    ensure!(
        status.success(),
        "{program} {} failed: {status}",
        args.join(" ")
    );
    Ok(())
}

fn target(root: &Path) -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .map(|value| root.join(value))
        .unwrap_or(root.join("target"))
}

fn build_extension(root: &Path, output: &Path) -> Result<String> {
    run(
        root,
        "cargo",
        &[
            "build",
            "--release",
            "-p",
            "services-ui",
            "--target",
            "wasm32-unknown-unknown",
        ],
    )?;
    if output.exists() {
        fs::remove_dir_all(output)?;
    }
    fs::create_dir_all(output.join("wasm"))?;
    let wasm = target(root).join("wasm32-unknown-unknown/release/services_ui.wasm");
    run(
        root,
        "wasm-bindgen",
        &[
            "--target",
            "web",
            "--out-name",
            "services_ui",
            "--out-dir",
            output
                .join("wasm")
                .to_str()
                .context("Non-UTF8 output path")?,
            wasm.to_str().context("Non-UTF8 WASM path")?,
        ],
    )?;
    let source = root.join("adapters/extension/src");
    for file in ["devtools.html", "devtools.js", "panel.html"] {
        fs::copy(source.join(file), output.join(file))?;
    }
    fs::copy(source.join("panel.js"), output.join("panel.js"))?;
    fs::create_dir_all(output.join("shared"))?;
    let shared = root.join("rust/ui/assets");
    copy_tree(&shared.join("vendor"), &output.join("shared/vendor"))?;
    fs::copy(shared.join("styles.css"), output.join("shared/styles.css"))?;
    let mut manifest: Value = serde_json::from_slice(&fs::read(source.join("manifest.json"))?)?;
    manifest["content_security_policy"] = json!({
        "extension_pages":"script-src 'self' 'wasm-unsafe-eval'; object-src 'self'"
    });
    let id = extension_id(
        manifest["key"]
            .as_str()
            .context("Missing extension public key")?,
    )?;
    fs::write(
        output.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    println!("Extension: {}\nExtension ID: {id}", output.display());
    Ok(id)
}

fn include_licenses(root: &Path, payload: &Path) -> Result<()> {
    let output = Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--locked"])
        .current_dir(root)
        .output()?;
    ensure!(
        output.status.success(),
        "Read Cargo dependency metadata: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: Value = serde_json::from_slice(&output.stdout)?;
    let packages = metadata["packages"]
        .as_array()
        .context("Missing Cargo package metadata")?;
    let mut report = Vec::new();
    for package in packages {
        if package["source"].is_null() {
            continue;
        }
        let directory = Path::new(
            package["manifest_path"]
                .as_str()
                .context("Missing manifest path")?,
        )
        .parent()
        .context("Missing crate directory")?;
        let destination = payload.join("licenses").join(format!(
            "{}-{}",
            package["name"].as_str().unwrap(),
            package["version"].as_str().unwrap()
        ));
        fs::create_dir_all(&destination)?;
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_uppercase();
            if name.starts_with("LICENSE")
                || name.starts_with("COPYING")
                || name.starts_with("NOTICE")
            {
                copy_tree(&entry.path(), &destination.join(entry.file_name()))?;
            }
        }
        report.push(json!({"name":package["name"],"version":package["version"],
            "license":package["license"],"repository":package["repository"]}));
    }
    fs::write(
        payload.join("THIRD-PARTY.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    Ok(())
}

fn build_distribution(root: &Path, args: &[String]) -> Result<()> {
    ensure!(
        cfg!(all(target_os = "macos", target_arch = "aarch64")),
        "Build this distribution on macOS Apple Silicon"
    );
    let mut repository = String::new();
    let mut branch = "distribution".to_owned();
    let mut index = 0;
    while index < args.len() {
        let value = args
            .get(index + 1)
            .context("Build option requires a value")?;
        match args[index].as_str() {
            "--repository" => repository = value.clone(),
            "--branch" => branch = value.clone(),
            flag => bail!("Unknown build option: {flag}"),
        }
        index += 2;
    }
    let valid_component = |value: &str| {
        !value.is_empty()
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    };
    if !repository.is_empty() {
        let parts: Vec<_> = repository.split('/').collect();
        ensure!(
            parts.len() == 2 && parts.iter().all(|part| valid_component(part)),
            "--repository must be OWNER/REPO"
        );
    }
    ensure!(
        !branch.is_empty() && branch.split('/').all(valid_component),
        "Invalid branch"
    );
    run(
        root,
        "cargo",
        &[
            "build",
            "--release",
            "-p",
            "services-native",
            "-p",
            "services-installer",
        ],
    )?;
    let work = tempfile::tempdir()?;
    let payload = work.path().join("ServicesDevTools");
    fs::create_dir_all(payload.join("native/bin"))?;
    for binary in ["services-devtools", "services-install"] {
        fs::copy(
            target(root).join("release").join(binary),
            payload.join("native/bin").join(binary),
        )?;
    }
    let id = build_extension(root, &payload.join("extension"))?;
    let manifest: Value =
        serde_json::from_slice(&fs::read(payload.join("extension/manifest.json"))?)?;
    let metadata = json!({
        "application":"ServicesDevTools","formatVersion":1,"protocolVersion":1,
        "platform":"darwin","arch":"arm64","minimumMacOS":"13.5","version":manifest["version"],
        "buildId":format!("{}-{}",manifest["version"].as_str().unwrap(),unique_id()),
        "extensionId":id,"runtime":"rust","repository":repository,"branch":branch,"notarized":false,
    });
    fs::write(
        payload.join("distribution.json"),
        serde_json::to_vec_pretty(&metadata)?,
    )?;
    include_licenses(root, &payload)?;
    let archive_name = "services-macos-arm64.tar.gz";
    let temporary_archive = work.path().join(archive_name);
    let status = Command::new("/usr/bin/tar")
        .args([
            "-czf",
            temporary_archive.to_str().unwrap(),
            "-C",
            work.path().to_str().unwrap(),
            "ServicesDevTools",
        ])
        .env("COPYFILE_DISABLE", "1")
        .status()?;
    ensure!(status.success(), "Create distribution archive: {status}");
    let output = root.join("dist/distribution");
    fs::create_dir_all(&output)?;
    let archive = output.join(archive_name);
    fs::copy(temporary_archive, &archive)?;
    let digest = Sha256::digest(fs::read(&archive)?);
    fs::write(
        output.join("checksums.txt"),
        format!("{digest:x}  {archive_name}\n"),
    )?;
    let installer = fs::read_to_string(root.join("scripts/distribution/install.sh"))?
        .replace("@REPOSITORY@", &repository)
        .replace("@BRANCH@", &branch);
    fs::write(output.join("install.sh"), installer)?;
    println!(
        "Distribution: {}\nLocal install: ./install.sh",
        output.display()
    );
    if !repository.is_empty() {
        println!(
            "Publish the three files to branch {branch}, then:\ncurl -fsSL https://raw.githubusercontent.com/{repository}/{branch}/install.sh | bash"
        );
    } else {
        println!("Remote installation is not configured; rebuild with --repository OWNER/REPO.");
    }
    println!("This distribution is not Apple-notarized.");
    Ok(())
}

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let root = root();
    let result = match args.first().map(String::as_str) {
        Some("build-extension") => {
            build_extension(&root, &root.join("adapters/extension/dist")).map(|_| ())
        }
        Some("build") => build_distribution(&root, &args[1..]),
        Some("test-browser") => browser::run(&root, false),
        Some("test-agent") => browser::run(&root, true),
        Some("copilot-fixture") => copilot_fixture::run(),
        _ => Err(anyhow::anyhow!(
            "Usage: cargo run -p xtask -- build-extension | build [--repository OWNER/REPO] [--branch distribution] | test-browser | test-agent"
        )),
    };
    if let Err(error) = result {
        eprintln!("xtask failed: {error:#}");
        std::process::exit(1);
    }
}
