use axum::{
    Router,
    body::{Body, Bytes, to_bytes},
    extract::Request,
    http::{HeaderMap, HeaderValue},
    response::Response,
};
use http_body::Frame;
use http_body_util::StreamBody;
use prost::Message;
use serde_json::{Value, json};
use services_sdk::{
    driver::{decode, encode},
    grpc::{self, reflection},
};
use services_testkit::{DataDir, native::Pipe};
use std::time::Duration;

fn frames(mut input: &[u8]) -> Vec<Vec<u8>> {
    let mut out = vec![];
    while input.len() >= 5 {
        let len = u32::from_be_bytes(input[1..5].try_into().unwrap()) as usize;
        out.push(input[5..5 + len].to_vec());
        input = &input[5 + len..];
    }
    out
}

fn framed(message: &[u8]) -> Bytes {
    let mut bytes = vec![0];
    bytes.extend((message.len() as u32).to_be_bytes());
    bytes.extend(message);
    bytes.into()
}

fn answer(messages: Vec<Vec<u8>>, mut trailers: HeaderMap) -> Response {
    trailers.insert("grpc-status", HeaderValue::from_static("0"));
    let mut parts: Vec<Result<Frame<Bytes>, std::convert::Infallible>> = messages
        .iter()
        .map(|message| Ok(Frame::data(framed(message))))
        .collect();
    parts.push(Ok(Frame::trailers(trailers)));
    Response::builder()
        .header("content-type", "application/grpc")
        .header("x-served-by", "fixture")
        .body(Body::new(StreamBody::new(futures_util::stream::iter(
            parts,
        ))))
        .unwrap()
}

fn status(code: u32, message: &'static str) -> Response {
    Response::builder()
        .header("content-type", "application/grpc")
        .header("grpc-status", code.to_string())
        .header("grpc-message", message)
        .body(Body::empty())
        .unwrap()
}

async fn handle(request: Request) -> Response {
    let path = request.uri().path().to_owned();
    let token = request.headers().get("x-token").cloned();
    let trace = request.headers().get("x-trace-bin").cloned();
    let body = to_bytes(request.into_body(), usize::MAX).await.unwrap();
    let messages = frames(&body);
    match path.as_str() {
        "/test.Echo/Say" => {
            let mut trailers = HeaderMap::new();
            if let Some(token) = token {
                trailers.insert("x-token", token);
            }
            if let Some(trace) = trace {
                trailers.insert("x-trace-bin", trace);
            }
            answer(messages, trailers)
        }
        "/test.Echo/Twice" => {
            let twice = messages
                .iter()
                .flat_map(|m| [m.clone(), m.clone()])
                .collect();
            answer(twice, HeaderMap::new())
        }
        "/test.Echo/Slow" => {
            tokio::time::sleep(Duration::from_secs(30)).await;
            answer(messages, HeaderMap::new())
        }
        "/grpc.reflection.v1.ServerReflection/ServerReflectionInfo" => status(12, ""),
        "/grpc.reflection.v1alpha.ServerReflection/ServerReflectionInfo" => {
            let ask = reflection::ReflectionRequest::decode(messages[0].as_slice()).unwrap();
            let reply = match ask.request.unwrap() {
                reflection::Ask::ListServices(_) => reflection::ReflectionResponse {
                    list_services_response: Some(reflection::ListServiceResponse {
                        service: vec![reflection::ServiceResponse {
                            name: "test.Echo".into(),
                        }],
                    }),
                    ..Default::default()
                },
                reflection::Ask::FileContainingSymbol(symbol) => reflection::ReflectionResponse {
                    file_descriptor_response: Some(reflection::FileDescriptorResponse {
                        file_descriptor_proto: vec![symbol.into_bytes()],
                    }),
                    ..Default::default()
                },
                reflection::Ask::FileByFilename(_) => reflection::ReflectionResponse {
                    error_response: Some(reflection::ErrorResponse {
                        error_code: 5,
                        error_message: "not found".into(),
                    }),
                    ..Default::default()
                },
            };
            answer(vec![reply.encode_to_vec()], HeaderMap::new())
        }
        _ => status(3, "fixture-invalid"),
    }
}

async fn server() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = listener.local_addr().unwrap().to_string();
    let task = tokio::spawn(async move {
        axum::serve(listener, Router::new().fallback(handle))
            .await
            .unwrap();
    });
    (target, task)
}

fn call(id: u32, op: &str, request: &grpc::Request) -> Value {
    super::call("grpc", id, op, json!(request))
}

