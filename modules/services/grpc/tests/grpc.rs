//! gRPC module behaviour: endpoints, messages, descriptors, calls, responses, reflection
//! through the `grpc` driver, completion, highlighting and grpcurl.

use prost::Message;
use serde_json::{Value, json};
use services_module_grpc::{Grpc, parse_endpoint, proto, render};
use services_sdk::driver::{decode, encode};
use services_sdk::grpc::reflection::{
    Ask, FileDescriptorResponse, ListServiceResponse, ReflectionRequest, ReflectionResponse,
    ServiceResponse,
};
use services_sdk::lang::marker;
use services_sdk::{Check, Cmd, DriverCall, Event, Failure, Module, Section};

const RUN: &str = "console:grpc:1";

/// Runs `check` with its variables resolved from `vars`: the driver call the run makes, or
/// the error it finished with.
fn build(grpc: &mut Grpc, check: &Check, vars: &[(&str, &str)]) -> Result<DriverCall, String> {
    let cmds = grpc.update(Event::Run {
        tag: RUN.into(),
        check: check.clone(),
    });
    let failed = |cmd: &Cmd| -> Result<DriverCall, String> {
        assert_eq!(cmd.name, "run.result");
        Err(cmd.args()["error"].as_str().unwrap().to_owned())
    };
    if cmds[0].name != "vars.resolve" {
        return failed(&cmds[0]);
    }
    let mut text = cmds[0].args()["text"].as_str().unwrap().to_owned();
    for (name, value) in vars {
        text = text.replace(&format!("{{{{{name}}}}}"), value);
    }
    let cmds = grpc.update(Event::Reply {
        tag: cmds[0].tag.clone(),
        result: Ok(json!(text)),
    });
    if cmds[0].name != "driver.call" {
        return failed(&cmds[0]);
    }
    let args = cmds[0].args();
    Ok(DriverCall::new(
        args["driver"].as_str().unwrap(),
        &args["request"],
    ))
}

/// The result the run ends with after its call answers `response`.
fn finish(grpc: &mut Grpc, response: Value) -> Value {
    let cmds = grpc.update(Event::Reply {
        tag: format!("{RUN}:call"),
        result: Ok(response),
    });
    assert_eq!(cmds.last().unwrap().name, "run.result");
    cmds.last().unwrap().args()
}

/// A check as the Kernel builds it: sections split at marker lines.
fn check(text: &str) -> Check {
    let mut sections: Vec<Section> = vec![];
    let mut body: Vec<&str> = vec![];
    let mut current: Option<(String, String, u32)> = Some(("leftover".into(), String::new(), 0));
    let mut close = |current: Option<(String, String, u32)>, body: &mut Vec<&str>| {
        if let Some((name, head, line)) = current {
            let mut text = head;
            if !body.is_empty() {
                text = format!("{text}\n{}", body.join("\n"));
            }
            body.clear();
            let text = text.trim().to_owned();
            if name != "leftover" || !text.is_empty() {
                sections.push(Section { name, text, line });
            }
        }
    };
    for (i, line) in text.split('\n').enumerate() {
        if let Some((name, rest)) = marker(line) {
            close(current.take(), &mut body);
            let head = if name == "endpoint" {
                rest.to_owned()
            } else {
                String::new()
            };
            current = Some((name, head, i as u32));
        } else {
            body.push(line);
        }
    }
    close(current, &mut body);
    Check {
        service: "grpc".into(),
        text: text.into(),
        sections,
        options: vec![],
    }
}

const PROTO: &str = r#"syntax = "proto3";
    package fixture;
    import "google/protobuf/timestamp.proto";
    message Message { string text = 1; int64 count = 2; Nested child = 3; repeated string tags = 4; google.protobuf.Timestamp at = 5; bytes payload = 6; oneof choice { string selected = 7; int32 number = 8; } repeated StoreView products_by_store_view = 9; repeated bool flags = 10; repeated int32 ids = 11; }
    message Nested { bool enabled = 1; }
    message StoreView { string store_view_code = 1; repeated Product products = 2; }
    message Product { string sku = 1; repeated string tags = 2; }
    service Echo {
        rpc Unary(Message) returns (Message);
        rpc Server(Message) returns (stream Message);
        rpc Client(stream Message) returns (Message);
        rpc Bidi(stream Message) returns (stream Message);
        rpc Fails(Message) returns (Message);
    }"#;

