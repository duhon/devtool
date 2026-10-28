use serde_json::{Value, json};
use services_native::{
    Runtime,
    adapters::{
        framing::{FrameReader, encode},
        native, web,
    },
};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::mpsc,
};
use tokio_util::sync::CancellationToken;

const CHECK: &str = "019a1234-5678-7890-abcd-012345678901";
const OTHER: &str = "019a1234-5678-7890-abcd-012345678902";
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Data(PathBuf);
impl Data {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/history-integration")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn runtime(&self) -> Runtime {
        Runtime::with_data_dir(&self.0).unwrap()
    }
}
impl Drop for Data {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn context(source: &str) -> Value {
    json!({"checkId":CHECK,"sessionId":OTHER,"title":"#check immutable","source":source})
}
async fn list(runtime: &Runtime, check: &str) -> Value {
    let reply = runtime
        .dispatch(&json!({"service":"history","op":"list","checkId":check}))
        .await;
    assert_eq!(reply["ok"], true, "{reply}");
    reply["result"].clone()
}
async fn get(runtime: &Runtime, id: &Value) -> String {
    let reply = runtime
        .dispatch(&json!({"service":"history","op":"get","id":id}))
        .await;
    assert_eq!(reply["ok"], true, "{reply}");
    reply["result"]["text"].as_str().unwrap().into()
}
async fn terminal(
    runtime: &Runtime,
    command: &str,
    context: Value,
    cancel: CancellationToken,
) -> Value {
    let (tx, mut rx) = mpsc::channel(128);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let reply = runtime
        .execute(
            &json!({"service":"terminal","history":context,
        "params":{"endpoint":"sh -c","command":command}}),
            cancel,
            Some(tx),
        )
        .await
        .unwrap();
    drain.await.unwrap();
    reply
}
#[tokio::test]
async fn unused_profile_secrets_do_not_mask_ordinary_values_but_used_aliases_and_authorization_do()
{
    let data = Data::new();
    let runtime = data.runtime();
    runtime
        .dictionary_store
        .dispatch(&json!({
            "op":"save","name":"unused","variables":{"password":"fixture"}
        }))
        .unwrap();
    runtime.dictionary_store.dispatch(&json!({
        "op":"save","name":"credentials","variables":{
            "API_TOKEN":"used-private-token","FIRST":"{{credentials:API_TOKEN}}","SECOND":"{{credentials:FIRST}}",
            "SOURCE_TOKEN":"source-private-token"
        }
    })).unwrap();
    runtime
        .variable_store
        .dispatch(&json!({
            "op":"save","name":"var","variables":{
                "VALUE":"companion-fixture-only","ALIAS":"{{credentials:SECOND}}",
                "SOURCE_ALIAS":"{{credentials:SOURCE_TOKEN}}","UNUSED_PASSWORD":"fixture"
            }
        }))
        .unwrap();
    let fixture = Server::new(axum::Router::new().fallback(|| async {
        "companion-fixture-only used-private-token source-private-token raw-auth-secret"
    }))
    .await;
    let url = format!("{}/history-fixture", fixture.url);
    let source = format!(
        "#check ordinary fixture\n#url\n{url}\n#headers\nAuthorization: Bearer raw-auth-secret\n#body\n{{{{var:VALUE}}}} {{{{:ALIAS}}}} {{{{var:SOURCE_ALIAS}}}}"
    );
    let reply = runtime
        .dispatch(&json!({
            "service":"rest","history":context(&source),"params":{
                "method":"POST","url":url,
                "headers":{"Authorization":"Bearer raw-auth-secret"},
                "body":"{{var:VALUE}} {{:ALIAS}}"
            }
        }))
        .await;
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["history"]["saved"], true, "{reply}");
    let text = get(&runtime, &reply["history"]["id"]).await;
    assert!(text.contains("/history-fixture"), "{text}");
    assert!(text.contains("companion-fixture-only"), "{text}");
    let (request, response) = text.split_once("\n\n#responce OK\n").unwrap();
    assert!(!request.contains("used-private-token"), "{request}");
    assert!(!request.contains("source-private-token"), "{request}");
    assert!(!request.contains("raw-auth-secret"), "{request}");
    assert!(request.contains("[redacted]"), "{request}");
    assert_eq!(
        response,
        "companion-fixture-only used-private-token source-private-token raw-auth-secret"
    );
    let (tx, mut rx) = mpsc::channel(128);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let reply = runtime.execute(&json!({
        "service":"terminal",
        "history":context("#check environment fixture\n#env\n{\"VALUE\":\"{{var:VALUE}}\"}\n#command\nprintf \"$VALUE\""),
        "params":{"endpoint":"sh -c","env":{"VALUE":"{{var:VALUE}}"},"command":"printf \"$VALUE\""}
    }),CancellationToken::new(),Some(tx)).await.unwrap();
    drain.await.unwrap();
    let text = get(&runtime, &reply["history"]["id"]).await;
    assert!(text.contains("companion-fixture-only"), "{text}");
}
#[tokio::test]
async fn history_preserves_http_response_bytes_and_only_masks_the_request() {
    const BODY: &str = "\r\n{\n  \"data\": {\"products\": []}, \"debug\": {\"keep\": true}, \"token\": \"response-secret\"\n}\t\n";
    let data = Data::new();
    let runtime = data.runtime();
    let fixture = Server::new(
        axum::Router::new()
            .route(
                "/error",
                axum::routing::get(|| async { (axum::http::StatusCode::BAD_REQUEST, BODY) }),
            )
            .fallback(|| async { BODY }),
    )
    .await;
    for (path, status) in [("/", "OK"), ("/error", "WARNING")] {
        let url = format!("{}{path}", fixture.url);
        let source =
            format!("#check verbatim\n#endpoint\n{url}\n#headers\nX-Api-Key: response-secret");
        let reply = runtime
            .dispatch(&json!({
                "service":"rest","history":context(&source),
                "params":{"method":"GET","url":url,"headers":{"X-Api-Key":"response-secret"}}
            }))
            .await;
        assert_eq!(reply["history"]["saved"], true, "{reply}");
        assert_eq!(reply["result"]["body"], BODY, "{reply}");
        assert!(reply["result"]["debug"].is_object(), "{reply}");
        let text = get(&runtime, &reply["history"]["id"]).await;
        let marker = format!("\n\n#responce {status}\n");
        let (request, response) = text.split_once(&marker).unwrap();
        assert!(!request.contains("response-secret"), "{request}");
        assert!(!request.contains("#effective-parameters"), "{request}");
        assert_eq!(response.as_bytes(), BODY.as_bytes());
    }
}
#[tokio::test]
async fn three_immutable_executions_resolve_ordinary_variables_mask_secrets_and_survive_restart() {
    let data = Data::new();
    let runtime = data.runtime();
    runtime
        .dictionary_store
        .dispatch(
            &json!({"op":"save","name":"shared","variables":{"ORDINARY":"dictionary-visible"}}),
        )
        .unwrap();
    for n in 1..=3 {
        runtime.variable_store.dispatch(&json!({"op":"save","name":"var","variables":{
            "VALUE":format!("edit-{n}"),"API_TOKEN":"private-variable","ALIAS":"{{var:API_TOKEN}}"
        }})).unwrap();
        let source = "#check immutable\n#endpoint\nsh -c\n#env\n{\"VALUE\":\"{{var:VALUE}}\"}\n#command\nprintf '%s %s %s' '{{var:VALUE}}' '{{shared:ORDINARY}}' '{{var:ALIAS}}'";
        let result = terminal(
            &runtime,
            "printf '%s %s %s' '{{var:VALUE}}' '{{shared:ORDINARY}}' '{{var:ALIAS}}'",
            context(source),
            CancellationToken::new(),
        )
        .await;
        assert_eq!(result["code"], 0);
        assert_eq!(result["history"]["saved"], true, "{result}");
    }
    drop(runtime);
    let runtime = data.runtime();
    let entries = list(&runtime, CHECK).await;
    assert_eq!(entries["total"], 3);
    assert_eq!(list(&runtime, OTHER).await["total"], 0);
    for (index, entry) in entries["entries"].as_array().unwrap().iter().enumerate() {
        let text = get(&runtime, &entry["id"]).await;
        assert!(text.contains(&format!("edit-{}", 3 - index)), "{text}");
        assert!(
            text.contains("#env") && text.contains("#command") && text.contains("#terminal-output")
        );
        assert!(text.contains("dictionary-visible"), "{text}");
        let (request, response) = text.split_once("\n\n#responce OK\n").unwrap();
        assert!(!request.contains("private-variable"), "{request}");
        assert!(response.contains("private-variable"), "{response}");
        assert!(!text.contains("{{var:") && !text.contains("{{shared:"));
        assert_eq!(entry["status"], "success");
        assert!(entry["endedAt"].as_u64() >= entry["startedAt"].as_u64());
    }
    let first = runtime
        .dispatch(&json!({"service":"history","op":"list","checkId":CHECK,"limit":2}))
        .await["result"]
        .clone();
    assert_eq!(first["entries"].as_array().unwrap().len(), 2);
    let next=runtime.dispatch(&json!({"service":"history","op":"list","checkId":CHECK,"before":first["nextBefore"],"limit":2})).await["result"].clone();
    assert_eq!(next["entries"].as_array().unwrap().len(), 1);
    assert!(next["nextBefore"].is_null());
}
#[tokio::test]
async fn terminal_error_stop_and_native_stream_capture_stdout_stderr() {
    let data = Data::new();
    let runtime = data.runtime();
    let result = terminal(
        &runtime,
        "printf out; printf err >&2; exit 7",
        context("#check fail\n#command\nexit 7"),
        CancellationToken::new(),
    )
    .await;
    let text = get(&runtime, &result["history"]["id"]).await;
    assert!(
        text.contains("stdout")
            && text.contains("stderr")
            && text.contains("out")
            && text.contains("err")
    );
    assert_eq!(list(&runtime, CHECK).await["entries"][0]["status"], "error");
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let stopping = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        token.cancel();
    });
    let result = terminal(
        &runtime,
        "printf ready; sleep 30",
        context("#check stopped\n#command\nsleep 30"),
        cancel,
    )
    .await;
    stopping.await.unwrap();
    assert_eq!(result["cancelled"], true);
    assert!(
        get(&runtime, &result["history"]["id"])
            .await
            .contains("ready")
    );
    assert_eq!(
        list(&runtime, CHECK).await["entries"][0]["status"],
        "stopped"
    );
    let (mut input, host_input) = tokio::io::duplex(65536);
    let (host_output, mut output) = tokio::io::duplex(65536);
    let host = tokio::spawn(native::run(runtime.clone(), host_input, host_output));
    input.write_all(&encode(&json!({"id":17,"service":"terminal","history":context("#check native\n#command\nprintf native-stream"),
        "params":{"endpoint":"sh -c","command":"printf native-stream"}})).unwrap()).await.unwrap();
    let mut frames = FrameReader::default();
    let mut buffer = [0; 8192];
    loop {
        let n = tokio::time::timeout(std::time::Duration::from_secs(10), output.read(&mut buffer))
            .await
            .unwrap()
            .unwrap();
        assert!(n > 0);
        if let Some(exit) = frames
            .feed(&buffer[..n])
            .unwrap()
            .into_iter()
            .find(|v| v["type"] == "exit")
        {
            assert_eq!(exit["info"]["history"]["saved"], true);
            assert!(
                get(&runtime, &exit["info"]["history"]["id"])
                    .await
                    .contains("native-stream")
            );
            break;
        }
    }
    drop(input);
    host.await.unwrap().unwrap();
}
#[tokio::test]
async fn unavailable_and_corrupt_history_do_not_fail_or_repeat_real_execution_and_legacy_is_not_recorded()
 {
    let data = Data::new();
    let runtime = data.runtime();
    let mut message =
        json!({"service":"terminal","params":{"endpoint":"sh -c","command":"printf legacy"}});
    let (tx, mut rx) = mpsc::channel(128);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    assert!(
        runtime
            .execute(&message, CancellationToken::new(), Some(tx))
            .await
            .unwrap()
            .get("history")
            .is_none()
    );
    drain.await.unwrap();
    assert!(!data.0.join(".console-history.sqlite3").exists());
    std::fs::write(data.0.join(".console-history.sqlite3"), "not sqlite").unwrap();
    let marker = data.0.join("executed");
    message["params"]["command"] = json!(format!("printf x >> '{}'", marker.display()));
    message["history"] = context("#check storage failure");
    let (tx, mut rx) = mpsc::channel(128);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let result = runtime
        .execute(&message, CancellationToken::new(), Some(tx))
        .await
        .unwrap();
    drain.await.unwrap();
    assert_eq!(result["code"], 0);
    assert_eq!(result["history"]["saved"], false);
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "x");
    assert_eq!(
        std::fs::read_to_string(data.0.join(".console-history.sqlite3")).unwrap(),
        "not sqlite"
    );
    assert_eq!(
        runtime
            .dispatch(&json!({"service":"history","op":"list","checkId":CHECK}))
            .await["ok"],
        false
    );
    std::fs::remove_file(data.0.join(".console-history.sqlite3")).unwrap();
    std::fs::create_dir(data.0.join(".console-history.sqlite3")).unwrap();
    let result = terminal(
        &runtime,
        "printf still-real",
        context("#check unavailable"),
        CancellationToken::new(),
    )
    .await;
    assert_eq!(result["code"], 0);
    assert_eq!(result["history"]["saved"], false);
}
struct Server {
    url: String,
    task: tokio::task::JoinHandle<()>,
}
impl Server {
    async fn new(router: axum::Router) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Self { url, task }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn websocket_send(stream: &mut tokio::net::TcpStream, value: &Value) {
    let payload = serde_json::to_vec(value).unwrap();
    let mut frame = vec![0x81];
    if payload.len() < 126 {
        frame.push(0x80 | payload.len() as u8);
    } else {
        frame.push(0x80 | 126);
        frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    }
    let mask = [1u8, 2, 3, 4];
    frame.extend_from_slice(&mask);
    frame.extend(
        payload
            .into_iter()
            .enumerate()
            .map(|(i, b)| b ^ mask[i % 4]),
    );
    stream.write_all(&frame).await.unwrap();
}
async fn websocket_read(stream: &mut tokio::net::TcpStream) -> Value {
    let mut head = [0; 2];
    stream.read_exact(&mut head).await.unwrap();
    let length = match head[1] & 0x7f {
        126 => {
            let mut size = [0; 2];
            stream.read_exact(&mut size).await.unwrap();
            u16::from_be_bytes(size) as usize
        }
        127 => {
            let mut size = [0; 8];
            stream.read_exact(&mut size).await.unwrap();
            u64::from_be_bytes(size) as usize
        }
        n => n as usize,
    };
    assert!(length < 1024 * 1024);
    let mut data = vec![0; length];
    stream.read_exact(&mut data).await.unwrap();
    serde_json::from_slice(&data).unwrap()
}
#[tokio::test]
async fn http_terminal_websocket_envelope_records_stream_and_stop() {
    let data = Data::new();
    let runtime = data.runtime();
    let server = Server::new(web::router(runtime.clone())).await;
    let address = server.url.strip_prefix("http://").unwrap();
    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
    stream.write_all(format!("GET /api/services/terminal/stream HTTP/1.1\r\nHost: {address}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n").as_bytes()).await.unwrap();
    let mut headers = Vec::new();
    while !headers.ends_with(b"\r\n\r\n") {
        headers.push(stream.read_u8().await.unwrap());
        assert!(headers.len() < 8192);
    }
    assert!(
        String::from_utf8(headers)
            .unwrap()
            .starts_with("HTTP/1.1 101")
    );
    websocket_send(&mut stream,&json!({"service":"terminal","params":{"endpoint":"sh -c","command":"printf websocket-out; printf websocket-err >&2; sleep 30"},
            "history":context("#check WebSocket\n#command\nprintf websocket-out; printf websocket-err >&2; sleep 30")})).await;
    let mut stopped = false;
    let exit = loop {
        let event = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            websocket_read(&mut stream),
        )
        .await
        .unwrap();
        if event["type"] == "exit" {
            break event;
        }
        if !stopped {
            websocket_send(&mut stream, &json!({"type":"stop"})).await;
            stopped = true;
        }
    };
    assert_eq!(exit["cancelled"], true, "{exit}");
    assert_eq!(exit["history"]["saved"], true, "{exit}");
    let text = get(&runtime, &exit["history"]["id"]).await;
    assert!(text.contains("websocket-out") && text.contains("websocket-err"));
    assert_eq!(
        list(&runtime, CHECK).await["entries"][0]["status"],
        "stopped"
    );
}
#[tokio::test]
async fn http_envelope_flat_service_history_routes_and_error_metadata_share_persistent_history() {
    let data = Data::new();
    let runtime = data.runtime();
    let fixture = Server::new(axum::Router::new().fallback(|| async { "echo-visible" })).await;
    let server = Server::new(web::router(runtime.clone())).await;
    let client = reqwest::Client::new();
    let params = json!({"method":"GET","url":fixture.url});
    for (path, body) in [
        (
            "/api/console",
            json!({"service":"rest","params":params,"history":context("#check HTTP\n#method\nGET\n#url\nresolved-url")}),
        ),
        (
            "/api/services/rest/request",
            json!({"method":"GET","url":fixture.url,"history":context("#check flat HTTP")}),
        ),
    ] {
        let reply = client
            .post(format!("{}{path}", server.url))
            .json(&body)
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap();
        assert_eq!(reply["history"]["saved"], true, "{reply}");
    }
    let entries = client
        .post(format!("{}/api/console/history", server.url))
        .json(&json!({"service":"history","op":"list","checkId":CHECK}))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(entries["total"], 2);
    assert_eq!(entries["entries"][0]["service"], "rest");
    assert!(
        get(&runtime, &entries["entries"][0]["id"])
            .await
            .contains("echo-visible")
    );
    let failed=runtime.dispatch(&json!({"id":44,"service":"rest","history":context("#check error"),"params":{"url":"not a URL"}})).await;
    assert_eq!(failed["ok"], false);
    assert_eq!(failed["history"]["saved"], true, "{failed}");
    assert_eq!(list(&runtime, CHECK).await["entries"][0]["status"], "error");
    assert!(
        get(&runtime, &failed["history"]["id"])
            .await
            .contains("#responce ERROR")
    );
    let mut other_context = context("#check other");
    other_context["checkId"] = json!(OTHER);
    terminal(
        &runtime,
        "printf other-service",
        other_context,
        CancellationToken::new(),
    )
    .await;
    assert_eq!(
        list(&runtime, OTHER).await["entries"][0]["service"],
        "terminal"
    );
    assert_eq!(list(&runtime, CHECK).await["total"], 3);
}
