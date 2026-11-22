use axum::{
    Router,
    body::Body,
    extract::{Request as Incoming, State},
    http::{StatusCode, header},
    response::Response,
};
use futures_util::stream;
use serde_json::{Value, json};
use services_native::Runtime;
use services_sdk::kubernetes::{self, Event};
use services_testkit::{DataDir, native::Pipe};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const LAST_APPLIED: &str = "kubectl.kubernetes.io/last-applied-configuration";

fn call(id: u32, op: &str, request: &kubernetes::Request) -> Value {
    super::call("kubernetes", id, op, json!(request))
}

/// A runtime whose kubernetes driver reads only `kubeconfig`: the tests never reach the user's
/// kubeconfig, whatever `KUBECONFIG` and `HOME` are.
fn runtime(data: &DataDir, kubeconfig: &str) -> Runtime {
    let mut runtime = data.runtime();
    runtime.drivers = runtime.drivers.clone().with_kubeconfig(kubeconfig);
    runtime
}

async fn answer(runtime: &Runtime, request: &kubernetes::Request) -> kubernetes::Response {
    let reply = runtime.dispatch(&call(1, "call", request)).await;
    assert_eq!(reply["ok"], true, "{reply}");
    kubernetes::Response::parse(&reply["result"]).unwrap()
}

fn stage(reply: &Value) -> &str {
    reply["failure"]["stage"].as_str().unwrap_or_default()
}

/// What the fake API server saw: the bearer tokens it was sent.
#[derive(Default)]
struct Seen {
    tokens: Mutex<Vec<String>>,
    /// Tokens answered with 401.
    revoked: Mutex<Vec<String>>,
}

async fn handle(State(seen): State<Arc<Seen>>, request: Incoming) -> Response {
    let (parts, body) = request.into_parts();
    let token = parts
        .headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim_start_matches("Bearer ").to_owned())
        .unwrap_or_default();
    seen.tokens.lock().unwrap().push(token.clone());
    let header = |name| {
        parts
            .headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    };
    let reply = |status: u16, kind: &'static str, body: Value| {
        Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, kind)
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    if seen.revoked.lock().unwrap().contains(&token) {
        return reply(
            401,
            "application/json",
            json!({"kind": "Status", "reason": "Unauthorized", "message": "Unauthorized", "code": 401}),
        );
    }
    let path = parts.uri.path();
    let secret = json!({
        "kind": "Secret", "apiVersion": "v1",
        "metadata": {"name": "s", "annotations": {LAST_APPLIED: "{\"data\":{\"password\":\"cGFzcw==\"}}", "keep": "me"}},
        "data": {"password": "cGFzcw==", "user": "YWRtaW4="},
    });
    match path {
        "/echo" => {
            let bytes = http_body_util::BodyExt::collect(body)
                .await
                .unwrap()
                .to_bytes();
            reply(
                200,
                "application/json",
                json!({
                    "method": parts.method.as_str(),
                    "uri": parts.uri.to_string(),
                    "accept": header(header::ACCEPT),
                    "contentType": header(header::CONTENT_TYPE),
                    "body": String::from_utf8_lossy(&bytes),
                }),
            )
        }
        "/api/v1/namespaces/ns/secrets/s" => reply(200, "application/json", secret),
        "/api/v1/namespaces/ns/secrets/typed" | "/elsewhere" => reply(200, "text/plain", secret),
        "/api/v1/namespaces/ns/secrets/garbled" => Response::builder()
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Body::from("password: cGFzcw=="))
            .unwrap(),
        "/api/v1/namespaces/ns/secrets" if header(header::ACCEPT).contains("as=Table") => reply(
            200,
            "application/json;as=Table",
            json!({
                "kind": "Table", "apiVersion": "meta.k8s.io/v1",
                "columnDefinitions": [{"name": "Name"}],
                "rows": [{"cells": ["s"], "object": {
                    "kind": "PartialObjectMetadata",
                    "metadata": {"name": "s", "annotations": {LAST_APPLIED: "{\"stringData\":{\"p\":\"x\"}}"}},
                }}],
            }),
        ),
        "/api/v1/namespaces/ns/secrets" => reply(
            200,
            "application/json",
            json!({"kind": "SecretList", "items": [
                {"metadata": {"name": "a"}, "data": {"k": "dg=="}},
                {"metadata": {"name": "b"}, "stringData": {"k": "plain"}},
            ]}),
        ),
        "/api/v1/namespaces/ns/configmaps/c" => reply(
            200,
            "application/json",
            json!({"kind": "ConfigMap", "metadata": {"name": "c"}, "data": {"k": "visible"}}),
        ),
        "/api/v1/namespaces/ns/pods/missing" => reply(
            404,
            "application/json",
            json!({"kind": "Status", "status": "Failure", "reason": "NotFound",
                "message": "pods \"missing\" not found", "code": 404}),
        ),
        "/big" => reply(
            200,
            "application/json",
            json!({"items": (0..2000).collect::<Vec<_>>()}),
        ),
        "/text" => Response::builder()
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Body::from("hello"))
            .unwrap(),
        "/boom" => Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Body::from("upstream exploded"))
            .unwrap(),
        "/finite" => Response::builder()
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Body::from("one\r\ntwo\nthree"))
            .unwrap(),
        // Lines in odd chunks, then the connection stays open.
        "/logs" | "/watch" => {
            let lines: Vec<String> = if path == "/logs" {
                vec!["first\nsec".into(), "ond\n".into(), "third\n".into()]
            } else {
                vec![
                    format!("{}\n", json!({"type": "ADDED", "object": secret})),
                    format!(
                        "{}\n\n",
                        json!({"type": "DELETED", "object": {"kind": "Pod"}})
                    ),
                ]
            };
            let chunks = stream::unfold((lines, 0), |(lines, at)| async move {
                match lines.get(at).cloned() {
                    Some(line) => Some((
                        Ok::<_, std::io::Error>(bytes::Bytes::from(line)),
                        (lines, at + 1),
                    )),
                    None => std::future::pending().await,
                }
            });
            Response::builder()
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from_stream(chunks))
                .unwrap()
        }
        "/whoami" => reply(200, "application/json", json!({"token": token})),
        _ => reply(
            404,
            "application/json",
            json!({"kind": "Status", "reason": "NotFound", "message": "no such path", "code": 404}),
        ),
    }
}

