use super::framing::{FrameReader, encode, response_frames, response_frames_parts};
use crate::runtime::{Reply, error_reply};
use crate::{Result, Runtime};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::mpsc,
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

pub async fn reject_startup(
    error: &crate::Error,
    mut input: impl AsyncRead + Unpin,
    mut output: impl AsyncWrite + Unpin,
) -> Result<()> {
    let mut reader = FrameReader::default();
    let mut buffer = [0u8; 16384];
    loop {
        let count = input.read(&mut buffer).await?;
        if count == 0 {
            return Ok(());
        }
        for message in reader.feed(&buffer[..count])? {
            let mut reply = error.json();
            reply["id"] = message.get("id").cloned().unwrap_or(Value::Null);
            reply["ok"] = json!(false);
            output.write_all(&encode(&reply)?).await?;
            output.flush().await?;
        }
    }
}

pub async fn run(
    runtime: Runtime,
    input: impl AsyncRead + Unpin,
    output: impl AsyncWrite + Unpin + Send + 'static,
) -> Result<()> {
    let (tx, mut rx) = mpsc::channel::<Reply>(256);
    let chunked_responses = Arc::new(AtomicBool::new(false));
    let writer_chunks = chunked_responses.clone();
    let mut writer = tokio::spawn(async move {
        let mut output = output;
        while let Some(reply) = rx.recv().await {
            let chunked = writer_chunks.load(Ordering::Relaxed);
            let frames = match reply {
                Reply::Value(value) => response_frames(&value, chunked)?,
                Reply::Text { id, parts } => response_frames_parts(parts, id, chunked)?,
            };
            for frame in frames {
                output.write_all(&frame?).await?;
            }
            output.flush().await?;
        }
        Ok::<(), crate::Error>(())
    });
    let mut input = input;
    let mut reader = FrameReader::default();
    let mut buffer = [0u8; 16384];
    let root_cancel = CancellationToken::new();
    let mut active = HashMap::new();
    let mut agent_replies: HashMap<String, mpsc::Sender<Value>> = HashMap::new();
    let mut tasks = JoinSet::new();
    let mut failure = None;
    let mut writer_finished = false;
    loop {
        tokio::select! {
            result=&mut writer=>{
                writer_finished=true;
                failure=Some(match result {
                    Ok(Err(error))=>error,
                    Err(error)=>crate::Error::new(format!("Native writer failed: {error}")),
                    Ok(Ok(()))=>crate::Error::new("Native writer stopped unexpectedly"),
                });
                break;
            },
            read=input.read(&mut buffer)=>{
                let n=match read{Ok(0)=>break,Ok(n)=>n,Err(e)=>{failure=Some(e.into());break}};
                let messages=match reader.feed(&buffer[..n]){Ok(v)=>v,Err(e)=>{failure=Some(e);break}};
                for message in messages {
                    if message["type"]=="health" && message["capabilities"]["chunkedResponses"]==true {
                        chunked_responses.store(true,Ordering::Relaxed);
                    }
                    active.retain(|_,token:&mut CancellationToken|!token.is_cancelled());
                    agent_replies.retain(|_,sender|!sender.is_closed());
                    let id=message.get("id").cloned().unwrap_or(Value::Null);
                    let key=id.to_string();
                    if message["type"]=="stop"{
                        if let Some(token)=active.get(&key){CancellationToken::cancel(token);}
                        continue;
                    }
                    if message["type"]=="agentToolResult" {
                        if let Some(sender)=agent_replies.get(&key)
                            && sender.try_send(message).is_err()
                            && let Some(token)=active.get(&key) {
                            token.cancel();
                        }
                        continue;
                    }
                    let runtime=runtime.clone();
                    let tx=tx.clone();
                    if message["service"]=="agent" {
                        let token=root_cancel.child_token();
                        let cleanup=token.clone();
                        if let Some(previous)=active.insert(key.clone(),token.clone()){previous.cancel();}
                        let (reply_tx,reply_rx)=mpsc::channel(32);
                        agent_replies.insert(key,reply_tx);
                        tasks.spawn(async move {
                            let (event_tx,mut event_rx)=mpsc::channel(128);
                            let params=message.get("params").cloned().unwrap_or(json!({}));
                            let sensitive=runtime.agent_session_sensitive_values(&params);
                            let result=match sensitive {
                                Err(error)=>Err(error),
                                Ok(sensitive)=>{
                                    let run=runtime.agent.run(&params,event_tx,reply_rx,token,sensitive);
                                    tokio::pin!(run);
                                    loop {
                                        tokio::select! {
                                            result=&mut run=>break result,
                                            event=event_rx.recv()=>if let Some(event)=event
                                                && tx.send(json!({"id":id,"type":"data","event":event}).into()).await.is_err(){cleanup.cancel();return;}
                                        }
                                    }
                                }
                            };
                            while let Ok(event)=event_rx.try_recv(){let _=tx.send(json!({"id":id,"type":"data","event":event}).into()).await;}
                            let info=match result {Ok(info)=>info,Err(error)=>error.json()};
                            let _=tx.send(json!({"id":id,"type":"exit","info":info}).into()).await;
                            cleanup.cancel();
                        });
                    } else if message["type"]=="clipboardRead" {
                        tasks.spawn(async move {
                            let reply=clipboard_reply(id,read_clipboard().await);
                            let _=tx.send(reply.into()).await;
                        });
                    }else if message["type"]!="discover" && runtime.registry.is_streaming(message["service"].as_str().unwrap_or_default()) {
                        let token=root_cancel.child_token();
                        let cleanup=token.clone();
                        if let Some(previous)=active.insert(key,token.clone()){previous.cancel();}
                        tasks.spawn(async move {
                            let (event_tx,mut event_rx)=mpsc::channel(128);
                            let run=runtime.execute_raw(&message,token,Some(event_tx));
                            tokio::pin!(run);
                            let result=loop {
                                tokio::select! {
                                    event=event_rx.recv()=>if let Some(event)=event {if tx.send(json!({"id":id,"type":"data","event":event}).into()).await.is_err(){return}}else{break run.await},
                                    result=&mut run=>break result,
                                }
                            };
                            while let Ok(event)=event_rx.try_recv(){let _=tx.send(json!({"id":id,"type":"data","event":event}).into()).await;}
                            let reply=match result {
                                Ok(executed)=>Reply::exit(&id,&executed),
                                Err(e)=>error_reply(&e,id).into(),
                            };
                            let _=tx.send(reply).await;
                            cleanup.cancel();
                        });
                    }else{
                        let token=root_cancel.child_token();
                        let cleanup=token.clone();
                        if let Some(previous)=active.insert(key,token.clone()){previous.cancel();}
                        tasks.spawn(async move{
                            let reply=runtime.dispatch_reply(&message,token).await;
                            let _=tx.send(reply).await;
                            cleanup.cancel();
                        });
                    }
                }
            }
            _=tasks.join_next(),if !tasks.is_empty()=>{active.retain(|_,token|!token.is_cancelled());},
        }
    }
    root_cancel.cancel();
    // Drop pending network requests immediately, but allow terminal children to reap.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while tasks.join_next().await.is_some() {}
    })
    .await;
    tasks.shutdown().await;
    runtime.agent.shutdown().await;
    drop(tx);
    if !writer_finished {
        writer
            .await
            .map_err(|e| crate::Error::new(e.to_string()))??;
    }
    if let Some(error) = failure {
        return Err(error);
    }
    Ok(())
}

