use crate::core::error::text;
use crate::{Result, Runtime};
use axum::{
    Json, Router,
    extract::{
        Path, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
type Reply = (StatusCode, Json<Value>);
async fn session_context(
    State(runtime): State<Runtime>,
    headers: HeaderMap,
    Json(message): Json<Value>,
) -> Reply {
    if !agent_origin(&headers) {
        return reply(Err(crate::Error::new(
            "Session context requires the same loopback origin",
        )));
    }
    if !matches!(
        message["service"].as_str(),
        Some(
            "sessions"
                | "vars"
                | "dictionary"
                | "history"
                | "rest"
                | "graphql"
                | "grpc"
                | "mysql"
                | "splunk"
        )
    ) {
        return reply(Err(crate::Error::new("Invalid session context service")));
    }
    let owned = match owned_scope(&runtime, &headers).await {
        Ok(owned) => owned,
        Err(error) => return reply(Err(error)),
    };
    let value = if let Some((scope, id)) = owned {
        let value = runtime
            .dispatch_cancellable(&message, scope.request(id).await)
            .await;
        scope.finish(id).await;
        value
    } else {
        runtime.dispatch(&message).await
    };
    if value["ok"] == true {
        (StatusCode::OK, Json(value["result"].clone()))
    } else {
        (StatusCode::BAD_REQUEST, Json(value))
    }
}
fn reply(result: Result<Value>) -> Reply {
    match result {
        Ok(value) => (StatusCode::OK, Json(value)),
        Err(error) => (StatusCode::BAD_REQUEST, Json(error.json())),
    }
}
async fn console(
    State(runtime): State<Runtime>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Reply {
    let owned = match owned_scope(&runtime, &headers).await {
        Ok(owned) => owned,
        Err(error) => return reply(Err(error)),
    };
    let value = if let Some((scope, id)) = owned {
        let value = runtime
            .dispatch_cancellable(&body, scope.request(id).await)
            .await;
        scope.finish(id).await;
        value
    } else {
        runtime.dispatch(&body).await
    };
    runtime_reply(value)
}
fn runtime_reply(value: Value) -> Reply {
    if value["ok"] == true {
        (StatusCode::OK, Json(value["result"].clone()))
    } else {
        (StatusCode::BAD_REQUEST, Json(value))
    }
}
async fn history(State(runtime): State<Runtime>, Json(mut body): Json<Value>) -> Reply {
    body["service"] = json!("history");
    runtime_reply(runtime.dispatch(&body).await)
}
fn execution_message(service: &str, body: &Value) -> Value {
    let mut params = body.get("params").cloned().unwrap_or_else(|| body.clone());
    if let Some(fields) = params.as_object_mut() {
        fields.remove("history");
    }
    let mut message = json!({"service":service,"params":params});
    if let Some(context) = body.get("history") {
        message["history"] = context.clone();
    }
    message
}
async fn discover(
    State(runtime): State<Runtime>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    let owned = match owned_scope(&runtime, &headers).await {
        Ok(owned) => owned,
        Err(error) => return Json(error.json()),
    };
    let token = if let Some((scope, id)) = &owned {
        scope.request(*id).await
    } else {
        CancellationToken::new()
    };
    let result = runtime
        .registry
        .discover_cancellable(
            &text(&body, "service"),
            body.get("params").unwrap_or(&json!({})),
            token,
        )
        .await
        .unwrap_or_else(|e| e.json());
    if let Some((scope, id)) = owned {
        scope.finish(id).await;
    }
    Json(result)
}
async fn service(
    State(runtime): State<Runtime>,
    Path((service, endpoint)): Path<(String, String)>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Reply {
    let expected = match service.as_str() {
        "mysql" => "query",
        "splunk" => "search",
        "rest" | "graphql" | "grpc" => "request",
        _ => "",
    };
    if endpoint != expected || expected.is_empty() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"Unknown service endpoint"})),
        );
    }
    let owned = match owned_scope(&runtime, &headers).await {
        Ok(owned) => owned,
        Err(error) => return reply(Err(error)),
    };
    let message = execution_message(&service, &body);
    let value = if let Some((scope, id)) = owned {
        let result = runtime
            .dispatch_cancellable(&message, scope.request(id).await)
            .await;
        scope.finish(id).await;
        result
    } else {
        runtime.dispatch(&message).await
    };
    runtime_reply(value)
}
async fn owned_scope(
    runtime: &Runtime,
    headers: &HeaderMap,
) -> Result<Option<(std::sync::Arc<crate::core::operations::Scope>, u32)>> {
    match (
        headers.get("x-services-scope"),
        headers.get("x-services-request"),
    ) {
        (None, None) => Ok(None),
        (Some(scope), Some(id)) => {
            if !agent_origin(headers) {
                return Err(crate::Error::new(
                    "Console execution requires the same loopback origin",
                ));
            }
            let scope = runtime
                .operations
                .get(
                    scope
                        .to_str()
                        .map_err(|_| crate::Error::new("Invalid Console lifecycle header"))?,
                )
                .await?;
            let id = id
                .to_str()
                .ok()
                .and_then(|id| id.parse::<u32>().ok())
                .ok_or_else(|| crate::Error::new("Invalid Console request identity"))?;
            Ok(Some((scope, id)))
        }
        _ => Err(crate::Error::new("Incomplete Console lifecycle headers")),
    }
}
fn profile_dispatch(runtime: &Runtime, store: &str, message: &Value) -> Result<Value> {
    match store {
        "vars" => runtime.variable_store.dispatch(message),
        "dictionary" => runtime.dictionary_store.dispatch(message),
        _ => Err(crate::core::error::Error::new("Unknown profile service")),
    }
}
async fn profiles(State(runtime): State<Runtime>, Path(store): Path<String>) -> Reply {
    reply(profile_dispatch(&runtime, &store, &json!({"op":"list"})))
}
async fn save(
    State(runtime): State<Runtime>,
    Path((store, name)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Reply {
    reply(profile_dispatch(
        &runtime,
        &store,
        &json!({"op":"save","name":name,"variables":body.get("variables").cloned().unwrap_or(json!({}))}),
    ))
}
async fn delete(
    State(runtime): State<Runtime>,
    Path((store, name)): Path<(String, String)>,
) -> Reply {
    reply(profile_dispatch(
        &runtime,
        &store,
        &json!({"op":"delete","name":name}),
    ))
}
async fn rename(
    State(runtime): State<Runtime>,
    Path((store, name)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Reply {
    reply(profile_dispatch(
        &runtime,
        &store,
        &json!({"op":"rename","name":name,"newName":body["name"]}),
    ))
}
async fn order(
    State(runtime): State<Runtime>,
    Path(store): Path<String>,
    Json(body): Json<Value>,
) -> Reply {
    reply(profile_dispatch(
        &runtime,
        &store,
        &json!({"op":"reorder","order":body["order"]}),
    ))
}
async fn hints(State(runtime): State<Runtime>, Path(name): Path<String>) -> Json<Value> {
    Json(runtime.hints.get(&name))
}
async fn save_hints(
    State(runtime): State<Runtime>,
    Path(name): Path<String>,
    Json(body): Json<Value>,
) -> Reply {
    reply(runtime.hints.save(&name, &text(&body, "text")))
}
async fn configs() -> Json<Value> {
    Json(json!(services_contracts::service_configs()))
}
async fn templates(State(runtime): State<Runtime>) -> Reply {
    reply(runtime.templates.get())
}
async fn save_templates(State(runtime): State<Runtime>, Json(body): Json<Value>) -> Reply {
    reply(runtime.templates.save(&body))
}
async fn health() -> Json<Value> {
    Json(json!({"protocolVersion":services_contracts::PROTOCOL_VERSION}))
}
async fn upgrade(State(runtime): State<Runtime>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| terminal(runtime, socket))
}
async fn terminal(runtime: Runtime, socket: WebSocket) {
    let (mut sink, mut source) = socket.split();
    let Some(Ok(message)) = source.next().await else {
        return;
    };
    let params: Value = match serde_json::from_slice(&message.into_data()) {
        Ok(params) => params,
        Err(e) => {
            let _=sink.send(Message::Text(json!({"type":"exit","code":null,"error":"Start message must be valid JSON","debug":{"error":{"name":"Error","message":e.to_string()}}}).to_string().into())).await;
            return;
        }
    };
    let cancel = CancellationToken::new();
    let (tx, mut rx) = mpsc::channel(128);
    let message = execution_message("terminal", &params);
    let run = runtime.execute(&message, cancel.clone(), Some(tx));
    tokio::pin!(run);
    let mut connected = true;
    let result = loop {
        tokio::select! {
            result=&mut run=>break result,
            event=rx.recv()=>if let Some(event)=event
                && connected && sink.send(Message::Text(event.to_string().into())).await.is_err(){connected=false;cancel.cancel();},
            message=source.next(),if connected=>match message {
                Some(Ok(Message::Text(message)))=>{
                    if serde_json::from_str::<Value>(&message).ok().is_some_and(|v|v["type"]=="stop"){cancel.cancel();}
                }
                Some(Ok(Message::Close(_)))|Some(Err(_))|None=>{connected=false;cancel.cancel();}
                _=>{}
            },
        }
    };
    if connected {
        while let Ok(event) = rx.try_recv() {
            let _ = sink.send(Message::Text(event.to_string().into())).await;
        }
        let value = match result {
            Ok(mut info) => {
                info["type"] = json!("exit");
                info
            }
            Err(e) => {
                let mut info = e.json();
                info["type"] = json!("exit");
                info["code"] = Value::Null;
                info
            }
        };
        let _ = sink.send(Message::Text(value.to_string().into())).await;
        let _ = sink.close().await;
    }
}
fn agent_origin(headers: &HeaderMap) -> bool {
    let Some(host) = headers.get("host").and_then(|value| value.to_str().ok()) else {
        return false;
    };
    let Some(origin) = headers.get("origin").and_then(|value| value.to_str().ok()) else {
        return false;
    };
    let Ok(url) = url::Url::parse(origin) else {
        return false;
    };
    let loopback = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
    loopback
        && matches!(url.scheme(), "http" | "https")
        && origin == format!("{}://{host}", url.scheme())
        && url.username().is_empty()
        && url.password().is_none()
}
async fn upgrade_operations(
    State(runtime): State<Runtime>,
    Path(id): Path<String>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if !agent_origin(&headers) {
        return (
            StatusCode::FORBIDDEN,
            "Console lifecycle requires the same loopback origin",
        )
            .into_response();
    }
    ws.max_message_size(1024)
        .on_upgrade(move |mut socket| async move {
            let scope = match runtime.operations.open(&id).await {
                Ok(scope) => scope,
                Err(error) => {
                    if let Err(send_error) = socket
                        .send(Message::Text(
                            json!({"error":error.to_string()}).to_string().into(),
                        ))
                        .await
                    {
                        eprintln!(
                            "Console lifecycle rejection: {error}; response failed: {send_error}"
                        );
                    }
                    return;
                }
            };
            let _guard = scope.cancel.clone().drop_guard();
            let (mut sink, mut source) = socket.split();
            if sink
                .send(Message::Text(json!({"ready":true}).to_string().into()))
                .await
                .is_ok()
            {
                while let Some(Ok(message)) = source.next().await {
                    match message {
                        Message::Text(text) => {
                            let request = serde_json::from_str::<Value>(&text);
                            match request {
                                Ok(request) if request["type"] == "stop" => {
                                    if let Some(id) =
                                        request["id"].as_u64().and_then(|id| u32::try_from(id).ok())
                                    {
                                        scope.request(id).await.cancel();
                                    } else {
                                        break;
                                    }
                                }
                                Ok(request) if request["type"] == "stopAll" => {
                                    scope.cancel.cancel();
                                    break;
                                }
                                _ => break,
                            }
                        }
                        Message::Close(_) => break,
                        _ => {}
                    }
                }
            }
            runtime.operations.close(&id).await;
        })
}
async fn upgrade_agent(
    State(runtime): State<Runtime>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if !agent_origin(&headers) {
        return (
            StatusCode::FORBIDDEN,
            "Agent requires the same loopback origin",
        )
            .into_response();
    }
    ws.max_message_size(256 * 1024)
        .on_upgrade(move |socket| agent_socket(runtime, socket))
}
async fn agent_socket(runtime: Runtime, socket: WebSocket) {
    let (mut sink, mut source) = socket.split();
    let Some(Ok(Message::Text(first))) = source.next().await else {
        return;
    };
    let params = serde_json::from_str::<Value>(&first);
    let cancel = CancellationToken::new();
    let (events, mut event_rx) = mpsc::channel(128);
    let (replies, reply_rx) = mpsc::channel(32);
    let run = async {
        let params = params
            .map_err(|error| crate::Error::new(format!("Invalid Agent start message: {error}")))?;
        let sensitive = runtime.agent_session_sensitive_values(&params)?;
        runtime
            .agent
            .run(&params, events, reply_rx, cancel.clone(), sensitive)
            .await
    };
    tokio::pin!(run);
    let mut connected = true;
    let result = loop {
        tokio::select! {
            result = &mut run => break result,
            event = event_rx.recv() => if let Some(event)=event
                && sink.send(Message::Text(event.to_string().into())).await.is_err() {connected=false;cancel.cancel();},
            incoming = source.next(), if connected => match incoming {
                Some(Ok(Message::Text(message))) => match serde_json::from_str::<Value>(&message) {
                    Ok(value) if value["type"]=="stop" => cancel.cancel(),
                    Ok(value) if value["type"]=="agentToolResult" => if replies.try_send(value).is_err() {cancel.cancel();},
                    _ => {cancel.cancel();}
                },
                Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => {},
                _ => {connected=false;cancel.cancel();}
            }
        }
    };
    if connected {
        while let Ok(event) = event_rx.try_recv() {
            let _ = sink.send(Message::Text(event.to_string().into())).await;
        }
        let mut info = match result {
            Ok(value) => value,
            Err(error) => error.json(),
        };
        info["type"] = json!("exit");
        let _ = sink.send(Message::Text(info.to_string().into())).await;
        let _ = sink.close().await;
    }
}
fn static_ui_dir(
    explicit: Option<std::ffi::OsString>,
    executable: Option<std::path::PathBuf>,
    cwd: Option<std::path::PathBuf>,
) -> Option<std::path::PathBuf> {
    if let Some(path) = explicit {
        return Some(path.into());
    }
    if let Some(executable) = &executable
        && let Some(bin) = executable.parent()
        && bin.file_name().is_some_and(|name| name == "bin")
        && let Some(native) = bin.parent()
        && native.file_name().is_some_and(|name| name == "native")
        && let Some(application) = native.parent()
    {
        return Some(
            if application
                .file_name()
                .is_some_and(|name| name == "extension")
            {
                application.to_owned()
            } else {
                application.join("extension")
            },
        );
    }
    let executable_dir = executable.as_deref().and_then(std::path::Path::parent);
    for base in executable_dir.into_iter().chain(cwd.as_deref()) {
        for ancestor in base.ancestors() {
            let candidate = ancestor.join("adapters/extension/dist");
            if candidate.is_dir() {
                return Some(candidate);
            }
        }
    }
    None
}
pub fn router(runtime: Runtime) -> Router {
    router_with_ui(
        runtime,
        static_ui_dir(
            std::env::var_os("SERVICES_UI_DIR"),
            std::env::current_exe().ok(),
            std::env::current_dir().ok(),
        ),
    )
}
fn router_with_ui(runtime: Runtime, ui_dir: Option<std::path::PathBuf>) -> Router {
    let router = Router::new()
        .route("/api/console/session-context", post(session_context))
        .route("/api/console", post(console))
        .route("/api/console/history", post(history))
        .route("/api/console/discover", post(discover))
        .route("/api/services/terminal/stream", get(upgrade))
        .route("/api/agent/stream", get(upgrade_agent))
        .route("/api/operations/{id}", get(upgrade_operations))
        .route("/api/services/{service}/{endpoint}", post(service))
        .route("/api/services", get(configs))
        .route("/api/health", get(health))
        .route("/{store}/api/profiles", get(profiles))
        .route("/{store}/api/profiles/{name}", post(save).delete(delete))
        .route("/{store}/api/profiles/{name}/rename", post(rename))
        .route("/{store}/api/order", post(order))
        .route("/api/console/hints/{name}", get(hints).put(save_hints))
        .route("/api/console/templates", get(templates).put(save_templates))
        .layer(axum::extract::DefaultBodyLimit::max(5 * 1024 * 1024))
        .with_state(runtime);
    if let Some(path) = ui_dir.filter(|path| path.is_dir()) {
        let index = path.join("index.html");
        router.fallback_service(
            tower_http::services::ServeDir::new(path)
                .fallback(tower_http::services::ServeFile::new(index)),
        )
    } else {
        router.fallback(|| async {
            (
                StatusCode::NOT_FOUND,
                "Set SERVICES_UI_DIR to the compiled browser UI directory",
            )
                .into_response()
        })
    }
}

pub async fn run(runtime: Runtime, port: u16) -> Result<()> {
    let agent = runtime.agent.clone();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    eprintln!(
        "Services web gateway listening on http://{}",
        listener.local_addr()?
    );
    axum::serve(listener, router(runtime))
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            agent.shutdown().await;
        })
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn agent_rejects_cross_origin_missing_and_rebinding_origins() {
        let mut headers = HeaderMap::new();
        headers.insert("host", "127.0.0.1:4569".parse().unwrap());
        for origin in [
            None,
            Some("null"),
            Some("https://evil.invalid"),
            Some("http://localhost:4569"),
            Some("http://127.0.0.1:4568"),
            Some("http://127.0.0.1:4569/"),
        ] {
            headers.remove("origin");
            if let Some(origin) = origin {
                headers.insert("origin", origin.parse().unwrap());
            }
            assert!(!agent_origin(&headers), "{origin:?}");
        }
        headers.insert("origin", "http://127.0.0.1:4569".parse().unwrap());
        assert!(agent_origin(&headers));
        headers.insert("host", "attacker.invalid:4569".parse().unwrap());
        headers.insert("origin", "http://attacker.invalid:4569".parse().unwrap());
        assert!(!agent_origin(&headers));
    }
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("target/test-data")
                .join(format!(
                    "web-path-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn directory(&self, path: &str) -> PathBuf {
            let path = self.0.join(path);
            std::fs::create_dir_all(&path).unwrap();
            path
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn ui_paths_prefer_explicit_then_relocated_release_then_runtime_source_tree() {
        let fixture = Fixture::new();
        let extension = fixture.directory("relocated-release/extension");
        let executable = fixture
            .0
            .join("relocated-release/native/bin/services-devtools");
        let source = fixture.directory("repo/adapters/extension/dist");
        let cwd = fixture.directory("repo/rust/native");
        assert_eq!(
            static_ui_dir(None, Some(executable.clone()), Some(cwd.clone())),
            Some(extension.clone())
        );
        let explicit = fixture.0.join("explicit");
        assert_eq!(
            static_ui_dir(
                Some(explicit.clone().into_os_string()),
                Some(executable),
                Some(cwd.clone())
            ),
            Some(explicit)
        );
        let dev_exe = fixture.0.join("repo/target/release/services-devtools");
        assert_eq!(
            static_ui_dir(None, Some(dev_exe), None),
            Some(source.clone())
        );
        assert_eq!(static_ui_dir(None, None, Some(cwd)), Some(source));
        assert_eq!(static_ui_dir(None, None, None), None);
        let installed = fixture.directory("installed/extension");
        assert_eq!(
            static_ui_dir(
                None,
                Some(installed.join("native/bin/services-devtools")),
                None
            ),
            Some(installed)
        );
    }

    #[tokio::test]
    async fn relocated_installed_ui_serves_index_wasm_and_spa_fallback_without_env() {
        let fixture = Fixture::new();
        let extension = fixture.directory("installed/extension");
        std::fs::write(
            extension.join("index.html"),
            "<html>installed rust UI</html>",
        )
        .unwrap();
        std::fs::write(extension.join("services_ui_bg.wasm"), b"\0asm\x01\0\0\0").unwrap();
        let executable = extension.join("native/bin/services-devtools");
        let ui_dir = static_ui_dir(None, Some(executable), Some(fixture.0.clone()));
        let router = router_with_ui(
            Runtime::with_data_dir(fixture.0.join("data")).unwrap(),
            ui_dir,
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let client = reqwest::Client::new();
        for path in ["/", "/vars"] {
            let response = client
                .get(format!("http://{address}{path}"))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.text().await.unwrap(),
                "<html>installed rust UI</html>"
            );
        }
        let response = client
            .get(format!("http://{address}/services_ui_bg.wasm"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.bytes().await.unwrap().as_ref(), b"\0asm\x01\0\0\0");
        server.abort();
        let _ = server.await;
    }
}
