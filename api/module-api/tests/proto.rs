//! The hand-written prost types match `proto/*.proto`.

use prost::Message;
use prost_reflect::{DescriptorPool, DynamicMessage};
use services_module_api::proto::{host, module};

fn pool() -> DescriptorPool {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/proto");
    let files = protox::compile(["host.proto", "module.proto"], [root]).expect("protos compile");
    DescriptorPool::from_file_descriptor_set(files).expect("descriptor pool")
}

/// Every descriptor field is set by a fully populated message and nothing is unknown.
fn check(pool: &DescriptorPool, name: &str, message: &impl Message) {
    let descriptor = pool
        .get_message_by_name(name)
        .unwrap_or_else(|| panic!("{name} is missing in the protos"));
    let bytes = message.encode_to_vec();
    let dynamic = DynamicMessage::decode(descriptor.clone(), bytes.as_slice())
        .unwrap_or_else(|error| panic!("{name}: {error}"));
    assert_eq!(
        dynamic.unknown_fields().count(),
        0,
        "{name}: unknown fields"
    );
    for field in descriptor.fields() {
        if field.containing_oneof().is_none() {
            assert!(
                dynamic.has_field(&field),
                "{name}.{} is not set",
                field.name()
            );
        }
    }
    assert_eq!(dynamic.encode_to_vec(), bytes, "{name}: round trip");
}

#[test]
fn messages_match_the_protos() {
    let pool = pool();
    let header = host::Header {
        name: "a".into(),
        value: "b".into(),
    };
    let response = host::HttpResponse {
        status: 200,
        status_text: "OK".into(),
        http_version: "HTTP/1.1".into(),
        headers: vec![header.clone()],
        body: b"x".to_vec(),
        headers_us: 7,
        remote_address: Some("127.0.0.1".into()),
        remote_port: 80,
        remote_family: "IPv4".into(),
    };
    check(&pool, "services.host.v3.Header", &header);
    check(
        &pool,
        "services.host.v3.HttpRequest",
        &host::HttpRequest {
            method: "GET".into(),
            url: "http://x".into(),
            headers: vec![header],
            body: Some(b"b".to_vec()),
            text: true,
            timeout_ms: 5,
            error_for_status: true,
            no_redirects: true,
            no_retries: true,
        },
    );
    check(&pool, "services.host.v3.HttpResponse", &response);
    check(
        &pool,
        "services.host.v3.HttpStreamMessage",
        &host::HttpStreamMessage {
            kind: Some(host::HttpStreamKind::Head(response)),
        },
    );
    check(
        &pool,
        "services.host.v3.HttpStreamMessage",
        &host::HttpStreamMessage {
            kind: Some(host::HttpStreamKind::Chunk(b"c".to_vec())),
        },
    );
    check(
        &pool,
        "services.host.v3.HttpFailure",
        &host::HttpFailure {
            causes: vec!["a".into()],
            connect: true,
            builder: true,
            timeout: true,
            status: Some(404),
            decode: true,
        },
    );
    check(
        &pool,
        "services.host.v3.Context",
        &host::Context {
            data_dir: "/d".into(),
            host_version: "3".into(),
        },
    );
    check(
        &pool,
        "services.host.v3.LogRequest",
        &host::LogRequest {
            level: "info".into(),
            message: "m".into(),
        },
    );
    check(&pool, "services.host.v3.Empty", &host::Empty {});
    check(
        &pool,
        "services.module.v3.Manifest",
        &module::Manifest {
            id: "rest".into(),
            name: "REST".into(),
            version: "3".into(),
            execute: true,
            discover: true,
            stream: true,
            spec_json: "{}".into(),
        },
    );
    check(
        &pool,
        "services.module.v3.ExecuteRequest",
        &module::ExecuteRequest {
            params_json: "{}".into(),
            history: true,
        },
    );
    check(
        &pool,
        "services.module.v3.ExecuteResponse",
        &module::ExecuteResponse {
            result_json: "{}".into(),
            status: module::RunStatus::Stopped as i32,
            outcome: "OK".into(),
            history_text: Some("t".into()),
        },
    );
    check(
        &pool,
        "services.module.v3.DiscoverRequest",
        &module::DiscoverRequest {
            params_json: "{}".into(),
        },
    );
    check(
        &pool,
        "services.module.v3.DiscoverResponse",
        &module::DiscoverResponse {
            result_json: "{}".into(),
        },
    );
    check(
        &pool,
        "services.module.v3.Event",
        &module::Event { json: "{}".into() },
    );
    check(
        &pool,
        "services.module.v3.ModuleError",
        &module::ModuleError {
            debug_json: Some("{}".into()),
            duration_ms: Some(1),
        },
    );
}

#[test]
fn field_counts_and_enums_match() {
    let pool = pool();
    let counts = [
        ("services.host.v3.HttpRequest", 9),
        ("services.host.v3.HttpResponse", 9),
        ("services.host.v3.HttpStreamMessage", 2),
        ("services.host.v3.HttpFailure", 6),
        ("services.module.v3.Manifest", 7),
        ("services.module.v3.ExecuteResponse", 4),
        ("services.module.v3.ModuleError", 2),
    ];
    for (name, count) in counts {
        let descriptor = pool.get_message_by_name(name).unwrap();
        assert_eq!(descriptor.fields().count(), count, "{name}");
    }
    let status = pool
        .get_enum_by_name("services.module.v3.RunStatus")
        .unwrap();
    let values: Vec<_> = status
        .values()
        .map(|value| (value.name().to_string(), value.number()))
        .collect();
    assert_eq!(
        values,
        [
            (
                "RUN_STATUS_SUCCESS".to_string(),
                module::RunStatus::Success as i32
            ),
            (
                "RUN_STATUS_ERROR".to_string(),
                module::RunStatus::Error as i32
            ),
            (
                "RUN_STATUS_STOPPED".to_string(),
                module::RunStatus::Stopped as i32
            ),
        ]
    );
}

#[test]
fn method_names_match_the_services() {
    let pool = pool();
    let methods = |service: &str| -> Vec<String> {
        let service = pool.get_service_by_name(service).unwrap();
        let short = service.name().to_string();
        service
            .methods()
            .map(|method| format!("{short}/{}", method.name()))
            .collect()
    };
    assert_eq!(
        methods("services.host.v3.Host"),
        [host::HTTP, host::HTTP_STREAM, host::CONTEXT, host::LOG]
    );
    assert_eq!(
        methods("services.module.v3.Module"),
        [
            module::MANIFEST,
            module::EXECUTE,
            module::DISCOVER,
            module::STREAM
        ]
    );
}
