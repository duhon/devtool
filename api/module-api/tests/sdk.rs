//! A module served by the SDK, called by the host client, both linked statically.

use serde_json::{Value, json};
use services_module_api::client::Module;
use services_module_api::error::{Error, Result};
use services_module_api::proto::host;
use services_module_api::sdk::{self, Call, Events, Manifest, RunStatus, Service, Threads};
use services_module_api::stabby;
use services_module_api::transport::{
    self, CancelArc, Endpoint, EndpointBox, Frame, FrameSinkBox, FrameSinkDyn, FrameSourceBox,
    Status, code,
};
use stabby::future::DynFutureUnsync;
use stabby::result::Result as SResult;
use stabby::string::String as SString;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

struct FakeHost;

fn host_reply(method: &str, request: &Frame) -> std::result::Result<Frame, Status> {
    match method {
        host::HTTP => {
            let request: host::HttpRequest = request.decode()?;
            if request.url.contains("down") {
                let failure = host::HttpFailure {
                    causes: vec![
                        "error sending request for url (http://down/)".into(),
                        "client error (Connect)".into(),
                        "Connection refused".into(),
                    ],
                    connect: true,
                    ..Default::default()
                };
                return Err(Status::new(code::UNAVAILABLE, "http failed").with_details(&failure));
            }
            Ok(Frame::message(&host::HttpResponse {
                status: 201,
                status_text: "Created".into(),
                http_version: "HTTP/1.1".into(),
                headers: vec![
                    host::Header {
                        name: "x-a".into(),
                        value: "1".into(),
                    },
                    host::Header {
                        name: "x-method".into(),
                        value: request.method,
                    },
                ],
                body: request.body.unwrap_or_else(|| b"no body".to_vec()),
                headers_us: 1500,
                remote_address: Some("127.0.0.1".into()),
                remote_port: 8080,
                remote_family: "IPv4".into(),
            }))
        }
        host::CONTEXT => Ok(Frame::message(&host::Context {
            data_dir: "/data".into(),
            host_version: "3.0.0".into(),
        })),
        other => Err(Status::unimplemented(other)),
    }
}

impl Endpoint for FakeHost {
    extern "C" fn unary<'a>(
        &'a self,
        method: SString,
        request: Frame,
        _cancel: CancelArc,
    ) -> DynFutureUnsync<'a, SResult<Frame, Status>> {
        transport::ready(host_reply(&method, &request))
    }

    extern "C" fn server_stream<'a>(
        &'a self,
        _method: SString,
        _request: Frame,
        sink: FrameSinkBox,
        _cancel: CancelArc,
    ) -> DynFutureUnsync<'a, SResult<Frame, Status>> {
        transport::guarded(async move {
            let head = host::HttpStreamMessage {
                kind: Some(host::HttpStreamKind::Head(host::HttpResponse {
                    status: 200,
                    ..Default::default()
                })),
            };
            sink.send(Frame::message(&head)).await;
            for chunk in 0..1000u32 {
                let chunk = host::HttpStreamMessage {
                    kind: Some(host::HttpStreamKind::Chunk(chunk.to_string().into_bytes())),
                };
                if !sink.send(Frame::message(&chunk)).await {
                    return Err(Status::new(code::CANCELLED, "reader left"));
                }
            }
            Ok(Frame::new(&[], &[]))
        })
    }

    extern "C" fn duplex<'a>(
        &'a self,
        method: SString,
        _open: Frame,
        _input: FrameSourceBox,
        _sink: FrameSinkBox,
        _cancel: CancelArc,
    ) -> DynFutureUnsync<'a, SResult<Frame, Status>> {
        transport::ready(Err(Status::unimplemented(&method)))
    }
}

struct Echo {
    soft: bool,
}

impl Service for Echo {
    fn manifest(&self) -> Manifest {
        Manifest {
            id: "echo".into(),
            name: "Echo".into(),
            version: "1".into(),
            execute: true,
            discover: true,
            stream: true,
            spec_json: String::new(),
        }
    }

    fn soft_cancel(&self) -> bool {
        self.soft
    }

    async fn execute(&self, call: Call) -> Result<Value> {
        match call.params["do"].as_str() {
            Some("fail") => {
                let mut error = Error::new("bad").with_debug(json!({"q":1}));
                error.duration_ms = Some(12);
                Err(error)
            }
            Some("panic") => panic!("boom"),
            Some("wait") => {
                call.cancel.cancelled().await;
                Ok(json!({"cancelled":true,"phase":"cleanup"}))
            }
            Some("http") => {
                let headers = json!({"h":1});
                let url = call.params["url"].as_str().unwrap_or("http://x/");
                services_module_api::http::request(
                    &call.host,
                    "post",
                    url,
                    &headers,
                    Some("payload".into()),
                )
                .await
            }
            Some("stream") => {
                let request = host::HttpRequest::default();
                let (head, mut body) = call.host.http_stream(&request).await?;
                let mut chunks = Vec::new();
                while let Some(chunk) = body.chunk().await? {
                    chunks.push(String::from_utf8(chunk).unwrap());
                    if chunks.len() == 3 {
                        break;
                    }
                }
                Ok(json!({"status":head.status,"chunks":chunks}))
            }
            Some("context") => Ok(json!({"dir":call.host.context().await?.data_dir})),
            _ => Ok(json!({"status":404,"body":{"echo":call.params}})),
        }
    }

    async fn discover(&self, call: Call) -> Result<Value> {
        Ok(json!({"schema":call.params["url"]}))
    }

    async fn stream(&self, call: Call, events: Events) -> Result<Value> {
        let count = call.params["count"].as_u64().unwrap_or(0);
        for index in 0..count {
            events.send(json!({"line":index})).await.unwrap();
        }
        Ok(json!({"code":0,"lines":count}))
    }
}