/// A fake API server over plain HTTP; the URL is the kubeconfig's `server`.
async fn serve() -> (String, Arc<Seen>) {
    let seen = Arc::new(Seen::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new().fallback(handle).with_state(seen.clone());
    tokio::spawn(async move { axum::serve(listener, app).await });
    (url, seen)
}

/// Writes a kubeconfig (JSON, which is YAML) with a context `ctx` for `server` and `user`.
fn kubeconfig(dir: &Path, name: &str, server: &str, user: Value) -> String {
    let path: PathBuf = dir.join(name);
    let config = json!({
        "apiVersion": "v1", "kind": "Config",
        "clusters": [{"name": "fake", "cluster": {"server": server}}],
        "users": [{"name": "me", "user": user}],
        "contexts": [{"name": "ctx", "context": {"cluster": "fake", "user": "me", "namespace": "ns"}}],
    });
    std::fs::write(&path, config.to_string()).unwrap();
    path.to_string_lossy().into_owned()
}

fn script(dir: &Path, name: &str, body: &str) -> Value {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    json!({"exec": {
        "apiVersion": "client.authentication.k8s.io/v1",
        "command": path,
        "interactiveMode": "Always",
    }})
}

#[tokio::test]
async fn kubernetes_contexts_merge_kubeconfigs_without_a_cluster_or_a_current_context() {
    let data = DataDir::new();
    let dir = data.path();
    let one = kubeconfig(
        dir,
        "one",
        "https://one.invalid:6443",
        json!({"token": "t"}),
    );
    std::fs::write(
        dir.join("two"),
        json!({
            "apiVersion": "v1", "kind": "Config", "current-context": "other",
            "clusters": [{"name": "c2", "cluster": {"server": "https://two.invalid"}}],
            "contexts": [
                {"name": "other", "context": {"cluster": "c2", "user": "u"}},
                {"name": "ctx", "context": {"cluster": "c2", "user": "u"}},
            ],
        })
        .to_string(),
    )
    .unwrap();
    let missing = dir.join("missing").to_string_lossy().into_owned();
    let paths = std::env::join_paths([
        &one,
        &missing,
        &dir.join("two").to_string_lossy().into_owned(),
    ])
    .unwrap();

    let response = answer(
        &runtime(&data, paths.to_str().unwrap()),
        &kubernetes::Request::contexts(),
    )
    .await;
    let names: Vec<_> = response.contexts.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["ctx", "other"], "the first file wins a name");
    assert_eq!(response.current.as_deref(), Some("other"));
    let ctx = &response.contexts[0];
    assert_eq!(
        (
            ctx.cluster.as_str(),
            ctx.server.as_str(),
            ctx.namespace.as_deref()
        ),
        ("fake", "https://one.invalid:6443", Some("ns"))
    );
    assert_eq!(response.contexts[1].namespace, None);

    let none = answer(&runtime(&data, ""), &kubernetes::Request::contexts()).await;
    assert!(
        none.contexts.is_empty() && none.current.is_none(),
        "{none:?}"
    );

    std::fs::write(dir.join("bad"), "contexts: [[[").unwrap();
    let bad = dir.join("bad").to_string_lossy().into_owned();
    let reply = runtime(&data, &bad)
        .dispatch(&call(2, "call", &kubernetes::Request::contexts()))
        .await;
    assert_eq!(stage(&reply), "auth", "{reply}");
}