fn with_proto(method: &str, message: &str) -> String {
    let proto: Vec<_> = PROTO.lines().map(str::trim).collect();
    format!(
        "#endpoint\ngrpc://localhost:50051/fixture.Echo/{method}\n#headers\nX-Token: sent-token\n#proto\n{}\n#message\n{message}",
        proto.join("\n")
    )
}

fn statuses(cmds: &[Cmd]) -> Vec<String> {
    cmds.iter()
        .filter(|cmd| cmd.name == "status.set")
        .map(|cmd| cmd.args()["text"].as_str().unwrap().to_owned())
        .collect()
}

/// The tag, request and reflection question of the driver call among `cmds`.
fn reflection_call(cmds: &[Cmd]) -> (String, Value, Ask) {
    let cmd = cmds
        .iter()
        .find(|cmd| cmd.name == "driver.call")
        .expect("a driver call");
    let args = cmd.args();
    assert_eq!(args["driver"], "grpc");
    let request = args["request"].clone();
    let bytes = decode(request["messages"][0].as_str().unwrap()).unwrap();
    let ask = ReflectionRequest::decode(bytes.as_slice())
        .unwrap()
        .request
        .unwrap();
    (cmd.tag.clone(), request, ask)
}

fn answer(status: i32, reply: Option<ReflectionResponse>) -> Value {
    let messages: Vec<_> = reply.iter().map(|r| encode(&r.encode_to_vec())).collect();
    json!({"status": status, "message": if status == 0 { "" } else { "nope" }, "messages": messages})
}

fn files(names: &[&str]) -> ReflectionResponse {
    let pool = proto::compile(PROTO).unwrap();
    let file_descriptor_proto = pool
        .file_descriptor_protos()
        .filter(|file| names.contains(&file.name()))
        .map(|file| file.encode_to_vec())
        .collect();
    ReflectionResponse {
        file_descriptor_response: Some(FileDescriptorResponse {
            file_descriptor_proto,
        }),
        ..ReflectionResponse::default()
    }
}

#[test]
fn endpoints_messages_and_proto_sources() {
    assert_eq!(
        parse_endpoint("grpcs://localhost:443/fixture.Echo/Unary"),
        (
            "grpcs://localhost:443".into(),
            "fixture.Echo".into(),
            "Unary".into()
        )
    );
    assert_eq!(
        parse_endpoint("localhost:443/fixture.Echo/"),
        ("localhost:443".into(), "fixture.Echo".into(), "".into())
    );
    assert_eq!(
        parse_endpoint("grpc://localhost:443"),
        ("grpc://localhost:443".into(), "".into(), "".into())
    );
    assert_eq!(
        proto::messages("[{\"text\":\"a\"},{\"text\":\"b\"}]")
            .unwrap()
            .len(),
        2
    );
    assert_eq!(proto::messages(" ").unwrap(), vec![json!({})]);
    assert!(proto::messages("not json").is_err());
    assert!(
        proto::compile("not proto")
            .unwrap_err()
            .starts_with("Invalid proto:")
    );
}

#[test]
fn message_fields_feed_json_completion() {
    let pool = proto::compile(PROTO).unwrap();
    let message = pool.get_message_by_name("fixture.Message").unwrap();
    let fields = proto::fields(&message, 0);
    assert_eq!(
        fields[2],
        json!({"name":"child","kind":"object","fields":[{"name":"enabled","kind":"bare"}]})
    );
    assert_eq!(fields[3]["items"], json!({"kind":"string"}));
    let field = |name: &str| fields.iter().find(|field| field["name"] == name).unwrap();
    assert_eq!(
        field("products_by_store_view"),
        &json!({"name":"products_by_store_view","kind":"array","items":{
            "kind":"object","fields":[
                {"name":"store_view_code","kind":"string"},
                {"name":"products","kind":"array","items":{"kind":"object","fields":[
                    {"name":"sku","kind":"string"},
                    {"name":"tags","kind":"array","items":{"kind":"string"}}
                ]}}
            ]
        }})
    );
    assert_eq!(field("flags")["items"], json!({"kind":"boolean"}));
    assert_eq!(field("ids")["items"], json!({"kind":"number"}));
    assert_eq!(
        proto::fields(&message, 6)[8]["items"],
        json!({"kind":"object"})
    );
}

