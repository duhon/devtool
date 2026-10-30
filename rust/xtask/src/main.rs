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

/// A module built as a package: a `cdylib` crate with a `manifest.json` (the shipped ones are
/// in `rust/modules/<dir>`) and its console part (`ui/`, built for `wasm32-unknown-unknown`)
/// when it has one.
#[derive(Clone)]
struct Package {
    id: String,
    version: String,
    library: String,
    ui: Option<String>,
}

/// A crate's `version`, following `version.workspace = true` to the root manifest.
fn crate_version(root: &Path, manifest: &Path) -> Result<String> {
    let version = |path: &Path| -> Result<Option<String>> {
        Ok(fs::read_to_string(path)?.lines().find_map(|line| {
            line.strip_prefix("version = \"")?
                .strip_suffix('"')
                .map(str::to_owned)
        }))
    };
    match version(manifest)? {
        Some(version) => Ok(version),
        None => version(&root.join("Cargo.toml"))?
            .with_context(|| format!("No version for {}", manifest.display())),
    }
}

fn crate_name(manifest: &Path) -> Result<String> {
    fs::read_to_string(manifest)?
        .lines()
        .find_map(|line| line.strip_prefix("name = \"")?.strip_suffix('"'))
        .map(str::to_owned)
        .with_context(|| format!("No package name in {}", manifest.display()))
}

fn is_module(dir: &Path) -> bool {
    fs::read_to_string(dir.join("Cargo.toml")).is_ok_and(|text| text.contains("\"cdylib\""))
}

/// The package of the module crate in `dir`.
fn package_at(root: &Path, dir: &Path) -> Result<Package> {
    ensure!(
        is_module(dir),
        "{} is not a module crate (a cdylib with a manifest.json)",
        dir.display()
    );
    let cargo = dir.join("Cargo.toml");
    let manifest = dir.join("manifest.json");
    let manifest: Value = serde_json::from_slice(
        &fs::read(&manifest).with_context(|| format!("Reading {}", manifest.display()))?,
    )?;
    let ui = dir.join("ui/Cargo.toml");
    let package = Package {
        id: manifest["id"]
            .as_str()
            .context("Module manifest without id")?
            .to_owned(),
        version: crate_version(root, &cargo)?,
        library: crate_name(&cargo)?,
        ui: ui.exists().then(|| crate_name(&ui)).transpose()?,
    };
    services_contracts::package::check_id(&package.id).map_err(anyhow::Error::msg)?;
    services_contracts::package::version(&package.version).map_err(anyhow::Error::msg)?;
    Ok(package)
}

/// The shipped modules: the module crates in `rust/modules`.
fn packages(root: &Path) -> Result<Vec<Package>> {
    let mut packages = Vec::new();
    for entry in fs::read_dir(root.join("rust/modules"))? {
        let dir = entry?.path();
        if is_module(&dir) {
            packages.push(package_at(root, &dir)?);
        }
    }
    packages.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(packages)
}

/// Builds `packages` and lays each out as `<modules>/<id>/{package.json,module.dylib,ui.wasm}`;
/// `link` symlinks the build outputs instead of copying them (a development data directory).
fn build_packages(
    root: &Path,
    packages: &[Package],
    modules: &Path,
    release: bool,
    link: bool,
) -> Result<()> {
    let profile = if release { "release" } else { "debug" };
    for wasm in [false, true] {
        let mut args = vec!["build", "--lib"];
        if release {
            args.push("--release");
        }
        if wasm {
            args.extend(["--target", "wasm32-unknown-unknown"]);
        }
        let crates: Vec<&str> = packages
            .iter()
            .filter_map(|package| {
                if wasm {
                    package.ui.as_deref()
                } else {
                    Some(&package.library)
                }
            })
            .collect();
        if crates.is_empty() {
            continue;
        }
        for name in crates {
            args.extend(["-p", name]);
        }
        run(root, "cargo", &args)?;
    }
    for package in packages {
        let dir = modules.join(&package.id);
        fs::create_dir_all(&dir)?;
        fs::write(
            dir.join(services_contracts::package::PACKAGE_FILE),
            serde_json::to_vec_pretty(&services_contracts::package::Package {
                id: package.id.clone(),
                version: package.version.clone(),
            })?,
        )?;
        let mut files = vec![(
            target(root)
                .join(profile)
                .join(format!("lib{}.dylib", package.library.replace('-', "_"))),
            "module.dylib",
        )];
        if let Some(ui) = &package.ui {
            files.push((
                target(root)
                    .join("wasm32-unknown-unknown")
                    .join(profile)
                    .join(format!("{}.wasm", ui.replace('-', "_"))),
                "ui.wasm",
            ));
        }
        for (built, name) in files {
            let placed = dir.join(name);
            if fs::symlink_metadata(&placed).is_ok() {
                fs::remove_file(&placed)?;
            }
            if link {
                std::os::unix::fs::symlink(&built, &placed)?;
            } else {
                fs::copy(&built, &placed)?;
            }
        }
    }
    Ok(())
}

const PACKAGE_USAGE: &str = "package <module id | module crate folder>... [--out FOLDER]";

