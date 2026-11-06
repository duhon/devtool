use serde_json::{Value, json};
use services_native::{
    Runtime,
    adapters::{
        framing::{FrameReader, encode},
        native,
    },
};
use services_testkit::{DataDir, http::Server};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::mpsc,
};
use tokio_util::sync::CancellationToken;

const CHECK: &str = "019a1234-5678-7890-abcd-012345678901";

const OTHER: &str = "019a1234-5678-7890-abcd-012345678902";

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
async fn only_used_secrets_and_authorization_are_masked() {
    let data = DataDir::new();
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
async fn http_response_bytes_are_kept_and_only_the_request_is_masked() {
    const BODY: &str = "\r\n{\n  \"data\": {\"products\": []}, \"debug\": {\"keep\": true}, \"token\": \"response-secret\"\n}\t\n";
    let data = DataDir::new();
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
async fn executions_are_immutable_masked_and_survive_restart() {
    let data = DataDir::new();
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
async fn terminal_error_stop_and_stream_capture_stdout_and_stderr() {
    let data = DataDir::new();
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
async fn broken_history_never_fails_or_repeats_the_execution() {
    let data = DataDir::new();
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
    assert!(!data.path().join(".console-history.sqlite3").exists());
    std::fs::write(data.path().join(".console-history.sqlite3"), "not sqlite").unwrap();
    let marker = data.path().join("executed");
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
        std::fs::read_to_string(data.path().join(".console-history.sqlite3")).unwrap(),
        "not sqlite"
    );
    assert_eq!(
        runtime
            .dispatch(&json!({"service":"history","op":"list","checkId":CHECK}))
            .await["ok"],
        false
    );
    std::fs::remove_file(data.path().join(".console-history.sqlite3")).unwrap();
    std::fs::create_dir(data.path().join(".console-history.sqlite3")).unwrap();
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
