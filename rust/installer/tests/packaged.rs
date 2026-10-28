#![cfg(all(target_os = "macos", target_arch = "aarch64"))]

use serde_json::{Value, json};
use services_installer::locations;
use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

struct OwnedChild(Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            // Kill only the group allocated for this fixture.
            unsafe { libc::kill(-(self.0.id() as i32), libc::SIGKILL) };
        }
        let _ = self.0.wait();
    }
}

struct Fixture {
    directory: tempfile::TempDir,
    home: PathBuf,
    package: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::Builder::new()
            .prefix(".services-packaged-'test-")
            .tempdir_in(repository())
            .expect("create isolated packaged fixture inside checkout");
        let home = directory.path().join("user's home");
        let package = directory.path().join("package's files");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&package).unwrap();
        fs::create_dir(directory.path().join("scratch")).unwrap();
        fs::write(home.join(".zprofile"),
            "export SERVICES_DEVTOOLS_INSTALL_ENV=installed-shell-fixture\nprintf 'fixture startup banner\\n'\n").unwrap();
        fs::write(
            home.join(".zshrc"),
            "export SERVICES_DEVTOOLS_INSTALL_INTERACTIVE=installed-interactive-fixture\n",
        )
        .unwrap();
        for file in ["install.sh", "checksums.txt", "services-macos-arm64.tar.gz"] {
            fs::copy(repository().join("dist/distribution").join(file), package.join(file))
                .unwrap_or_else(|_| panic!("missing distribution prerequisite {file}; run cargo run -p xtask -- build first"));
        }
        Self {
            directory,
            home,
            package,
        }
    }

    fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = Command::new(program);
        command
            .env_clear()
            .env("HOME", &self.home)
            .env("ZDOTDIR", &self.home)
            .env("SHELL", "/bin/zsh")
            .env("PATH", "/usr/bin:/bin")
            .env("TMPDIR", self.directory.path().join("scratch"))
            .current_dir(&self.home)
            .process_group(0);
        command
    }

    fn run(&self, mut command: Command) -> Output {
        let out = self.directory.path().join("command.stdout");
        let err = self.directory.path().join("command.stderr");
        command
            .stdin(Stdio::null())
            .stdout(fs::File::create(&out).unwrap())
            .stderr(fs::File::create(&err).unwrap());
        let mut child = OwnedChild(command.spawn().expect("spawn isolated packaged command"));
        let deadline = Instant::now() + Duration::from_secs(120);
        let status = loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "packaged command exceeded 120 seconds"
            );
            thread::sleep(Duration::from_millis(25));
        };
        Output {
            status,
            stdout: fs::read(out).unwrap(),
            stderr: fs::read(err).unwrap(),
        }
    }

    fn successful(&self, command: Command) -> String {
        let output = self.run(command);
        assert!(
            output.status.success(),
            "packaged command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("packaged command output must be UTF-8")
    }

    fn installer(&self, args: &[&str]) -> Command {
        let mut command = self.command("/bin/bash");
        command.arg(self.package.join("install.sh")).args(args);
        command
    }

    fn expected_output(&self) -> String {
        format!(
            "Services DevTools {}: installation completed.\n\
             chrome://extensions -> Developer mode -> Load unpacked\n\
             Cmd+Shift+G, paste:\n\
             {}\n\
             Enter -> Select\n",
            env!("CARGO_PKG_VERSION"),
            locations(&self.home).base.join("extension").display()
        )
    }

    fn assert_single_copy(&self) {
        let paths = locations(&self.home);
        for name in ["releases", "current", "native"] {
            assert_eq!(
                fs::symlink_metadata(paths.base.join(name))
                    .unwrap_err()
                    .kind(),
                std::io::ErrorKind::NotFound
            );
        }
        assert!(
            paths
                .base
                .join("extension/native/bin/services-devtools")
                .is_file()
        );
    }
}

struct HttpFixture {
    address: std::net::SocketAddr,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl HttpFixture {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind owned HTTP fixture");
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        stream
                            .set_write_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let mut bytes = Vec::new();
                        let mut byte = [0; 1];
                        while bytes.len() < 16384 && !bytes.ends_with(b"\r\n\r\n") {
                            if stream.read(&mut byte).unwrap_or(0) == 0 {
                                break;
                            }
                            bytes.push(byte[0]);
                        }
                        let request = String::from_utf8_lossy(&bytes);
                        let path = request.split_whitespace().nth(1).unwrap_or("/");
                        let body = if path == "/openapi.json" {
                            json!({"openapi":"3.0.0","info":{"title":"Install smoke test","version":"1.0"},
                                "paths":{"/smoke":{"get":{"responses":{"200":{"description":"OK"}}}}}}).to_string()
                        } else {
                            json!({"installedHost":true,"path":path}).to_string()
                        };
                        let _ = write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            address,
            stop,
            worker: Some(worker),
        }
    }
}