/// Package files (`<id>-<version>.module.tgz`, Settings → Modules → Install from file) of
/// shipped modules (by id) and module crates (by folder), built for release into `--out`
/// (`target/packages` by default). Returns the files it writes.
fn package_files(root: &Path, args: &[String]) -> Result<Vec<PathBuf>> {
    let mut out = target(root).join("packages");
    let mut selected = Vec::<Package>::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--out" {
            out = PathBuf::from(args.next().context("--out needs a folder")?);
            continue;
        }
        let package = if Path::new(arg).join("Cargo.toml").is_file() {
            package_at(root, &fs::canonicalize(arg)?)?
        } else {
            packages(root)?
                .into_iter()
                .find(|package| package.id == *arg)
                .with_context(|| {
                    format!("{arg:?} is neither a shipped module id nor a module crate folder")
                })?
        };
        ensure!(
            selected.iter().all(|other| other.id != package.id),
            "Module {} is named twice",
            package.id
        );
        selected.push(package);
    }
    ensure!(
        !selected.is_empty(),
        "Usage: cargo run -p xtask -- {PACKAGE_USAGE}"
    );
    let staging = tempfile::tempdir()?;
    build_packages(root, &selected, staging.path(), true, false)?;
    fs::create_dir_all(&out)?;
    let mut files = Vec::new();
    for package in &selected {
        let archive = services_contracts::package::archive::pack(&staging.path().join(&package.id))
            .map_err(anyhow::Error::msg)?;
        let file = out.join(services_contracts::package::archive_name(
            &services_contracts::package::Package {
                id: package.id.clone(),
                version: package.version.clone(),
            },
        ));
        fs::write(&file, archive)?;
        files.push(file);
    }
    Ok(files)
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
    build_packages(
        root,
        &packages(root)?,
        &payload.join(services_installer::MODULES),
        true,
        false,
    )?;
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
    let version = manifest["version"].as_str().unwrap();
    let installer = fs::read_to_string(root.join("scripts/distribution/install.sh"))?
        .replace("@REPOSITORY@", &repository)
        .replace("@VERSION@", version);
    fs::write(output.join("install.sh"), installer)?;
    println!(
        "Distribution: {}\nLocal install: ./install.sh",
        output.display()
    );
    if !repository.is_empty() {
        println!(
            "Once GitHub release devtool-v{version} has these files:\ncurl -fsSL https://github.com/{repository}/releases/latest/download/install.sh | bash"
        );
    } else {
        println!("Remote installation is not configured; rebuild with --repository OWNER/REPO.");
    }
    println!("This distribution is not Apple-notarized.");
    Ok(())
}

const TEST_USAGE: &str = "cargo run -p xtask -- test behavior [--module NAME] | contract | ui | browser [--scenario NAME]... [--jobs N] [--list] [--fail-fast] | install | perf | all";

/// One command per test type, so a failure says which kind of promise broke.
fn test(root: &Path, args: &[String]) -> Result<()> {
    let words = |line: &str| {
        line.split_whitespace()
            .map(String::from)
            .collect::<Vec<_>>()
    };
    let mut commands = match args.first().map(String::as_str) {
        Some("behavior") => vec![words(
            "test -p services-native --lib --test modules --test core",
        )],
        Some("contract") => vec![
            words("test -p services-native --test conformance --test host"),
            words("test -p services-contracts"),
        ],
        Some("ui") => vec![words("test -p services-ui")],
        Some("browser") => return browser::run(root, browser::Selection::parse(&args[1..])?),
        Some("install") => vec![words("test -p services-installer")],
        Some("perf") => vec![words(
            "test -p services-native --release --test perf -- --ignored --nocapture --test-threads 1",
        )],
        Some("all") => vec![words("test --workspace")],
        _ => bail!("Usage: {TEST_USAGE}"),
    };
    match &args[1..] {
        [] => {}
        [flag, module] if flag == "--module" && args[0] == "behavior" => {
            commands[0].extend(["--".into(), format!("{module}::")]);
        }
        _ => bail!("Usage: {TEST_USAGE}"),
    }
    for command in commands {
        let command: Vec<&str> = command.iter().map(String::as_str).collect();
        run(root, "cargo", &command)?;
    }
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
        Some("modules") => {
            // The development host's data directory is the repository root by default.
            let data = std::env::var_os("SERVICES_DATA_DIR").map_or(root.clone(), PathBuf::from);
            let release = args.get(1).is_some_and(|flag| flag == "--release");
            packages(&root).and_then(|packages| {
                build_packages(
                    &root,
                    &packages,
                    &data.join(services_installer::MODULES),
                    release,
                    true,
                )
            })
        }
        Some("package") => package_files(&root, &args[1..]).map(|files| {
            for file in files {
                println!("{}", file.display());
            }
        }),
        Some("test") => test(&root, &args[1..]),
        Some("test-browser") => browser::run(&root, browser::Selection::all()),
        Some("test-agent") => browser::run(&root, browser::Selection::group(browser::Group::Agent)),
        Some("test-settings") => {
            browser::run(&root, browser::Selection::group(browser::Group::Settings))
        }
        Some("copilot-fixture") => copilot_fixture::run(),
        _ => Err(anyhow::anyhow!(
            "Usage: cargo run -p xtask -- build-extension | build [--repository OWNER/REPO] [--branch distribution] | modules [--release] | {PACKAGE_USAGE} | test-browser | test-agent | test-settings\n       {TEST_USAGE}"
        )),
    };
    if let Err(error) = result {
        eprintln!("xtask failed: {error:#}");
        std::process::exit(1);
    }
}
