use axum::{
    Router,
    body::to_bytes,
    extract::{Request, State},
    response::IntoResponse,
};
use serde_json::{Value, json};
use services_native::{
    Runtime,
    adapters::{
        cli,
        framing::{FrameReader, encode},
        native,
    },
    services::{mysql, openapi, terminal},
};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::mpsc,
};
use tokio_util::sync::CancellationToken;
static NEXT: AtomicU64 = AtomicU64::new(0);
#[tokio::test]
async fn templates_are_global_scoped_literal_and_conflict_checked() {
    let data = Data::new();
    let runtime = data.runtime();
    let save = |service: &str, section: &str, text: &str, base: &str| {
        json!({"service":"templates","op":"save",
        "serviceName":service,"section":section,"text":text,"base":base,"sessionVariables":{"OTHER":"not a template"}})
    };
    let text = "@my_create_book comment\n{\n  \"id\": 123,\n  \"token\": \"{{var:TOKEN}}\"\n}";
    let result = runtime.dispatch(&save("rest", "body", text, "")).await;
    assert!(result.get("error").is_none(), "{result}");
    assert_eq!(result["result"]["templates"]["rest"]["body"], text);
    let other = data.runtime();
    let loaded = other
        .dispatch(&json!({"service":"templates","op":"get"}))
        .await;
    assert_eq!(loaded["result"]["templates"]["rest"]["body"], text);
    let changed = "@different\n{\"id\":2}";
    assert!(
        other.dispatch(&save("rest", "body", changed, "")).await["error"]
            .as_str()
            .unwrap()
            .contains("CONFLICT")
    );
    assert!(
        other
            .dispatch(&save(
                "mysql",
                "query",
                "@my_create_book SQL\nSELECT 1;",
                ""
            ))
            .await
            .get("error")
            .is_none()
    );
    assert!(
        other
            .dispatch(&save("rest", "body", "@dup\n1\n@dup\n2", text))
            .await["error"]
            .is_string()
    );
    assert!(
        other
            .dispatch(&save("rest", "query", "@wrong\nno", ""))
            .await["error"]
            .is_string()
    );
    assert!(
        other
            .dispatch(&save("rest", "body", changed, text))
            .await
            .get("error")
            .is_none()
    );
    let loaded = runtime
        .dispatch(&json!({"service":"templates","op":"get"}))
        .await;
    assert_eq!(loaded["result"]["templates"]["rest"]["body"], changed);
    assert_eq!(
        loaded["result"]["templates"]["mysql"]["query"],
        "@my_create_book SQL\nSELECT 1;"
    );
    assert!(
        other
            .dispatch(&save("rest", "body", "", changed))
            .await
            .get("error")
            .is_none()
    );
    std::fs::write(data.0.join(".console-templates.json"), "not-json").unwrap();
    assert!(
        runtime
            .dispatch(&json!({"service":"templates","op":"get"}))
            .await["error"]
            .is_string()
    );
}
struct Data(PathBuf);
impl Data {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-data")
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
#[tokio::test]
async fn session_variables_are_request_local_and_never_replace_global_stores() {
    let data = Data::new();
    let runtime = data.runtime();
    runtime
        .variable_store
        .dispatch(&json!({"op":"save","name":"var","variables":{"TOKEN":"legacy"}}))
        .unwrap();
    runtime
        .dictionary_store
        .dispatch(&json!({"op":"save","name":"shared","variables":{"REF":"{{var:TOKEN}}"}}))
        .unwrap();
    let a = json!({"TOKEN":"session-a","API_TOKEN":"session-secret-a","NESTED":{"API_TOKEN":"nested-session-secret"}});
    let b = json!({"TOKEN":"session-b"});
    for (variables, expected) in [(&a, "session-a"), (&b, "session-b"), (&a, "session-a")] {
        let view = runtime
            .variable_store
            .session_dispatch(&json!({"op":"list"}), variables)
            .unwrap();
        assert_eq!(view["profiles"]["var"], *variables);
        assert_eq!(
            view["context"]["resolvedProfiles"]["shared"]["REF"],
            expected
        );
        let params = runtime.variable_store.resolve_params("terminal",&json!({
            "endpoint":"sh -c","command":"printf '{{var:TOKEN}} {{shared:REF}}'","__consoleVariables":variables
        })).unwrap();
        assert_eq!(params["command"], format!("printf '{expected} {expected}'"));
        assert!(params.get("__consoleVariables").is_none());
    }
    assert_eq!(
        runtime
            .variable_store
            .session_dispatch(
                &json!({"op":"save","name":"var","variables":{"TOKEN":"new"}}),
                &a
            )
            .unwrap()["profiles"]["var"]["TOKEN"],
        "new"
    );
    assert_eq!(
        runtime.variable_store.list().unwrap()["profiles"]["var"]["TOKEN"],
        "legacy"
    );
    assert_eq!(
        runtime.dictionary_store.list().unwrap()["profiles"]["shared"]["REF"],
        "{{var:TOKEN}}"
    );
    assert!(
        runtime
            .agent_session_sensitive_values(&json!({"__consoleVariables":a}))
            .unwrap()
            .contains(&"session-secret-a".to_owned())
    );
    assert!(
        runtime
            .agent_session_sensitive_values(&json!({"__consoleVariables":a}))
            .unwrap()
            .contains(&"nested-session-secret".to_owned())
    );
    assert!(
        runtime
            .variable_store
            .resolve_params(
                "terminal",
                &json!({"command":"{{var:TOKEN}}","__consoleVariables":{}})
            )
            .is_err()
    );
    assert!(
        runtime
            .variable_store
            .session_dispatch(&json!({"op":"list"}), &json!([]))
            .is_err()
    );
    let boot = runtime
        .dispatch(&json!({"service":"sessions","op":"bootstrap"}))
        .await;
    assert_eq!(boot["result"]["variables"]["TOKEN"], "legacy");
    assert!(boot["result"]["hints"]["rest"].is_string());
}
struct Server {
    url: String,
    task: tokio::task::JoinHandle<()>,
}
impl Server {
    async fn new(router: Router) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self { url, task }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
type Seen = Arc<Mutex<Vec<Value>>>;
async fn fixture(State(seen): State<Seen>, request: Request) -> axum::response::Response {
    let path = request.uri().path().to_string();
    let query = request.uri().query().unwrap_or("").to_string();
    let method = request.method().to_string();
    let auth = request
        .headers()
        .get("x-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = to_bytes(request.into_body(), 1024 * 1024).await.unwrap();
    let body = String::from_utf8_lossy(&bytes).to_string();
    seen.lock()
        .unwrap()
        .push(json!({"path":path,"query":query,"method":method,"token":auth,"body":body}));
    if path == "/api/openapi.json" || path == "/protected/openapi.json" {
        if path == "/protected/openapi.json" && auth != "schema-token" {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                "Schema requires X-Token",
            )
                .into_response();
        }
        return axum::Json(json!({"openapi":"3.0.0","servers":[{"url":"/api"}],"paths":{"/pets":{"post":{"parameters":[{"in":"header","name":"X-Token"}],"requestBody":{"content":{"application/json":{"schema":{"$ref":"#/components/schemas/Pet"}}}}}}},"components":{"schemas":{"Pet":{"properties":{"name":{"type":"string"},"owner":{"type":"object","properties":{"id":{"type":"integer"}}}}}}}})).into_response();
    }
    if path == "/swagger-api/swagger.json" {
        return axum::Json(json!({"swagger":"2.0","basePath":"/swagger-api","paths":{
            "/pets":{"get":{},"post":{"parameters":[{"in":"body","name":"body","schema":{"$ref":"#/definitions/Pet"}}]}}
        },"definitions":{"Pet":{"properties":{"name":{"type":"string"}}}}})).into_response();
    }
    if path == "/openapi31/openapi.json" {
        return axum::Json(json!({"openapi":"3.1.0","servers":[{"url":"/openapi31"}],"paths":{"/pets":{"get":{}}}})).into_response();
    }
    if path == "/empty-api/openapi.json" {
        return axum::Json(json!({"openapi":"3.1.0","paths":{}})).into_response();
    }
    if path == "/graphql" || path == "/graphql-protected" {
        if path == "/graphql-protected" && auth != "discovery-token" {
            return axum::Json(json!({"errors":[{"message":"Discovery requires X-Token"}]}))
                .into_response();
        }
        let body: Value = serde_json::from_str(&body).unwrap();
        if body["query"].as_str().unwrap_or("").contains("__schema") {
            return axum::Json(json!({"data":{"__schema":{
                "queryType":{"name":"Query"},"mutationType":null,"subscriptionType":{"name":"Subscription"},
                "directives":[{"name":"include","locations":["FIELD","FRAGMENT_SPREAD","INLINE_FRAGMENT"],"isRepeatable":false,
                    "args":[{"name":"if","defaultValue":null,"type":{"kind":"NON_NULL","ofType":{"kind":"SCALAR","name":"Boolean"}}}]}],
                "types":[
                    {"name":"Query","kind":"OBJECT","fields":[{"name":"pets",
                        "type":{"kind":"NON_NULL","ofType":{"kind":"LIST","ofType":{"kind":"OBJECT","name":"Pet"}}},
                        "args":[{"name":"filter","defaultValue":null,"type":{"kind":"INPUT_OBJECT","name":"PetFilter"}}]}]},
                    {"name":"Pet","kind":"OBJECT","fields":[{"name":"id","type":{"kind":"SCALAR","name":"ID"}}]},
                    {"name":"PetLike","kind":"INTERFACE","possibleTypes":[{"name":"Pet"}]},
                    {"name":"SearchResult","kind":"UNION","possibleTypes":[{"name":"Pet"},{"name":"Query"}]},
                    {"name":"PetFilter","kind":"INPUT_OBJECT","inputFields":[
                        {"name":"state","defaultValue":"OPEN","type":{"kind":"ENUM","name":"State"}},
                        {"name":"ids","defaultValue":null,"type":{"kind":"LIST","ofType":{"kind":"NON_NULL","ofType":{"kind":"SCALAR","name":"ID"}}}}]},
                    {"name":"State","kind":"ENUM","enumValues":[{"name":"OPEN"},{"name":"CLOSED"}]},
                    {"name":"Subscription","kind":"OBJECT","fields":[{"name":"petChanged","type":{"kind":"OBJECT","name":"Pet"}}]},
                    {"name":"ID","kind":"SCALAR"}
                ]
            }}})).into_response();
        }
        return (
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            json!({"data":{"echo":body["variables"]}}).to_string(),
        )
            .into_response();
    }
    if path.ends_with("/search/v2/jobs/export") || path.ends_with("/search/jobs/export") {
        return axum::Json(json!({"preview":false,"fields":[{"name":"host"},{"name":"count"}],"results":[{"host":"fixture","count":"42"}],"messages":[{"type":"INFO","text":"fixture message"}],"lastrow":true})).into_response();
    }
    if path == "/slow" {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    if path == "/cancel-slow" {
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
    if path == "/echo" || path == "/slow" {
        return (
            axum::http::StatusCode::CREATED,
            [("x-response", "fixture")],
            body,
        )
            .into_response();
    }
    (axum::http::StatusCode::NOT_FOUND, "not found").into_response()
}
async fn server() -> (Server, Seen) {
    let seen = Arc::new(Mutex::new(vec![]));
    let router = Router::new().fallback(fixture).with_state(seen.clone());
    (Server::new(router).await, seen)
}
#[test]
fn framing_accepts_fragments_unicode_and_discards_malformed_json() {
    let a = json!({"id":"α","params":{"body":"🦀"}});
    let mut bytes = encode(&a).unwrap();
    bytes.extend([1, 0, 0, 0, b'!']);
    bytes.extend(encode(&json!({"id":2})).unwrap());
    let mut reader = FrameReader::default();
    let mut messages = vec![];
    for byte in bytes {
        messages.extend(reader.feed(&[byte]).unwrap());
    }
    assert_eq!(messages, vec![a, json!({"id":2})]);
    assert!(
        FrameReader::default()
            .feed(&u32::MAX.to_le_bytes())
            .is_err()
    );
}
#[test]
fn profile_services_migrate_once_and_write_independent_files() {
    let data = Data::new();
    let path = data.0.join(".variable-profiles.json");
    let legacy = json!({"order":["shop","var","default"],"profiles":{
        "shop":{"HOST":"fixture.invalid","FROM_VAR":"{{var:BASE}}"},
        "var":{"BASE":"local","HOST":"local.invalid","FROM_DICT":"{{shop:HOST}}"},
        "default":{"TOKEN":"preserved"}
    }});
    let original = serde_json::to_vec_pretty(&legacy).unwrap();
    std::fs::write(&path, &original).unwrap();
    let rt = data.runtime();
    let vars = rt.variable_store.list().unwrap();
    let dictionary = rt.dictionary_store.list().unwrap();
    assert_eq!(vars["order"], json!(["var"]));
    assert!(vars["profiles"]["env"].is_object());
    assert!(vars["profiles"].get("shop").is_none());
    assert_eq!(dictionary["order"], json!(["shop", "default"]));
    assert!(dictionary["profiles"].get("var").is_none());
    assert!(dictionary["profiles"].get("env").is_none());
    assert_eq!(dictionary["resolvedProfiles"]["shop"]["FROM_VAR"], "local");
    assert_eq!(
        vars["resolvedProfiles"]["var"]["FROM_DICT"],
        "fixture.invalid"
    );
    assert_eq!(
        std::fs::read(data.0.join(".variable-profiles.json.pre-split.bak")).unwrap(),
        original
    );
    let resolved = rt
        .registry
        .resolve(
            "rest",
            &json!({"url":"{{:HOST}}/{{var:FROM_DICT}}/{{shop:FROM_VAR}}"}),
        )
        .unwrap();
    assert_eq!(resolved["url"], "local.invalid/fixture.invalid/local");
    let dictionary_path = data.0.join(".dictionary-profiles.json");
    let dictionary_before = std::fs::read(&dictionary_path).unwrap();
    rt.variable_store
        .dispatch(&json!({"op":"save","name":"var","variables":{"BASE":"changed"}}))
        .unwrap();
    assert_eq!(std::fs::read(&dictionary_path).unwrap(), dictionary_before);
    let variables_before = std::fs::read(&path).unwrap();
    rt.dictionary_store
        .dispatch(&json!({"op":"save","name":"new","variables":{"REF":"{{var:BASE}}"}}))
        .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), variables_before);
    assert_eq!(
        data.runtime().dictionary_store.list().unwrap()["resolvedProfiles"]["new"]["REF"],
        "changed"
    );
    for message in [
        json!({"op":"save","name":"other","variables":{}}),
        json!({"op":"delete","name":"var"}),
        json!({"op":"rename","name":"var","newName":"other"}),
        json!({"op":"reorder","order":["var"]}),
    ] {
        assert!(rt.variable_store.dispatch(&message).is_err());
    }
    for name in ["var", "env"] {
        assert!(
            rt.dictionary_store
                .dispatch(&json!({"op":"save","name":name,"variables":{}}))
                .is_err()
        );
        assert!(
            rt.dictionary_store
                .dispatch(&json!({"op":"rename","name":"new","newName":name}))
                .is_err()
        );
    }
    let variable_disk: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(
        variable_disk["profiles"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        vec!["var"]
    );
}
#[test]
fn profile_migration_rejects_bad_data_and_dictionary_can_be_empty() {
    let data = Data::new();
    let path = data.0.join(".variable-profiles.json");
    std::fs::write(&path, b"{invalid").unwrap();
    assert!(data.runtime().variable_store.list().is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"{invalid");
    std::fs::write(&path, br#"{"profiles":{"default":{"X":"kept"}}}"#).unwrap();
    let rt = data.runtime();
    assert_eq!(
        rt.variable_store.list().unwrap()["profiles"]["var"],
        json!({})
    );
    assert_eq!(
        rt.dictionary_store.list().unwrap()["profiles"]["default"]["X"],
        "kept"
    );
    rt.dictionary_store
        .dispatch(&json!({"op":"delete","name":"default"}))
        .unwrap();
    assert_eq!(rt.dictionary_store.list().unwrap()["order"], json!([]));
    assert_eq!(
        data.runtime().dictionary_store.list().unwrap()["profiles"],
        json!({})
    );
    assert_eq!(rt.variable_store.list().unwrap()["order"], json!(["var"]));
}
#[test]
fn profile_migration_resumes_partial_writes_and_rejects_conflicts() {
    for conflict in [false, true] {
        let data = Data::new();
        let path = data.0.join(".variable-profiles.json");
        let legacy =
            serde_json::to_vec(&json!({"order":["shop"],"profiles":{"shop":{"HOST":"original"}}}))
                .unwrap();
        std::fs::write(&path, &legacy).unwrap();
        std::fs::write(
            data.0.join(".variable-profiles.json.pre-split.bak"),
            &legacy,
        )
        .unwrap();
        let dictionary_path = data.0.join(".dictionary-profiles.json");
        let partial=serde_json::to_vec(&json!({"version":2,"order":["shop"],"profiles":{"shop":{"HOST":if conflict {"different"} else {"original"}}}})).unwrap();
        std::fs::write(&dictionary_path, &partial).unwrap();
        let result = data.runtime().variable_store.list();
        if conflict {
            assert!(result.is_err());
            assert_eq!(std::fs::read(&path).unwrap(), legacy);
            assert_eq!(std::fs::read(&dictionary_path).unwrap(), partial);
        } else {
            assert!(result.is_ok());
            let variables: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(variables["version"], 2);
            assert_eq!(
                data.runtime().dictionary_store.list().unwrap()["profiles"]["shop"]["HOST"],
                "original"
            );
        }
    }
    let data = Data::new();
    let path = data.0.join(".variable-profiles.json");
    let original = br#"{"profiles":{"default":{}}}"#;
    std::fs::write(&path, original).unwrap();
    std::fs::write(
        data.0.join(".variable-profiles.json.pre-split.bak"),
        b"partial backup",
    )
    .unwrap();
    assert!(data.runtime().variable_store.list().is_err());
    assert_eq!(std::fs::read(path).unwrap(), original);
}
#[test]
fn independent_runtime_connections_serialize_profile_migration_and_mutations() {
    let data = Data::new();
    std::thread::scope(|scope| {
        for index in 0..8 {
            let data = &data;
            scope.spawn(move || {
                data.runtime().dictionary_store.dispatch(&json!({"op":"save","name":format!("profile-{index}"),"variables":{"X":index}})).unwrap();
            });
        }
    });
    assert_eq!(
        data.runtime().dictionary_store.list().unwrap()["order"]
            .as_array()
            .unwrap()
            .len(),
        8
    );
}
#[test]
fn variables_persist_resolve_priority_and_keep_env_readonly() {
    let data = Data::new();
    let rt = data.runtime();
    let vars = &rt.dictionary_store;
    vars.dispatch(&json!({"op":"save","name":"default","variables":{"name":"first","url":"https://{{second:host}}","same":"{{default:name}}"}})).unwrap();
    vars.dispatch(&json!({"op":"save","name":"second","variables":{"name":"second","host":"fixture.invalid"}})).unwrap();
    let resolved=rt.registry.resolve("rest",&json!({"method":"GET","url":"{{:url}}/{{default:same}}","headers":{"X":"{{second:name}}"}})).unwrap();
    assert_eq!(resolved["url"], "https://fixture.invalid/first");
    assert_eq!(resolved["headers"]["X"], "second");
    assert_eq!(
        data.runtime().dictionary_store.list().unwrap()["resolvedProfiles"]["default"]["url"],
        "https://fixture.invalid"
    );
    vars.dispatch(&json!({"op":"reorder","order":["second","default"]}))
        .unwrap();
    assert_eq!(
        rt.registry
            .resolve("rest", &json!({"method":"GET","url":"{{:name}}"}))
            .unwrap()["url"],
        "second"
    );
    assert!(
        vars.dispatch(&json!({"op":"reorder","order":["second","second"]}))
            .is_err()
    );
    for op in ["save", "delete", "rename"] {
        assert!(
            vars.dispatch(&json!({"op":op,"name":"env","variables":{},"newName":"x"}))
                .is_err()
        );
    }
    assert_eq!(
        vars.list().unwrap()["context"]["profiles"]["env"]["PATH"],
        std::env::var("PATH").unwrap()
    );
    let renamed = vars
        .dispatch(&json!({"op":"rename","name":"second","newName":"renamed"}))
        .unwrap();
    assert!(
        renamed["validation"]["default"]
            .as_str()
            .unwrap()
            .contains("second")
    );
    assert_eq!(renamed["order"], json!(["renamed", "default"]));
}
#[test]
fn variables_nested_json_preserves_keys_types_and_resolution_errors() {
    let data = Data::new();
    let rt = data.runtime();
    let sku = "SKU-\"blue\"\\path\nnext";
    rt.variable_store
        .dispatch(&json!({"op":"save","name":"var","variables":{
            "sku":sku,"enabled":true,"count":42
        }}))
        .unwrap();
    let input = json!({
        "skus":["{{var:sku}}"],
        "filter":{"items":[{"sku":"prefix-{{:sku}}","nested":[["{{var:sku}}"]]}]},
        "enabled":true,"count":42,"nothing":null,
        "booleanText":"{{var:enabled}}","numberText":"{{var:count}}",
        "{{var:sku}}":"literal key"
    });
    let expected = json!({
        "skus":[sku],
        "filter":{"items":[{"sku":format!("prefix-{sku}"),"nested":[[sku]]}]},
        "enabled":true,"count":42,"nothing":null,
        "booleanText":"true","numberText":"42",
        "{{var:sku}}":"literal key"
    });
    for (service, field) in [
        ("graphql", "variables"),
        ("rest", "headers"),
        ("grpc", "headers"),
        ("grpc", "requestMessage"),
        ("splunk", "headers"),
        ("terminal", "env"),
    ] {
        let mut params = json!({});
        params[field] = input.clone();
        assert_eq!(
            rt.registry.resolve(service, &params).unwrap()[field],
            expected,
            "{service}.{field}"
        );
        if field != "requestMessage" {
            params[field] = json!(input.to_string());
            let resolved = rt.registry.resolve(service, &params).unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(resolved[field].as_str().unwrap()).unwrap(),
                expected,
                "{service}.{field} JSON string"
            );
        }
    }
    assert_eq!(
        rt.registry
            .resolve("graphql", &json!({"variables":""}))
            .unwrap()["variables"],
        ""
    );
    assert_eq!(
        rt.registry
            .resolve("graphql", &json!({"variables":null}))
            .unwrap()["variables"],
        Value::Null
    );
    assert_eq!(
        rt.registry
            .resolve("splunk", &json!({"count":"{{var:count}}"}))
            .unwrap()["count"],
        "{{var:count}}"
    );
    let error = rt
        .registry
        .resolve(
            "graphql",
            &json!({"variables":{"items":[{"sku":"{{var:missing}}"}]}}),
        )
        .unwrap_err();
    assert!(error.message.contains("missing") && error.message.contains("var"));
    let error = rt
        .registry
        .resolve(
            "graphql",
            &json!({"variables":{"items":[{"sku":"{{unknown:sku}}"}]}}),
        )
        .unwrap_err();
    assert!(error.message.contains("Unknown profile"));
    let error = rt
        .registry
        .resolve("graphql", &json!({"variables":"{\"skus\":["}))
        .unwrap_err();
    assert!(error.message.starts_with("variables must be valid JSON"));
}

#[tokio::test]
async fn variables_graphql_materialization_matches_actual_network_payload() {
    let data = Data::new();
    let rt = data.runtime();
    let (server, seen) = server().await;
    let sku = "SKU-\"blue\"\\path\nnext";
    rt.variable_store
        .dispatch(&json!({"op":"save","name":"var","variables":{"sku":sku}}))
        .unwrap();
    let query = "query ($skus: [String]) {\n  products (skus: $skus) {\n    sku\n    images {\n      url\n    }\n  }\n}";
    let variables = json!({"skus":["{{var:sku}}"]});
    for variables in [variables.clone(), json!(variables.to_string())] {
        let params =
            json!({"url":format!("{}/graphql",server.url),"query":query,"variables":variables});
        let response = rt.registry.call("graphql", &params).await.unwrap();
        let body: Value = serde_json::from_str(response["body"].as_str().unwrap()).unwrap();
        assert_eq!(body["data"]["echo"], json!({"skus":[sku]}));
        let request = seen.lock().unwrap().last().unwrap().clone();
        let sent: Value = serde_json::from_str(request["body"].as_str().unwrap()).unwrap();
        assert_eq!(sent, json!({"query":query,"variables":{"skus":[sku]}}));
    }
    let error = rt
        .registry
        .call(
            "graphql",
            &json!({
                "url":format!("{}/graphql",server.url),"query":query,
                "variables":{"skus":["{{var:missing}}"]}
            }),
        )
        .await
        .unwrap_err();
    assert!(error.message.contains("missing"));
    assert_eq!(
        seen.lock().unwrap().len(),
        2,
        "A missing nested variable must fail before any network request"
    );
}

#[test]
fn variable_mutations_return_full_state_including_env_and_resolved_profiles() {
    let data = Data::new();
    let vars = data.runtime().dictionary_store;
    for message in [
        json!({"op":"save","name":"default","variables":{"BASE":"fixture","REF":"{{default:BASE}}"}}),
        json!({"op":"save","name":"second","variables":{}}),
        json!({"op":"reorder","order":["second","default"]}),
        json!({"op":"rename","name":"second","newName":"renamed"}),
        json!({"op":"delete","name":"renamed"}),
    ] {
        let reply = vars.dispatch(&message).unwrap();
        assert!(reply["context"]["profiles"]["env"].is_object());
        assert!(reply["profiles"].get("env").is_none());
        assert!(reply["profiles"].get("var").is_none());
        assert_eq!(reply["resolvedProfiles"]["default"]["REF"], "fixture");
        // Do not include real environment values in assertion failures.
        assert!(
            reply == vars.list().unwrap(),
            "Mutation must return the complete Variables state"
        );
    }
}

#[tokio::test]
async fn clipboard_is_not_exposed_through_shared_runtime_dispatch() {
    let data = Data::new();
    let reply = data
        .runtime()
        .dispatch(&json!({"id":1,"type":"clipboardRead"}))
        .await;
    assert_eq!(reply["ok"], false);
    assert!(reply["error"].as_str().unwrap().contains("Unknown service"));
    assert!(reply.get("result").is_none());
}

#[test]
fn variable_cycles_tolerant_display_and_global_fallback() {
    let data = Data::new();
    let vars = data.runtime().dictionary_store;
    vars.dispatch(&json!({"op":"save","name":"default","variables":{"value":"{{default:value}}"}}))
        .unwrap();
    vars.dispatch(
        &json!({"op":"save","name":"fallback","variables":{"value":"ok","indirect":"{{:value}}"}}),
    )
    .unwrap();
    let list = vars.list().unwrap();
    assert!(
        list["validation"]["default"]
            .as_str()
            .unwrap()
            .starts_with("Circular profile reference")
    );
    assert!(
        list["resolvedProfiles"]["default"]["value"]
            .as_str()
            .unwrap()
            .starts_with("⚠ ")
    );
    assert_eq!(list["resolvedProfiles"]["fallback"]["indirect"], "ok");
    assert!(
        vars.resolve_params("rest", &json!({"url":"literal"}))
            .is_err()
    );
    vars.dispatch(&json!({"op":"reorder","order":["fallback","default"]}))
        .unwrap();
    assert_eq!(
        vars.resolve_params("rest", &json!({"url":"{{:value}}"}))
            .unwrap()["url"],
        "ok"
    );
}
#[test]
fn cli_uses_contract_aliases_positionals_json_and_repeatable_options() {
    let config = services_contracts::service_config("splunk").unwrap();
    let args = [
        "http://fixture",
        "index=main",
        "-o",
        "from=-24h",
        "--options=app=search",
        "-o",
        "count=0",
        "-H",
        "{\"X\":\"v\"}",
    ]
    .map(str::to_string);
    let params = cli::parse_params(&config, &args).unwrap();
    assert_eq!(
        params,
        json!({"baseUrl":"http://fixture","headers":{"X":"v"},"search":"index=main","earliestTime":"-24h","app":"search","count":"0"})
    );
    assert!(cli::parse_params(&config, &["-o=unknown=x".into()]).is_err());
    let rest = services_contracts::service_config("rest").unwrap();
    assert_eq!(
        cli::parse_params(&rest, &["--endpoint=http://fixture".into()]).unwrap()["method"],
        "GET"
    );
    assert!(cli::parse_params(&rest, &["--endpoint=x".into(), "-H".into(), "bad".into()]).is_err());
}
#[test]
fn terminal_argv_quotes_and_env_match_shell_contract() {
    assert_eq!(
        terminal::parse_argv("ssh 'some host' -i \"a b\\\"c\" x\\ y").unwrap(),
        vec!["ssh", "some host", "-i", "a b\"c", "x y"]
    );
    assert_eq!(
        terminal::parse_argv("sh -c ''").unwrap(),
        vec!["sh", "-c", ""]
    );
    assert!(terminal::parse_argv("sh 'bad").is_err());
    assert!(terminal::parse_argv("sh \"bad").is_err());
    assert_eq!(
        terminal::build_command("printf \"$VALUE\"", &json!({"VALUE":"a'b"})),
        "export VALUE='a'\\''b'; printf \"$VALUE\""
    );
}
#[tokio::test]
async fn rest_and_graphql_fixture_requests_headers_errors_discovery() {
    let data = Data::new();
    let rt = data.runtime();
    let (server, seen) = server().await;
    let response=rt.registry.call("rest",&json!({"method":"post","url":format!("{}/echo",server.url),"headers":{"X-Token":"secret"},"body":"hello"})).await.unwrap();
    assert_eq!(response["status"], 201);
    assert_eq!(response["statusText"], "Created");
    assert_eq!(response["body"], "hello");
    assert_eq!(response["headers"]["x-response"], "fixture");
    assert_eq!(seen.lock().unwrap()[0]["token"], "secret");
    assert_eq!(response["debug"]["request"]["method"], "POST");
    assert!(response["durationMs"].is_number());
    let graphql=rt.registry.call("graphql",&json!({"url":format!("{}/graphql",server.url),"query":"query Echo { echo }","variables":{"a":1}})).await.unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(graphql["body"].as_str().unwrap()).unwrap()["data"]["echo"]["a"],
        1
    );
    let schema = rt
        .registry
        .discover("graphql", &json!({"url":format!("{}/graphql",server.url)}))
        .await
        .unwrap();
    assert_eq!(schema["queryType"], "Query");
    assert_eq!(
        schema["types"]["Query"]["fields"][0],
        json!({"name":"pets","typeName":"Pet","isObject":true,
            "type":{"kind":"NON_NULL","ofType":{"kind":"LIST","ofType":{"kind":"OBJECT","name":"Pet"}}},
            "args":[{"name":"filter","defaultValue":null,"type":{"kind":"INPUT_OBJECT","name":"PetFilter"}}]})
    );
    assert_eq!(schema["subscriptionType"], "Subscription");
    assert_eq!(schema["types"]["PetFilter"]["kind"], "INPUT_OBJECT");
    assert_eq!(
        schema["types"]["PetFilter"]["inputFields"][0]["defaultValue"],
        "OPEN"
    );
    assert_eq!(
        schema["types"]["PetFilter"]["inputFields"][1]["type"]["ofType"]["kind"],
        "NON_NULL"
    );
    assert_eq!(
        schema["types"]["State"]["enumValues"],
        json!(["OPEN", "CLOSED"])
    );
    assert_eq!(schema["types"]["ID"]["kind"], "SCALAR");
    assert_eq!(schema["types"]["PetLike"]["possibleTypes"], json!(["Pet"]));
    assert_eq!(
        schema["types"]["SearchResult"]["possibleTypes"],
        json!(["Pet", "Query"])
    );
    assert_eq!(schema["types"]["Pet"]["possibleTypes"], json!([]));
    assert_eq!(schema["directives"][0]["name"], "include");
    assert_eq!(
        schema["directives"][0]["args"][0]["type"]["ofType"]["name"],
        "Boolean"
    );
    assert_eq!(schema["directives"][0]["isRepeatable"], false);
    assert_eq!(
        schema["directives"][0]["locations"],
        json!(["FIELD", "FRAGMENT_SPREAD", "INLINE_FRAGMENT"])
    );
    let introspection = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request["path"] == "/graphql")
        .map(|request| serde_json::from_str::<Value>(request["body"].as_str().unwrap()).unwrap())
        .find(|request| request["query"].as_str().unwrap().contains("__schema"))
        .unwrap();
    for metadata in [
        "subscriptionType",
        "args",
        "inputFields",
        "enumValues",
        "defaultValue",
        "possibleTypes",
        "directives",
        "locations",
        "isRepeatable",
    ] {
        assert!(
            introspection["query"].as_str().unwrap().contains(metadata),
            "{metadata}"
        );
    }
    let api = rt
        .registry
        .discover("rest", &json!({"url":format!("{}/api/pets/1",server.url)}))
        .await
        .unwrap();
    assert_eq!(api["paths"], json!(["/api/pets"]));
    assert_eq!(api["methodsByPath"]["/api/pets"], json!(["POST"]));
    assert_eq!(
        api["bodySchemas"]["POST /api/pets"][1]["fields"][0],
        json!({"name":"id","kind":"bare","type":"integer"})
    );
    let protected = rt
        .registry
        .discover(
            "rest",
            &json!({
                "url":format!("{}/protected/pets",server.url),
                "headers":{"X-Token":"schema-token"}
            }),
        )
        .await
        .unwrap();
    assert_eq!(protected["paths"], json!(["/api/pets"]));
    assert!(
        seen.lock()
            .unwrap()
            .iter()
            .any(|request| request["path"] == "/protected/openapi.json"
                && request["token"] == "schema-token")
    );
    assert!(
        rt.registry
            .discover("graphql", &json!({"url":"not a URL"}))
            .await
            .unwrap_err()
            .message
            .contains("Invalid URL")
    );
    let error = rt
        .registry
        .call("rest", &json!({"url":"x"}))
        .await
        .unwrap_err();
    assert_eq!(error.message, "method is required");
    assert!(error.debug.is_some());
}
#[tokio::test]
async fn rest_discovery_supports_swagger_openapi_and_explicit_missing_schema() {
    let data = Data::new();
    let rt = data.runtime();
    let (server, seen) = server().await;
    for (prefix, base, expected) in [
        ("swagger-api", "/swagger-api", "/swagger-api/pets"),
        ("openapi31", "/openapi31", "/openapi31/pets"),
        ("api", "/api", "/api/pets"),
    ] {
        let before = seen.lock().unwrap().len();
        let schema = rt
            .registry
            .discover("rest", &json!({"url":format!("{}/{prefix}/",server.url)}))
            .await
            .unwrap();
        assert_eq!(schema["basePath"], base);
        assert_eq!(schema["paths"], json!([expected]));
        assert!(schema.get("warning").is_none());
        assert_eq!(
            seen.lock().unwrap().len() - before,
            7,
            "A discovered schema must stop further parent-path probes"
        );
        if prefix == "swagger-api" {
            assert_eq!(schema["methodsByPath"][expected], json!(["GET", "POST"]));
            assert_eq!(
                schema["bodySchemas"]["POST /swagger-api/pets"][0]["name"],
                "name"
            );
        }
    }
    for prefix in ["missing-api", "empty-api"] {
        let result = rt
            .registry
            .discover("rest", &json!({"url":format!("{}/{prefix}/",server.url)}))
            .await
            .unwrap();
        assert!(
            result["warning"]
                .as_str()
                .unwrap()
                .contains("No OpenAPI or Swagger")
        );
        assert!(result.get("paths").is_none());
        assert!(
            result["debug"]["request"]["attempts"]
                .as_array()
                .is_some_and(|attempts| !attempts.is_empty())
        );
    }
    let error = rt
        .registry
        .discover(
            "rest",
            &json!({"url":format!("{}/protected/pets",server.url)}),
        )
        .await
        .unwrap_err();
    assert!(error.message.contains("401"));
    let before = seen.lock().unwrap().len();
    for url in ["hhttp://fake.com", "https://", "not a URL"] {
        assert!(
            rt.registry
                .discover("rest", &json!({"url":url}))
                .await
                .is_err()
        );
    }
    assert_eq!(
        seen.lock().unwrap().len(),
        before,
        "Invalid URLs must not issue schema probes"
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    assert!(
        rt.registry
            .discover("rest", &json!({"url":format!("http://{address}/")}))
            .await
            .is_err()
    );
}
#[tokio::test]
async fn rest_discovery_prefers_magento_when_multiple_schemas_are_available() {
    let data = Data::new();
    let rt = data.runtime();
    let router = Router::new()
        .route(
            "/rest/default/schema",
            axum::routing::get(|request: Request| async move {
                assert_eq!(request.uri().query(), Some("services=all"));
                axum::Json(json!({"swagger":"2.0","paths":{"/magento":{"get":{}}}}))
            }),
        )
        .route(
            "/openapi.json",
            axum::routing::get(|| async {
                axum::Json(json!({"openapi":"3.0.0","paths":{"/generic":{"get":{}}}}))
            }),
        );
    let server = Server::new(router).await;
    let schema = rt
        .registry
        .discover("rest", &json!({"url":format!("{}/",server.url)}))
        .await
        .unwrap();
    assert_eq!(
        schema["specUrl"],
        format!("{}/rest/default/schema?services=all", server.url)
    );
    assert_eq!(schema["baseUrl"], server.url);
    assert_eq!(schema["paths"], json!(["/magento"]));
}
#[tokio::test]
async fn rest_discovery_finds_httpbin_style_swagger_at_spec_json() {
    let data = Data::new();
    let rt = data.runtime();
    let router = Router::new().route(
        "/spec.json",
        axum::routing::get(|| async {
            axum::Json(json!({"swagger":"2.0","basePath":"/","paths":{
                "/get":{"get":{}},"/post":{"post":{}}
            }}))
        }),
    );
    let server = Server::new(router).await;
    let schema = rt
        .registry
        .discover("rest", &json!({"url":format!("{}/",server.url)}))
        .await
        .unwrap();
    assert_eq!(schema["specUrl"], format!("{}/spec.json", server.url));
    assert_eq!(schema["paths"], json!(["/get", "/post"]));
    assert_eq!(schema["methodsByPath"]["/get"], json!(["GET"]));
    assert_eq!(schema["methodsByPath"]["/post"], json!(["POST"]));
    assert!(schema.get("warning").is_none());
}
#[tokio::test]
async fn graphql_discovery_does_not_cache_errors_as_empty_schemas() {
    let data = Data::new();
    let rt = data.runtime();
    let (server, seen) = server().await;
    let params = json!({"url":format!("{}/graphql-protected",server.url)});
    let error = rt.registry.discover("graphql", &params).await.unwrap_err();
    assert!(error.message.contains("Discovery requires X-Token"));
    let mut params = params;
    params["headers"] = json!({"X-Token":"discovery-token"});
    let schema = rt.registry.discover("graphql", &params).await.unwrap();
    assert_eq!(schema["types"]["Query"]["fields"][0]["name"], "pets");
    assert_eq!(
        seen.lock().unwrap().last().unwrap()["token"],
        "discovery-token"
    );
    let failures = Server::new(Router::new().fallback(|request: Request| async move {
        let (status, body) = match request.uri().path() {
            "/errors" => (
                200,
                json!({"errors":[{"message":"Introspection is disabled"}]}).to_string(),
            ),
            "/missing" => (200, json!({"data":{"__schema":null}}).to_string()),
            "/empty" => (
                200,
                json!({"data":{"__schema":{"queryType":{"name":"Query"},"types":[]}}}).to_string(),
            ),
            "/partial" => (
                200,
                json!({"data":{"__schema":{}},"errors":[{"message":"Partial schema denied"}]})
                    .to_string(),
            ),
            "/http" => (403, "Forbidden".into()),
            _ => (200, "not JSON".into()),
        };
        (axum::http::StatusCode::from_u16(status).unwrap(), body)
    }))
    .await;
    for (path, expected) in [
        ("errors", "Introspection is disabled"),
        ("missing", "no schema"),
        ("empty", "query fields"),
        ("partial", "Partial schema denied"),
        ("http", "403"),
        ("invalid", "JSON"),
    ] {
        let error = rt
            .registry
            .discover("graphql", &json!({"url":format!("{}/{path}",failures.url)}))
            .await
            .unwrap_err();
        assert!(
            error.message.contains(expected),
            "{path}: {}",
            error.message
        );
        assert!(
            error.debug.is_some(),
            "{path}: preserve discovery error diagnostics"
        );
    }
}
#[tokio::test]
async fn splunk_export_app_time_count_and_debug() {
    let data = Data::new();
    let (server, seen) = server().await;
    let result=data.runtime().registry.call("splunk",&json!({"baseUrl":server.url,"search":"index=fixture","app":"search","count":"0","earliestTime":"-1h","latestTime":"now","headers":{"Authorization":"Splunk fixture-token","X-Token":"splunk-fixture"}})).await.unwrap();
    assert_eq!(result["columns"], json!(["host", "count"]));
    assert_eq!(result["rows"][0]["host"], "fixture");
    assert_eq!(result["resultCount"], 1);
    assert_eq!(result["debug"]["export"]["streamComplete"], true);
    assert_eq!(result["debug"]["export"]["stoppedAfterCount"], false);
    assert!(result["debug"].get("job").is_none());
    assert_eq!(
        result["debug"]["response"]["messages"][0]["text"],
        "fixture message"
    );
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0]["token"], "splunk-fixture");
    assert_eq!(
        result["debug"]["request"]["headers"]["Authorization"],
        "Splunk fixture-token"
    );
    assert_eq!(
        seen[0]["path"],
        "/servicesNS/-/search/search/v2/jobs/export"
    );
    let form: std::collections::HashMap<_, _> =
        url::form_urlencoded::parse(seen[0]["body"].as_str().unwrap().as_bytes())
            .into_owned()
            .collect();
    assert_eq!(form["search"], "search index=fixture");
    assert_eq!(form["earliest_time"], "-1h");
    assert_eq!(form["latest_time"], "now");
    assert_eq!(form["output_mode"], "json");
    assert_eq!(form["preview"], "false");
    assert!(!form.contains_key("count"));
    assert!(!form.contains_key("exec_mode"));
}