#[tokio::test]
async fn grpc_call_moves_bytes_metadata_and_status() {
    let (data, (target, _server)) = (DataDir::new(), server().await);
    let runtime = data.runtime();
    let health = runtime.dispatch(&json!({"id": 0, "type": "health"})).await;
    assert_eq!(
        health["result"]["drivers"],
        json!({"http": "1", "grpc": "1", "mysql": "1", "process": "1", "tcp": "1"})
    );

    let request = grpc::Request::new(&format!("grpc://{target}"), "test.Echo", "Say")
        .message(b"one")
        .message(&[0, 255])
        .with_metadata("x-token", "secret")
        .with_metadata("x-trace-bin", &encode(&[1, 2, 3]));
    let reply = runtime.dispatch(&call(1, "call", &request)).await;
    assert_eq!(reply["ok"], true, "{reply}");
    let response = grpc::Response::parse(&reply["result"]).unwrap();
    assert_eq!(response.status, 0);
    assert_eq!(
        response.messages().unwrap(),
        vec![b"one".to_vec(), vec![0, 255]]
    );
    assert!(
        response
            .headers
            .contains(&("x-served-by".into(), "fixture".into()))
    );
    assert!(
        response
            .trailers
            .contains(&("x-token".into(), "secret".into()))
    );
    let trace = response
        .trailers
        .iter()
        .find(|(name, _)| name == "x-trace-bin");
    assert_eq!(decode(&trace.unwrap().1).unwrap(), vec![1, 2, 3]);
    assert!(response.timing.total_us >= response.timing.first_byte_us);

    let unknown = grpc::Request::new(&target, "test.Echo", "Missing");
    let reply = runtime.dispatch(&call(2, "call", &unknown)).await;
    let response = grpc::Response::parse(&reply["result"]).unwrap();
    assert_eq!(
        (response.status, response.status_name()),
        (3, "INVALID_ARGUMENT")
    );
    assert_eq!(response.message, "fixture-invalid");
}

#[tokio::test]
async fn grpc_reflection_is_plain_calls_falling_back_to_v1alpha() {
    let (data, (target, _server)) = (DataDir::new(), server().await);
    let runtime = data.runtime();
    let ask = |id, version, query| {
        let message = call(id, "call", &reflection::request(&target, version, &query));
        let runtime = &runtime;
        async move { runtime.dispatch(&message).await }
    };
    let reply = ask(1, "v1", reflection::Query::ListServices).await;
    let response = grpc::Response::parse(&reply["result"]).unwrap();
    assert_eq!(response.status, reflection::UNIMPLEMENTED);

    let reply = ask(2, "v1alpha", reflection::Query::ListServices).await;
    let response = grpc::Response::parse(&reply["result"]).unwrap();
    assert_eq!(
        reflection::answer(&response).unwrap(),
        reflection::Answer::Services(vec!["test.Echo".into()])
    );
    let symbol = reflection::Query::FileContainingSymbol("test.Echo".into());
    let reply = ask(3, "v1alpha", symbol).await;
    let response = grpc::Response::parse(&reply["result"]).unwrap();
    assert_eq!(
        reflection::answer(&response).unwrap(),
        reflection::Answer::Files(vec![b"test.Echo".to_vec()])
    );
    let file = reflection::Query::FileByFilename("x.proto".into());
    let reply = ask(4, "v1alpha", file).await;
    let response = grpc::Response::parse(&reply["result"]).unwrap();
    assert_eq!(
        reflection::answer(&response).unwrap_err(),
        "Reflection failed: not found"
    );
}

#[tokio::test]
async fn grpc_stream_sends_headers_then_each_message_and_stops_on_cancel() {
    let (data, (target, _server)) = (DataDir::new(), server().await);
    let mut pipe = Pipe::start(data.runtime());
    let request = grpc::Request::new(&target, "test.Echo", "Twice").message(b"hi");
    pipe.send(&call(1, "stream", &request)).await;
    let (events, exit) = super::events(&mut pipe).await;
    let events: Vec<grpc::Event> = events
        .iter()
        .map(|e| grpc::Event::parse(e).unwrap())
        .collect();
    assert!(matches!(&events[0], grpc::Event::Headers(headers) if !headers.is_empty()));
    assert_eq!(
        events[1..],
        [
            grpc::Event::Message(encode(b"hi")),
            grpc::Event::Message(encode(b"hi"))
        ]
    );
    let response = grpc::Response::parse(&exit["info"]).unwrap();
    assert_eq!(response.status, 0);
    assert!(response.messages.is_empty());

    let slow = grpc::Request::new(&target, "test.Echo", "Slow");
    pipe.send(&call(2, "stream", &slow)).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    pipe.send(&json!({"id": 2, "type": "stop"})).await;
    let (_, reply) = super::events(&mut pipe).await;
    assert_eq!(reply["failure"]["stage"], "cancelled", "{reply}");
}

#[tokio::test]
async fn grpc_failures_name_their_stage() {
    let (data, (target, _server)) = (DataDir::new(), server().await);
    let runtime = data.runtime();
    let refused = grpc::Request::new("127.0.0.1:1", "test.Echo", "Say");
    let reply = runtime.dispatch(&call(1, "call", &refused)).await;
    assert_eq!(reply["ok"], false);
    assert_eq!(reply["failure"]["stage"], "connect", "{reply}");

    let mut slow = grpc::Request::new(&target, "test.Echo", "Slow");
    slow.timeout_ms = Some(100);
    let reply = runtime.dispatch(&call(2, "call", &slow)).await;
    assert_eq!(reply["failure"]["stage"], "timeout", "{reply}");

    let mut bad = grpc::Request::new(&target, "test.Echo", "Say");
    bad.path = "no-slash".into();
    let reply = runtime.dispatch(&call(3, "call", &bad)).await;
    assert_eq!(reply["failure"]["stage"], "send", "{reply}");
}