#[tokio::test]
async fn kubernetes_request_sends_the_call_and_answers_json_text_and_status() {
    let data = DataDir::new();
    let (server, _) = serve().await;
    let config = kubeconfig(data.path(), "config", &server, json!({"token": "static"}));
    let runtime = runtime(&data, &config);

    let mut patch = kubernetes::Request::new("ctx", "PATCH", "/echo");
    patch.query.insert("fieldManager".into(), "a b&c".into());
    patch.accept = Some("application/json;as=Table;v=v1;g=meta.k8s.io".into());
    patch.content_type = Some("application/merge-patch+json".into());
    patch.body = Some("{\"a\":1}".into());
    let response = answer(&runtime, &patch).await;
    assert_eq!(response.status, 200);
    assert_eq!(
        response.body,
        json!({
            "method": "PATCH",
            "uri": "/echo?fieldManager=a+b%26c",
            "accept": "application/json;as=Table;v=v1;g=meta.k8s.io",
            "contentType": "application/merge-patch+json",
            "body": "{\"a\":1}",
        })
    );
    assert!(response.error.is_none() && !response.truncated);
    assert!(response.timing.total_us >= response.timing.first_byte_us);

    let whoami = answer(&runtime, &kubernetes::Request::get("ctx", "/whoami")).await;
    assert_eq!(whoami.body["token"], "static");

    let text = answer(&runtime, &kubernetes::Request::get("ctx", "/text")).await;
    assert_eq!(text.body, json!("hello"));

    let missing = answer(
        &runtime,
        &kubernetes::Request::get("ctx", "/api/v1/namespaces/ns/pods/missing"),
    )
    .await;
    assert_eq!(missing.status, 404);
    let error = missing.error.unwrap();
    assert_eq!(
        (error.reason.as_str(), error.message.as_str(), error.code),
        ("NotFound", "pods \"missing\" not found", 404)
    );

    let boom = answer(&runtime, &kubernetes::Request::get("ctx", "/boom")).await;
    let error = boom.error.unwrap();
    assert_eq!(
        (boom.status, error.reason.as_str(), error.message.as_str()),
        (500, "InternalServerError", "upstream exploded")
    );
}

#[tokio::test]
async fn kubernetes_max_bytes_truncates_the_answer() {
    let data = DataDir::new();
    let (server, _) = serve().await;
    let config = kubeconfig(data.path(), "config", &server, json!({"token": "t"}));
    let runtime = runtime(&data, &config);

    let whole = answer(&runtime, &kubernetes::Request::get("ctx", "/big")).await;
    assert!(!whole.truncated && whole.body["items"].as_array().unwrap().len() == 2000);

    let mut limited = kubernetes::Request::get("ctx", "/big");
    limited.max_bytes = Some(100);
    let cut = answer(&runtime, &limited).await;
    assert!(cut.truncated);
    assert_eq!(cut.body.as_str().unwrap().len(), 100, "{:?}", cut.body);

    limited.max_bytes = Some(1_000_000);
    assert!(!answer(&runtime, &limited).await.truncated);
}

