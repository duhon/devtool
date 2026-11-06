//! Local HTTP fixtures. Tests never reach the network.

use axum::{
    Router,
    body::to_bytes,
    extract::{Request, State},
    response::IntoResponse,
};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

pub struct Server {
    pub url: String,
    task: tokio::task::JoinHandle<()>,
}
impl Server {
    pub async fn new(router: Router) -> Self {
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
pub type Seen = Arc<Mutex<Vec<Value>>>;
/// The shared fixture handler, for routers that wrap it with their own behavior.
pub async fn respond(State(seen): State<Seen>, request: Request) -> axum::response::Response {
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
/// One server for REST/OpenAPI, GraphQL, Splunk export and `/echo`, `/slow`,
/// `/cancel-slow`. `Seen` records every request it received.
pub async fn services() -> (Server, Seen) {
    let seen = Arc::new(Mutex::new(vec![]));
    let router = Router::new().fallback(respond).with_state(seen.clone());
    (Server::new(router).await, seen)
}

/// Answers like [`services`] without recording the request.
pub async fn answer(request: Request) -> axum::response::Response {
    respond(State(Arc::default()), request).await
}