impl Drop for HttpFixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.worker
            .take()
            .unwrap()
            .join()
            .expect("HTTP fixture must stop cleanly");
    }
}

struct Host {
    child: OwnedChild,
    replies: mpsc::Receiver<Result<Value, String>>,
    reader: Option<thread::JoinHandle<()>>,
}

impl Host {
    fn new(fixture: &Fixture, path: &Path) -> Self {
        let mut command = fixture.command(path);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = OwnedChild(command.spawn().expect("spawn installed native host"));
        let mut stdout = child.0.stdout.take().unwrap();
        let (sender, replies) = mpsc::channel();
        let reader = thread::spawn(move || {
            loop {
                let result = (|| -> Result<Value, String> {
                    let mut header = [0; 4];
                    stdout
                        .read_exact(&mut header)
                        .map_err(|_| "native host closed its frame stream".to_owned())?;
                    let size = u32::from_le_bytes(header) as usize;
                    if size > 16 * 1024 * 1024 {
                        return Err("native frame exceeds test limit".into());
                    }
                    let mut body = vec![0; size];
                    stdout
                        .read_exact(&mut body)
                        .map_err(|_| "native frame was truncated".to_owned())?;
                    serde_json::from_slice(&body)
                        .map_err(|_| "native frame is not valid JSON".to_owned())
                })();
                let failed = result.is_err();
                if sender.send(result).is_err() || failed {
                    break;
                }
            }
        });
        Self {
            child,
            replies,
            reader: Some(reader),
        }
    }

    fn request(&mut self, message: Value) -> (Value, Vec<Value>) {
        let id = message["id"].clone();
        let body = serde_json::to_vec(&message).unwrap();
        let stdin = self
            .child
            .0
            .stdin
            .as_mut()
            .expect("native host stdin must remain open");
        stdin.write_all(&(body.len() as u32).to_le_bytes()).unwrap();
        stdin.write_all(&body).unwrap();
        stdin.flush().unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut events = Vec::new();
        loop {
            let reply = self
                .replies
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|_| panic!("native request {id} did not respond within 20 seconds"))
                .unwrap_or_else(|error| panic!("native request {id}: {error}"));
            assert_eq!(reply["id"], id, "native response must match request ID");
            if reply["type"] == "data" {
                events.push(reply["event"].clone());
            } else {
                return (reply, events);
            }
        }
    }

    fn close(&mut self) {
        self.child.0.stdin.take();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.0.try_wait().unwrap() {
                assert!(
                    status.success(),
                    "native host must exit successfully after stdin closes"
                );
                break;
            }
            assert!(
                Instant::now() < deadline,
                "native host did not exit after closing stdin"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        if !matches!(self.child.0.try_wait(), Ok(Some(_))) {
            unsafe { libc::kill(-(self.child.0.id() as i32), libc::SIGKILL) };
        }
        let _ = self.child.0.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).expect("read fixture JSON file"))
        .expect("fixture JSON must be valid")
}