#[tokio::test]
async fn splunk_preview_offset_returns_one_provisional_snapshot_before_eof() {
    use futures_util::StreamExt;
    for (preview, offset, count, expected) in [
        (true, 2, 2, vec!["b2", "b3"]),
        (true, 2, 0, vec!["b2", "b3"]),
        (true, 0, 30, vec!["a0"]),
        (false, 2, 2, vec!["f2", "f3"]),
    ] {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let forms = seen.clone();
        let server=Server::new(Router::new().fallback(move |request: Request| {
            let forms=forms.clone();
            async move {
                let request=to_bytes(request.into_body(),1024*1024).await.unwrap();
                let form=url::form_urlencoded::parse(&request).into_owned().collect::<std::collections::HashMap<_,_>>();
                forms.lock().unwrap().push(form);
                let mut body=String::new();
                for (prefix,preview,rows) in [("a",true,1),("b",true,4),("f",false,4)] {
                    for n in 0..rows {
                        body.push_str(&format!("{}\n",json!({
                            "preview":preview,"offset":n,"lastrow":n+1==rows,"result":{"n":format!("{prefix}{n}")}
                        })));
                    }
                }
                let stream=futures_util::stream::once(async {Ok::<_,std::io::Error>(body)})
                    .chain(futures_util::stream::pending());
                axum::body::Body::from_stream(stream).into_response()
            }
        })).await;
        let data = Data::new();
        let runtime = data.runtime();
        let result=tokio::time::timeout(Duration::from_secs(3),runtime.registry.call("splunk",&json!({
            "baseUrl":server.url,"search":"index=fixture","preview":preview,"offset":offset,"count":count
        }))).await.expect("preview/offset must not wait for EOF").unwrap();
        assert_eq!(
            result["rows"],
            json!(expected.iter().map(|n| json!({"n":n})).collect::<Vec<_>>())
        );
        assert_eq!(result["resultCount"], expected.len());
        assert_eq!(result["preview"], preview);
        assert_eq!(result["offset"], offset);
        assert_eq!(result["debug"]["export"]["rowsSkipped"], offset);
        assert_eq!(result["debug"]["export"]["streamComplete"], false);
        if preview {
            assert!(result["warning"].as_str().unwrap().contains("provisional"));
            assert_eq!(
                services_contracts::response_outcome("splunk", &result),
                "WARNING"
            );
        } else {
            assert!(result.get("warning").is_none());
            assert_eq!(
                services_contracts::response_outcome("splunk", &result),
                "OK"
            );
        }
        let forms = seen.lock().unwrap();
        assert_eq!(forms.len(), 1);
        assert_eq!(forms[0]["preview"], if preview { "true" } else { "false" });
        assert!(!forms[0].contains_key("offset") && !forms[0].contains_key("count"));
    }
}