#[tokio::test]
async fn kubernetes_redacts_secrets_and_only_secrets() {
    let data = DataDir::new();
    let (server, _) = serve().await;
    let config = kubeconfig(data.path(), "config", &server, json!({"token": "t"}));
    let runtime = runtime(&data, &config);
    let get = |path: &str| kubernetes::Request::get("ctx", path);

    let secret = answer(&runtime, &get("/api/v1/namespaces/ns/secrets/s"))
        .await
        .body;
    assert_eq!(
        secret["data"],
        json!({"password": "<redacted>", "user": "<redacted>"})
    );
    assert_eq!(
        secret["metadata"]["annotations"][LAST_APPLIED],
        "<redacted>"
    );
    assert_eq!(secret["metadata"]["annotations"]["keep"], "me");

    let list = answer(&runtime, &get("/api/v1/namespaces/ns/secrets"))
        .await
        .body;
    assert_eq!(list["items"][0]["data"], json!({"k": "<redacted>"}));
    assert_eq!(list["items"][1]["stringData"], json!({"k": "<redacted>"}));

    let mut table = get("/api/v1/namespaces/ns/secrets");
    table.accept = Some("application/json;as=Table;v=v1;g=meta.k8s.io".into());
    let table = answer(&runtime, &table).await.body;
    assert_eq!(table["rows"][0]["cells"], json!(["s"]));
    assert_eq!(
        table["rows"][0]["object"]["metadata"]["annotations"][LAST_APPLIED],
        "<redacted>"
    );

    let config_map = answer(&runtime, &get("/api/v1/namespaces/ns/configmaps/c"))
        .await
        .body;
    assert_eq!(config_map["data"], json!({"k": "visible"}));
}

#[tokio::test]
async fn kubernetes_never_returns_a_secret_it_could_not_redact() {
    let data = DataDir::new();
    let (server, _) = serve().await;
    let config = kubeconfig(data.path(), "config", &server, json!({"token": "t"}));
    let runtime = runtime(&data, &config);
    let leaked = |body: &Value| body.to_string().contains("cGFzcw");

    let mut cut = kubernetes::Request::get("ctx", "/api/v1/namespaces/ns/secrets/s");
    cut.max_bytes = Some(150);
    let cut = answer(&runtime, &cut).await;
    assert!(cut.truncated && !leaked(&cut.body), "{:?}", cut.body);
    assert!(cut.body.as_str().unwrap().starts_with("<redacted:"));

    // Truncated while the path does not say it is a Secret.
    let mut elsewhere = kubernetes::Request::get("ctx", "/elsewhere");
    elsewhere.max_bytes = Some(150);
    let elsewhere = answer(&runtime, &elsewhere).await;
    assert!(elsewhere.truncated && !leaked(&elsewhere.body));

    // Not served as JSON, yet parsed and redacted.
    let typed = answer(
        &runtime,
        &kubernetes::Request::get("ctx", "/api/v1/namespaces/ns/secrets/typed"),
    )
    .await;
    assert_eq!(typed.body["data"]["password"], "<redacted>");
    let elsewhere = answer(&runtime, &kubernetes::Request::get("ctx", "/elsewhere")).await;
    assert_eq!(elsewhere.body["data"]["user"], "<redacted>");

    let garbled = answer(
        &runtime,
        &kubernetes::Request::get("ctx", "/api/v1/namespaces/ns/secrets/garbled"),
    )
    .await;
    assert!(garbled.body.as_str().unwrap().starts_with("<redacted:"));
    assert!(!leaked(&garbled.body));
}

