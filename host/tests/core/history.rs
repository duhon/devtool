use serde_json::{Value, json};
use services_native::{
    Runtime,
    adapters::{
        framing::{FrameReader, encode},
        native,
    },
};
use services_testkit::{DataDir, http::Server};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const CHECK: &str = "019a1234-5678-7890-abcd-012345678901";

const OTHER: &str = "019a1234-5678-7890-abcd-012345678902";

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

/// Records a run the way the App does: `vars.resolve` gives the check's masked source and
/// title, then `history.insert` saves them with the run's response.
async fn record(runtime: &Runtime, source: &str, status: &str, response: &str) -> Value {
    let resolved = runtime
        .dispatch(&json!({"service":"vars","op":"resolve","value":"",
            "source":source,"title":"#check immutable"}))
        .await;
    assert_eq!(resolved["ok"], true, "{resolved}");
    let outcome = match status {
        "success" => "OK",
        "stopped" => "Stopped",
        _ => "ERROR",
    };
    let source = resolved["result"]["source"].as_str().unwrap();
    runtime
        .dispatch(
            &json!({"service":"history","op":"insert","checkId":CHECK,"sessionId":OTHER,
            "title":resolved["result"]["title"],"module":"terminal","started":1000,"ended":1002,
            "status":status,"text":format!("{source}\n\n#responce {outcome}\n{response}")}),
        )
        .await
}