#[test]
fn checks_with_proto_become_grpc_driver_calls_and_responses_print_as_json() {
    let mut grpc = Grpc::default();
    let check = check(&with_proto(
        "Unary",
        r#"{"text":"one","count":"9007199254740993","at":"2026-10-17T01:00:00Z","payload":"AQID","selected":"yes","unknown":1}"#,
    ));
    let call = build(&mut grpc, &check, &[]).unwrap();
    assert_eq!(call.driver, "grpc");
    let request: Value = serde_json::from_slice(&call.request).unwrap();
    assert_eq!(request["target"], "grpc://localhost:50051");
    assert_eq!(request["path"], "/fixture.Echo/Unary");
    assert_eq!(request["metadata"], json!([["x-token", "sent-token"]]));
    assert_eq!(request["timeoutMs"], 120000);
    assert_eq!(request["connectMs"], 5000);
    assert!(
        request["console"]["proto"]
            .as_str()
            .unwrap()
            .contains("service Echo")
    );
    let sent = request["messages"].as_array().unwrap();
    assert_eq!(sent.len(), 1);
    // The server echoes the message.
    let rendered = finish(&mut grpc, json!({"status":0,"messages":[sent[0]]}));
    assert_eq!(
        (&rendered["language"], &rendered["outcome"]),
        (&json!("json"), &json!("OK"))
    );
    let body: Value = serde_json::from_str(rendered["body"].as_str().unwrap()).unwrap();
    assert_eq!(body["count"], "9007199254740993");
    assert_eq!(body["at"], "2026-10-17T01:00:00Z");
    assert_eq!(body["tags"], json!([]));
    assert_eq!(body["payload"], json!({"type":"Buffer","data":[1,2,3]}));
    assert_eq!(body["choice"], "selected");
    assert_eq!(body["child"], Value::Null);

    let invalid = build(&mut grpc, &check_text("Unary", r#"{"count":"x"}"#), &[]).unwrap_err();
    assert!(invalid.starts_with("Invalid request message:"), "{invalid}");
    assert_eq!(
        build(&mut grpc, &check_text("Missing", "{}"), &[]).unwrap_err(),
        "Method not found: fixture.Echo/Missing"
    );
}

fn check_text(method: &str, message: &str) -> Check {
    check(&with_proto(method, message))
}

#[test]
fn call_shapes_streams_and_statuses() {
    let mut grpc = Grpc::default();
    let mut request = |method: &str, message: &str| -> Value {
        build(&mut grpc, &check_text(method, message), &[])
            .unwrap()
            .request()
    };
    let two = r#"[{"text":"a"},{"text":"b"}]"#;
    assert_eq!(
        request("Unary", two)["messages"].as_array().unwrap().len(),
        1
    );
    assert_eq!(
        request("Client", two)["messages"].as_array().unwrap().len(),
        2
    );
    let bidi = request("Bidi", two);
    let pool = proto::compile(PROTO).unwrap();
    let rendered = render(
        "/fixture.Echo/Bidi",
        Some(&pool),
        &json!({"status":0,"messages":bidi["messages"]}),
    )
    .unwrap();
    let body: Value = serde_json::from_str(&rendered.text).unwrap();
    assert_eq!(body.as_array().unwrap().len(), 2);
    assert_eq!(body[1]["text"], "b");
    let server = render(
        "/fixture.Echo/Server",
        Some(&pool),
        &json!({"status":0,"messages":[]}),
    )
    .unwrap();
    assert_eq!(server.text, "[]");
    let unary = render(
        "/fixture.Echo/Unary",
        Some(&pool),
        &json!({"status":0,"messages":[]}),
    )
    .unwrap();
    assert_eq!(unary.text, "{}");
    let failed = render(
        "/fixture.Echo/Fails",
        Some(&pool),
        &json!({"status":3,"message":"fixture-invalid","trailers":[["grpc-status","3"]]}),
    )
    .unwrap();
    assert_eq!(failed.outcome, "ERROR");
    assert_eq!(
        serde_json::from_str::<Value>(&failed.text).unwrap(),
        json!({"error":"fixture-invalid"})
    );
    let unknown = render(
        "/fixture.Echo/Unary",
        None,
        &json!({"status":0,"messages":["AQID"]}),
    )
    .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&unknown.text).unwrap(),
        json!({"messages":["AQID"]})
    );
    let mut grpc = Grpc::default();
    assert_eq!(
        build(
            &mut grpc,
            &check("#endpoint\ngrpc://localhost:1\n#proto\nsyntax = \"proto3\";"),
            &[]
        )
        .unwrap_err(),
        "serviceName and methodName are required"
    );
}

