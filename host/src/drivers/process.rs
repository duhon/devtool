//! The process driver: runs a program in its own process group. Stop or a timeout sends SIGTERM
//! to the group and SIGKILL after a grace period; output is read to its end or for a short
//! while after the exit, as long as something detached keeps it open.

use super::{Failure, emit, http::complete_text, micros, millis};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use serde_json::{Value, json};
use services_sdk::{
    driver::Stage,
    process::{Exit, Request},
};
use std::{process::Stdio, sync::Mutex, time::Instant};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    sync::mpsc,
};
use tokio_util::sync::CancellationToken;

/// Between SIGTERM and SIGKILL.
const GRACE_MS: u64 = 2_000;
/// How long output may stay open after the exit.
const DRAIN_MS: u64 = 2_000;
/// Output a call keeps per stream; a stream has no limit.
const CALL_OUTPUT: usize = 16 * 1024 * 1024;

/// Kills the whole process group when dropped.
struct Group(u32);

impl Group {
    fn signal(&self, _signal: i32) {
        #[cfg(unix)]
        unsafe {
            libc::kill(-(self.0 as i32), _signal);
        }
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        self.signal(libc::SIGKILL);
    }
}

#[derive(Clone, Copy)]
enum Pipe {
    Stdout,
    Stderr,
}

impl Pipe {
    fn event(self, text: bool) -> &'static str {
        match (self, text) {
            (Pipe::Stdout, true) => "stdout",
            (Pipe::Stderr, true) => "stderr",
            (Pipe::Stdout, false) => "stdoutBase64",
            (Pipe::Stderr, false) => "stderrBase64",
        }
    }
}

/// What a call keeps of the output.
#[derive(Default)]
struct Kept {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    truncated: bool,
    first_byte_us: Option<u64>,
}

