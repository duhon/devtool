#![cfg(all(target_os = "macos", target_arch = "aarch64"))]

use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

const ARCHIVE: &str = "services-macos-arm64.tar.gz";

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

struct OwnedChild(Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            // Only this test's dedicated process group, including subprocesses.
            unsafe { libc::kill(-(self.0.id() as i32), libc::SIGKILL) };
        }
        let _ = self.0.wait();
    }
}

struct Fixture {
    directory: tempfile::TempDir,
    package: PathBuf,
    cwd: PathBuf,
    source: String,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::Builder::new()
            .prefix(".services-bootstrap-'test-")
            .tempdir_in(repository())
            .expect("create isolated bootstrap fixture in checkout");
        let package = directory.path().join("package's files");
        let cwd = directory.path().join("unrelated working directory");
        fs::create_dir_all(&package).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir(directory.path().join("scratch")).unwrap();
        let source = fs::read_to_string(repository().join("scripts/distribution/install.sh"))
            .expect("retained bootstrap source must exist");
        fs::write(package.join("install.sh"), &source).unwrap();
        Self {
            directory,
            package,
            cwd,
            source,
        }
    }

    fn command(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        command
            .env_clear()
            .env("HOME", self.directory.path())
            .env("ZDOTDIR", self.directory.path())
            .env("SHELL", "/bin/zsh")
            .env("PATH", "/usr/bin:/bin")
            .env("TMPDIR", self.directory.path().join("scratch"))
            .current_dir(&self.cwd)
            .process_group(0);
        command
    }

    fn execute(&self, mut command: Command, input: Option<&str>) -> Output {
        let stdout = self.directory.path().join("stdout");
        let stderr = self.directory.path().join("stderr");
        command
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(fs::File::create(&stdout).unwrap())
            .stderr(fs::File::create(&stderr).unwrap());
        let mut child = OwnedChild(command.spawn().expect("spawn bootstrap fixture command"));
        if let Some(input) = input {
            child
                .0
                .stdin
                .take()
                .unwrap()
                .write_all(input.as_bytes())
                .unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(20);
        let status = loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "bootstrap fixture command exceeded 20 seconds"
            );
            thread::sleep(Duration::from_millis(20));
        };
        Output {
            status,
            stdout: fs::read(stdout).unwrap(),
            stderr: fs::read(stderr).unwrap(),
        }
    }

    fn run(&self, piped: bool, args: &[&str]) -> Output {
        let mut command = self.command("/bin/bash");
        if piped {
            command.args(["-s", "--"]);
        } else {
            command.arg(self.package.join("install.sh"));
        }
        command.args(args);
        self.execute(command, piped.then_some(self.source.as_str()))
    }

    fn invalid_package(&self, target: &Path) {
        fs::write(target.join(ARCHIVE), "invalid archive").unwrap();
        fs::write(
            target.join("checksums.txt"),
            format!("{}  {ARCHIVE}\n", "0".repeat(64)),
        )
        .unwrap();
    }

    fn failure(&self, output: Output, expected: &str) {
        assert!(!output.status.success(), "bootstrap unexpectedly succeeded");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(
            stderr.contains(expected),
            "expected {expected:?}, got {stderr}"
        );
        assert!(
            !self
                .directory
                .path()
                .join("Library/Application Support/ServicesDevTools")
                .exists(),
            "rejected bootstrap must not install any files"
        );
        assert_eq!(
            fs::read_dir(self.directory.path().join("scratch"))
                .unwrap()
                .count(),
            0,
            "bootstrap must clean its extraction workspace on failure"
        );
    }
}

