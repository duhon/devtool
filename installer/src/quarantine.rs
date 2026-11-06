use std::{io, path::Path};

#[cfg(target_os = "macos")]
pub(super) fn read(path: &Path) -> io::Result<Option<Vec<u8>>> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let path = CString::new(path.as_os_str().as_bytes())?;
    let size = unsafe {
        libc::getxattr(
            path.as_ptr(),
            c"com.apple.quarantine".as_ptr(),
            std::ptr::null_mut(),
            0,
            0,
            0,
        )
    };
    if size < 0 {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(libc::ENOATTR) {
            Ok(None)
        } else {
            Err(error)
        };
    }
    if size > 4096 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Quarantine metadata exceeds the size limit",
        ));
    }
    let mut value = vec![0; size as usize];
    let count = unsafe {
        libc::getxattr(
            path.as_ptr(),
            c"com.apple.quarantine".as_ptr(),
            value.as_mut_ptr().cast(),
            value.len(),
            0,
            0,
        )
    };
    if count < 0 {
        return Err(io::Error::last_os_error());
    }
    value.truncate(count as usize);
    Ok(Some(value))
}

#[cfg(target_os = "macos")]
fn create(path: &Path, value: &[u8]) -> io::Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let path = CString::new(path.as_os_str().as_bytes())?;
    let result = unsafe {
        libc::setxattr(
            path.as_ptr(),
            c"com.apple.quarantine".as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
            libc::XATTR_CREATE,
        )
    };
    if result == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::EEXIST) {
        return Ok(());
    }
    Err(error)
}

pub fn copy(source: &Path, target: &Path) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    if let Some(value) = read(source)? {
        create(target, &value)?;
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (source, target);
    Ok(())
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn downloaded_file_origin_survives_copy_without_replacing_existing_quarantine() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("download");
        let target = directory.path().join("copy");
        std::fs::write(&source, "ordinary text").unwrap();
        std::fs::write(&target, "ordinary text").unwrap();
        create(&source, b"0081;0;FixtureDownloader;").unwrap();
        assert!(
            read(&source)
                .unwrap()
                .is_some_and(|value| !value.is_empty())
        );
        let expected = read(&target).unwrap().or_else(|| read(&source).unwrap());
        copy(&source, &target).unwrap();
        assert_eq!(read(&target).unwrap(), expected);
        let before = read(&target).unwrap().unwrap();
        create(&target, b"0081;0;OtherDownloader;").unwrap();
        assert_eq!(read(&target).unwrap().unwrap(), before);
    }

    #[test]
    fn copying_an_unmarked_file_does_not_add_quarantine() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let target = directory.path().join("copy");
        std::fs::write(&source, "ordinary text").unwrap();
        std::fs::write(&target, "ordinary text").unwrap();
        assert_eq!(read(&source).unwrap(), None);
        assert_eq!(read(&target).unwrap(), None);
        copy(&source, &target).unwrap();
        assert_eq!(read(&target).unwrap(), None);
    }
}
