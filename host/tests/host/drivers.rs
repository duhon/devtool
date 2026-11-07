use axum::{
    Router,
    body::Body,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Redirect},
    routing::{any, get},
};
use serde_json::{Value, json};
use services_testkit::{DataDir, http::Server, native::Pipe};
use std::time::Duration;

async fn echo(method: axum::http::Method, headers: HeaderMap, body: String) -> Json {
    let auth = headers
        .get(header::AUTHORIZATION)
        .map(|v| v.to_str().unwrap().to_owned());
    axum::Json(json!({"method": method.as_str(), "auth": auth, "body": body}))
}
type Json = axum::Json<Value>;

async fn server() -> Server {
    Server::new(
        Router::new()
            .route("/echo", any(echo))
            .route("/see-other", any(|| async { Redirect::to("/echo") }))
            .route(
                "/moved",
                any(|| async {
                    (
                        StatusCode::TEMPORARY_REDIRECT,
                        [(header::LOCATION, "/echo")],
                    )
                }),
            )
            .route(
                "/away",
                any(|headers: HeaderMap| async move {
                    let host = headers[header::HOST]
                        .to_str()
                        .unwrap()
                        .replace("127.0.0.1", "localhost");
                    Redirect::temporary(&format!("http://{host}/echo"))
                }),
            )
            .route(
                "/refused",
                any(|| async { Redirect::temporary("http://127.0.0.1:1/x") }),
            )
            .route("/loop", get(|| async { Redirect::temporary("/loop") }))
            .route(
                "/stream",
                get(|| async {
                    let crab = "🦀".as_bytes().to_vec();
                    let parts: Vec<Result<Vec<u8>, std::io::Error>> = vec![
                        Ok(b"a".to_vec()),
                        Ok(crab[..2].to_vec()),
                        Ok(crab[2..].to_vec()),
                    ];
                    Body::from_stream(futures_util::stream::iter(parts)).into_response()
                }),
            )
            .route(
                "/slow",
                get(|| async {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    "late"
                }),
            ),
    )
    .await
}

fn call(id: u32, op: &str, request: Value) -> Value {
    json!({"id": id, "service": "drivers", "driver": "http", "op": op, "request": request})
}

#[tokio::test]
async fn http_call_follows_redirects_itself_and_reports_each_hop() {
    let (data, server) = (DataDir::new(), server().await);
    let runtime = data.runtime();
    let health = runtime.dispatch(&json!({"id": 0, "type": "health"})).await;
    assert_eq!(health["result"]["drivers"]["http"], "1");

    let request = json!({"method": "POST", "url": format!("{}/see-other", server.url),
        "headers": [["Authorization", "Bearer x"]], "body": "hi", "text": true});
    let reply = runtime.dispatch(&call(1, "call", request.clone())).await;
    let result = &reply["result"];
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(result["status"], 200);
    assert_eq!(result["redirects"][0]["status"], 303);
    assert!(result["url"].as_str().unwrap().ends_with("/echo"));
    let body: Value = serde_json::from_str(result["body"].as_str().unwrap()).unwrap();
    assert_eq!(
        body,
        json!({"method": "GET", "auth": "Bearer x", "body": ""})
    );
    assert_eq!(result["remote"]["address"], "127.0.0.1");
    assert!(
        result["timing"]["totalUs"].as_u64().unwrap()
            >= result["timing"]["firstByteUs"].as_u64().unwrap()
    );

    let mut kept = request.clone();
    kept["url"] = json!(format!("{}/moved", server.url));
    let reply = runtime.dispatch(&call(2, "call", kept)).await;
    let body: Value = serde_json::from_str(reply["result"]["body"].as_str().unwrap()).unwrap();
    assert_eq!(body["method"], "POST");
    assert_eq!(body["body"], "hi");

    let mut none = request.clone();
    none["redirects"] = json!(0);
    let reply = runtime.dispatch(&call(3, "call", none)).await;
    assert_eq!(reply["result"]["status"], 303);
    assert_eq!(reply["result"]["redirects"], json!([]));

    let looping = json!({"method": "GET", "url": format!("{}/loop", server.url), "redirects": 3});
    let reply = runtime.dispatch(&call(4, "call", looping)).await;
    assert_eq!(reply["result"]["status"], 307);
    assert_eq!(reply["result"]["redirects"].as_array().unwrap().len(), 3);
    assert!(reply["result"]["bodyBase64"].is_string());
}

#[tokio::test]
async fn http_failures_name_their_stage() {
    let (data, server) = (DataDir::new(), server().await);
    let runtime = data.runtime();
    let away = json!({"method": "GET", "url": format!("{}/away", server.url),
        "headers": [["authorization", "secret"]], "text": true});
    let reply = runtime.dispatch(&call(0, "call", away)).await;
    let body: Value = serde_json::from_str(reply["result"]["body"].as_str().unwrap()).unwrap();
    assert_eq!(
        body["auth"],
        Value::Null,
        "credentials do not follow a redirect to another origin"
    );

    let refused = json!({"method": "GET", "url": format!("{}/refused", server.url)});
    let reply = runtime.dispatch(&call(1, "call", refused)).await;
    assert_eq!(reply["ok"], false);
    assert_eq!(reply["failure"]["stage"], "connect", "{reply}");
    assert!(reply["failure"]["causes"].as_array().unwrap().len() > 1);

    let slow = json!({"method": "GET", "url": format!("{}/slow", server.url), "timeoutMs": 100});
    let reply = runtime.dispatch(&call(2, "call", slow)).await;
    assert_eq!(reply["failure"]["stage"], "timeout", "{reply}");

    let bad = json!({"method": "GET", "url": "not a url"});
    let reply = runtime.dispatch(&call(3, "call", bad)).await;
    assert_eq!(reply["failure"]["stage"], "send");
    let reply = runtime
        .dispatch(&json!({"id": 4, "service": "drivers", "driver": "ftp", "request": {}}))
        .await;
    assert_eq!(reply["error"], "Unknown driver \"ftp\"");
}

#[tokio::test]
async fn http_stream_sends_head_then_whole_characters_and_stops_on_cancel() {
    let (data, server) = (DataDir::new(), server().await);
    let mut pipe = Pipe::start(data.runtime());
    let stream = json!({"method": "GET", "url": format!("{}/stream", server.url), "text": true});
    pipe.send(&call(1, "stream", stream)).await;
    let mut text = String::new();
    let head = pipe.read().await;
    assert_eq!(head["event"]["head"]["status"], 200, "{head}");
    loop {
        let message = pipe.read().await;
        if message["type"] == "exit" {
            assert!(message["info"]["timing"]["totalUs"].is_u64());
            break;
        }
        text += message["event"]["chunk"].as_str().unwrap();
    }
    assert_eq!(text, "a🦀");

    let slow = json!({"method": "GET", "url": format!("{}/slow", server.url)});
    pipe.send(&call(2, "stream", slow)).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    pipe.send(&json!({"id": 2, "type": "stop"})).await;
    let reply = tokio::time::timeout(Duration::from_secs(5), pipe.read())
        .await
        .unwrap();
    assert_eq!(reply["failure"]["stage"], "cancelled", "{reply}");
}