#[tokio::test]
async fn kubernetes_stream_sends_lines_and_redacted_watch_events() {
    let data = DataDir::new();
    let (server, _) = serve().await;
    let config = kubeconfig(data.path(), "config", &server, json!({"token": "t"}));
    let mut pipe = Pipe::start(runtime(&data, &config));

    let mut finite = kubernetes::Request::get("ctx", "/finite");
    finite.stream = true;
    pipe.send(&call(1, "stream", &finite)).await;
    let (events, exit) = super::events(&mut pipe).await;
    let lines: Vec<_> = events
        .iter()
        .map(|event| Event::parse(event).unwrap())
        .collect();
    assert_eq!(
        lines,
        ["one", "two", "three"].map(|line| Event::Line(line.into()))
    );
    let response = kubernetes::Response::parse(&exit["info"]).unwrap();
    assert_eq!(
        (response.status, response.body, response.truncated),
        (200, Value::Null, false)
    );

    // A deadline ends a stream that stays open.
    let mut logs = kubernetes::Request::get("ctx", "/logs");
    logs.stream = true;
    logs.timeout_ms = Some(500);
    pipe.send(&call(2, "stream", &logs)).await;
    let (events, exit) = super::events(&mut pipe).await;
    assert_eq!(
        events,
        [
            json!({"line": "first"}),
            json!({"line": "second"}),
            json!({"line": "third"})
        ]
    );
    assert!(kubernetes::Response::parse(&exit["info"]).is_ok(), "{exit}");

    logs.timeout_ms = None;
    logs.max_bytes = Some(8);
    pipe.send(&call(3, "stream", &logs)).await;
    let (events, exit) = super::events(&mut pipe).await;
    assert_eq!(events, [json!({"line": "first"})]);
    assert!(
        kubernetes::Response::parse(&exit["info"])
            .unwrap()
            .truncated
    );

    let mut watch = kubernetes::Request::get("ctx", "/api/v1/namespaces/ns/secrets/s");
    watch.path = "/watch".into();
    watch.query.insert("watch".into(), "true".into());
    watch.stream = true;
    watch.timeout_ms = Some(500);
    pipe.send(&call(4, "stream", &watch)).await;
    let (events, _) = super::events(&mut pipe).await;
    assert_eq!(events.len(), 2, "{events:?}");
    let Event::Object(added) = Event::parse(&events[0]).unwrap() else {
        panic!("{events:?}")
    };
    assert_eq!(added["type"], "ADDED");
    assert_eq!(added["object"]["data"]["password"], "<redacted>");
    assert_eq!(
        added["object"]["metadata"]["annotations"][LAST_APPLIED],
        "<redacted>"
    );
}

#[tokio::test]
async fn kubernetes_cancel_stops_a_stream() {
    let data = DataDir::new();
    let (server, _) = serve().await;
    let config = kubeconfig(data.path(), "config", &server, json!({"token": "t"}));
    let mut pipe = Pipe::start(runtime(&data, &config));
    let mut logs = kubernetes::Request::get("ctx", "/logs");
    logs.stream = true;
    pipe.send(&call(1, "stream", &logs)).await;
    for _ in 0..3 {
        let message = pipe.read().await;
        assert_eq!(message["type"], "data", "{message}");
    }
    pipe.send(&json!({"id": 1, "type": "stop"})).await;
    let reply = pipe.read().await;
    assert_eq!(stage(&reply), "cancelled", "{reply}");
    assert!(
        reply["failure"]["causes"]
            .to_string()
            .contains("not rolled back")
    );
}

#[tokio::test]
async fn kubernetes_exec_plugin_runs_without_stdin_and_is_renewed_on_401() {
    let data = DataDir::new();
    let (server, seen) = serve().await;
    let dir = data.path();
    // `cat` returns only when stdin is closed: an inherited stdin would hang or be consumed.
    let counter = dir.join("count");
    let plugin = script(
        dir,
        "plugin",
        &format!(
            "cat >/dev/null\n\
             case \"$KUBERNETES_EXEC_INFO\" in *'\"interactive\":false'*) ;; *) echo bad info >&2; exit 1;; esac\n\
             n=$(cat {counter} 2>/dev/null || echo 0); n=$((n+1)); echo $n > {counter}\n\
             echo '{{\"apiVersion\":\"client.authentication.k8s.io/v1\",\"kind\":\"ExecCredential\",\"status\":{{\"token\":\"tok-'$n'\"}}}}'",
            counter = counter.display()
        ),
    );
    let config = kubeconfig(dir, "config", &server, plugin);
    let runtime = runtime(&data, &config);
    let whoami = kubernetes::Request::get("ctx", "/whoami");

    assert_eq!(answer(&runtime, &whoami).await.body["token"], "tok-1");
    assert_eq!(
        answer(&runtime, &whoami).await.body["token"],
        "tok-1",
        "cached client"
    );

    seen.revoked.lock().unwrap().push("tok-1".into());
    let renewed = answer(&runtime, &whoami).await;
    assert_eq!(
        (renewed.status, renewed.body["token"].clone()),
        (200, json!("tok-2"))
    );
}

