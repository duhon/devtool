use serde_json::{Value, json};
use services_sdk::process::{self, Event, Exit};
use services_testkit::{DataDir, native::Pipe};
use std::time::Duration;

fn call(id: u32, op: &str, request: &process::Request) -> Value {
    super::call("process", id, op, json!(request))
}

fn sh(script: &str) -> process::Request {
    process::Request::new("sh", &["-c", script])
}

#[tokio::test]
async fn process_call_runs_with_env_cwd_stdin_and_reports_the_exit() {
    let data = DataDir::new();
    let runtime = data.runtime();
    let mut request = sh("read line; echo \"$GREETING $line\"; pwd; echo oops >&2; exit 3");
    request.env.push(("GREETING".into(), "hello".into()));
    let cwd = std::fs::canonicalize(std::env::temp_dir()).unwrap();
    request.cwd = Some(cwd.to_string_lossy().into_owned());
    request.stdin = Some("world\n".into());
    let reply = runtime.dispatch(&call(1, "call", &request)).await;
    assert_eq!(reply["ok"], true, "{reply}");
    let exit = Exit::parse(&reply["result"]).unwrap();
    assert_eq!(exit.code, Some(3));
    assert!(exit.pid > 0);
    let stdout = exit.stdout.unwrap();
    assert!(stdout.starts_with("hello world\n"), "{stdout}");
    let printed = std::fs::canonicalize(stdout.lines().nth(1).unwrap()).unwrap();
    assert_eq!(printed, cwd);
    assert_eq!(exit.stderr.as_deref(), Some("oops\n"));
    assert!(!exit.truncated && !exit.output_incomplete);

    let mut binary = sh("printf '\\377'");
    binary.text = false;
    let reply = runtime.dispatch(&call(2, "call", &binary)).await;
    let exit = Exit::parse(&reply["result"]).unwrap();
    assert_eq!(exit.stdout_base64.as_deref(), Some("/w=="));
    assert_eq!(exit.code, Some(0));

    let reply = runtime
        .dispatch(&call(3, "call", &sh("kill -TERM $$")))
        .await;
    let exit = Exit::parse(&reply["result"]).unwrap();
    assert_eq!((exit.code, exit.signal.as_deref()), (None, Some("SIGTERM")));
}