fn module(threads: Threads, soft: bool) -> Module {
    let host: services_module_api::transport::EndpointArc = stabby::sync::Arc::new(FakeHost).into();
    let endpoint: EndpointBox = sdk::serve(Echo { soft }, host, threads).unwrap();
    Module::new(endpoint).unwrap()
}

fn both() -> [Module; 2] {
    [
        module(Threads::Host, false),
        module(Threads::Own("echo-test"), false),
    ]
}

async fn execute(module: &Module, params: Value) -> std::result::Result<Value, Status> {
    let reply = module
        .execute(params.to_string(), false, CancellationToken::new())
        .await?;
    Ok(serde_json::from_slice(&reply.result_json).unwrap())
}

#[tokio::test(flavor = "multi_thread")]
async fn manifest_and_execute() {
    for module in both() {
        assert_eq!(module.manifest.id, "echo");
        let reply = module
            .execute(json!({"a":1}).to_string(), true, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            reply.result_json,
            r#"{"status":404,"body":{"echo":{"a":1}}}"#
        );
        assert_eq!(reply.status, RunStatus::Error as i32);
        assert_eq!(reply.outcome, "WARNING");
        assert_eq!(
            reply.history_text.as_deref(),
            Some("{\n  \"echo\": {\n    \"a\": 1\n  }\n}")
        );
        let reply = module
            .execute("{}".into(), false, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(reply.history_text, None);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn errors_keep_debug_and_duration() {
    for module in both() {
        let status = execute(&module, json!({"do":"fail"})).await.unwrap_err();
        assert_eq!(status.code, code::UNKNOWN);
        let error = Error::from_status(&status);
        assert_eq!(
            error.json(),
            json!({"error":"bad","debug":{"request":{"q":1},"error":{"name":"Error","message":"bad"}},"durationMs":12})
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn panics_become_internal_errors() {
    for module in both() {
        let status = execute(&module, json!({"do":"panic"})).await.unwrap_err();
        assert_eq!(status.code, code::INTERNAL);
        assert_eq!(&*status.message, "module panicked: boom");
        // The module still answers.
        assert!(execute(&module, json!({})).await.is_ok());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_drops_the_call_unless_the_service_handles_it() {
    for (module, soft) in [
        (module(Threads::Host, false), false),
        (module(Threads::Own("echo-test"), false), false),
        (module(Threads::Own("echo-test"), true), true),
    ] {
        let token = CancellationToken::new();
        let stop = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            stop.cancel();
        });
        let reply = module
            .execute(json!({"do":"wait"}).to_string(), false, token)
            .await
            .unwrap();
        let expected = if soft {
            json!({"cancelled":true,"phase":"cleanup"})
        } else {
            json!({"cancelled":true,"warning":sdk::CANCELLED_WARNING})
        };
        assert_eq!(
            serde_json::from_slice::<Value>(&reply.result_json).unwrap(),
            expected
        );
        assert_eq!(reply.status, RunStatus::Stopped as i32);
        assert_eq!(reply.outcome, "Stopped");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn discover_and_stream() {
    for module in both() {
        let schema = module
            .discover(json!({"url":"u"}).to_string(), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(schema, r#"{"schema":"u"}"#);
        let mut events = Vec::new();
        let reply = module
            .stream(
                json!({"count":300}).to_string(),
                true,
                CancellationToken::new(),
                |event| {
                    events.push(event);
                    async {}
                },
            )
            .await
            .unwrap();
        assert_eq!(events.len(), 300);
        assert_eq!(events[299], r#"{"line":299}"#);
        assert_eq!(reply.result_json, r#"{"code":0,"lines":300}"#);
        assert_eq!(reply.status, RunStatus::Success as i32);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn http_request_through_the_host() {
    for module in both() {
        let value = execute(&module, json!({"do":"http"})).await.unwrap();
        assert_eq!(value["status"], 201);
        assert_eq!(value["statusText"], "Created");
        assert_eq!(value["headers"], json!({"x-a":"1","x-method":"POST"}));
        assert_eq!(value["body"], "payload");
        let connection = &value["debug"]["connection"];
        assert!(connection["timing"]["ttfbMs"].as_u64().unwrap() >= 1);
        assert_eq!(
            connection["events"][0]["data"],
            json!({"httpVersion":"HTTP/1.1","status":201})
        );
        assert_eq!(connection["events"][1]["data"], json!({"byteLength":7}));
        assert_eq!(connection["remoteAddress"], "127.0.0.1");
        assert_eq!(connection["remotePort"], 8080);
        assert_eq!(connection["remoteFamily"], "IPv4");
        assert_eq!(
            value["debug"]["request"],
            json!({"method":"POST","url":"http://x/","headers":{"h":1}})
        );

        let status = execute(&module, json!({"do":"http","url":"http://down/"}))
            .await
            .unwrap_err();
        let error = Error::from_status(&status).json();
        assert_eq!(
            error["error"],
            "Request failed: error sending request for url (http://down/)"
        );
        assert_eq!(
            error["debug"]["error"]["cause"]["cause"],
            json!({"name":"Error","message":"Connection refused"})
        );
        assert_eq!(error["debug"]["connection"]["events"], json!([]));
        assert_eq!(error["debug"]["request"]["url"], "http://down/");
        assert!(error["durationMs"].is_u64());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn http_stream_and_context() {
    for module in both() {
        let value = execute(&module, json!({"do":"stream"})).await.unwrap();
        assert_eq!(value, json!({"status":200,"chunks":["0","1","2"]}));
        let value = execute(&module, json!({"do":"context"})).await.unwrap();
        assert_eq!(value, json!({"dir":"/data"}));
    }
}
