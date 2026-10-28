use crate::{Error, Result, core::process::ChildGroup};
use std::{
    collections::BTreeMap,
    ffi::CStr,
    os::unix::{ffi::OsStrExt, process::CommandExt},
    path::PathBuf,
    process::Stdio,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::io::{AsyncRead, AsyncReadExt};

const TIMEOUT: Duration = Duration::from_secs(5);
const MAX_STDOUT: usize = 1024 * 1024;
const MAX_STDERR: usize = 64 * 1024;

pub async fn launch_native(args: &[String]) -> Result<()> {
    let shell = user_shell()?;
    let marker = format!(
        "services-devtools-env-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::new("Cannot create shell environment boundary"))?
            .as_nanos()
    );
    let mut command = tokio::process::Command::new(&shell);
    command
        .args([
            "-i",
            "-l",
            "-c",
            &format!("/usr/bin/printf '\\0%s\\0' '{marker}'; exec /usr/bin/env -0"),
        ])
        .env("SHELL", &shell);
    let bytes = capture(command, TIMEOUT).await?;
    let environment = parse(&bytes, &marker)?;
    let mut native = std::process::Command::new(std::env::current_exe()?);
    native
        .arg("__native-host-shell")
        .args(args.iter().skip(1))
        .env_clear()
        .envs(environment);
    // Shell configuration must not redirect the installation's data or UI.
    for name in ["SERVICES_DATA_DIR", "SERVICES_UI_DIR"] {
        match std::env::var_os(name) {
            Some(value) => {
                native.env(name, value);
            }
            None => {
                native.env_remove(name);
            }
        }
    }
    Err(Error::new(format!(
        "Could not start native host with user shell environment: {}",
        native.exec()
    )))
}

fn user_shell() -> Result<PathBuf> {
    let shell = match std::env::var_os("SHELL") {
        Some(shell) => PathBuf::from(shell),
        None => account_shell()?,
    };
    if !shell.is_absolute() {
        return Err(Error::new("User SHELL must be an absolute executable path"));
    }
    match shell.file_name().and_then(|name| name.to_str()) {
        Some("zsh" | "bash" | "sh" | "dash" | "ksh" | "fish") => Ok(shell),
        _ => Err(Error::new(
            "Automatic environment loading requires zsh, bash, sh, dash, ksh or fish as the user shell",
        )),
    }
}

fn account_shell() -> Result<PathBuf> {
    let mut size = 16384;
    loop {
        let mut buffer = vec![0u8; size];
        let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut found = std::ptr::null_mut();
        let status = unsafe {
            libc::getpwuid_r(
                libc::getuid(),
                entry.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut found,
            )
        };
        if status == libc::ERANGE && size < MAX_STDOUT {
            size *= 2;
            continue;
        }
        if status != 0 || found.is_null() {
            return Err(Error::new(
                "Could not determine the user login shell; set SHELL to its absolute path",
            ));
        }
        let shell = unsafe { (*found).pw_shell };
        if shell.is_null() {
            return Err(Error::new("The user account has no login shell"));
        }
        let shell = unsafe { CStr::from_ptr(shell) };
        if shell.to_bytes().is_empty() {
            return Err(Error::new("The user account has no login shell"));
        }
        return Ok(PathBuf::from(std::ffi::OsStr::from_bytes(shell.to_bytes())));
    }
}

async fn capture(mut command: tokio::process::Command, timeout: Duration) -> Result<Vec<u8>> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // An interactive shell in the installer's terminal must not stop on SIGTTIN.
    unsafe {
        command.as_std_mut().pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .map_err(|error| Error::new(format!("Could not start user shell: {error}")))?;
    let group = ChildGroup(
        child
            .id()
            .ok_or_else(|| Error::new("User shell has no process ID"))?,
    );
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::new("User shell has no output pipe"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| Error::new("User shell has no error pipe"))?;
    let result = tokio::time::timeout(timeout, async {
        tokio::try_join!(
            read_bounded(stdout, MAX_STDOUT),
            read_bounded(stderr, MAX_STDERR),
            async { child.wait().await.map_err(Error::from) }
        )
    })
    .await;
    group.signal(libc::SIGKILL);
    child.wait().await?;
    let (stdout, _, status) = result.map_err(|_| {
        Error::new(
            "User shell environment loading timed out. Check shell startup files for prompts or slow commands, then retry the connection",
        )
    })??;
    if !status.success() {
        return Err(Error::new(format!(
            "User shell environment loading failed ({status}). Check shell startup files, then retry the connection"
        )));
    }
    Ok(stdout)
}

async fn read_bounded(reader: impl AsyncRead + Unpin, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() > limit {
        return Err(Error::new(
            "User shell startup output exceeds its size limit; check shell startup files",
        ));
    }
    Ok(bytes)
}

fn parse(bytes: &[u8], marker: &str) -> Result<BTreeMap<String, String>> {
    let boundary = format!("\0{marker}\0");
    let start = bytes
        .windows(boundary.len())
        .position(|part| part == boundary.as_bytes())
        .ok_or_else(|| Error::new("User shell did not return its environment"))?
        + boundary.len();
    let payload = &bytes[start..];
    if !payload.ends_with(&[0]) {
        return Err(Error::new("User shell returned an incomplete environment"));
    }
    let mut environment = BTreeMap::new();
    for entry in payload[..payload.len() - 1].split(|byte| *byte == 0) {
        let separator = entry
            .iter()
            .position(|byte| *byte == b'=')
            .filter(|index| *index > 0)
            .ok_or_else(|| Error::new("User shell returned an invalid environment entry"))?;
        let name = std::str::from_utf8(&entry[..separator])
            .map_err(|_| Error::new("User shell environment contains a non-UTF-8 name"))?;
        let value = std::str::from_utf8(&entry[separator + 1..])
            .map_err(|_| Error::new("User shell environment contains a non-UTF-8 value"))?;
        if environment.insert(name.into(), value.into()).is_some() {
            return Err(Error::new(
                "User shell returned duplicate environment entries",
            ));
        }
    }
    Ok(environment)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_is_nul_delimited_and_startup_noise_is_not_parsed() {
        let environment = parse(
            b"startup banner\n\0fixture\0VALUE=line 1\nline 2=ok\0EMPTY=\0",
            "fixture",
        )
        .unwrap();
        assert_eq!(environment.len(), 2);
        assert_eq!(environment["VALUE"], "line 1\nline 2=ok");
        assert_eq!(environment["EMPTY"], "");
        for bytes in [
            &b"missing boundary"[..],
            &b"\0fixture\0VALUE=truncated"[..],
            &b"\0fixture\0invalid\0"[..],
            &b"\0fixture\0VALUE=one\0VALUE=two\0"[..],
            &b"\0fixture\0VALUE=\xff\0"[..],
        ] {
            assert!(parse(bytes, "fixture").is_err());
        }
    }

    #[test]
    fn account_login_shell_is_an_absolute_path() {
        assert!(account_shell().unwrap().is_absolute());
    }

    #[tokio::test]
    async fn shell_capture_bounds_time_output_and_errors_without_leaking_content() {
        for (script, timeout, expected) in [
            ("printf private-fixture >&2; exit 7", TIMEOUT, "failed"),
            ("/bin/sleep 30", Duration::from_millis(50), "timed out"),
            ("/usr/bin/head -c 1100000 /dev/zero", TIMEOUT, "size limit"),
            (
                "/usr/bin/head -c 70000 /dev/zero >&2",
                TIMEOUT,
                "size limit",
            ),
        ] {
            let mut command = tokio::process::Command::new("/bin/sh");
            command.args(["-c", script]).env_clear();
            let error = capture(command, timeout).await.unwrap_err();
            assert!(error.message.contains(expected), "{error}");
            assert!(!error.message.contains("private-fixture"));
        }
    }
}