#[tokio::test]
async fn splunk_offset_beyond_final_results_returns_an_empty_final_portion() {
    let data = Data::new();
    let (server, _) = server().await;
    let result = data
        .runtime()
        .registry
        .call(
            "splunk",
            &json!({
                "baseUrl":server.url,"search":"index=fixture","offset":"100","count":"30"
            }),
        )
        .await
        .unwrap();
    assert_eq!(result["rows"], json!([]));
    assert_eq!(result["resultCount"], 0);
    assert_eq!(result["preview"], false);
    assert_eq!(result["debug"]["export"]["streamComplete"], true);
    assert_eq!(result["debug"]["export"]["rowsSkipped"], 1);
}

#[tokio::test]
async fn splunk_export_keeps_network_error_causes_in_debug() {
    let data = Data::new();
    let error = data
        .runtime()
        .registry
        .call(
            "splunk",
            &json!({
                "baseUrl":"http://127.0.0.1:0","search":"index=fixture"
            }),
        )
        .await
        .unwrap_err();
    assert!(error.message.contains("Request failed"));
    assert!(error.debug.unwrap()["connection"]["error"]["cause"].is_object());
}

#[tokio::test]
async fn splunk_export_reports_http_and_stream_errors_without_retrying_or_returning_partial_rows() {
    for (status, body, expected) in [
        (
            401,
            r#"{"messages":[{"type":"ERROR","text":"access denied"}]}"#,
            "access denied",
        ),
        (500, "upstream unavailable", "HTTP 500"),
        (
            200,
            "{\"preview\":false,\"result\":{\"x\":1}}\n{\"preview\":false,\"result\":",
            "truncated",
        ),
        (
            200,
            "{\"preview\":false,\"result\":{\"x\":1}}\n{\"messages\":[{\"type\":\"ERROR\",\"text\":\"bad search\"}]}\n",
            "bad search",
        ),
    ] {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = calls.clone();
        let server = Server::new(Router::new().fallback(move || {
            seen.fetch_add(1, Ordering::SeqCst);
            async move { (axum::http::StatusCode::from_u16(status).unwrap(), body) }
        }))
        .await;
        let data = Data::new();
        let runtime = data.runtime();
        let error = runtime
            .registry
            .call(
                "splunk",
                &json!({
                    "baseUrl":server.url,"search":"index=fixture","count":"0"
                }),
            )
            .await
            .unwrap_err();
        assert!(error.message.contains(expected), "{error}");
        let debug = error.debug.unwrap();
        assert_eq!(debug["response"]["status"], status);
        assert_eq!(debug["connection"]["events"].as_array().unwrap().len(), 2);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        for (invalid, label) in [
            (json!({"count":"-1"}), "Count"),
            (json!({"offset":"-1"}), "Offset"),
            (json!({"preview":"yes"}), "Preview"),
        ] {
            let mut params = json!({"baseUrl":server.url,"search":"index=fixture"});
            params
                .as_object_mut()
                .unwrap()
                .extend(invalid.as_object().unwrap().clone());
            assert!(
                runtime
                    .registry
                    .call("splunk", &params)
                    .await
                    .unwrap_err()
                    .message
                    .contains(label)
            );
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "invalid Count must not consume an API call"
        );
    }
}