#[test]
fn reflection_falls_back_to_v1alpha_and_asks_for_each_dependency() {
    let mut grpc = Grpc::default();
    let text = "#endpoint\ngrpc://{{host}}/fixture.Echo/Unary\n#headers\nAuthorization: Bearer t\n#message\n{\"text\":\"{{name}}\"}";
    let cmds = grpc.update(Event::Document(check(text)));
    assert!(statuses(&cmds).is_empty(), "quiet while editing");
    assert!(cmds.iter().all(|cmd| cmd.name != "vars.resolve"));
    let (tag, request, ask) = reflection_call(&cmds);
    assert_eq!(request["target"], "grpc://{{host}}");
    assert_eq!(
        request["path"],
        "/grpc.reflection.v1.ServerReflection/ServerReflectionInfo"
    );
    assert_eq!(request["metadata"], json!([["authorization", "Bearer t"]]));
    assert_eq!(request["timeoutMs"], 5000);
    assert_eq!(ask, Ask::FileContainingSymbol("fixture.Echo".into()));
    assert!(
        build(&mut grpc, &check(text), &[])
            .unwrap_err()
            .contains("being loaded through server reflection")
    );

    let cmds = grpc.update(Event::Reply {
        tag,
        result: Ok(answer(12, None)),
    });
    let (tag, request, ask) = reflection_call(&cmds);
    assert_eq!(
        request["path"],
        "/grpc.reflection.v1alpha.ServerReflection/ServerReflectionInfo"
    );
    assert_eq!(ask, Ask::FileContainingSymbol("fixture.Echo".into()));
    let cmds = grpc.update(Event::Reply {
        tag,
        result: Ok(answer(0, Some(files(&["input.proto"])))),
    });
    let (tag, request, ask) = reflection_call(&cmds);
    assert_eq!(
        request["path"],
        "/grpc.reflection.v1alpha.ServerReflection/ServerReflectionInfo"
    );
    assert_eq!(
        ask,
        Ask::FileByFilename("google/protobuf/timestamp.proto".into())
    );
    let cmds = grpc.update(Event::Reply {
        tag,
        result: Ok(answer(0, Some(files(&["google/protobuf/timestamp.proto"])))),
    });
    assert!(statuses(&cmds).is_empty());
    assert!(cmds.iter().all(|cmd| cmd.name != "driver.call"));

    let call = build(&mut grpc, &check(text), &[("host", "h:1"), ("name", "Ann")]).unwrap();
    let request = call.request();
    assert_eq!(request["console"], json!({}));
    assert_eq!(request["target"], "grpc://h:1");
    let rendered = finish(
        &mut grpc,
        json!({"status":0,"messages":[request["messages"][0]]}),
    );
    let body: Value = serde_json::from_str(rendered["body"].as_str().unwrap()).unwrap();
    assert_eq!(body["text"], "Ann");

    let fields: Vec<_> = grpc
        .complete(&check("#endpoint\ngrpc://{{host}}/fixture.Echo/Unary\n#headers\nAuthorization: Bearer t\n#message\n{\n  \"te"), 999)
        .items
        .into_iter()
        .map(|item| item.label)
        .collect();
    assert!(
        fields.iter().any(|label| label.contains("text")),
        "{fields:?}"
    );
    let methods: Vec<_> = grpc
        .complete(
            &check("#endpoint\ngrpc://{{host}}/fixture.Echo/U\n#headers\nAuthorization: Bearer t"),
            40,
        )
        .items
        .into_iter()
        .map(|item| item.label)
        .collect();
    assert_eq!(methods, vec!["Unary"]);
}

