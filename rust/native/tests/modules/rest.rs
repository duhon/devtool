use axum::{Router, extract::Request};
use serde_json::{Value, json};
use services_testkit::{
    Host,
    http::{self, Server},
};

#[tokio::test]
async fn requests_headers_errors_and_discovery_against_a_live_server() {
    let host = Host::new();
    let (server, seen) = http::services().await;
    let reply = host.call("rest",json!({"method":"post","url":format!("{}/echo",server.url),"headers":{"X-Token":"secret"},"body":"hello"})).await;
    let response = reply.ok();
    assert_eq!(response["status"], 201, "{reply}");
    assert_eq!(response["statusText"], "Created");
    assert_eq!(response["body"], "hello");
    assert_eq!(response["headers"]["x-response"], "fixture");
    assert_eq!(seen.lock().unwrap()[0]["token"], "secret");
    assert_eq!(response["debug"]["request"]["method"], "POST");
    assert!(response["durationMs"].is_number(), "{reply}");
    let graphql = host.call("graphql",json!({"url":format!("{}/graphql",server.url),"query":"query Echo { echo }","variables":{"a":1}})).await;
    assert_eq!(
        serde_json::from_str::<Value>(graphql.ok()["body"].as_str().unwrap()).unwrap()["data"]["echo"]
            ["a"],
        1,
        "{graphql}"
    );
    let schema = host
        .discover("graphql", json!({"url":format!("{}/graphql",server.url)}))
        .await
        .schema()
        .clone();
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
    let api = host
        .discover("rest", json!({"url":format!("{}/api/pets/1",server.url)}))
        .await
        .schema()
        .clone();
    assert_eq!(api["paths"], json!(["/api/pets"]));
    assert_eq!(api["methodsByPath"]["/api/pets"], json!(["POST"]));
    assert_eq!(
        api["bodySchemas"]["POST /api/pets"][1]["fields"][0],
        json!({"name":"id","kind":"bare","type":"integer"})
    );
    let protected = host
        .discover(
            "rest",
            json!({
                "url":format!("{}/protected/pets",server.url),
                "headers":{"X-Token":"schema-token"}
            }),
        )
        .await
        .schema()
        .clone();
    assert_eq!(protected["paths"], json!(["/api/pets"]));
    assert!(
        seen.lock()
            .unwrap()
            .iter()
            .any(|request| request["path"] == "/protected/openapi.json"
                && request["token"] == "schema-token")
    );
    let reply = host.discover("graphql", json!({"url":"not a URL"})).await;
    assert!(reply.discovery_failure().contains("Invalid URL"), "{reply}");
    let reply = host.call("rest", json!({"url":"x"})).await;
    assert_eq!(reply.error(), "method is required");
    assert!(reply["debug"].is_object(), "{reply}");
}

#[tokio::test]
async fn discovery_reads_swagger_and_openapi_and_reports_a_missing_schema() {
    let host = Host::new();
    let (server, seen) = http::services().await;
    for (prefix, base, expected) in [
        ("swagger-api", "/swagger-api", "/swagger-api/pets"),
        ("openapi31", "/openapi31", "/openapi31/pets"),
        ("api", "/api", "/api/pets"),
    ] {
        let before = seen.lock().unwrap().len();
        let schema = host
            .discover("rest", json!({"url":format!("{}/{prefix}/",server.url)}))
            .await
            .schema()
            .clone();
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
        let result = host
            .discover("rest", json!({"url":format!("{}/{prefix}/",server.url)}))
            .await
            .schema()
            .clone();
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
    let reply = host
        .discover(
            "rest",
            json!({"url":format!("{}/protected/pets",server.url)}),
        )
        .await;
    assert!(reply.discovery_failure().contains("401"), "{reply}");
    let before = seen.lock().unwrap().len();
    for url in ["hhttp://fake.com", "https://", "not a URL"] {
        host.discover("rest", json!({"url":url}))
            .await
            .discovery_failure();
    }
    assert_eq!(
        seen.lock().unwrap().len(),
        before,
        "Invalid URLs must not issue schema probes"
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    host.discover("rest", json!({"url":format!("http://{address}/")}))
        .await
        .discovery_failure();
}

#[tokio::test]
async fn discovery_prefers_the_magento_schema_when_several_exist() {
    let host = Host::new();
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
    let schema = host
        .discover("rest", json!({"url":format!("{}/",server.url)}))
        .await
        .schema()
        .clone();
    assert_eq!(
        schema["specUrl"],
        format!("{}/rest/default/schema?services=all", server.url)
    );
    assert_eq!(schema["baseUrl"], server.url);
    assert_eq!(schema["paths"], json!(["/magento"]));
}

#[tokio::test]
async fn discovery_finds_swagger_at_spec_json() {
    let host = Host::new();
    let router = Router::new().route(
        "/spec.json",
        axum::routing::get(|| async {
            axum::Json(json!({"swagger":"2.0","basePath":"/","paths":{
                "/get":{"get":{}},"/post":{"post":{}}
            }}))
        }),
    );
    let server = Server::new(router).await;
    let schema = host
        .discover("rest", json!({"url":format!("{}/",server.url)}))
        .await
        .schema()
        .clone();
    assert_eq!(schema["specUrl"], format!("{}/spec.json", server.url));
    assert_eq!(schema["paths"], json!(["/get", "/post"]));
    assert_eq!(schema["methodsByPath"]["/get"], json!(["GET"]));
    assert_eq!(schema["methodsByPath"]["/post"], json!(["POST"]));
    assert!(schema.get("warning").is_none());
}