#[test]
#[ignore = "Build distribution first with cargo run -p xtask -- build; requires macOS arm64"]
fn archived_install_native_services_update_and_uninstall() {
    let fixture = Fixture::new();
    let paths = locations(&fixture.home);
    let source = read_json(&repository().join("adapters/extension/src/manifest.json"));
    let version = source["version"].as_str().expect("source manifest version");
    let archive = fixture.package.join("services-macos-arm64.tar.gz");
    let mut list = fixture.command("/usr/bin/tar");
    list.arg("-tzf").arg(&archive);
    let members = fixture.successful(list);
    for member in members.lines() {
        assert!(
            !member.ends_with(".pem")
                && !member.contains("prototypes/")
                && !member.contains(".variable-profiles.json")
                && !member.contains("/.git/")
                && !member.contains("native/bin/node")
                && !member.contains("native/app/")
                && !member.contains("node_modules/"),
            "archive contains forbidden runtime, development or user data member: {member}"
        );
    }
    assert!(
        members.contains("extension/wasm/services_ui_bg.wasm"),
        "archive must ship compiled WASM UI"
    );
    let extracted = fixture.directory.path().join("extracted payload");
    fs::create_dir(&extracted).unwrap();
    let mut extract = fixture.command("/usr/bin/tar");
    extract.arg("-xzf").arg(&archive).arg("-C").arg(&extracted);
    fixture.successful(extract);
    assert!(
        extracted
            .join("ServicesDevTools/extension/manifest.json")
            .is_file(),
        "real archive must extract a complete extension"
    );
    assert!(
        extracted
            .join("ServicesDevTools/native/bin/services-devtools")
            .is_file(),
        "real archive must extract a native binary"
    );

    let invalid = fixture.run(fixture.installer(&["--no-open", "--sha256", &"0".repeat(64)]));
    assert!(!invalid.status.success(), "invalid checksum must fail");
    assert!(
        String::from_utf8_lossy(&invalid.stderr).contains("checksum mismatch"),
        "invalid checksum must produce an explicit error"
    );
    assert!(
        !paths.base.exists(),
        "invalid checksum must install nothing"
    );
    let expected = fixture.expected_output();
    assert_eq!(fixture.successful(fixture.installer(&[])), expected);
    fixture.assert_single_copy();
    assert_eq!(
        expected.lines().count(),
        5,
        "installation output must include completion followed by four Chrome instruction lines"
    );
    let registration = read_json(&paths.registration);
    assert_eq!(
        registration["path"],
        paths
            .base
            .join("extension/native/host")
            .to_string_lossy()
            .as_ref()
    );
    assert_eq!(
        registration["allowed_origins"],
        json!(["chrome-extension://ckicpgdfkgcmgnclbfbcbkillcpaffnb/"])
    );
    let mut binary_version =
        fixture.command(paths.base.join("extension/native/bin/services-devtools"));
    binary_version.arg("--version");
    assert_eq!(
        fixture.successful(binary_version).trim(),
        format!("services-devtools {version}")
    );

    let server = HttpFixture::new();
    let mut host = Host::new(&fixture, &paths.base.join("extension/native/host"));
    let (health, _) = host.request(json!({"id":1,"type":"health"}));
    assert_eq!(health["ok"], true, "installed host health must succeed");
    assert_eq!(health["result"]["protocolVersion"], 1);
    let (response, _) = host.request(json!({"id":2,"service":"rest","params":{
        "method":"GET","url":format!("http://{}/smoke?env={{{{env:SERVICES_DEVTOOLS_INSTALL_ENV}}}}", server.address)}}));
    assert_eq!(
        response["ok"], true,
        "REST must succeed with shell-imported environment"
    );
    let body: Value = serde_json::from_str(
        response["result"]["body"]
            .as_str()
            .expect("REST body must be text"),
    )
    .unwrap();
    assert_eq!(
        body,
        json!({"installedHost":true,"path":"/smoke?env=installed-shell-fixture"})
    );
    let (saved, _) = host.request(json!({"id":3,"service":"vars","op":"save","name":"var","variables":{"marker":"survives-update"}}));
    assert_eq!(saved["ok"], true, "Variables save must succeed: {saved}");
    assert!(
        saved["result"]["profiles"]["env"]["SERVICES_DEVTOOLS_INSTALL_ENV"]
            == "installed-shell-fixture",
        ".zprofile environment was not imported"
    );
    assert!(
        saved["result"]["profiles"]["env"]["SERVICES_DEVTOOLS_INSTALL_INTERACTIVE"]
            == "installed-interactive-fixture",
        ".zshrc environment was not imported"
    );
    let variables = paths.data.join(".variable-profiles.json");
    assert!(
        variables.is_file(),
        "Variables must persist in isolated installation data"
    );
    let mut cli = fixture.command(paths.base.join("dev"));
    cli.args([
        "terminal",
        "--endpoint=/bin/sh -c",
        "--command=printf '{{var:marker}}'",
    ]);
    assert!(
        fixture.successful(cli).contains("survives-update"),
        "installed CLI must use saved Variables"
    );
    let (terminal, events) = host.request(json!({"id":4,"service":"terminal","params":{"endpoint":"/bin/sh -c","command":"printf installed-terminal"}}));
    assert_eq!(
        terminal["info"]["code"], 0,
        "Terminal must exit successfully"
    );
    let text: String = events
        .iter()
        .filter_map(|event| event["data"].as_str())
        .collect();
    assert!(
        text.contains("installed-terminal"),
        "Terminal must stream fixture output"
    );
    let (discovery, _) = host.request(json!({"id":5,"type":"discover","service":"rest","params":{"url":format!("http://{}", server.address)}}));
    assert_eq!(discovery["ok"], true, "OpenAPI discovery must succeed");
    assert_eq!(discovery["result"]["paths"], json!(["/smoke"]));
    assert_eq!(
        discovery["result"]["methodsByPath"]["/smoke"],
        json!(["GET"])
    );
    host.close();
    drop(host);

    let updated_expected = fixture.expected_output();
    assert!(
        fs::symlink_metadata(paths.base.join("extension"))
            .unwrap()
            .is_dir()
    );
    let extension_path = fs::canonicalize(paths.base.join("extension")).unwrap();
    let before = fs::read(&variables).unwrap();
    // Seed remembered ownership instead of opening Chrome; CDP tests cover discovery.
    let custom = fixture
        .home
        .join("Chrome's closed profile/NativeMessagingHosts/com.chrome_extentions.host.json");
    fs::create_dir_all(custom.parent().unwrap()).unwrap();
    fs::copy(&paths.registration, &custom).unwrap();
    let mut state = read_json(&paths.state);
    state["registrations"]
        .as_array_mut()
        .expect("installer must record registrations")
        .push(json!({"path":custom,"previousRegistration":null}));
    fs::write(&paths.state, serde_json::to_vec_pretty(&state).unwrap()).unwrap();
    fs::remove_file(&custom).unwrap();
    assert_eq!(
        fixture.successful(fixture.installer(&["--no-open"])),
        updated_expected
    );
    assert!(
        custom.exists(),
        "update must repair owned registration for a closed browser"
    );
    assert_eq!(
        read_json(&custom),
        read_json(&paths.registration),
        "repaired custom registration must use installed host"
    );
    let mut explicit = fixture.installer(&["--no-open", "--archive"]);
    explicit.arg(&archive);
    assert_eq!(fixture.successful(explicit), updated_expected);
    fixture.assert_single_copy();
    assert_eq!(
        fs::canonicalize(paths.base.join("extension")).unwrap(),
        extension_path
    );
    assert_eq!(
        read_json(&paths.state)["registrations"]
            .as_array()
            .unwrap()
            .len(),
        2,
        "updates must deduplicate remembered registrations"
    );
    assert!(
        fs::read(&variables).unwrap() == before,
        "updates must preserve Variables byte-for-byte"
    );

    let listener =
        TcpListener::bind("127.0.0.1:0").expect("reserve installed web adapter test port");
    let address = listener.local_addr().unwrap();
    drop(listener);
    let mut web = fixture.command(paths.base.join("extension/native/bin/services-devtools"));
    web.args(["web", "--port", &address.port().to_string()])
        .env("SERVICES_DATA_DIR", &paths.data)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut web = OwnedChild(web.spawn().expect("spawn installed web adapter"));
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut stream = loop {
        assert!(
            web.0.try_wait().unwrap().is_none(),
            "installed web adapter exited before readiness"
        );
        if let Ok(stream) = TcpStream::connect_timeout(&address, Duration::from_millis(100)) {
            break stream;
        }
        assert!(
            Instant::now() < deadline,
            "installed web adapter was not ready within 15 seconds"
        );
        thread::sleep(Duration::from_millis(50));
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        stream,
        "GET /panel.html HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut page = String::new();
    stream
        .take(2 * 1024 * 1024)
        .read_to_string(&mut page)
        .expect("read installed UI response");
    assert!(
        page.starts_with("HTTP/1.1 200"),
        "installed web adapter must serve adjacent UI outside source cwd"
    );
    assert!(
        page.contains("panel.js"),
        "installed UI must reference its JS loader"
    );
    drop(web);
    fixture.successful(fixture.command(paths.base.join("uninstall")));
    for path in [
        &paths.registration,
        &custom,
        &paths.current,
        &paths.releases,
        &paths.base.join("dev"),
    ] {
        assert!(
            !path.exists(),
            "uninstall must remove owned path {}",
            path.display()
        );
    }
    assert!(
        fs::read(&variables).unwrap() == before,
        "uninstall must preserve Variables byte-for-byte"
    );
}
