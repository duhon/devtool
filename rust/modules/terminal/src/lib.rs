//! Terminal: runs a command through a shell endpoint and streams its output.

mod process;

use process::ChildGroup;
use serde_json::{Value, json};
use services_module_api::error::{Error, Result, required, string};
use services_module_api::sdk::{self, Call, Events, Manifest, Service, Threads};
use services_module_api::transport::{EndpointArc, EndpointBox, Status};
use std::{
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    sync::mpsc,
};
use tokio_util::sync::CancellationToken;
pub struct Terminal;

impl Service for Terminal {
    fn manifest(&self) -> Manifest {
        Manifest {
            execute: false,
            discover: false,
            stream: true,
            ..Manifest::from_json(include_str!("../manifest.json"), env!("CARGO_PKG_VERSION"))
        }
    }

    fn soft_cancel(&self) -> bool {
        true
    }

    async fn stream(&self, call: Call, events: Events) -> Result<Value> {
        run(&call.params, call.cancel, events).await
    }

    fn outcome(&self, result: &Value) -> &'static str {
        if let Some(outcome) = sdk::common_outcome(result) {
            return outcome;
        }
        match result["code"].as_i64() {
            Some(1) => "ERROR",
            Some(0) if result["outputIncomplete"] != true => "OK",
            _ => "WARNING",
        }
    }
}

pub fn endpoint(host: EndpointArc) -> std::result::Result<EndpointBox, Status> {
    sdk::serve(Terminal, host, Threads::Own("terminal-module"))
}

services_module_api::export!(endpoint);