#[test]
fn hints_list_services_and_discover_reports_failures() {
    let mut grpc = Grpc::default();
    let text = "#endpoint\ngrpc://localhost:50051/";
    let cmds = grpc.update(Event::Hints {
        check: check(text),
        pos: text.len() as u32,
    });
    assert_eq!(statuses(&cmds), vec!["Discovery…"]);
    let (tag, _, ask) = reflection_call(&cmds);
    assert_eq!(ask, Ask::ListServices("*".into()));
    let services = ReflectionResponse {
        list_services_response: Some(ListServiceResponse {
            service: ["fixture.Echo", "grpc.reflection.v1.ServerReflection"]
                .map(|name| ServiceResponse { name: name.into() })
                .to_vec(),
        }),
        ..ReflectionResponse::default()
    };
    let done = grpc.update(Event::Reply {
        tag,
        result: Ok(answer(0, Some(services))),
    });
    assert_eq!(statuses(&done), vec!["Discovery · OK"]);
    let completion = grpc.complete(&check(text), text.len() as u32);
    let labels: Vec<_> = completion
        .items
        .iter()
        .map(|item| item.label.as_str())
        .collect();
    assert_eq!(labels, vec!["fixture.Echo/"]);
    assert_eq!(completion.from as usize, text.len());
    assert!(
        grpc.update(Event::Hints {
            check: check(text),
            pos: text.len() as u32
        })
        .is_empty()
    );

    let cmds = grpc.update(Event::Discover(check(
        "#endpoint\ngrpc://localhost:50051/fixture.Echo/Unary",
    )));
    let (tag, _, _) = reflection_call(&cmds);
    let failed = grpc.update(Event::Reply {
        tag,
        result: Err(Failure::new("connection refused")),
    });
    assert_eq!(
        statuses(&failed),
        vec!["Discovery · ERROR · Reflection failed: connection refused"]
    );
    assert_eq!(
        build(
            &mut grpc,
            &check("#endpoint\ngrpc://localhost:50051/fixture.Echo/Unary"),
            &[]
        )
        .unwrap_err(),
        "Reflection failed: connection refused"
    );
    assert_eq!(
        statuses(&grpc.update(Event::Discover(check("#endpoint\n#message\n{}")))),
        vec!["Discovery · WARNING · Invalid or incomplete service address for discovery"]
    );
    assert_eq!(
        statuses(&grpc.update(Event::Discover(check(&with_proto("Unary", "{}"))))),
        vec!["Discovery · OK"]
    );
}

#[test]
fn grpcurl_export_and_import() {
    let grpc = Grpc::default();
    let text = "#endpoint\ngrpc://localhost:50051/fixture.Echo/Unary\n#headers\nX-Token: it's\n#message\n{\"text\":\"a\"}";
    let command = grpc.export(&check(text)).unwrap();
    assert_eq!(
        command,
        "grpcurl \\\n  -plaintext \\\n  -emit-defaults \\\n  -H 'X-Token: it'\\''s' \\\n  -d '{\n  \"text\": \"a\"\n}' \\\n  localhost:50051 \\\n  fixture.Echo/Unary"
    );
    assert_eq!(
        grpc.import(&command).unwrap(),
        text.replace("{\"text\":\"a\"}", "{\n  \"text\": \"a\"\n}")
    );
    assert_eq!(
        grpc.import("grpcurl -d @ example.com:443 pkg.Svc.Method")
            .unwrap(),
        "#endpoint\ngrpcs://example.com:443/pkg.Svc/Method"
    );
    assert!(grpc.import("curl x").is_err());
    assert!(grpc.import("grpcurl -plaintext host:1").is_err());
}

#[test]
fn highlighting_and_folds() {
    let grpc = Grpc::default();
    let lines: Vec<String> = [
        "#endpoint",
        "grpcs://{{host}}:443/fixture.Echo/Unary",
        "#headers",
        "x-token: abc",
        "#message",
        "{",
        "  \"text\": 1",
        "}",
    ]
    .map(String::from)
    .to_vec();
    let highlighted = grpc.highlight(&[], &lines);
    let styled = |i: usize| -> Vec<(String, String)> {
        highlighted[i]
            .tokens
            .iter()
            .map(|token| {
                let start = token.start as usize;
                (
                    lines[i][start..start + token.len as usize].to_owned(),
                    token.kind.clone(),
                )
            })
            .collect()
    };
    assert_eq!(
        styled(1),
        vec![
            ("grpcs://".into(), "keyword".into()),
            (":443".into(), "def".into()),
            ("fixture.Echo".into(), "property".into()),
            ("Unary".into(), "variable-3".into()),
        ]
    );
    assert_eq!(styled(0), vec![("#endpoint".into(), "comment".into())]);
    assert!(styled(6).iter().any(|(_, kind)| kind == "number"));
    let again = grpc.highlight(&highlighted[5].state, &lines[6..7]);
    assert_eq!(again[0].tokens, highlighted[6].tokens);
    let folds = grpc.fold(&check(&lines.join("\n")));
    assert_eq!(folds.len(), 1);
    assert_eq!((folds[0].start, folds[0].end), (5, 7));
}