#[tokio::test]
async fn splunk_export_legacy_fallback_is_explicit_paced_and_only_for_404() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let seen = calls.clone();
    let server = Server::new(Router::new().fallback(move |request: Request| {
        seen.lock()
            .unwrap()
            .push((request.uri().path().to_string(), std::time::Instant::now()));
        async {
            if request.uri().path().contains("/v2/") {
                return axum::http::StatusCode::NOT_FOUND.into_response();
            }
            fixture(State(Arc::new(Mutex::new(vec![]))), request).await
        }
    }))
    .await;
    let data = Data::new();
    let result = data
        .runtime()
        .registry
        .call(
            "splunk",
            &json!({
                "baseUrl":format!("{}/proxy",server.url),"app":"some app","search":"index=fixture"
            }),
        )
        .await
        .unwrap();
    assert_eq!(result["rows"][0]["host"], "fixture");
    assert_eq!(result["debug"]["export"]["legacyFallbacks"], 1);
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[0].0,
        "/proxy/servicesNS/-/some%20app/search/v2/jobs/export"
    );
    assert_eq!(
        calls[1].0,
        "/proxy/servicesNS/-/some%20app/search/jobs/export"
    );
    assert!(calls[1].1.duration_since(calls[0].1) >= Duration::from_secs(13));
}