pub fn parse_argv(input: &str) -> Result<Vec<String>> {
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    let mut out = vec![];
    while i < chars.len() {
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i == chars.len() {
            break;
        }
        let mut token = String::new();
        while i < chars.len() && !chars[i].is_whitespace() {
            match chars[i] {
                '\'' => {
                    i += 1;
                    while i < chars.len() && chars[i] != '\'' {
                        token.push(chars[i]);
                        i += 1;
                    }
                    if i == chars.len() {
                        return Err(Error::new("Unterminated single quote in endpoint"));
                    }
                    i += 1;
                }
                '"' => {
                    i += 1;
                    while i < chars.len() && chars[i] != '"' {
                        if chars[i] == '\\'
                            && i + 1 < chars.len()
                            && matches!(chars[i + 1], '"' | '\\')
                        {
                            i += 1;
                        }
                        token.push(chars[i]);
                        i += 1;
                    }
                    if i == chars.len() {
                        return Err(Error::new("Unterminated double quote in endpoint"));
                    }
                    i += 1;
                }
                '\\' if i + 1 < chars.len() => {
                    i += 1;
                    token.push(chars[i]);
                    i += 1;
                }
                c => {
                    token.push(c);
                    i += 1;
                }
            }
        }
        out.push(token);
    }
    Ok(out)
}
pub fn build_command(command: &str, env: &Value) -> String {
    let exports: Vec<_> = env
        .as_object()
        .into_iter()
        .flatten()
        .map(|(name, value)| format!("{name}='{}'", string(value).replace('\'', "'\\''")))
        .collect();
    if exports.is_empty() {
        command.into()
    } else {
        format!("export {}; {command}", exports.join(" "))
    }
}
pub async fn run(
    params: &Value,
    cancel: CancellationToken,
    events: mpsc::Sender<Value>,
) -> Result<Value> {
    let endpoint = required(params, "endpoint")?;
    let command = required(params, "command")?;
    let argv = parse_argv(&endpoint)?;
    if argv.is_empty() {
        return Err(Error::new("endpoint must not be empty"));
    }
    let env = params.get("env").cloned().unwrap_or(json!({}));
    if !env.is_object() {
        return Err(Error::new("env must be an object of name/value pairs"));
    }
    let started = Instant::now();
    let request = json!({"endpoint":endpoint,"argv":argv,"env":env});
    let mut cmd = tokio::process::Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .arg(build_command(&command, &env))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.as_std_mut().process_group(0);
    }
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(error) => {
            return Ok(
                json!({"code":null,"signal":null,"error":error.to_string(),"durationMs":started.elapsed().as_millis(),"debug":{"request":request,"connection":{"events":[{"event":"error","atMs":started.elapsed().as_millis(),"data":{"name":"Error","message":error.to_string()}}]}}}),
            );
        }
    };
    let pid = child.id().unwrap();
    let group = ChildGroup(pid);
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let (status, cancelled, output_incomplete) = {
        let output = async {
            tokio::try_join!(
                read_output(stdout, "stdout", events.clone()),
                read_output(stderr, "stderr", events),
            )?;
            Ok::<_, std::io::Error>(())
        };
        let wait = async {
            let result = tokio::select! {
                result=child.wait()=>(result, false),
                _=cancel.cancelled()=>{
                    group.signal(libc::SIGTERM);
                    let result = match tokio::time::timeout(Duration::from_secs(2),child.wait()).await {
                        Ok(result)=>result,
                        Err(_)=>{group.signal(libc::SIGKILL);child.wait().await}
                    };
                    (result, true)
                }
            };
            group.signal(libc::SIGKILL);
            result
        };
        tokio::pin!(output);
        tokio::pin!(wait);
        let mut output_result = None;
        let (status, cancelled) = tokio::select! {
            result=&mut wait=>result,
            result=&mut output=>{
                if result.is_err() { cancel.cancel(); }
                output_result = Some(result);
                wait.await
            }
        };
        let incomplete = if let Some(result) = output_result {
            result?;
            false
        } else {
            tokio::select! {
                biased;
                result=&mut output=>{result?;false},
                _=cancel.cancelled()=>true,
                _=tokio::time::sleep(Duration::from_secs(2))=>true,
            }
        };
        (status?, cancelled || cancel.is_cancelled(), incomplete)
    };
    let code = status.code();
    #[cfg(unix)]
    let signal = {
        use std::os::unix::process::ExitStatusExt;
        status.signal().map(|signal| match signal {
            libc::SIGTERM => "SIGTERM".to_string(),
            libc::SIGKILL => "SIGKILL".to_string(),
            libc::SIGINT => "SIGINT".to_string(),
            libc::SIGHUP => "SIGHUP".to_string(),
            libc::SIGSEGV => "SIGSEGV".to_string(),
            libc::SIGQUIT => "SIGQUIT".to_string(),
            libc::SIGABRT => "SIGABRT".to_string(),
            libc::SIGILL => "SIGILL".to_string(),
            libc::SIGFPE => "SIGFPE".to_string(),
            libc::SIGPIPE => "SIGPIPE".to_string(),
            libc::SIGALRM => "SIGALRM".to_string(),
            libc::SIGUSR1 => "SIGUSR1".to_string(),
            libc::SIGUSR2 => "SIGUSR2".to_string(),
            libc::SIGTRAP => "SIGTRAP".to_string(),
            libc::SIGBUS => "SIGBUS".to_string(),
            libc::SIGXCPU => "SIGXCPU".to_string(),
            libc::SIGXFSZ => "SIGXFSZ".to_string(),
            _ => format!("SIG{signal}"),
        })
    };
    #[cfg(not(unix))]
    let signal: Option<String> = None;
    let elapsed = started.elapsed().as_millis();
    let mut result = json!({"code":code,"signal":signal,"durationMs":elapsed,"cancelled":cancelled,
    "debug":{"request":request,"connection":{"events":[
        {"event":"spawn","atMs":0,"data":{"pid":pid}},
        {"event":"exit","atMs":elapsed,"data":{"code":code,"signal":signal}},
        {"event":"close","atMs":elapsed,"data":{"code":code,"signal":signal}}
    ]}}});
    if output_incomplete {
        result["outputIncomplete"] = json!(true);
        result["warning"] = json!(
            "Output capture ended before stdout/stderr closed. Detached background processes may still be running; close their resources explicitly."
        );
    }
    Ok(result)
}
async fn read_output(
    mut reader: impl AsyncRead + Unpin,
    kind: &str,
    events: mpsc::Sender<Value>,
) -> std::io::Result<()> {
    let mut buf = [0u8; 8192];
    let mut pending = Vec::new();
    loop {
        match reader.read(&mut buf).await {
            Ok(0) => break,
            Err(error) => return Err(error),
            Ok(n) => {
                pending.extend_from_slice(&buf[..n]);
                let valid = match std::str::from_utf8(&pending) {
                    Ok(_) => pending.len(),
                    Err(e) => {
                        if e.error_len().is_some() {
                            pending.len()
                        } else {
                            e.valid_up_to()
                        }
                    }
                };
                if valid > 0 {
                    let data = String::from_utf8_lossy(&pending[..valid]).to_string();
                    pending.drain(..valid);
                    if events.send(json!({"type":kind,"data":data})).await.is_err() {
                        break;
                    }
                }
            }
        }
    }
    if !pending.is_empty() {
        let _ = events
            .send(json!({"type":kind,"data":String::from_utf8_lossy(&pending)}))
            .await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incomplete_output_is_a_warning_unless_stopped_or_failed() {
        for (result, expected) in [
            (json!({"code":0,"outputIncomplete":true}), "WARNING"),
            (
                json!({"code":0,"outputIncomplete":true,"cancelled":true}),
                "Stopped",
            ),
            (
                json!({"code":0,"outputIncomplete":true,"error":"read failed"}),
                "ERROR",
            ),
            (json!({"code":1,"outputIncomplete":true}), "ERROR"),
            (json!({"code":0}), "OK"),
            (json!({"code":2}), "WARNING"),
        ] {
            assert_eq!(Terminal.outcome(&result), expected, "{result}");
        }
    }

    #[test]
    fn argv_quotes_and_env_match_the_shell_contract() {
        assert_eq!(
            parse_argv("ssh 'some host' -i \"a b\\\"c\" x\\ y").unwrap(),
            vec!["ssh", "some host", "-i", "a b\"c", "x y"]
        );
        assert_eq!(parse_argv("sh -c ''").unwrap(), vec!["sh", "-c", ""]);
        assert!(parse_argv("sh 'bad").is_err());
        assert!(parse_argv("sh \"bad").is_err());
        assert_eq!(
            build_command("printf \"$VALUE\"", &json!({"VALUE":"a'b"})),
            "export VALUE='a'\\''b'; printf \"$VALUE\""
        );
    }
}