/// Runs `command` with the process driver and returns its stdout.
async fn shell(runtime: &Runtime, command: &str) -> String {
    let reply = runtime
        .dispatch(&json!({"service":"drivers","driver":"process","op":"call",
            "request":{"program":"sh","args":["-c",command],"text":true}}))
        .await;
    assert_eq!(reply["ok"], true, "{reply}");
    reply["result"]["stdout"].as_str().unwrap_or("").to_owned()
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
        "#check ordinary fixture\n#endpoint\nPOST {url}\n#headers\nAuthorization: ****** {{{{:ALIAS}}}} {{{{var:SOURCE_ALIAS}}}}"
    );
    let request = json!({"method":"POST","url":url,
        "headers":[["Authorization","******"]],"body":"{{var:VALUE}} {{:ALIAS}}","text":true});
    let resolved = runtime
        .dispatch(&json!({"service":"vars","op":"resolve","value":request,
            "source":source,"title":"#check {{var:VALUE}}"}))
        .await;
    assert_eq!(resolved["ok"], true, "{resolved}");
    let resolved = &resolved["result"];
    assert_eq!(
        resolved["value"]["body"],
        "companion-fixture-only used-private-token"
    );
    runtime
        .dictionary_store
        .dispatch(&json!({"op":"save","name":"quoting","variables":{"TEXT":"a \"quoted\"\nline"}}))
        .unwrap();
    let json_mode = runtime
        .dispatch(&json!({"service":"vars","op":"resolve","mode":"json",
            "value":"{\"query\":\"{{quoting:TEXT}}\"}"}))
        .await;
    assert_eq!(
        json_mode["result"]["value"], "{\"query\":\"a \\\"quoted\\\"\\nline\"}",
        "{json_mode}"
    );
    let called = runtime
        .dispatch(
            &json!({"service":"drivers","driver":"http","op":"call","request":resolved["value"]}),
        )
        .await;
    let response = called["result"]["body"].as_str().unwrap();
    let saved = runtime
        .dispatch(&json!({"service":"history","op":"insert","checkId":CHECK,"sessionId":OTHER,
            "title":resolved["title"],"module":"rest","started":1,"ended":3,"status":"success",
            "text":format!("{}\n\n#responce OK\n{response}", resolved["source"].as_str().unwrap())}))
        .await;
    assert_eq!(saved["result"]["saved"], true, "{saved}");
    let text = get(&runtime, &saved["result"]["id"]).await;
    assert!(text.contains("/history-fixture"), "{text}");
    let (request, response) = text.split_once("\n\n#responce OK\n").unwrap();
    assert!(!request.contains("used-private-token"), "{request}");
    assert!(!request.contains("source-private-token"), "{request}");
    assert!(!request.contains("raw-auth-secret"), "{request}");
    assert!(request.contains("[redacted]"), "{request}");
    assert_eq!(
        response,
        "companion-fixture-only used-private-token source-private-token raw-auth-secret"
    );
    let listed = list(&runtime, CHECK).await;
    assert_eq!(listed["entries"][0]["service"], "rest", "{listed}");
    assert_eq!(listed["entries"][0]["durationMs"], 2, "{listed}");
    let saved = record(
        &runtime,
        "#check environment fixture\n#env\n{\"VALUE\":\"{{var:VALUE}}\"}\n#command\nprintf \"$VALUE\"",
        "success",
        "",
    )
    .await;
    let text = get(&runtime, &saved["result"]["id"]).await;
    assert!(text.contains("companion-fixture-only"), "{text}");
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
        let resolved = runtime
            .dispatch(&json!({"service":"vars","op":"resolve",
                "value":"printf '%s %s %s' '{{var:VALUE}}' '{{shared:ORDINARY}}' '{{var:ALIAS}}'"}))
            .await;
        let output = shell(&runtime, resolved["result"]["value"].as_str().unwrap()).await;
        let saved = record(&runtime, source, "success", &output).await;
        assert_eq!(saved["result"]["saved"], true, "{saved}");
    }
    drop(runtime);
    let runtime = data.runtime();
    let entries = list(&runtime, CHECK).await;
    assert_eq!(entries["total"], 3);
    assert_eq!(list(&runtime, OTHER).await["total"], 0);
    for (index, entry) in entries["entries"].as_array().unwrap().iter().enumerate() {
        let text = get(&runtime, &entry["id"]).await;
        assert!(text.contains(&format!("edit-{}", 3 - index)), "{text}");
        assert!(text.contains("#env") && text.contains("#command"));
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
async fn failed_and_stopped_runs_keep_their_status_and_inserts_cross_native_messaging() {
    let data = DataDir::new();
    let runtime = data.runtime();
    let saved = record(
        &runtime,
        "#check fail\n#command\nexit 7",
        "error",
        "out\nerr",
    )
    .await;
    let text = get(&runtime, &saved["result"]["id"]).await;
    assert!(text.contains("#responce ERROR\nout\nerr"), "{text}");
    assert_eq!(list(&runtime, CHECK).await["entries"][0]["status"], "error");
    let saved = record(
        &runtime,
        "#check stopped\n#command\nsleep 30",
        "stopped",
        "ready",
    )
    .await;
    assert!(
        get(&runtime, &saved["result"]["id"])
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
    input
        .write_all(
            &encode(&json!({"id":17,"service":"history","op":"insert","checkId":CHECK,
                "sessionId":OTHER,"title":"#check native","module":"terminal","started":1000,
                "ended":1001,"status":"success","text":"#check native\n\n#responce OK\nnative-stream"}))
            .unwrap(),
        )
        .await
        .unwrap();
    let mut frames = FrameReader::default();
    let mut buffer = [0; 8192];
    loop {
        let n = tokio::time::timeout(std::time::Duration::from_secs(10), output.read(&mut buffer))
            .await
            .unwrap()
            .unwrap();
        assert!(n > 0);
        if let Some(reply) = frames.feed(&buffer[..n]).unwrap().into_iter().next() {
            assert_eq!(reply["id"], 17);
            assert_eq!(reply["result"]["saved"], true, "{reply}");
            assert!(
                get(&runtime, &reply["result"]["id"])
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
async fn broken_history_fails_the_insert_and_is_left_alone() {
    let data = DataDir::new();
    let runtime = data.runtime();
    assert!(!data.path().join(".console-history.sqlite3").exists());
    std::fs::write(data.path().join(".console-history.sqlite3"), "not sqlite").unwrap();
    let saved = record(&runtime, "#check storage failure", "success", "x").await;
    assert_eq!(saved["ok"], false, "{saved}");
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
    let saved = record(&runtime, "#check unavailable", "success", "still-real").await;
    assert_eq!(saved["ok"], false, "{saved}");
}
