use anyhow::Result;
use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
pub fn discover(home: &Path) -> Result<Vec<PathBuf>> {
    use anyhow::{Context, ensure};
    use std::{fs, os::unix::fs::MetadataExt, process::Command};

    let home = fs::canonicalize(home)?;
    let output = Command::new("/bin/ps")
        .args(["-A", "-ww", "-o", "uid=,pid=,comm="])
        .output()?;
    ensure!(
        output.status.success(),
        "Could not inspect running Chrome processes"
    );
    let uid = unsafe { libc::geteuid() };
    let mut profiles = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut fields = line.split_whitespace();
        if fields.next().and_then(|value| value.parse::<u32>().ok()) != Some(uid) {
            continue;
        }
        let pid = fields.next().and_then(|value| value.parse::<i32>().ok());
        let executable = fields.collect::<Vec<_>>().join(" ");
        if !["Google Chrome", "Google Chrome for Testing", "Chromium"]
            .iter()
            .any(|name| executable.ends_with(&format!("/Contents/MacOS/{name}")))
        {
            continue;
        }
        let Some(pid) = pid else { continue };
        let profile = (|| {
            let args = process_arguments(pid)?;
            let Some(profile) = user_data_directory(&args) else {
                return Ok(None);
            };
            ensure!(
                profile.is_absolute(),
                "Relative Chrome directory cannot be registered automatically"
            );
            let profile = fs::canonicalize(profile).context("Open Chrome user-data directory")?;
            // Never modify another account or paths outside the installation's HOME.
            if !profile.starts_with(&home) {
                return Ok(None);
            }
            let metadata = fs::metadata(&profile)?;
            ensure!(
                metadata.is_dir() && metadata.uid() == uid,
                "Chrome directory is not owned by this user"
            );
            Ok::<_, anyhow::Error>(Some(profile))
        })();
        match profile {
            Ok(Some(profile)) if !profiles.contains(&profile) => profiles.push(profile),
            Ok(_) => {}
            Err(error) => {
                eprintln!("Could not detect custom directory for Chrome PID {pid}: {error:#}")
            }
        }
    }
    Ok(profiles)
}

#[cfg(not(target_os = "macos"))]
pub fn discover(_home: &Path) -> Result<Vec<PathBuf>> {
    Ok(Vec::new())
}

#[cfg(target_os = "macos")]
fn process_arguments(pid: i32) -> Result<Vec<Vec<u8>>> {
    use anyhow::ensure;
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut size = 0;
    let status = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    ensure!(
        (4..=16 * 1024 * 1024).contains(&size),
        "Invalid process argument buffer size"
    );
    let mut bytes = vec![0; size];
    let status = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            bytes.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    bytes.truncate(size);
    parse_arguments(&bytes)
}

#[cfg(any(target_os = "macos", test))]
fn parse_arguments(bytes: &[u8]) -> Result<Vec<Vec<u8>>> {
    use anyhow::{Context, ensure};
    ensure!(bytes.len() >= 4, "Truncated process argument header");
    let count = i32::from_ne_bytes(bytes[..4].try_into().unwrap());
    ensure!(
        (1..=16384).contains(&count),
        "Invalid process argument count"
    );
    let executable_end = bytes[4..]
        .iter()
        .position(|byte| *byte == 0)
        .context("Missing executable terminator")?
        + 4;
    let mut offset = executable_end;
    while bytes.get(offset) == Some(&0) {
        offset += 1;
    }
    let mut args = Vec::new();
    // Stop at argc: the rest of this OS buffer can contain private environment values.
    for _ in 0..count {
        let remaining = bytes.get(offset..).context("Truncated process arguments")?;
        let length = remaining
            .iter()
            .position(|byte| *byte == 0)
            .context("Unterminated process argument")?;
        args.push(remaining[..length].to_vec());
        offset += length + 1;
    }
    Ok(args)
}

#[cfg(any(target_os = "macos", test))]
fn user_data_directory(args: &[Vec<u8>]) -> Option<PathBuf> {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
    let mut directory = None;
    for (index, arg) in args.iter().enumerate() {
        let value = arg.strip_prefix(b"--user-data-dir=").or_else(|| {
            (arg == b"--user-data-dir")
                .then(|| args.get(index + 1).map(Vec::as_slice).unwrap_or_default())
        });
        if let Some(value) = value {
            directory =
                (!value.is_empty()).then(|| PathBuf::from(OsString::from_vec(value.to_vec())));
        }
    }
    directory
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_argument_boundaries_and_does_not_parse_environment() {
        let args = [
            b"/Applications/Google Chrome".as_slice(),
            b"--user-data-dir=/Users/test/Profile -- private",
            b"--incognito",
            b"",
        ];
        let mut bytes = (args.len() as i32).to_ne_bytes().to_vec();
        bytes.extend(b"/Applications/Google Chrome\0\0");
        for arg in args {
            bytes.extend(arg);
            bytes.push(0);
        }
        bytes.extend(b"PRIVATE_VALUE=not-an-argument\0--user-data-dir=/not/a/profile\0");
        let parsed = parse_arguments(&bytes).unwrap();
        assert_eq!(parsed, args);
        assert_eq!(
            user_data_directory(&parsed),
            Some(PathBuf::from("/Users/test/Profile -- private"))
        );
    }

    #[test]
    fn accepts_separate_flag_and_uses_last_directory() {
        let args = [
            "chrome",
            "--user-data-directory=/wrong",
            "--user-data-dir=/first",
            "--user-data-dir",
            "/second profile",
        ]
        .map(|arg| arg.as_bytes().to_vec());
        assert_eq!(
            user_data_directory(&args),
            Some(PathBuf::from("/second profile"))
        );
        assert!(user_data_directory(&[b"chrome".to_vec()]).is_none());
        for last in ["--user-data-dir=", "--user-data-dir"] {
            let args =
                ["chrome", "--user-data-dir=/first", last].map(|arg| arg.as_bytes().to_vec());
            assert!(user_data_directory(&args).is_none());
        }
    }

    #[test]
    fn rejects_truncated_process_argument_buffers() {
        for bytes in [
            vec![],
            (-1i32).to_ne_bytes().to_vec(),
            [1i32.to_ne_bytes().as_slice(), b"chrome\0\0unterminated"].concat(),
        ] {
            assert!(parse_arguments(&bytes).is_err());
        }
    }
}