fn clipboard_reply(id: Value, result: Result<String>) -> Value {
    match result {
        Ok(text) => json!({"id":id,"ok":true,"result":{"text":text}}),
        Err(error) => {
            let mut reply = error.json();
            reply["id"] = id;
            reply["ok"] = json!(false);
            reply
        }
    }
}

async fn read_clipboard() -> Result<String> {
    #[cfg(target_os = "macos")]
    {
        let mut command = tokio::process::Command::new("/usr/bin/pbpaste");
        command.args(["-Prefer", "txt"]);
        read_clipboard_command(command).await
    }
    #[cfg(not(target_os = "macos"))]
    Err(crate::Error::new(
        "Native clipboard import is currently supported only on macOS",
    ))
}

#[cfg(target_os = "macos")]
async fn read_clipboard_command(mut command: tokio::process::Command) -> Result<String> {
    // Chrome caps host replies at 1 MiB; JSON escaping can expand text sixfold.
    const MAX_TEXT: usize = 128 * 1024;
    let read = async {
        let mut child = command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| crate::Error::new(format!("Could not start clipboard reader: {e}")))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| crate::Error::new("Clipboard reader has no output pipe"))?;
        let mut bytes = Vec::new();
        stdout
            .take((MAX_TEXT + 1) as u64)
            .read_to_end(&mut bytes)
            .await?;
        if bytes.len() > MAX_TEXT {
            return Err(crate::Error::new("Clipboard text exceeds 128 KiB"));
        }
        let status = child.wait().await?;
        if !status.success() {
            return Err(crate::Error::new(format!(
                "Clipboard reader exited with {status}"
            )));
        }
        String::from_utf8(bytes)
            .map_err(|_| crate::Error::new("Clipboard does not contain valid UTF-8 text"))
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), read)
        .await
        .map_err(|_| crate::Error::new("Clipboard read timed out"))?
}

#[cfg(all(test, target_os = "macos"))]
mod clipboard_tests {
    use super::*;
    use tokio::process::Command;

    #[tokio::test]
    async fn clipboard_text_and_protocol_preserve_unicode_and_newlines() {
        let text = "curl 'https://fixture.invalid/тест'\n";
        let mut command = Command::new("/usr/bin/printf");
        command.args(["%s", text]);
        let reply = clipboard_reply(json!(7), read_clipboard_command(command).await);
        assert_eq!(reply, json!({"id":7,"ok":true,"result":{"text":text}}));
    }

    #[tokio::test]
    async fn clipboard_reader_rejects_failure_invalid_text_and_oversize_output() {
        for (program, args, expected) in [
            ("/usr/bin/false", vec![], "exited with"),
            ("/usr/bin/printf", vec!["\\377"], "valid UTF-8"),
            ("/usr/bin/printf", vec!["%0131073d", "0"], "exceeds 128 KiB"),
            ("/not/a/clipboard/reader", vec![], "Could not start"),
        ] {
            let mut command = Command::new(program);
            command.args(args);
            let reply = clipboard_reply(json!(8), read_clipboard_command(command).await);
            assert_eq!(reply["id"], 8);
            assert_eq!(reply["ok"], false);
            assert!(reply["error"].as_str().unwrap().contains(expected));
            assert!(reply.get("result").is_none());
        }
    }

    #[tokio::test]
    async fn clipboard_reader_timeout_is_an_error() {
        let mut command = Command::new("/bin/sleep");
        command.arg("10");
        let error = read_clipboard_command(command).await.unwrap_err();
        assert_eq!(error.message, "Clipboard read timed out");
    }
}