#[tokio::test]
async fn kubernetes_exec_plugin_failures_are_auth_failures_with_scrubbed_stderr() {
    let data = DataDir::new();
    let (server, _) = serve().await;
    let dir = data.path();
    let request = kubernetes::Request::get("ctx", "/whoami");
    let reply = |config: &str, request: &kubernetes::Request| {
        let runtime = runtime(&data, config);
        let request = request.clone();
        async move { runtime.dispatch(&call(1, "call", &request)).await }
    };

    let device = script(
        dir,
        "device",
        "echo 'To sign in, open https://microsoft.com/devicelogin and enter the code ABCD1234. token=eyJhbGciOiJSUzI1NiJ9.payload.sig' >&2\nexit 1",
    );
    let config = kubeconfig(dir, "device-config", &server, device);
    let failed = reply(&config, &request).await;
    assert_eq!(stage(&failed), "auth", "{failed}");
    let text = failed.to_string();
    assert!(
        text.contains("devicelogin") && text.contains("ABCD1234"),
        "{text}"
    );
    assert!(text.contains("Log in once in a terminal"), "{text}");
    assert!(
        !text.contains("eyJhbGci") && !text.contains("payload.sig"),
        "{text}"
    );

    let hang = script(dir, "hang", "sleep 60");
    let config = kubeconfig(dir, "hang-config", &server, hang);
    let mut timed = request.clone();
    timed.timeout_ms = Some(2_000);
    let started = Instant::now();
    let failed = reply(&config, &timed).await;
    assert_eq!(stage(&failed), "auth", "{failed}");
    assert!(failed.to_string().contains("did not finish"), "{failed}");
    assert!(started.elapsed() < Duration::from_secs(5));

    let junk = script(dir, "junk", "echo not json");
    let config = kubeconfig(dir, "junk-config", &server, junk);
    assert_eq!(stage(&reply(&config, &request).await), "auth");

    let config = kubeconfig(
        dir,
        "missing-config",
        &server,
        json!({"exec": {"apiVersion": "client.authentication.k8s.io/v1", "command": "/nonexistent/plugin", "installHint": "install it"}}),
    );
    let failed = reply(&config, &request).await;
    assert_eq!(stage(&failed), "auth", "{failed}");
    assert!(failed.to_string().contains("install it"), "{failed}");
}

#[tokio::test]
async fn kubernetes_failures_name_their_stage() {
    let data = DataDir::new();
    let (server, _) = serve().await;
    let config = kubeconfig(data.path(), "config", &server, json!({"token": "t"}));
    let runtime = runtime(&data, &config);
    let run = |request: kubernetes::Request| {
        let runtime = &runtime;
        async move { runtime.dispatch(&call(1, "call", &request)).await }
    };

    assert_eq!(
        stage(&run(kubernetes::Request::get("nope", "/x")).await),
        "auth"
    );
    let mut no_context = kubernetes::Request::get("", "/x");
    no_context.context = None;
    assert_eq!(stage(&run(no_context).await), "send");
    assert_eq!(
        stage(&run(kubernetes::Request::new("ctx", "TRACE", "/x")).await),
        "send"
    );
    assert_eq!(
        stage(&run(kubernetes::Request::get("ctx", "x")).await),
        "send"
    );

    // A closed port: the context's server is not listening.
    let closed = kubeconfig(
        data.path(),
        "closed",
        "http://127.0.0.1:1",
        json!({"token": "t"}),
    );
    let reply = self::runtime(&data, &closed)
        .dispatch(&call(2, "call", &kubernetes::Request::get("ctx", "/x")))
        .await;
    assert_eq!(stage(&reply), "connect", "{reply}");
    assert!(!reply.to_string().contains("\"t\""), "{reply}");
}

// The tests below run against a real API server (the k3s of `host/scripts/kubernetes-integration.sh`).
// `KUBERNETES_TEST_KUBECONFIG` lists the kubeconfig files of that throwaway cluster, with the
// contexts `default` (admin), `k3s-exec` (admin by an exec plugin) and `k3s-restricted` (a
// service account without rights); they pass without running when it is not set.

fn cluster() -> Option<String> {
    let value = std::env::var("KUBERNETES_TEST_KUBECONFIG")
        .ok()
        .filter(|value| !value.is_empty());
    if value.is_none() {
        eprintln!("KUBERNETES_TEST_KUBECONFIG is not set; skipping");
    }
    value
}

