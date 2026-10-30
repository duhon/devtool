#![cfg(target_os = "macos")]

use serde_json::{Value, json};
use services_native::adapters::framing::{FrameReader, encode};
use std::{
    path::PathBuf,
    process::Stdio,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, Command},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-data")
            .join(format!(
                "shell-'home-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(&path).unwrap();
        services_testkit::packages::install(&path.join("data"));
        Self(path)
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_services-devtools"));
        command
            .env_clear()
            .env("HOME", &self.0)
            .env("ZDOTDIR", &self.0)
            .env("SHELL", "/bin/zsh")
            .env("PATH", "/usr/bin:/bin")
            .env("SERVICES_DATA_DIR", self.0.join("data"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    }

    fn write(&self, name: &str, text: &str) {
        std::fs::write(self.0.join(name), text).unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

async fn request(child: &mut Child, message: Value) -> Vec<Value> {
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(&encode(&message).unwrap())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(8), async {
        let mut frames = FrameReader::default();
        let mut replies = vec![];
        let mut buffer = [0u8; 8192];
        loop {
            let count = child
                .stdout
                .as_mut()
                .unwrap()
                .read(&mut buffer)
                .await
                .unwrap();
            assert_ne!(count, 0, "Native host disconnected before replying");
            for reply in frames.feed(&buffer[..count]).unwrap() {
                assert_eq!(reply["id"], message["id"]);
                let done = reply["type"] != "data";
                replies.push(reply);
                if done {
                    return replies;
                }
            }
        }
    })
    .await
    .expect("Native host response must be bounded")
}

async fn finish(mut child: Child) -> std::process::Output {
    drop(child.stdin.take());
    tokio::time::timeout(Duration::from_secs(5), child.wait_with_output())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn login_and_interactive_environment_reaches_variables_substitution_and_children() {
    let fixture = Fixture::new();
    fixture.write(
        ".zprofile",
        "printf profile-banner; printf private-stderr-fixture >&2\n\
         printf login >> \"$HOME/start-count\"\n\
         export SDT_LOGIN_VALUE='login fixture'\n",
    );
    fixture.write(
        ".zshrc",
        "printf interactive-banner\n\
         printf interactive >> \"$HOME/start-count\"\n\
         export SDT_INTERACTIVE_VALUE='interactive fixture'\n\
         export SDT_MULTILINE='line 1\nline 2=ok'\n\
         export SDT_EMPTY=''\n\
         export PATH=\"$HOME/fixture-bin:$PATH\"\n\
         export SERVICES_DATA_DIR=\"$HOME/wrong-data\"\n",
    );
    std::fs::create_dir(fixture.0.join("fixture-bin")).unwrap();
    fixture.write(
        "fixture-bin/sdt-shell-probe",
        "#!/bin/sh\nprintf shell-path-fixture\n",
    );
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        fixture.0.join("fixture-bin/sdt-shell-probe"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let mut child = fixture.command().arg("native-host").spawn().unwrap();
    let replies = request(&mut child, json!({"id":1,"service":"vars","op":"list"})).await;
    let env = &replies[0]["result"]["profiles"]["env"];
    assert_eq!(env["SDT_LOGIN_VALUE"], "login fixture");
    assert_eq!(env["SDT_INTERACTIVE_VALUE"], "interactive fixture");
    assert_eq!(env["SDT_MULTILINE"], "line 1\nline 2=ok");
    assert_eq!(env["SDT_EMPTY"], "");
    assert_eq!(
        env["SERVICES_DATA_DIR"],
        fixture.0.join("data").to_str().unwrap()
    );
    let replies = request(
        &mut child,
        json!({"id":2,"service":"dictionary","op":"save","name":"default",
            "variables":{"JOIN":"{{env:SDT_LOGIN_VALUE}}/{{env:SDT_INTERACTIVE_VALUE}}"}}),
    )
    .await;
    assert_eq!(
        replies[0]["result"]["resolvedProfiles"]["default"]["JOIN"],
        "login fixture/interactive fixture"
    );
    let replies = request(
        &mut child,
        json!({"id":3,"service":"terminal","params":{
            "endpoint":"/bin/sh -c",
            "command":"sdt-shell-probe; printf '|%s|%s' \"$SDT_LOGIN_VALUE\" \"$SDT_INTERACTIVE_VALUE\""
        }}),
    )
    .await;
    let output = replies
        .iter()
        .filter(|reply| reply["type"] == "data" && reply["event"]["type"] == "stdout")
        .filter_map(|reply| reply["event"]["data"].as_str())
        .collect::<String>();
    assert_eq!(
        output,
        "shell-path-fixture|login fixture|interactive fixture"
    );
    assert_eq!(replies.last().unwrap()["info"]["code"], 0);
    let output = finish(child).await;
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    assert_eq!(
        std::fs::read_to_string(fixture.0.join("start-count")).unwrap(),
        "logininteractive",
        "Shell configuration must run once per native connection"
    );
    assert!(!fixture.0.join("wrong-data").exists());
}

#[tokio::test]
async fn native_environment_loads_with_a_controlling_terminal_and_piped_protocol() {
    use std::os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::process::CommandExt,
    };
    let fixture = Fixture::new();
    fixture.write(".zprofile", "export SDT_TTY_LOGIN=login-fixture\n");
    fixture.write(".zshrc", "export SDT_TTY_INTERACTIVE=interactive-fixture\n");
    let mut master = -1;
    let mut slave = -1;
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        0
    );
    let master = unsafe { OwnedFd::from_raw_fd(master) };
    let slave = unsafe { OwnedFd::from_raw_fd(slave) };
    for fd in [&master, &slave] {
        assert_eq!(
            unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) },
            0
        );
    }
    let terminal = slave.as_raw_fd();
    let mut command = fixture.command();
    // Match an installer launched from a terminal, with native protocol on pipes.
    unsafe {
        command.as_std_mut().pre_exec(move || {
            if libc::setsid() == -1
                || libc::ioctl(terminal, libc::TIOCSCTTY.into(), 0) == -1
                || libc::tcsetpgrp(terminal, libc::getpgrp()) == -1
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    let replies = request(&mut child, json!({"id":1,"service":"vars","op":"list"})).await;
    assert_eq!(replies[0]["ok"], true, "{}", replies[0]);
    let env = &replies[0]["result"]["profiles"]["env"];
    assert_eq!(env["SDT_TTY_LOGIN"], "login-fixture");
    assert_eq!(env["SDT_TTY_INTERACTIVE"], "interactive-fixture");
    let output = finish(child).await;
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[tokio::test]
async fn startup_failure_is_a_native_error_without_exposing_shell_output() {
    for (script, expected) in [
        (
            "printf private-fixture; printf private-fixture >&2; exit 7\n",
            "shell environment loading failed",
        ),
        ("exit 0\n", "did not return its environment"),
        ("/bin/sleep 30\n", "timed out"),
    ] {
        let fixture = Fixture::new();
        fixture.write(".zprofile", script);
        let mut child = fixture.command().spawn().unwrap();
        for id in [1, 2] {
            let replies = request(&mut child, json!({"id":id,"type":"health"})).await;
            assert_eq!(replies[0]["ok"], false);
            assert!(replies[0]["error"].as_str().unwrap().contains(expected));
            assert!(!replies[0].to_string().contains("private-fixture"));
        }
        let output = finish(child).await;
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("private-fixture"));
    }
}

#[tokio::test]
async fn cli_keeps_its_callers_environment_without_loading_shell_startup_files() {
    let fixture = Fixture::new();
    fixture.write(
        ".zprofile",
        "printf unexpected >> \"$HOME/start-count\"\nexit 7\n",
    );
    let output = fixture
        .command()
        .args([
            "cli",
            "terminal",
            "/bin/sh -c",
            "printf '%s' \"$SDT_LOGIN_VALUE\"",
        ])
        .env("SDT_LOGIN_VALUE", "caller-fixture")
        .output()
        .await
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"caller-fixture");
    assert!(!fixture.0.join("start-count").exists());
}
