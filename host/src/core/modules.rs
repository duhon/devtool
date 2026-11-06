//! Packaged modules: `<data>/modules/<id>/module.dylib` with the console's `ui.wasm` next
//! to it. A package that fails to load is skipped and reported; the host keeps running.

use services_module_api::{
    ABI_SYMBOL, ABI_VERSION, ENTRY, Entry,
    transport::{EndpointArc, EndpointBox},
};
use stabby::libloading::StabbyLibrary;
use std::{
    ffi::{CStr, c_char},
    path::{Path, PathBuf},
};

/// The modules folder inside the data directory.
pub const DIR: &str = "modules";
/// The console part of a package.
pub const UI: &str = "ui.wasm";

/// The native part of a package, `module.dylib` on macOS.
pub fn library_name() -> String {
    format!("module{}", std::env::consts::DLL_SUFFIX)
}

/// One package folder found in the modules folder.
pub struct Package {
    pub dir: PathBuf,
}

impl Package {
    pub fn library(&self) -> PathBuf {
        self.dir.join(library_name())
    }

    /// The console part, when the package ships one.
    pub fn ui(&self) -> Option<PathBuf> {
        let path = self.dir.join(UI);
        path.is_file().then_some(path)
    }
}

/// Package folders with a library, by folder name.
pub fn scan(root: &Path) -> Vec<Package> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut packages: Vec<Package> = entries
        .filter_map(|entry| entry.ok())
        // Hidden folders are an installer's staged or backed-up packages.
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
        .map(|entry| Package { dir: entry.path() })
        .filter(|package| package.library().is_file())
        .collect();
    packages.sort_by(|a, b| a.dir.cmp(&b.dir));
    packages
}

/// Loads a package library. It is never unloaded: module threads, TLS and the endpoints
/// it handed out may outlive `dlclose`.
pub fn load(library: &Path, host: EndpointArc) -> Result<EndpointBox, String> {
    // SAFETY: loading runs the library's initializers; packages are trusted like the host.
    let library = unsafe { libloading::Library::new(library) }.map_err(|e| e.to_string())?;
    // SAFETY: the ABI symbol has had this plain C signature in every module ABI.
    let abi = unsafe { library.get::<extern "C" fn() -> *const c_char>(ABI_SYMBOL) }
        .map_err(|_| "not a services module (no ABI version)".to_owned())?;
    // SAFETY: the symbol returns a static NUL-terminated string.
    let version = unsafe { CStr::from_ptr(abi()) }
        .to_string_lossy()
        .into_owned();
    if !compatible(&version, ABI_VERSION) {
        return Err(format!(
            "built for module ABI {version}, this host supports {ABI_VERSION}"
        ));
    }
    // SAFETY: stabby checks the entry's signature against the one the library reports.
    let entry: Entry = *unsafe { library.get_stabbied::<Entry>(ENTRY) }
        .map_err(|e| format!("incompatible entry: {e}"))?;
    let endpoint: Result<EndpointBox, _> = entry(host).into();
    let endpoint = endpoint.map_err(|status| format!("failed to start: {status}"))?;
    std::mem::forget(library);
    Ok(endpoint)
}

/// The hash the console caches a package's `ui.wasm` by (SHA-256, hex). Every start hashes
/// every package, so it uses ring's hardware SHA-256: `sha2` without `asm` is ~7× slower on arm64.
pub fn digest(wasm: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, wasm)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Same major version, and the module needs no newer minor than the host has.
fn compatible(module: &str, host: &str) -> bool {
    let split = |version: &str| {
        let (major, minor) = version.split_once('.')?;
        Some((major.parse::<u32>().ok()?, minor.parse::<u32>().ok()?))
    };
    matches!((split(module), split(host)), (Some(m), Some(h)) if m.0 == h.0 && m.1 <= h.1)
}

#[cfg(test)]
mod tests {
    use super::{compatible, digest};

    #[test]
    fn the_console_cache_key_is_the_sha256_hex_of_the_console_part() {
        assert_eq!(
            digest(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn a_module_needs_the_same_major_and_no_newer_minor() {
        assert!(compatible("3.0", "3.0"));
        assert!(compatible("3.0", "3.2"));
        assert!(!compatible("3.1", "3.0"));
        assert!(!compatible("2.9", "3.0"));
        assert!(!compatible("4.0", "3.0"));
        assert!(!compatible("3", "3.0"));
    }
}
