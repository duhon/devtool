//! Packaged modules for tests: each package crate is built once per test process into
//! its own target directory and linked into every test data directory's modules folder,
//! the way an installed host finds it.

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
};

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// `(crate, module id)` of every package crate: a `cdylib` in `rust/modules/<dir>`.
pub fn packages() -> &'static [(String, String)] {
    static PACKAGES: OnceLock<Vec<(String, String)>> = OnceLock::new();
    PACKAGES.get_or_init(|| scan("rust/modules"))
}

/// `(crate, module id)` of the example modules in `rust/examples/<dir>`: built like the
/// shipped ones, but never installed in a test data directory.
pub fn examples() -> &'static [(String, String)] {
    static EXAMPLES: OnceLock<Vec<(String, String)>> = OnceLock::new();
    EXAMPLES.get_or_init(|| scan("rust/examples"))
}

fn scan(folder: &str) -> Vec<(String, String)> {
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
        let manifest: serde_json::Value = serde_json::from_slice(
            &std::fs::read(dir.join("manifest.json")).expect("a package has a manifest"),
        )
        .expect("a package manifest is JSON");
        let id = manifest["id"]
            .as_str()
            .expect("a package manifest has an id");
        packages.push((name.to_owned(), id.to_owned()));
    }
    packages.sort();
    packages
}

/// Builds the package libraries (same profile as the tests) and returns where they are.
fn built() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT.get_or_init(|| {
        let target = workspace().join("target/modules");
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let mut command = Command::new(cargo);
        command
            .current_dir(workspace())
            .args(["build", "--quiet", "--lib", "--target-dir"])
            .arg(&target);
        for (package, _) in packages().iter().chain(examples()) {
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
            .expect("cargo must run to build the module packages");
        assert!(status.success(), "module packages must build");
        target.join(profile)
    })
}

/// The built library of a package crate.
pub fn library(package: &str) -> PathBuf {
    let name = package.replace('-', "_");
    built().join(format!(
        "{}{name}{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    ))
}

/// Lays out `<data>/modules/<id>/{package.json,module.dylib}` for every package.
pub fn install(data: &Path) {
    for (package, id) in packages() {
        let dir = data.join("modules").join(id);
        std::fs::create_dir_all(&dir).expect("modules folder must be creatable");
        let link = dir.join(format!("module{}", std::env::consts::DLL_SUFFIX));
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(library(package), &link).expect("package must be linkable");
        std::fs::write(
            dir.join(services_contracts::package::PACKAGE_FILE),
            serde_json::json!({"id": id, "version": env!("CARGO_PKG_VERSION")}).to_string(),
        )
        .expect("package.json must be writable");
    }
}

/// The package file of module `id` (shipped or an example), as `xtask package` builds it,
/// with `version` in its `package.json` (a shipped module reports the workspace version).
pub fn file(id: &str, version: &str) -> Vec<u8> {
    use services_contracts::package::{LIBRARY_FILE, PACKAGE_FILE, archive};
    let (package, _) = packages()
        .iter()
        .chain(examples())
        .find(|(_, package_id)| package_id == id)
        .expect("a package with this module id");
    let dir = std::env::temp_dir().join(format!(
        "services-package-file-{id}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("package folder must be creatable");
    std::os::unix::fs::symlink(library(package), dir.join(LIBRARY_FILE))
        .expect("package must be linkable");
    std::fs::write(
        dir.join(PACKAGE_FILE),
        serde_json::json!({"id": id, "version": version}).to_string(),
    )
    .expect("package.json must be writable");
    let file = archive::pack(&dir).expect("package must pack");
    let _ = std::fs::remove_dir_all(&dir);
    file
}
