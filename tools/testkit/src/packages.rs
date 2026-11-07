//! Packaged modules for tests: the SDK module crates are built for `wasm32-unknown-unknown`
//! once per test process into their own target directory and linked into every test data
//! directory's modules folder as `module.wasm`, with the manifest as `package.json`, the way
//! an installed host finds them.

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
};

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// `(crate, module id, version)` of every module crate: a `cdylib` in
/// `modules/services/<dir>` or `modules/panels/<dir>`.
pub fn packages() -> &'static [(String, String, String)] {
    static PACKAGES: OnceLock<Vec<(String, String, String)>> = OnceLock::new();
    PACKAGES.get_or_init(|| {
        let mut packages = scan("modules/services");
        packages.extend(scan("modules/panels"));
        packages.sort();
        packages
    })
}

/// `(crate, module id, version)` of the example modules in `examples/<dir>`: built
/// like the shipped ones, but never installed in a test data directory.
pub fn examples() -> &'static [(String, String, String)] {
    static EXAMPLES: OnceLock<Vec<(String, String, String)>> = OnceLock::new();
    EXAMPLES.get_or_init(|| scan("examples"))
}

/// The version of module `id`: the version of its crate.
pub fn version(id: &str) -> &'static str {
    packages()
        .iter()
        .chain(examples())
        .find(|(_, package_id, _)| package_id == id)
        .map(|(_, _, version)| version.as_str())
        .expect("a package with this module id")
}

fn scan(folder: &str) -> Vec<(String, String, String)> {
    let mut packages = Vec::new();
    let modules = workspace().join(folder);
    for entry in std::fs::read_dir(&modules).expect("module crates must be readable") {
        let dir = entry.expect("module crates must be listable").path();
        let Ok(cargo) = std::fs::read_to_string(dir.join("Cargo.toml")) else {
            continue;
        };
        if !cargo.contains("\"cdylib\"") {
            continue;
        }
        let name = cargo
            .lines()
            .find_map(|line| line.strip_prefix("name = \"")?.strip_suffix('"'))
            .expect("a package crate has a name");
        let id = manifest(&dir)["id"]
            .as_str()
            .expect("a package manifest has an id")
            .to_owned();
        let version = cargo
            .lines()
            .find_map(|line| line.strip_prefix("version = \"")?.strip_suffix('"'))
            .unwrap_or(env!("CARGO_PKG_VERSION"));
        packages.push((name.to_owned(), id, version.to_owned()));
    }
    packages.sort();
    packages
}

fn manifest(dir: &Path) -> serde_json::Value {
    serde_json::from_slice(
        &std::fs::read(dir.join("manifest.json")).expect("a package has a manifest"),
    )
    .expect("a package manifest is JSON")
}

/// The crate folder of module `id`.
fn crate_dir(id: &str) -> PathBuf {
    ["modules/services", "modules/panels", "examples"]
        .iter()
        .flat_map(|folder| {
            std::fs::read_dir(workspace().join(folder))
                .into_iter()
                .flatten()
        })
        .map(|entry| entry.expect("module crates must be listable").path())
        .find(|dir| dir.join("manifest.json").is_file() && manifest(dir)["id"] == id)
        .expect("a module crate with this id")
}

/// Its `package.json`: the manifest with `version`.
fn package_json(id: &str, version: &str) -> String {
    let mut manifest = manifest(&crate_dir(id));
    manifest["version"] = serde_json::json!(version);
    manifest.to_string()
}

/// Builds the modules for `wasm32-unknown-unknown` (same profile as the tests).
fn built_wasm() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT.get_or_init(|| {
        let target = workspace().join("target/modules");
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let mut command = Command::new(cargo);
        command
            .current_dir(workspace())
            .args([
                "build",
                "--quiet",
                "--lib",
                "--target",
                "wasm32-unknown-unknown",
            ])
            .arg("--target-dir")
            .arg(&target);
        for (package, _, _) in packages().iter().chain(examples()) {
            command.args(["-p", package]);
        }
        let profile = if cfg!(debug_assertions) {
            "debug"
        } else {
            command.arg("--release");
            "release"
        };
        let status = command
            .status()
            .expect("cargo must run to build the wasm modules");
        assert!(status.success(), "wasm modules must build");
        target.join("wasm32-unknown-unknown").join(profile)
    })
}

/// The built `module.wasm` of a module crate.
pub fn wasm(package: &str) -> PathBuf {
    built_wasm().join(format!("{}.wasm", package.replace('-', "_")))
}

/// Lays out `<data>/modules/<id>/{package.json,module.wasm}` for every package.
pub fn install(data: &Path) {
    for (package, id, version) in packages() {
        let dir = data.join("modules").join(id);
        std::fs::create_dir_all(&dir).expect("modules folder must be creatable");
        let link = dir.join(services_contracts::package::WASM_FILE);
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(wasm(package), &link).expect("package must be linkable");
        std::fs::write(
            dir.join(services_contracts::package::PACKAGE_FILE),
            package_json(id, version),
        )
        .expect("package.json must be writable");
    }
}

/// The package file of module `id` (shipped or an example), as `xtask package` builds it,
/// with `version` in its `package.json` (a module reports its crate's [`version`]).
pub fn file(id: &str, version: &str) -> Vec<u8> {
    use services_contracts::package::{PACKAGE_FILE, WASM_FILE, archive};
    let (package, _, _) = packages()
        .iter()
        .chain(examples())
        .find(|(_, package_id, _)| package_id == id)
        .expect("a package with this module id");
    let dir = std::env::temp_dir().join(format!(
        "services-package-file-{id}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("package folder must be creatable");
    std::os::unix::fs::symlink(wasm(package), dir.join(WASM_FILE))
        .expect("package must be linkable");
    std::fs::write(dir.join(PACKAGE_FILE), package_json(id, version))
        .expect("package.json must be writable");
    let file = archive::pack(&dir).expect("package must pack");
    let _ = std::fs::remove_dir_all(&dir);
    file
}
