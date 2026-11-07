//! Packaged modules: `<data>/modules/<id>/module.wasm` the App runs. A package that fails
//! to load is skipped and reported; the host keeps running.

use std::path::{Path, PathBuf};

/// The modules folder inside the data directory.
pub const DIR: &str = "modules";
/// A module the App runs.
pub const WASM: &str = "module.wasm";

/// One package folder found in the modules folder.
pub struct Package {
    pub dir: PathBuf,
}

impl Package {
    /// The module the App runs.
    pub fn wasm(&self) -> Option<PathBuf> {
        let path = self.dir.join(WASM);
        path.is_file().then_some(path)
    }
}

/// Package folders with a wasm module, by folder name.
pub fn scan(root: &Path) -> Vec<Package> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut packages: Vec<Package> = entries
        .filter_map(|entry| entry.ok())
        // Hidden folders are an installer's staged or backed-up packages.
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
        .map(|entry| Package { dir: entry.path() })
        .filter(|package| package.wasm().is_some())
        .collect();
    packages.sort_by(|a, b| a.dir.cmp(&b.dir));
    packages
}

/// The hash the App caches a package's `module.wasm` by (SHA-256, hex). Every start hashes
/// every package, so it uses ring's hardware SHA-256: `sha2` without `asm` is ~7× slower on arm64.
pub fn digest(wasm: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, wasm)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::digest;

    #[test]
    fn the_app_cache_key_is_the_sha256_hex_of_the_module() {
        assert_eq!(
            digest(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
