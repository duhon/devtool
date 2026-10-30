//! Installing a package file into the modules folder and removing packages. Folders are
//! swapped by renames: a running host keeps the library it mapped, and a failed install
//! leaves the previous package in place.

use services_contracts::{
    manifest::Manifest,
    package::{LIBRARY_FILE, Package, archive},
};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub struct Installed {
    pub manifest: Manifest,
    /// The version of the package it replaced.
    pub replaced: Option<String>,
}

/// Installs a package file: unpacks it into a hidden folder next to the packages, prepares
/// it for loading, lets `check` load it, then puts it in place of the package with its id.
pub fn install(
    modules: &Path,
    file: &[u8],
    check: impl FnOnce(&Path) -> Result<Manifest, String>,
) -> Result<Installed, String> {
    fs::create_dir_all(modules).map_err(|e| format!("{}: {e}", modules.display()))?;
    let staged = Hidden::new(modules, "install");
    fs::create_dir(&staged.0).map_err(|e| format!("{}: {e}", staged.0.display()))?;
    archive::unpack(file, &staged.0)?;
    prepare(&staged.0)?;
    let manifest = check(&staged.0)?;
    let target = modules.join(&manifest.id);
    let replaced = Package::read(&target).ok().map(|package| package.version);
    let previous = Hidden::new(modules, "removed");
    let had_previous = fs::symlink_metadata(&target).is_ok();
    if had_previous {
        fs::rename(&target, &previous.0)
            .map_err(|e| format!("Cannot replace {}: {e}", target.display()))?;
    }
    if let Err(error) = fs::rename(&staged.0, &target) {
        if had_previous {
            let _ = fs::rename(&previous.0, &target);
        }
        return Err(format!("Cannot place {}: {error}", target.display()));
    }
    Ok(Installed { manifest, replaced })
}

/// Removes the package in folder `name`, including one that failed to load.
pub fn remove(modules: &Path, name: &str) -> Result<(), String> {
    if name.is_empty() || name.starts_with('.') || name.contains(['/', '\\']) {
        return Err(format!("\"{name}\" is not a package folder"));
    }
    let target = modules.join(name);
    if fs::symlink_metadata(&target).is_err() {
        return Err(format!("Module \"{name}\" is not installed"));
    }
    let removed = Hidden::new(modules, "removed");
    fs::rename(&target, &removed.0).map_err(|e| format!("Cannot remove {}: {e}", target.display()))
}

/// macOS loads only signed code: the library gets an ad-hoc signature, like the libraries
/// the app update installs. Chrome quarantines every file its host writes, and loading a
/// quarantined library waits for a Gatekeeper prompt; like the app update, the install
/// clears the quarantine of the package the user chose. Signing rewrites the library, so
/// the quarantine is cleared after it.
#[cfg(target_os = "macos")]
fn prepare(package: &Path) -> Result<(), String> {
    let output = std::process::Command::new("/usr/bin/codesign")
        .args(["--force", "--sign", "-"])
        .arg(package.join(LIBRARY_FILE))
        .output()
        .map_err(|e| format!("Cannot run codesign: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "Cannot sign {LIBRARY_FILE}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    clear_quarantine(package)
}

#[cfg(not(target_os = "macos"))]
fn prepare(_: &Path) -> Result<(), String> {
    Ok(())
}

/// Clears the quarantine of the package folder and its files.
#[cfg(target_os = "macos")]
fn clear_quarantine(package: &Path) -> Result<(), String> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let entries = fs::read_dir(package).map_err(|e| format!("{}: {e}", package.display()))?;
    let mut paths = vec![package.to_path_buf()];
    for entry in entries {
        paths.push(entry.map_err(|e| e.to_string())?.path());
    }
    for path in paths {
        let name = CString::new(path.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
        // SAFETY: both arguments are NUL-terminated strings that outlive the call.
        let removed = unsafe {
            libc::removexattr(
                name.as_ptr(),
                c"com.apple.quarantine".as_ptr(),
                libc::XATTR_NOFOLLOW,
            )
        };
        let error = std::io::Error::last_os_error();
        if removed != 0 && error.raw_os_error() != Some(libc::ENOATTR) {
            return Err(format!(
                "Cannot clear the quarantine of {}: {error}",
                path.display()
            ));
        }
    }
    Ok(())
}

/// A hidden path in the modules folder (the host skips hidden folders), removed when dropped.
struct Hidden(PathBuf);

impl Hidden {
    fn new(modules: &Path, kind: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        Self(modules.join(format!(
            ".{kind}-{}-{nanos}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )))
    }
}

impl Drop for Hidden {
    fn drop(&mut self) {
        // A symlinked package (development) loses only its link.
        let _ = match fs::symlink_metadata(&self.0) {
            Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(&self.0),
            Ok(_) => fs::remove_file(&self.0),
            Err(_) => Ok(()),
        };
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use std::{ffi::CString, os::unix::ffi::OsStrExt, path::Path};

    fn quarantine(path: &Path) -> Option<isize> {
        let name = CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: a size query with NUL-terminated strings and no buffer.
        let size = unsafe {
            libc::getxattr(
                name.as_ptr(),
                c"com.apple.quarantine".as_ptr(),
                std::ptr::null_mut(),
                0,
                0,
                0,
            )
        };
        (size >= 0).then_some(size)
    }

    #[test]
    fn a_package_written_under_quarantine_is_cleared_for_loading() {
        let package = tempfile::tempdir().unwrap();
        let files = ["package.json", "module.dylib"].map(|name| package.path().join(name));
        for path in files
            .iter()
            .map(|path| path.as_path())
            .chain([package.path()])
        {
            if !path.is_dir() {
                std::fs::write(path, "x").unwrap();
            }
            let name = CString::new(path.as_os_str().as_bytes()).unwrap();
            let value = b"0281;6ae48c4d;Google Chrome;";
            // SAFETY: NUL-terminated strings and a buffer of the given length.
            let set = unsafe {
                libc::setxattr(
                    name.as_ptr(),
                    c"com.apple.quarantine".as_ptr(),
                    value.as_ptr().cast(),
                    value.len(),
                    0,
                    0,
                )
            };
            assert_eq!(set, 0, "{}", std::io::Error::last_os_error());
            assert!(quarantine(path).is_some());
        }
        super::clear_quarantine(package.path()).unwrap();
        for path in files
            .iter()
            .map(|path| path.as_path())
            .chain([package.path()])
        {
            assert_eq!(quarantine(path), None, "{}", path.display());
        }
        super::clear_quarantine(package.path()).unwrap();
    }
}