pub async fn run(
    request: Request,
    cancel: CancellationToken,
    events: Option<&mpsc::Sender<Value>>,
) -> Result<Value, Failure> {
    let started = Instant::now();
    if request.program.trim().is_empty() {
        return Err(Failure::new(Stage::Send, "The program is empty"));
    }
    let mut command = tokio::process::Command::new(&request.program);
    command
        .args(&request.args)
        .envs(request.env.iter().map(|(name, value)| (name, value)))
        .stdin(if request.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(cwd) = &request.cwd {
        command.current_dir(cwd);
    }
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.spawn().map_err(|error| {
        Failure::new(
            Stage::Connect,
            format!("Cannot start \"{}\": {error}", request.program),
        )
    })?;
    let pid = child.id().unwrap_or_default();
    let group = Group(pid);
    if let Some(events) = events {
        emit(events, json!({"spawned": {"pid": pid}})).await?;
    }
    if let (Some(data), Some(mut stdin)) = (request.stdin.clone(), child.stdin.take()) {
        tokio::spawn(async move {
            let _ = stdin.write_all(data.as_bytes()).await;
        });
    }
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let kept = Mutex::new(Kept::default());
    let output = async {
        tokio::try_join!(
            read(stdout, Pipe::Stdout, &request, events, &kept, started),
            read(stderr, Pipe::Stderr, &request, events, &kept, started),
        )
        .map(|_| ())
    };
    let deadline = async {
        match request.timeout_ms {
            Some(ms) => tokio::time::sleep(millis(ms)).await,
            None => std::future::pending().await,
        }
    };
    // Also stops the group when reading the output fails.
    let local = cancel.child_token();
    let wait = async {
        let result = tokio::select! {
            result = child.wait() => (result, None),
            () = local.cancelled() => (stop(&group, &mut child).await, Some(Stage::Cancelled)),
            () = deadline => (stop(&group, &mut child).await, Some(Stage::Timeout)),
        };
        group.signal(libc::SIGKILL);
        result
    };
    let (status, stopped, incomplete, read_result) = {
        tokio::pin!(output, wait);
        tokio::select! {
            (status, stopped) = &mut wait => {
                // A stopped group is not waited for.
                let drain = if stopped.is_some() { 0 } else { DRAIN_MS };
                tokio::select! {
                    biased;
                    result = &mut output => (status, stopped, false, Some(result)),
                    () = cancel.cancelled(), if stopped.is_none() => (status, Some(Stage::Cancelled), true, None),
                    () = tokio::time::sleep(millis(drain)) => (status, stopped, true, None),
                }
            }
            result = &mut output => {
                if result.is_err() {
                    local.cancel();
                }
                let (status, stopped) = wait.await;
                (status, stopped, false, Some(result))
            }
        }
    };
    let read_failed = matches!(read_result, Some(Err(_)));
    if let Some(stage) = stopped.filter(|_| !read_failed || cancel.is_cancelled()) {
        let message = match stage {
            Stage::Timeout => "The process timed out and was stopped",
            _ => "Cancelled",
        };
        return Err(Failure::new(stage, message));
    }
    if let Some(result) = read_result {
        result?;
    }
    let status = status.map_err(|error| Failure::chain(Stage::Decode, &error))?;
    let kept = kept.into_inner().unwrap_or_else(|e| e.into_inner());
    let total_us = micros(started);
    let mut exit = Exit {
        pid,
        code: status.code(),
        signal: signal(&status),
        output_incomplete: incomplete,
        truncated: kept.truncated,
        timing: services_sdk::http::Timing {
            first_byte_us: kept.first_byte_us.unwrap_or(total_us),
            total_us,
        },
        ..Exit::default()
    };
    if events.is_none() {
        if request.text {
            exit.stdout = Some(String::from_utf8_lossy(&kept.stdout).into_owned());
            exit.stderr = Some(String::from_utf8_lossy(&kept.stderr).into_owned());
        } else {
            exit.stdout_base64 = Some(B64.encode(&kept.stdout));
            exit.stderr_base64 = Some(B64.encode(&kept.stderr));
        }
    }
    Ok(json!(exit))
}

/// SIGTERM to the group, SIGKILL when it outlives the grace period.
async fn stop(
    group: &Group,
    child: &mut tokio::process::Child,
) -> std::io::Result<std::process::ExitStatus> {
    group.signal(libc::SIGTERM);
    match tokio::time::timeout(millis(GRACE_MS), child.wait()).await {
        Ok(result) => result,
        Err(_) => {
            group.signal(libc::SIGKILL);
            child.wait().await
        }
    }
}

async fn read(
    mut reader: impl AsyncRead + Unpin,
    pipe: Pipe,
    request: &Request,
    events: Option<&mpsc::Sender<Value>>,
    kept: &Mutex<Kept>,
    started: Instant,
) -> Result<(), Failure> {
    let mut buf = vec![0u8; 8192];
    let mut pending = Vec::new();
    loop {
        let n = reader
            .read(&mut buf)
            .await
            .map_err(|error| Failure::chain(Stage::Decode, &error))?;
        let chunk = &buf[..n];
        if n > 0 {
            let mut kept = kept.lock().unwrap_or_else(|e| e.into_inner());
            kept.first_byte_us.get_or_insert_with(|| micros(started));
            if events.is_none() {
                let truncated = kept.truncated;
                let out = match pipe {
                    Pipe::Stdout => &mut kept.stdout,
                    Pipe::Stderr => &mut kept.stderr,
                };
                let room = CALL_OUTPUT.saturating_sub(out.len());
                out.extend_from_slice(&chunk[..n.min(room)]);
                kept.truncated = truncated || n > room;
                continue;
            }
        }
        let Some(events) = events else { break };
        let data = if !request.text {
            if n == 0 {
                break;
            }
            Value::String(B64.encode(chunk))
        } else {
            pending.extend_from_slice(chunk);
            let text = if n == 0 {
                String::from_utf8_lossy(&std::mem::take(&mut pending)).into_owned()
            } else {
                complete_text(&mut pending)
            };
            if text.is_empty() && n > 0 {
                continue;
            }
            if text.is_empty() {
                break;
            }
            Value::String(text)
        };
        emit(events, json!({ pipe.event(request.text): data })).await?;
        if n == 0 {
            break;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn signal(status: &std::process::ExitStatus) -> Option<String> {
    use std::os::unix::process::ExitStatusExt;
    let name = |signal| match signal {
        libc::SIGHUP => "SIGHUP",
        libc::SIGINT => "SIGINT",
        libc::SIGQUIT => "SIGQUIT",
        libc::SIGILL => "SIGILL",
        libc::SIGTRAP => "SIGTRAP",
        libc::SIGABRT => "SIGABRT",
        libc::SIGBUS => "SIGBUS",
        libc::SIGFPE => "SIGFPE",
        libc::SIGKILL => "SIGKILL",
        libc::SIGUSR1 => "SIGUSR1",
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGUSR2 => "SIGUSR2",
        libc::SIGPIPE => "SIGPIPE",
        libc::SIGALRM => "SIGALRM",
        libc::SIGTERM => "SIGTERM",
        libc::SIGXCPU => "SIGXCPU",
        libc::SIGXFSZ => "SIGXFSZ",
        _ => "",
    };
    status.signal().map(|signal| match name(signal) {
        "" => format!("SIG{signal}"),
        name => name.to_owned(),
    })
}

#[cfg(not(unix))]
fn signal(_status: &std::process::ExitStatus) -> Option<String> {
    None
}