#[tokio::test]
async fn process_stream_sends_spawned_then_output_and_stops_the_group_on_cancel() {
    let data = DataDir::new();
    let mut pipe = Pipe::start(data.runtime());
    pipe.send(&call(1, "stream", &sh("echo out; echo err >&2")))
        .await;
    let (events, exit) = super::events(&mut pipe).await;
    let events: Vec<Event> = events.iter().map(|e| Event::parse(e).unwrap()).collect();
    assert!(matches!(events[0], Event::Spawned { pid } if pid > 0));
    assert!(events.contains(&Event::Stdout("out\n".into())));
    assert!(events.contains(&Event::Stderr("err\n".into())));
    let exit = Exit::parse(&exit["info"]).unwrap();
    assert_eq!(exit.code, Some(0));
    assert_eq!(exit.stdout, None);

    pipe.send(&call(2, "stream", &sh("echo ready; sleep 30 & wait")))
        .await;
    let mut ready = false;
    while !ready {
        let message = pipe.read().await;
        ready = message["event"]["stdout"] == "ready\n";
    }
    pipe.send(&json!({"id": 2, "type": "stop"})).await;
    let started = std::time::Instant::now();
    let (_, reply) = super::events(&mut pipe).await;
    assert_eq!(reply["failure"]["stage"], "cancelled", "{reply}");
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn process_failures_name_their_stage() {
    let data = DataDir::new();
    let runtime = data.runtime();
    let missing = process::Request::new("no-such-program-fixture", &[]);
    let reply = runtime.dispatch(&call(1, "call", &missing)).await;
    assert_eq!(reply["failure"]["stage"], "connect", "{reply}");
    assert!(
        reply["error"]
            .as_str()
            .unwrap()
            .contains("no-such-program-fixture")
    );

    let mut slow = sh("sleep 30");
    slow.timeout_ms = Some(200);
    let reply = tokio::time::timeout(
        Duration::from_secs(10),
        runtime.dispatch(&call(2, "call", &slow)),
    )
    .await
    .unwrap();
    assert_eq!(reply["failure"]["stage"], "timeout", "{reply}");
}

#[tokio::test]
async fn buffered_output_survives_a_normal_exit() {
    let data = DataDir::new();
    let mut pipe = Pipe::start(data.runtime());
    pipe.send(&call(
        1,
        "stream",
        &sh("head -c 262144 /dev/zero | tr '\\000' x"),
    ))
    .await;
    let (events, exit) = super::events(&mut pipe).await;
    let output: String = events
        .iter()
        .filter_map(|e| match Event::parse(e).unwrap() {
            Event::Stdout(text) => Some(text),
            _ => None,
        })
        .collect();
    let exit = Exit::parse(&exit["info"]).unwrap();
    assert_eq!(exit.code, Some(0));
    assert!(!exit.output_incomplete);
    assert_eq!(output, "x".repeat(262144));
}

#[cfg(unix)]
#[test]
#[ignore = "subprocess fixture for detached process output tests"]
fn detached_pipe_fixture() {
    use std::os::unix::process::CommandExt;
    let mode = std::env::var("SERVICES_PROCESS_PIPE_FIXTURE").unwrap();
    let child = std::process::Command::new("sleep")
        .arg("30")
        .process_group(0)
        .spawn()
        .unwrap();
    println!("fixture-child-pid:{}", child.id());
    println!("fixture-parent-pid:{}", std::process::id());
    // The owning test terminates this exact PID after the fixture parent exits.
    std::mem::forget(child);
    if mode == "hold" {
        std::thread::sleep(Duration::from_secs(30));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn detached_pipes_finish_and_remain_stoppable_after_parent_exit() {
    struct EscapedChild(i32);
    impl Drop for EscapedChild {
        fn drop(&mut self) {
            let result = unsafe { libc::kill(self.0, libc::SIGKILL) };
            if result != 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
                eprintln!("Failed to clean up owned process fixture child {}", self.0);
            }
        }
    }
    let executable = std::env::current_exe().unwrap();
    let command = format!(
        "'{}' --exact process::detached_pipe_fixture --ignored --nocapture",
        executable.to_str().unwrap().replace('\'', "'\\''")
    );
    let child_pid = regex::Regex::new(r"fixture-child-pid:(\d+)").unwrap();
    let parent_pid = regex::Regex::new(r"fixture-parent-pid:(\d+)").unwrap();
    for mode in ["automatic", "stop-after-exit", "stop-running"] {
        let data = DataDir::new();
        let mut pipe = Pipe::start(data.runtime());
        let mut request = sh(&command);
        request.env.push((
            "SERVICES_PROCESS_PIPE_FIXTURE".into(),
            if mode == "stop-running" {
                "hold"
            } else {
                "exit"
            }
            .into(),
        ));
        pipe.send(&call(1, "stream", &request)).await;
        let mut output = String::new();
        while !parent_pid.is_match(&output) {
            let message = tokio::time::timeout(Duration::from_secs(3), pipe.read())
                .await
                .unwrap();
            if let Ok(Event::Stdout(text)) = Event::parse(&message["event"]) {
                output.push_str(&text);
            }
        }
        let child = EscapedChild(child_pid.captures(&output).unwrap()[1].parse().unwrap());
        let parent: i32 = parent_pid.captures(&output).unwrap()[1].parse().unwrap();
        if mode == "stop-after-exit" {
            tokio::time::timeout(Duration::from_secs(1), async {
                while unsafe { libc::kill(parent, 0) } == 0 {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
        }
        let stopped_at = std::time::Instant::now();
        if mode != "automatic" {
            pipe.send(&json!({"id": 1, "type": "stop"})).await;
        }
        let (_, reply) = tokio::time::timeout(Duration::from_secs(4), super::events(&mut pipe))
            .await
            .unwrap_or_else(|_| panic!("{mode}: the process did not finish"));
        assert_eq!(
            unsafe { libc::kill(child.0, 0) },
            0,
            "the driver must not kill detached resources outside its process group"
        );
        if mode == "automatic" {
            let exit = Exit::parse(&reply["info"]).unwrap();
            assert_eq!(exit.code, Some(0), "{reply}");
            assert!(exit.output_incomplete, "{reply}");
        } else {
            assert_eq!(reply["failure"]["stage"], "cancelled", "{mode}: {reply}");
            assert!(
                stopped_at.elapsed() < Duration::from_secs(3),
                "{mode}: {reply}"
            );
        }
    }
}