fn with_body(
    mut request: kubernetes::Request,
    content_type: &str,
    body: Value,
) -> kubernetes::Request {
    request.content_type = Some(content_type.into());
    request.body = Some(body.to_string());
    request
}

#[tokio::test]
async fn kubernetes_k3s_reads_writes_and_redacts() {
    let Some(config) = cluster() else { return };
    let data = DataDir::new();
    let runtime = runtime(&data, &config);

    let contexts = answer(&runtime, &kubernetes::Request::contexts()).await;
    let names: Vec<_> = contexts.contexts.iter().map(|c| c.name.as_str()).collect();
    for name in ["default", "k3s-exec", "k3s-restricted"] {
        assert!(names.contains(&name), "{names:?}");
    }
    assert!(
        contexts.contexts[0]
            .server
            .starts_with("https://127.0.0.1:")
    );

    let pods = answer(
        &runtime,
        &kubernetes::Request::get("default", "/api/v1/namespaces/kube-system/pods"),
    )
    .await;
    assert_eq!(pods.status, 200);
    assert!(!pods.body["items"].as_array().unwrap().is_empty());

    let mut table = kubernetes::Request::get("default", "/api/v1/namespaces/kube-system/pods");
    table.accept = Some("application/json;as=Table;v=v1;g=meta.k8s.io".into());
    let table = answer(&runtime, &table).await;
    assert_eq!(table.body["kind"], "Table");
    assert_eq!(table.body["columnDefinitions"][0]["name"], "Name");

    let name = format!("sdt-it-{}", std::process::id());
    let maps = "/api/v1/namespaces/default/configmaps";
    let create = with_body(
        kubernetes::Request::new("default", "POST", maps),
        "application/json",
        json!({"apiVersion": "v1", "kind": "ConfigMap", "metadata": {"name": name}, "data": {"a": "1"}}),
    );
    assert_eq!(answer(&runtime, &create).await.status, 201);
    let item = format!("{maps}/{name}");
    let patch = with_body(
        kubernetes::Request::new("default", "PATCH", &item),
        "application/merge-patch+json",
        json!({"data": {"b": "2"}}),
    );
    let patched = answer(&runtime, &patch).await;
    assert_eq!(
        patched.body["data"],
        json!({"a": "1", "b": "2"}),
        "ConfigMap data is not redacted"
    );
    let again = answer(&runtime, &create).await;
    assert_eq!(
        (again.status, again.error.unwrap().reason.as_str()),
        (409, "AlreadyExists")
    );
    assert_eq!(
        answer(
            &runtime,
            &kubernetes::Request::new("default", "DELETE", &item)
        )
        .await
        .status,
        200
    );
    let gone = answer(&runtime, &kubernetes::Request::get("default", &item)).await;
    let error = gone.error.unwrap();
    assert_eq!(
        (gone.status, error.reason.as_str(), error.code),
        (404, "NotFound", 404)
    );

    let secrets = "/api/v1/namespaces/default/secrets";
    let secret = format!("{name}-secret");
    let create = with_body(
        kubernetes::Request::new("default", "POST", secrets),
        "application/json",
        json!({"apiVersion": "v1", "kind": "Secret",
            "metadata": {"name": secret, "annotations": {LAST_APPLIED: "{\"data\":{\"password\":\"c2VjcmV0\"}}"}},
            "stringData": {"password": "topsecret"}}),
    );
    let created = answer(&runtime, &create).await;
    assert_eq!(created.status, 201);
    assert!(
        !created.body.to_string().contains("c2VjcmV0")
            && !created.body.to_string().contains("topsecret")
    );
    let item = format!("{secrets}/{secret}");
    let got = answer(&runtime, &kubernetes::Request::get("default", &item)).await;
    assert_eq!(got.body["data"], json!({"password": "<redacted>"}));
    assert_eq!(
        got.body["metadata"]["annotations"][LAST_APPLIED],
        "<redacted>"
    );
    let mut table = kubernetes::Request::get("default", secrets);
    table.accept = Some("application/json;as=Table;v=v1;g=meta.k8s.io".into());
    let table = answer(&runtime, &table).await;
    assert!(
        !table.body.to_string().contains("c2VjcmV0"),
        "{}",
        table.body
    );
    answer(
        &runtime,
        &kubernetes::Request::new("default", "DELETE", &item),
    )
    .await;
}