#[tokio::test]
async fn splunk_export_returns_first_portion_before_eof_and_closes_the_connection() {
    struct Closed(Arc<std::sync::atomic::AtomicBool>);
    impl Drop for Closed {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    for (count, expected) in [(Some(json!("2")), 2), (Some(json!(2.0)), 2), (None, 100)] {
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (disconnected, seen) = (closed.clone(), calls.clone());
        let server = Server::new(Router::new().fallback(move || {
            seen.fetch_add(1, Ordering::SeqCst);
            let guard = Closed(disconnected.clone());
            async move {
                let mut body = "{\"preview\":true,\"result\":{\"host\":\"not final\"},\"lastrow\":true}\n".to_string();
                for n in 0..101 {
                    body.push_str(&format!("{}\n",json!({"preview":false,"offset":n,"result":{"host":format!("сервер🦀-{n}")}})));
                }
                let stream = futures_util::stream::unfold((Some(body), guard), |(body, guard)| async {
                    match body {
                        Some(body) => Some((Ok::<_,std::io::Error>(body), (None, guard))),
                        None => std::future::pending().await,
                    }
                });
                axum::body::Body::from_stream(stream).into_response()
            }
        })).await;
        let data = Data::new();
        let mut params = json!({"baseUrl":server.url,"search":"index=fixture | sort - _time"});
        if let Some(count) = count {
            params["count"] = count;
        }
        let runtime = data.runtime();
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            runtime.registry.call("splunk", &params),
        )
        .await
        .expect("must return without EOF or full search completion")
        .unwrap();
        assert_eq!(result["rows"].as_array().unwrap().len(), expected);
        assert_eq!(result["rows"][0]["host"], "сервер🦀-0");
        assert_eq!(result["resultCount"], expected);
        assert_eq!(result["debug"]["export"]["stoppedAfterCount"], true);
        assert_eq!(result["debug"]["export"]["streamComplete"], false);
        assert_eq!(result["debug"]["export"]["ignoredPreviewRecords"], 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        tokio::time::timeout(Duration::from_secs(3), async {
            while !closed.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("dropping the unfinished response must close its connection");
    }
}

#[tokio::test]
async fn splunk_export_zero_reads_to_eof_and_stop_drops_a_pending_stream() {
    let data = Data::new();
    let server = Server::new(Router::new().fallback(|| async {
        let stream = futures_util::stream::unfold(0, |index| async move {
            match index {
                0 => Some((
                    Ok::<_, std::io::Error>("{\"preview\":false,\"result\":{\"n\":1}}\n"),
                    1,
                )),
                1 => {
                    tokio::time::sleep(Duration::from_millis(75)).await;
                    Some((
                        Ok("{\"preview\":false,\"result\":{\"n\":2},\"lastrow\":true}\n"),
                        2,
                    ))
                }
                _ => None,
            }
        });
        axum::body::Body::from_stream(stream).into_response()
    }))
    .await;
    let started = std::time::Instant::now();
    let result = data
        .runtime()
        .registry
        .call(
            "splunk",
            &json!({
                "baseUrl":server.url,"search":"index=fixture","count":"0"
            }),
        )
        .await
        .unwrap();
    assert!(started.elapsed() >= Duration::from_millis(75));
    assert_eq!(result["rows"], json!([{"n":1},{"n":2}]));
    assert_eq!(result["debug"]["export"]["streamComplete"], true);

    let headers_sent = Arc::new(tokio::sync::Notify::new());
    let notify = headers_sent.clone();
    let server = Server::new(Router::new().fallback(move || {
        notify.notify_one();
        async {
            axum::body::Body::from_stream(futures_util::stream::pending::<
                std::result::Result<String, std::io::Error>,
            >())
            .into_response()
        }
    }))
    .await;
    let runtime = data.runtime();
    let cancel = CancellationToken::new();
    let stopped = cancel.clone();
    let task = tokio::spawn(async move {
        runtime
            .registry
            .call_cancellable(
                "splunk",
                &json!({
                    "baseUrl":server.url,"search":"index=fixture","count":"0"
                }),
                cancel,
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(3), headers_sent.notified())
        .await
        .unwrap();
    stopped.cancel();
    let result = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(result["cancelled"], true);
}
#[tokio::test]
async fn splunk_server_quota_is_shared_across_processes_paths_and_apps_not_other_servers() {
    type Calls = Arc<Mutex<Vec<(usize, std::time::Instant)>>>;
    async fn clock_server(id: usize, calls: Calls) -> Server {
        Server::new(Router::new().fallback(move |request: Request| {
            let calls = calls.clone();
            async move {
                calls.lock().unwrap().push((id, std::time::Instant::now()));
                fixture(State(Arc::new(Mutex::new(vec![]))), request).await
            }
        }))
        .await
    }
    let data = Data::new();
    let calls = Calls::default();
    let shared = clock_server(0, calls.clone()).await;
    let independent = clock_server(1, calls.clone()).await;
    let runtime = data.runtime();
    let other_runtime = data.runtime();
    let params =
        json!({"baseUrl":format!("{}/proxy", shared.url),"search":"index=main","app":"search"});
    let other_params = json!({"baseUrl":independent.url,"search":"index=main"});
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_services-devtools"));
    command
        .env("SERVICES_DATA_DIR", &data.0)
        .args(["cli", "splunk", &shared.url, "index=cli", "-o", "app=other"])
        .kill_on_drop(true);
    let (result, child, other_result) = tokio::join!(
        runtime.registry.call("splunk", &params),
        command.output(),
        other_runtime.registry.call("splunk", &other_params)
    );
    assert_eq!(result.unwrap()["rows"][0]["host"], "fixture");
    let child = child.unwrap();
    assert!(
        child.status.success(),
        "{}",
        String::from_utf8_lossy(&child.stderr)
    );
    assert!(String::from_utf8_lossy(&child.stdout).contains("fixture"));
    assert_eq!(other_result.unwrap()["rows"][0]["host"], "fixture");
    let calls = calls.lock().unwrap();
    let shared: Vec<_> = calls
        .iter()
        .filter(|(id, _)| *id == 0)
        .map(|(_, at)| *at)
        .collect();
    let independent: Vec<_> = calls
        .iter()
        .filter(|(id, _)| *id == 1)
        .map(|(_, at)| *at)
        .collect();
    assert_eq!(shared.len(), 2);
    assert_eq!(independent.len(), 1);
    for timestamps in [&shared, &independent] {
        for pair in timestamps.windows(2) {
            assert!(
                pair[1].duration_since(pair[0]) >= Duration::from_secs(13),
                "{timestamps:?}"
            );
        }
        for start in timestamps {
            assert!(
                timestamps
                    .iter()
                    .filter(
                        |at| **at >= *start && at.duration_since(*start) < Duration::from_secs(60)
                    )
                    .count()
                    <= 5
            );
        }
    }
    assert!(
        shared[0]
            .max(independent[0])
            .duration_since(shared[0].min(independent[0]))
            < Duration::from_secs(5),
        "Different servers must not wait for each other's quota"
    );
}

#[tokio::test]
async fn splunk_redirects_cannot_bypass_the_server_quota() {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = calls.clone();
    let server = Server::new(Router::new().fallback(move || {
        seen.fetch_add(1, Ordering::SeqCst);
        async {
            (
                axum::http::StatusCode::TEMPORARY_REDIRECT,
                [("location", "/redirect-target")],
            )
        }
    }))
    .await;
    let data = Data::new();
    let error = data
        .runtime()
        .registry
        .call(
            "splunk",
            &json!({"baseUrl":server.url,"search":"index=main"}),
        )
        .await
        .unwrap_err();
    assert!(
        error.message.contains("automatic redirects are disabled"),
        "{error}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn terminal_stream_and_cancellation_reap_own_process_group() {
    let (tx, mut rx) = mpsc::channel(128);
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let params = json!({"endpoint":"sh -c","env":{"VALUE":"a'b"},"command":"printf '%s' \"$VALUE\"; printf err >&2; sleep 30"});
    let task = tokio::spawn(async move { terminal::run(&params, token, tx).await.unwrap() });
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
#[cfg(unix)]
#[test]
#[ignore = "subprocess fixture for detached Terminal output tests"]
fn terminal_detached_pipe_fixture() {
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
async fn terminal_detached_pipes_finish_and_remain_stoppable_after_parent_exit() {
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
        "'{}' --exact terminal_detached_pipe_fixture --ignored --nocapture",
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
        let mut task = tokio::spawn(async move { terminal::run(&params, token, tx).await });
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
            assert_eq!(
                services_contracts::response_outcome("terminal", &result),
                "WARNING"
            );
        } else {
            assert!(
                stopped_at.elapsed() < Duration::from_secs(3),
                "{mode}: {result}"
            );
            assert_eq!(
                services_contracts::response_outcome("terminal", &result),
                "Stopped"
            );
        }
        while rx.recv().await.is_some() {}
    }
}

#[tokio::test]
async fn terminal_preserves_buffered_output_after_normal_exit() {
    let (tx, mut rx) = mpsc::channel(128);
    let params = json!({"endpoint":"sh -c","command":"head -c 262144 /dev/zero | tr '\\000' x"});
    let task =
        tokio::spawn(async move { terminal::run(&params, CancellationToken::new(), tx).await });
    let mut output = String::new();
    while let Some(event) = rx.recv().await {
        output.push_str(event["data"].as_str().unwrap());
    }
    let result = task.await.unwrap().unwrap();
    assert_eq!(result["code"], 0);
    assert_ne!(result["outputIncomplete"], true);
    assert_eq!(output, "x".repeat(262144));
}

async fn read_frame(reader: &mut (impl tokio::io::AsyncRead + Unpin)) -> Value {
    let n = reader.read_u32_le().await.unwrap();
    assert!(n as usize <= services_contracts::native_messaging::MAX_HOST_MESSAGE_BYTES);
    let mut bytes = vec![0; n as usize];
    reader.read_exact(&mut bytes).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}
#[tokio::test]
async fn native_large_responses_preserve_terminal_and_support_old_clients() {
    use services_contracts::native_messaging::ResponseAssembler;
    let data = Data::new();
    let body = format!("head{}tail", "🦀\"\\\n".repeat(180_000));
    let response = body.clone();
    let server = Server::new(Router::new().route(
        "/large",
        axum::routing::get(move || {
            let response = response.clone();
            async move { response }
        }),
    ))
    .await;
    let (mut input, input_host) = tokio::io::duplex(65536);
    let (output_host, mut output) = tokio::io::duplex(65536);
    let host = tokio::spawn(native::run(data.runtime(), input_host, output_host));
    let roundtrip = async {
        input.write_all(&encode(&json!({"id":1,"service":"terminal","params":{"endpoint":"sh -c","command":"printf ready; sleep 30"}})).unwrap()).await.unwrap();
        assert_eq!(read_frame(&mut output).await["event"]["data"], "ready");
        let request = |id| json!({"id":id,"service":"rest","params":{"method":"GET","url":format!("{}/large",server.url)}});
        input
            .write_all(&encode(&request(2)).unwrap())
            .await
            .unwrap();
        let error = read_frame(&mut output).await;
        assert_eq!(error["id"], 2);
        assert_eq!(error["ok"], false);
        assert!(
            error["error"]
                .as_str()
                .unwrap()
                .contains("Update the extension")
        );
        input
            .write_all(
                &encode(&json!({"id":3,"type":"health","capabilities":{"chunkedResponses":true}}))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(read_frame(&mut output).await["id"], 3);
        input
            .write_all(&encode(&request(4)).unwrap())
            .await
            .unwrap();
        input
            .write_all(&encode(&json!({"id":5,"type":"health"})).unwrap())
            .await
            .unwrap();
        let mut assembler = ResponseAssembler::default();
        let mut result = None;
        let mut healthy = false;
        let mut chunks = 0;
        while result.is_none() || !healthy {
            let message = read_frame(&mut output).await;
            if message["id"] == 4 {
                assert_eq!(message["type"], "nativeChunk");
                chunks += 1;
                if let Some(value) = assembler.push(&message).unwrap() {
                    result = Some(value);
                }
            } else {
                assert_eq!(message["id"], 5, "Unrelated Terminal must not exit");
                assert_eq!(message["result"]["protocolVersion"], 1);
                healthy = true;
            }
        }
        assert!(chunks > 1);
        assert_eq!(result.unwrap()["result"]["body"], body);
        input
            .write_all(&encode(&json!({"id":1,"type":"stop"})).unwrap())
            .await
            .unwrap();
        let exit = read_frame(&mut output).await;
        assert_eq!(exit["id"], 1);
        assert_eq!(exit["type"], "exit");
        assert_eq!(exit["info"]["cancelled"], true);
        input.shutdown().await.unwrap();
    };
    tokio::time::timeout(Duration::from_secs(10), roundtrip)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(4), host)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn native_writer_failure_returns_without_waiting_for_more_input() {
    let data = Data::new();
    let (mut input, input_host) = tokio::io::duplex(65536);
    let (output_host, output) = tokio::io::duplex(65536);
    drop(output);
    let host = tokio::spawn(native::run(data.runtime(), input_host, output_host));
    input
        .write_all(&encode(&json!({"id":1,"type":"health"})).unwrap())
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(4), host)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
}

#[tokio::test]
async fn native_concurrent_ids_terminal_stop_vars_and_health_eof() {
    let data = Data::new();
    let (server, _) = server().await;
    let (mut input, input_host) = tokio::io::duplex(65536);
    let (output_host, mut output) = tokio::io::duplex(65536);
    let host = tokio::spawn(native::run(data.runtime(), input_host, output_host));
    let slow = json!({"id":"slow","service":"rest","params":{"method":"POST","url":format!("{}/slow",server.url),"body":"body"}});
    input.write_all(&encode(&slow).unwrap()).await.unwrap();
    let health = encode(&json!({"id":"health","type":"health"})).unwrap();
    for bytes in health.chunks(2) {
        input.write_all(bytes).await.unwrap();
    }
    let first = read_frame(&mut output).await;
    assert_eq!(first["id"], "health");
    assert_eq!(first["result"]["protocolVersion"], 1);
    let slow = read_frame(&mut output).await;
    assert_eq!(slow["id"], "slow");
    assert_eq!(slow["result"]["status"], 201);
    input.write_all(&encode(&json!({"id":3,"service":"terminal","params":{"endpoint":"sh -c","command":"printf ready; sleep 30"}})).unwrap()).await.unwrap();
    let data = read_frame(&mut output).await;
    assert_eq!(data["type"], "data");
    assert_eq!(data["event"]["data"], "ready");
    input
        .write_all(&encode(&json!({"id":3,"type":"stop"})).unwrap())
        .await
        .unwrap();
    let exit = tokio::time::timeout(Duration::from_secs(4), read_frame(&mut output))
        .await
        .unwrap();
    assert_eq!(exit["type"], "exit");
    assert_eq!(exit["info"]["signal"], "SIGTERM");
    input
        .write_all(&encode(&json!({"id":4,"service":"vars","op":"list"})).unwrap())
        .await
        .unwrap();
    assert!(read_frame(&mut output).await["result"]["profiles"]["var"].is_object());
    input
        .write_all(&encode(&json!({"id":5,"type":"health"})).unwrap())
        .await
        .unwrap();
    input.shutdown().await.unwrap();
    assert_eq!(read_frame(&mut output).await["id"], 5);
    host.await.unwrap().unwrap();
}
#[tokio::test]
async fn native_eof_cancels_terminal_and_flushes_exit() {
    let data = Data::new();
    let (mut input, input_host) = tokio::io::duplex(65536);
    let (output_host, mut output) = tokio::io::duplex(65536);
    let host = tokio::spawn(native::run(data.runtime(), input_host, output_host));
    input.write_all(&encode(&json!({"id":1,"service":"terminal","params":{"endpoint":"sh -c","command":"printf ready; sleep 30"}})).unwrap()).await.unwrap();
    assert_eq!(read_frame(&mut output).await["type"], "data");
    input.shutdown().await.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(4), read_frame(&mut output))
            .await
            .unwrap()["type"],
        "exit"
    );
    host.await.unwrap().unwrap();
}
#[tokio::test]
async fn native_stop_and_eof_cancel_ordinary_requests_without_touching_siblings() {
    let data = Data::new();
    let (server, seen) = server().await;
    let (mut input, input_host) = tokio::io::duplex(65536);
    let (output_host, mut output) = tokio::io::duplex(65536);
    let host = tokio::spawn(native::run(data.runtime(), input_host, output_host));
    for (id, path) in [(1, "cancel-slow"), (2, "slow")] {
        input.write_all(&encode(&json!({"id":id,"service":"rest","params":{"method":"POST","url":format!("{}/{path}",server.url),"body":"fixture"}})).unwrap()).await.unwrap();
    }
    tokio::time::timeout(Duration::from_secs(2), async {
        while seen.lock().unwrap().len() < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    input
        .write_all(&encode(&json!({"id":1,"type":"stop"})).unwrap())
        .await
        .unwrap();
    let mut replies = vec![];
    for _ in 0..2 {
        replies.push(
            tokio::time::timeout(Duration::from_secs(2), read_frame(&mut output))
                .await
                .unwrap(),
        );
    }
    assert_eq!(
        replies.iter().find(|reply| reply["id"] == 1).unwrap()["result"]["cancelled"],
        true
    );
    assert_eq!(
        replies.iter().find(|reply| reply["id"] == 2).unwrap()["result"]["status"],
        201
    );
    for id in [3, 4] {
        input.write_all(&encode(&json!({"id":id,"service":"rest","params":{"method":"GET","url":format!("{}/cancel-slow",server.url)}})).unwrap()).await.unwrap();
    }
    tokio::time::timeout(Duration::from_secs(2), async {
        while seen.lock().unwrap().len() < 4 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    input.shutdown().await.unwrap();
    for _ in 0..2 {
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), read_frame(&mut output))
                .await
                .unwrap()["result"]["cancelled"],
            true
        );
    }
    tokio::time::timeout(Duration::from_secs(4), host)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
#[tokio::test]
async fn web_scope_close_cancels_only_its_window_and_rejects_late_requests() {
    let data = Data::new();
    let runtime = data.runtime();
    let operations = runtime.operations.clone();
    let web = Server::new(services_native::adapters::web::router(runtime)).await;
    let (fixture, seen) = server().await;
    let scope = "00000000-0000-0000-0000-000000000003";
    let owner = operations.open(scope).await.unwrap();
    let client = reqwest::Client::new();
    let request = client
        .post(format!("{}/api/services/rest/request", web.url))
        .header("Origin", &web.url)
        .header("x-services-scope", scope)
        .header("x-services-request", "1")
        .json(&json!({"method":"GET","url":format!("{}/cancel-slow",fixture.url)}))
        .send();
    let pending =
        tokio::spawn(async move { request.await.unwrap().json::<Value>().await.unwrap() });
    tokio::time::timeout(Duration::from_secs(2), async {
        while seen.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    operations.close(scope).await;
    assert!(owner.cancel.is_cancelled());
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), pending)
            .await
            .unwrap()
            .unwrap()["cancelled"],
        true
    );
    let result = client
        .post(format!("{}/api/services/rest/request", web.url))
        .header("Origin", &web.url)
        .header("x-services-scope", scope)
        .header("x-services-request", "2")
        .json(&json!({"method":"GET","url":format!("{}/echo",fixture.url)}))
        .send()
        .await
        .unwrap();
    assert_eq!(result.status(), 400);
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "Late requests must not reach the remote service"
    );
}
#[tokio::test]
async fn web_routes_variables_hints_console_service_and_discover() {
    let data = Data::new();
    let web = Server::new(services_native::adapters::web::router(data.runtime())).await;
    let (fixture, _) = server().await;
    let client = reqwest::Client::new();
    let result = client
        .post(format!("{}/api/services/rest/request", web.url))
        .json(&json!({"method":"POST","url":format!("{}/echo",fixture.url),"body":"web"}))
        .send()
        .await
        .unwrap();
    assert_eq!(result.status(), 200);
    assert_eq!(result.json::<Value>().await.unwrap()["body"], "web");
    let result = client
        .post(format!("{}/dictionary/api/profiles/second", web.url))
        .json(&json!({"variables":{"a":"b"}}))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(result["profiles"]["second"]["a"], "b");
    client
        .post(format!("{}/dictionary/api/profiles/second/rename", web.url))
        .json(&json!({"name":"renamed"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let result = client
        .put(format!("{}/api/console/hints/rest", web.url))
        .json(&json!({"text":"#endpoint\nhttp://fixture\n"}))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(result["text"], "#endpoint\nhttp://fixture\n");
    let result = client
        .get(format!("{}/api/console/hints/graphql", web.url))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert!(!result["text"].as_str().unwrap().contains("fixture"));
    let result = client
        .post(format!("{}/api/console/discover", web.url))
        .json(&json!({"service":"splunk"}))
        .send()
        .await
        .unwrap();
    assert_eq!(result.status(), 200);
    assert!(result.json::<Value>().await.unwrap()["error"].is_string());
    let result = client
        .post(format!("{}/api/console", web.url))
        .json(&json!({"service":"nope"}))
        .send()
        .await
        .unwrap();
    assert_eq!(result.status(), 400);
    assert!(
        result.json::<Value>().await.unwrap()["error"]
            .as_str()
            .unwrap()
            .contains("Unknown service")
    );
}
#[test]
fn openapi_fields_preserve_date_formats_through_references() {
    let spec =
        json!({"components":{"schemas":{"Timestamp":{"type":"string","format":"date-time"}}}});
    let schema = json!({"properties":{
        "publishDate":{"$ref":"#/components/schemas/Timestamp"},
        "startDate":{"type":"string","format":"date"},
        "title":{"type":"string"}
    }});
    assert_eq!(
        openapi::fields(&spec, &schema, 0),
        vec![
            json!({"name":"publishDate","kind":"string","format":"date-time"}),
            json!({"name":"startDate","kind":"string","format":"date"}),
            json!({"name":"title","kind":"string"})
        ]
    );
}
#[test]
fn openapi_enum_fields_preserve_values_types_references_and_array_items() {
    let spec = json!({"definitions":{"State":{"type":"string","enum":["pending","active","quote\"slash\\"]},
        "States":{"type":"array","items":{"$ref":"#/definitions/State"}}}});
    let schema = json!({"properties":{
        "state":{"$ref":"#/definitions/State"},
        "priority":{"type":"integer","enum":[1,2,3]},
        "enabled":{"type":"boolean","enum":[true,false]},
        "nullable":{"enum":["active",null]},
        "states":{"$ref":"#/definitions/States"},
        "nested":{"type":"object","properties":{"state":{"$ref":"#/definitions/State"}}}
    }});
    let fields = openapi::fields(&spec, &schema, 0);
    let field = |name: &str| fields.iter().find(|field| field["name"] == name).unwrap();
    assert_eq!(field("state")["enum"], spec["definitions"]["State"]["enum"]);
    assert_eq!(field("state")["type"], "string");
    assert_eq!(field("priority")["type"], "integer");
    assert_eq!(field("priority")["enum"], json!([1, 2, 3]));
    assert_eq!(field("enabled")["enum"], json!([true, false]));
    assert_eq!(field("nullable")["enum"], json!(["active", null]));
    assert_eq!(
        field("states")["items"]["enum"],
        spec["definitions"]["State"]["enum"]
    );
    assert_eq!(
        field("nested")["fields"][0]["enum"],
        spec["definitions"]["State"]["enum"]
    );
    assert!(field("states")["items"].get("name").is_none());
}
#[test]
fn openapi_required_body_fields_preserve_types_and_nested_references() {
    for version in [json!({"swagger":"2.0"}), json!({"openapi":"3.0.3"})] {
        let mut spec = json!({
            "definitions":{"Body":{
                "type":"object","required":["username","count","enabled","items","settings","id"],
                "properties":{
                    "username":{"type":"string"},
                    "count":{"type":"integer"},
                    "enabled":{"type":"boolean"},
                    "items":{"type":"array","items":{"type":"string"}},
                    "settings":{"$ref":"#/definitions/Settings"},
                    "id":{"type":"integer","readOnly":true},
                    "optional":{"type":"string"}
                }
            },"Settings":{"type":"object","required":["locale"],"properties":{
                "locale":{"type":"string"},"optionalChild":{"type":"string"}
            }}},
            "paths":{"/token":{"post":{}}}
        });
        spec.as_object_mut()
            .unwrap()
            .extend(version.as_object().unwrap().clone());
        let reference = json!({"$ref":"#/definitions/Body"});
        if spec["swagger"] == "2.0" {
            spec["paths"]["/token"]["post"]["parameters"] =
                json!([{"name":"body","in":"body","schema":reference}]);
        } else {
            spec["paths"]["/token"]["post"]["requestBody"] =
                json!({"content":{"application/json":{"schema":reference}}});
        }
        let summary = openapi::summarize(&spec);
        let fields = summary["bodySchemas"]["POST /token"].as_array().unwrap();
        let field = |name: &str| fields.iter().find(|field| field["name"] == name).unwrap();
        assert_eq!(field("username")["required"], true);
        assert_eq!(field("count")["type"], "integer");
        assert_eq!(field("enabled")["type"], "boolean");
        assert_eq!(field("id")["readOnly"], true);
        assert!(field("optional").get("required").is_none());
        let nested = field("settings")["fields"].as_array().unwrap();
        assert_eq!(
            nested
                .iter()
                .find(|field| field["name"] == "locale")
                .unwrap()["required"],
            true
        );
        assert!(
            nested
                .iter()
                .find(|field| field["name"] == "optionalChild")
                .unwrap()
                .get("required")
                .is_none()
        );
    }
}
#[test]
fn openapi_refs_swagger_parameters_and_cycles_are_bounded() {
    let spec = json!({"basePath":"/rest/","parameters":[{"name":"X-Global","in":"header"}],"definitions":{"Node":{"properties":{"id":{"type":"integer"},"child":{"$ref":"#/definitions/Node"}}}},"paths":{"/nodes":{"parameters":[{"in":"header","name":"X-Path"}],"post":{"parameters":[{"in":"body","schema":{"$ref":"#/definitions/Node"}}]}}}});
    let summary = openapi::summarize(&spec);
    assert_eq!(summary["paths"], json!(["/rest/nodes"]));
    assert_eq!(summary["headers"], json!(["X-Global", "X-Path"]));
    assert_eq!(
        summary["bodySchemas"]["POST /rest/nodes"][0]["kind"],
        "bare"
    );
    assert!(summary.to_string().len() < 4096);
}
#[test]
fn mysql_values_and_discovery_match_grid_shapes() {
    assert_eq!(mysql::value_json(mysql_async::Value::NULL), Value::Null);
    assert_eq!(mysql::value_json(mysql_async::Value::Int(-3)), json!(-3));
    assert_eq!(
        mysql::value_json(mysql_async::Value::Bytes(b"decimal".to_vec())),
        json!("decimal")
    );
    let schema = mysql::discovery_rows(vec![
        ("pets".into(), "id".into(), "int".into(), "PRI".into()),
        (
            "pets".into(),
            "name".into(),
            "varchar(20)".into(),
            "".into(),
        ),
    ]);
    assert_eq!(
        schema["tables"][0]["columns"][0],
        json!({"name":"id","type":"int","primaryKey":true})
    );
    assert_eq!(schema["tables"][0]["columns"][1]["primaryKey"], false);
}

#[test]
fn binary_default_native_fragmented_health_eof_cli_help_and_version() {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    let data = Data::new();
    let binary = env!("CARGO_BIN_EXE_services-devtools");
    let mut child = Command::new(binary)
        .env_clear()
        .env("SERVICES_DATA_DIR", &data.0)
        .env("HOME", &data.0)
        .env("SHELL", "/bin/sh")
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for chunk in encode(&json!({"id":42,"type":"health"})).unwrap().chunks(3) {
        stdin.write_all(chunk).unwrap();
    }
    drop(stdin);
    let mut bytes = vec![];
    child
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(child.wait().unwrap().success());
    assert_eq!(
        FrameReader::default().feed(&bytes).unwrap(),
        vec![json!({"id":42,"ok":true,"result":{"protocolVersion":1}})]
    );
    let version = Command::new(binary).arg("--version").output().unwrap();
    assert!(version.status.success());
    assert!(String::from_utf8_lossy(&version.stdout).contains(env!("CARGO_PKG_VERSION")));
    let help = Command::new(binary)
        .args(["cli", "splunk", "--help"])
        .output()
        .unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stderr).contains("repeatable"));
    let output = Command::new(binary)
        .args(["cli", "terminal", "sh -c", "printf cli-fixture; exit 7"])
        .env("SERVICES_DATA_DIR", &data.0)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(output.stdout, b"cli-fixture");
}

#[test]
fn binary_terminal_flushes_both_output_streams_before_exit() {
    let data = Data::new();
    for iteration in 0..32 {
        let code = if iteration % 2 == 0 { 7 } else { 0 };
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_services-devtools"))
            .args([
                "cli",
                "terminal",
                "sh -c",
                &format!("printf cli-fixture; printf cli-error >&2; exit {code}"),
            ])
            .env("SERVICES_DATA_DIR", &data.0)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(code), "iteration {iteration}");
        assert_eq!(output.stdout, b"cli-fixture", "iteration {iteration}");
        assert_eq!(output.stderr, b"cli-error", "iteration {iteration}");
    }
}

#[tokio::test]
async fn splunk_rate_limit_retries_once_preserves_trace_and_header_date() {
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let state = count.clone();
    let router = Router::new().fallback(move |request: Request| {
        let state = state.clone();
        async move {
            if state.fetch_add(1, Ordering::SeqCst) == 0 {
                return (
                    axum::http::StatusCode::TOO_MANY_REQUESTS,
                    [("retry-after", "Thu, 01 Jan 1970 00:00:00 GMT")],
                    axum::Json(json!({"messages":[{"text":"retry"}]})),
                )
                    .into_response();
            }
            fixture(State(Arc::new(Mutex::new(vec![]))), request).await
        }
    });
    let server = Server::new(router).await;
    let data = Data::new();
    let result = data
        .runtime()
        .registry
        .call(
            "splunk",
            &json!({"baseUrl":server.url,"search":"search\nindex=main"}),
        )
        .await
        .unwrap();
    assert_eq!(result["debug"]["export"]["rateLimitedRetries"], 1);
    assert_eq!(count.load(Ordering::SeqCst), 2);
    assert!(
        result["debug"]["connection"]["events"]
            .as_array()
            .unwrap()
            .len()
            >= 4
    );
}
