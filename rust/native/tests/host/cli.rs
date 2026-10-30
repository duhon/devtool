use serde_json::json;
use services_native::adapters::{
    cli,
    framing::{FrameReader, encode},
};
use services_testkit::DataDir;

#[test]
fn cli_parses_aliases_positionals_json_and_repeated_options() {
    let host = services_testkit::Host::new();
    let config = &host.runtime().registry.manifest("splunk").unwrap().params;
    let args = [
        "http://fixture",
        "index=main",
        "-o",
        "from=-24h",
        "--options=app=search",
        "-o",
        "count=0",
        "-H",
        "{\"X\":\"v\"}",
    ]
    .map(str::to_string);
    let params = cli::parse_params(config, &args).unwrap();
    assert_eq!(
        params,
        json!({"baseUrl":"http://fixture","headers":{"X":"v"},"search":"index=main","earliestTime":"-24h","app":"search","count":"0"})
    );
    assert!(cli::parse_params(config, &["-o=unknown=x".into()]).is_err());
    let rest = &host.runtime().registry.manifest("rest").unwrap().params;
    assert_eq!(
        cli::parse_params(rest, &["--endpoint=http://fixture".into()]).unwrap()["method"],
        "GET"
    );
    assert!(cli::parse_params(rest, &["--endpoint=x".into(), "-H".into(), "bad".into()]).is_err());
}

#[test]
fn binary_serves_native_messaging_and_cli_help_and_version() {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    let data = DataDir::new();
    let binary = env!("CARGO_BIN_EXE_services-devtools");
    let mut child = Command::new(binary)
        .env_clear()
        .env("SERVICES_DATA_DIR", data.path())
        .env("HOME", data.path())
        .env("SHELL", "/bin/sh")
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for chunk in encode(&json!({"id":42,"type":"health"})).unwrap().chunks(3) {
        stdin.write_all(chunk).unwrap();
    }
    drop(stdin);
    let mut bytes = vec![];
    child
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(child.wait().unwrap().success());
    assert_eq!(
        FrameReader::default().feed(&bytes).unwrap(),
        vec![json!({"id":42,"ok":true,"result":{"protocolVersion":1}})]
    );
    let version = Command::new(binary).arg("--version").output().unwrap();
    assert!(version.status.success());
    assert!(String::from_utf8_lossy(&version.stdout).contains(env!("CARGO_PKG_VERSION")));
    let help = Command::new(binary)
        .args(["cli", "splunk", "--help"])
        .env("SERVICES_DATA_DIR", data.path())
        .output()
        .unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stderr).contains("repeatable"));
    let output = Command::new(binary)
        .args(["cli", "terminal", "sh -c", "printf cli-fixture; exit 7"])
        .env("SERVICES_DATA_DIR", data.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(output.stdout, b"cli-fixture");
}

#[tokio::test(flavor = "multi_thread")]
async fn binary_exits_with_the_module_status_of_a_call() {
    let (server, _) = services_testkit::http::services().await;
    let data = DataDir::new();
    let url = server.url.clone();
    let call = move |path: &str| {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_services-devtools"))
            .args(["cli", "rest", "GET", &format!("{url}{path}")])
            .env("SERVICES_DATA_DIR", data.path())
            .output()
            .unwrap();
        let printed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        (output.status.code(), printed["status"].clone())
    };
    let (ok, missing) = tokio::task::spawn_blocking(move || (call("/echo"), call("/missing")))
        .await
        .unwrap();
    assert_eq!(ok, (Some(0), json!(201)));
    assert_eq!(missing, (Some(1), json!(404)));
}

#[test]
fn binary_flushes_terminal_stdout_and_stderr_before_exit() {
    let data = DataDir::new();
    for iteration in 0..32 {
        let code = if iteration % 2 == 0 { 7 } else { 0 };
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_services-devtools"))
            .args([
                "cli",
                "terminal",
                "sh -c",
                &format!("printf cli-fixture; printf cli-error >&2; exit {code}"),
            ])
            .env("SERVICES_DATA_DIR", data.path())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(code), "iteration {iteration}");
        assert_eq!(output.stdout, b"cli-fixture", "iteration {iteration}");
        assert_eq!(output.stderr, b"cli-error", "iteration {iteration}");
    }
}
