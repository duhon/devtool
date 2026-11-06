use serde_json::{Value, json};
use services_testkit::host::DataDir;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Runs a Terminal command through the host registry and the module transport; the answer
/// carries the module's `outcome`, as the console receives it.
async fn run(
    params: Value,
    cancel: CancellationToken,
    events: mpsc::Sender<Value>,
) -> services_native::Result<Value> {
    let data = DataDir::new();
    let output = data
        .runtime()
        .registry
        .stream_raw("terminal", params, cancel, events, false)
        .await?;
    let mut result = output.value()?;
    result["outcome"] = output.outcome.into();
    Ok(result)
}

#[tokio::test]
async fn stop_reaps_the_whole_process_group() {
    let (tx, mut rx) = mpsc::channel(128);
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let params = json!({"endpoint":"sh -c","env":{"VALUE":"a'b"},"command":"printf '%s' \"$VALUE\"; printf err >&2; sleep 30"});
    let task = tokio::spawn(async move { run(params, token, tx).await.unwrap() });
    let mut output = String::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .unwrap()
            .unwrap();
        output.push_str(event["data"].as_str().unwrap());
        if output.contains("a'b") {
            break;
        }
    }
    cancel.cancel();
    let info = tokio::time::timeout(Duration::from_secs(4), task)
        .await
        .unwrap()
        .unwrap();
    assert!(info["code"].is_null());
    assert_eq!(info["signal"], "SIGTERM");
    assert!(
        info["debug"]["connection"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["event"] == "close")
    );
}

#[tokio::test]
async fn a_failed_command_keeps_its_exit_code_and_stderr_tail() {
    let (tx, mut rx) = mpsc::channel(128);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let params = json!({"endpoint":"sh -c","command":"printf out; printf 'first\\nls: x: No such file\\n' >&2; exit 3"});
    let result = run(params, CancellationToken::new(), tx).await.unwrap();
    drain.await.unwrap();
    assert_eq!(result["code"], 3, "{result}");
    assert_eq!(result["stderr"], "first\nls: x: No such file\n", "{result}");

    let (tx, mut rx) = mpsc::channel(128);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let params = json!({"endpoint":"sh -c","command":"printf out"});
    let result = run(params, CancellationToken::new(), tx).await.unwrap();
    drain.await.unwrap();
    assert_eq!(result["code"], 0, "{result}");
    assert!(result.get("stderr").is_none(), "{result}");
}

#[cfg(unix)]
#[test]
#[ignore = "subprocess fixture for detached Terminal output tests"]
fn detached_pipe_fixture() {
    use std::os::unix::process::CommandExt;
    let mode = std::env::var("SERVICES_TERMINAL_PIPE_FIXTURE").unwrap();
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
                eprintln!("Failed to clean up owned Terminal fixture child {}", self.0);
            }
        }
    }
    let executable = std::env::current_exe().unwrap();
    let command = format!(
        "'{}' --exact terminal::detached_pipe_fixture --ignored --nocapture",
        executable.to_str().unwrap().replace('\'', "'\\''")
    );
    let child_pid = regex::Regex::new(r"fixture-child-pid:(\d+)").unwrap();
    let parent_pid = regex::Regex::new(r"fixture-parent-pid:(\d+)").unwrap();
    for mode in ["automatic", "stop-after-exit", "stop-running"] {
        let (tx, mut rx) = mpsc::channel(128);
        let cancel = CancellationToken::new();
        let params = json!({"endpoint":"sh -c","command":command,
            "env":{"SERVICES_TERMINAL_PIPE_FIXTURE":if mode=="stop-running" {"hold"} else {"exit"}}});
        let token = cancel.clone();
        let mut task = tokio::spawn(async move { run(params, token, tx).await });
        let mut output = String::new();
        while !parent_pid.is_match(&output) {
            let event = tokio::time::timeout(Duration::from_secs(3), rx.recv())
                .await
                .unwrap()
                .unwrap();
            output.push_str(event["data"].as_str().unwrap());
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
            cancel.cancel();
        }
        let result = match tokio::time::timeout(Duration::from_secs(4), &mut task).await {
            Ok(result) => result.unwrap().unwrap(),
            Err(error) => {
                task.abort();
                panic!("{mode}: Terminal did not finish: {error}");
            }
        };
        assert_eq!(result["cancelled"], mode != "automatic", "{mode}: {result}");
        assert_eq!(result["outputIncomplete"], true, "{mode}: {result}");
        assert!(
            result["warning"]
                .as_str()
                .unwrap()
                .contains("Detached background processes")
        );
        assert_eq!(
            unsafe { libc::kill(child.0, 0) },
            0,
            "Terminal must not kill detached resources outside its owned process group"
        );
        if mode == "automatic" {
            assert_eq!(result["code"], 0);
            assert_eq!(result["outcome"], "WARNING", "{result}");
        } else {
            assert!(
                stopped_at.elapsed() < Duration::from_secs(3),
                "{mode}: {result}"
            );
            assert_eq!(result["outcome"], "Stopped", "{result}");
        }
        while rx.recv().await.is_some() {}
    }
}

#[tokio::test]
async fn buffered_output_survives_a_normal_exit() {
    let (tx, mut rx) = mpsc::channel(128);
    let params = json!({"endpoint":"sh -c","command":"head -c 262144 /dev/zero | tr '\\000' x"});
    let task = tokio::spawn(async move { run(params, CancellationToken::new(), tx).await });
    let mut output = String::new();
    while let Some(event) = rx.recv().await {
        output.push_str(event["data"].as_str().unwrap());
    }
    let result = task.await.unwrap().unwrap();
    assert_eq!(result["code"], 0);
    assert_ne!(result["outputIncomplete"], true);
    assert_eq!(output, "x".repeat(262144));
}