#[test]
fn successful_install_is_gui_free_by_default_and_accepts_legacy_no_open() {
    let fixture = Fixture::new();
    assert!(
        !fixture.source.contains("/usr/bin/open"),
        "bootstrap must never launch a GUI"
    );
    let payload = fixture.package.join("payload");
    let bin = payload.join("ServicesDevTools/native/bin");
    fs::create_dir_all(&bin).unwrap();
    for (name, body) in [
        ("services-devtools", "#!/bin/sh\nexit 0\n"),
        (
            "services-install",
            "#!/bin/sh\nprintf 'fixture-installed\\n'\n",
        ),
    ] {
        let path = bin.join(name);
        fs::write(&path, body).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let archive = fixture.package.join(ARCHIVE);
    let mut tar = fixture.command("/usr/bin/tar");
    tar.arg("-czf")
        .arg(&archive)
        .arg("-C")
        .arg(payload)
        .arg("ServicesDevTools");
    let packed = fixture.execute(tar, None);
    assert!(
        packed.status.success(),
        "could not create bootstrap fixture archive"
    );
    let checksum = format!("{:x}", Sha256::digest(fs::read(&archive).unwrap()));
    fs::write(
        fixture.package.join("checksums.txt"),
        format!("{checksum}  {ARCHIVE}\n"),
    )
    .unwrap();
    for args in [&[][..], &["--no-open"][..]] {
        let output = fixture.run(false, args);
        assert!(
            output.status.success(),
            "bootstrap failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"fixture-installed\n");
        assert!(
            output.stderr.is_empty(),
            "successful bootstrap must have no stderr"
        );
    }
}

#[test]
fn adjacent_archive_is_selected_from_another_cwd_and_bad_checksum_installs_nothing() {
    let fixture = Fixture::new();
    fixture.invalid_package(&fixture.package);
    fixture.failure(
        fixture.run(false, &["--no-open"]),
        "Package checksum mismatch",
    );
}

#[test]
fn adjacent_archive_requires_checksum_without_download_fallback() {
    let fixture = Fixture::new();
    fixture.invalid_package(&fixture.package);
    fs::remove_file(fixture.package.join("checksums.txt")).unwrap();
    fixture.failure(
        fixture.run(false, &["--no-open"]),
        "place checksums.txt beside the archive",
    );
}

#[test]
fn unrelated_cwd_archive_does_not_change_file_or_piped_remote_route() {
    let fixture = Fixture::new();
    fixture.invalid_package(&fixture.cwd);
    for piped in [false, true] {
        fixture.failure(
            fixture.run(piped, &["--no-open"]),
            "Remote source is not configured",
        );
    }
}

#[test]
fn configured_remote_route_requires_a_baked_release_version() {
    let mut fixture = Fixture::new();
    fixture.source = fixture
        .source
        .replace("@REPOSITORY@", "owner/repository")
        .replace("@VERSION@", "main");
    fs::write(fixture.package.join("install.sh"), &fixture.source).unwrap();
    for piped in [false, true] {
        fixture.failure(fixture.run(piped, &[]), "Invalid release version");
    }
}

#[test]
fn root_launcher_finds_built_bootstrap_and_forwards_arguments() {
    let fixture = Fixture::new();
    fs::write(
        fixture.package.join("install.sh"),
        fs::read(repository().join("install.sh")).expect("root install launcher must exist"),
    )
    .unwrap();
    let built = fixture.package.join("dist/distribution/install.sh");
    fs::create_dir_all(built.parent().unwrap()).unwrap();
    fs::write(&built, "#!/bin/bash\nprintf '%s\\n' \"$0\" \"$@\"\n").unwrap();
    let output = fixture.run(false, &["--no-open", "--archive", "archive's path"]);
    assert!(output.status.success(), "root launcher failed");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<_> = stdout.lines().collect();
    assert_eq!(lines.len(), 4, "launcher must preserve every argument");
    assert_eq!(
        fs::canonicalize(lines[0]).unwrap(),
        fs::canonicalize(built).unwrap()
    );
    assert_eq!(&lines[1..], ["--no-open", "--archive", "archive's path"]);
}

#[test]
fn root_launcher_reports_build_instruction_for_missing_distribution() {
    let fixture = Fixture::new();
    fs::write(
        fixture.package.join("install.sh"),
        fs::read(repository().join("install.sh")).expect("root install launcher must exist"),
    )
    .unwrap();
    fixture.failure(
        fixture.run(false, &["--no-open"]),
        "cargo run -p xtask -- build",
    );
}