#[tokio::test]
async fn kubernetes_k3s_logs_follow_and_watch_stream() {
    let Some(config) = cluster() else { return };
    let data = DataDir::new();
    let runtime = runtime(&data, &config);
    let name = format!("sdt-log-{}", std::process::id());
    let pods = "/api/v1/namespaces/default/pods";
    let create = with_body(
        kubernetes::Request::new("default", "POST", pods),
        "application/json",
        json!({"apiVersion": "v1", "kind": "Pod", "metadata": {"name": name},
            "spec": {"terminationGracePeriodSeconds": 0, "containers": [{
                "name": "c", "image": "busybox:1.36",
                "command": ["sh", "-c", "i=0; while true; do echo line $i; i=$((i+1)); sleep 0.2; done"]}]}}),
    );

    let mut pipe = Pipe::start(runtime.clone());
    let mut watch = kubernetes::Request::get("default", pods);
    watch.query.insert("watch".into(), "true".into());
    watch
        .query
        .insert("fieldSelector".into(), format!("metadata.name={name}"));
    watch.stream = true;
    watch.timeout_ms = Some(90_000);
    pipe.send(&call(1, "stream", &watch)).await;
    assert_eq!(answer(&runtime, &create).await.status, 201);
    let mut seen = vec![];
    while seen.len() < 2 {
        let message = tokio::time::timeout(Duration::from_secs(30), pipe.read())
            .await
            .unwrap();
        assert_eq!(message["type"], "data", "{message}");
        let Event::Object(event) = Event::parse(&message["event"]).unwrap() else {
            panic!()
        };
        seen.push(event["type"].as_str().unwrap().to_owned());
    }
    assert_eq!(seen[0], "ADDED");
    pipe.send(&json!({"id": 1, "type": "stop"})).await;
    assert_eq!(stage(&pipe.read().await), "cancelled");

    let item = format!("{pods}/{name}");
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let pod = answer(&runtime, &kubernetes::Request::get("default", &item)).await;
        if pod.body["status"]["phase"] == "Running" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the pod did not start: {}",
            pod.body["status"]
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    let mut logs = kubernetes::Request::get("default", &format!("{item}/log"));
    logs.query.insert("tailLines".into(), "3".into());
    let text = answer(&runtime, &logs).await;
    assert!(
        text.body.as_str().unwrap().contains("line "),
        "{:?}",
        text.body
    );

    logs.query.insert("follow".into(), "true".into());
    logs.query.insert("tailLines".into(), "1".into());
    logs.stream = true;
    let mut pipe = Pipe::start(runtime.clone());
    pipe.send(&call(2, "stream", &logs)).await;
    for _ in 0..4 {
        let message = pipe.read().await;
        assert_eq!(message["type"], "data", "{message}");
        let Event::Line(line) = Event::parse(&message["event"]).unwrap() else {
            panic!()
        };
        assert!(line.starts_with("line "), "{line}");
    }
    pipe.send(&json!({"id": 2, "type": "stop"})).await;
    assert_eq!(stage(&pipe.read().await), "cancelled");

    let mut delete = kubernetes::Request::new("default", "DELETE", &item);
    delete.query.insert("gracePeriodSeconds".into(), "0".into());
    answer(&runtime, &delete).await;
}

#[tokio::test]
async fn kubernetes_k3s_authorization_and_exec_contexts() {
    let Some(config) = cluster() else { return };
    let data = DataDir::new();
    let runtime = runtime(&data, &config);
    let pods = "/api/v1/namespaces/kube-system/pods";

    let forbidden = answer(&runtime, &kubernetes::Request::get("k3s-restricted", pods)).await;
    let error = forbidden.error.unwrap();
    assert_eq!(
        (forbidden.status, error.reason.as_str(), error.code),
        (403, "Forbidden", 403)
    );
    assert!(error.message.contains("forbidden"), "{}", error.message);

    let exec = answer(
        &runtime,
        &kubernetes::Request::get("k3s-exec", "/api/v1/namespaces"),
    )
    .await;
    assert_eq!(exec.status, 200);
    assert!(!exec.body["items"].as_array().unwrap().is_empty());
}
