use serde_json::json;
use services_testkit::{
    DataDir, Host,
    mysql::{CancellationFixture, Server},
};
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn stop_kills_only_its_thread_and_drops_the_connection() {
    let control = Arc::new(CancellationFixture::default());
    let server = Server::controlled(control.clone()).await;
    let host = Arc::new(Host::new());
    let cancel = CancellationToken::new();
    let request = {
        let (host, dsn, token) = (host.clone(), server.dsn.clone(), cancel.clone());
        tokio::spawn(async move {
            host.call_cancellable(
                "mysql",
                json!({"connectionString":dsn,"sql":"SELECT blocked_fixture"}),
                token,
            )
            .await
        })
    };
    tokio::time::timeout(Duration::from_secs(3), control.started.notified())
        .await
        .expect("the blocked query must reach the server");
    let sibling = {
        let (host, dsn) = (host.clone(), server.dsn.clone());
        tokio::spawn(async move {
            host.call(
                "mysql",
                json!({"connectionString":dsn,"sql":"SELECT unrelated_fixture"}),
            )
            .await
        })
    };
    tokio::time::timeout(Duration::from_secs(3), control.started.notified())
        .await
        .expect("the sibling query must reach the server");
    cancel.cancel();
    let stopped = tokio::time::timeout(Duration::from_secs(4), request)
        .await
        .expect("stop must finish the request")
        .unwrap();
    assert_eq!(stopped.ok()["cancelled"], true, "{stopped}");
    assert_eq!(stopped.ok()["threadId"], 123, "{stopped}");
    assert_eq!(*control.killed.lock().await, vec![123]);
    assert!(
        !sibling.is_finished(),
        "Stop must not finish another connection's query"
    );
    control.release.cancel();
    let sibling = tokio::time::timeout(Duration::from_secs(3), sibling)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(sibling.ok()["rows"][0]["answer"], 42, "{sibling}");
    let next = host
        .call(
            "mysql",
            json!({"connectionString":server.dsn,"sql":"SELECT 42"}),
        )
        .await;
    assert_ne!(
        next.ok()["debug"]["connection"]["threadId"],
        123,
        "A stopped connection must not return to the pool\n{next}"
    );
}

#[tokio::test]
async fn dates_use_the_host_timezone_without_mutating_the_environment() {
    let server = Server::start().await;
    let data = DataDir::new();
    for (zone, winter, summer, calendar, overlap, gap) in [
        (
            "America/New_York",
            "2026-01-15T17:34:56.123Z",
            "2026-07-15T16:34:56.123Z",
            "2026-01-01T05:00:00.000Z",
            "2026-11-01T05:30:00.000Z",
            "2026-03-08T07:30:00.000Z",
        ),
        (
            "Asia/Kathmandu",
            "2026-01-15T06:49:56.123Z",
            "2026-07-15T06:49:56.123Z",
            "2025-12-31T18:15:00.000Z",
            "2026-10-31T19:45:00.000Z",
            "2026-03-07T20:45:00.000Z",
        ),
    ] {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_services-devtools"));
        command
            .args([
                "cli",
                "mysql",
                "--connection",
                &server.dsn,
                "--sql",
                "SELECT dates_fixture",
            ])
            .env("TZ", zone)
            .env("SERVICES_DATA_DIR", data.path())
            .kill_on_drop(true);
        let output = tokio::time::timeout(Duration::from_secs(10), command.output())
            .await
            .unwrap()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            response["rows"][0],
            json!({"winter":winter,"summer":summer,"calendar":calendar,"overlap":overlap,"gap":gap}),
            "{zone}"
        );
    }
}

#[tokio::test]
async fn queries_writes_errors_and_discovery_over_the_wire() {
    let server = Server::start().await;
    let host = Host::new();
    let sql = |sql: &str| json!({"connectionString":server.dsn,"sql":sql});

    let large = host.call("mysql", sql("SELECT large_fixture")).await;
    let original = json!({"id":9,"ok":true,"result":large.ok()});
    assert!(serde_json::to_vec(&original).unwrap().len() > 1024 * 1024);
    let mut reader = services_native::adapters::framing::FrameReader::default();
    let mut assembler = services_contracts::native_messaging::ResponseAssembler::default();
    let mut assembled = None;
    for frame in services_native::adapters::framing::response_frames(&original, true).unwrap() {
        let frame = frame.unwrap();
        assert!(frame.len() - 4 <= 1024 * 1024);
        for message in reader.feed(&frame).unwrap() {
            if let Some(value) = assembler.push(&message).unwrap() {
                assert!(assembled.replace(value).is_none());
            }
        }
    }
    assert_eq!(assembled, Some(original));
    let assembled = assembled.unwrap();
    let rows = assembled["result"]["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2048);
    assert!(
        rows.iter()
            .all(|row| row["payload"] == "fixture-🦀\\\"\n".repeat(16))
    );

    let select = host
        .call("mysql", sql("SELECT 42 AS answer, 'fixture' AS label"))
        .await;
    let result = select.ok();
    assert_eq!(result["columns"], json!(["answer", "label"]));
    assert_eq!(result["rows"], json!([{"answer":42,"label":"fixture"}]));
    assert_eq!(
        result["columnTypes"],
        json!({"answer":"number","label":"text"})
    );
    assert_eq!(result["debug"]["connection"]["threadId"], 123);

    let typed = host.call("mysql", sql("SELECT typed_fixture")).await;
    let typed = typed.ok();
    assert_eq!(
        typed["columnTypes"],
        json!({"id_alias":"number","amount":"number","enabled":"boolean","created":"datetime","document":"json","plain":"text"})
    );
    assert_eq!(typed["rows"][0]["amount"], "123456789012345678.123456789");
    assert_eq!(typed["rows"][0]["enabled"], 1);
    assert_eq!(typed["rows"][0]["document"], json!({"n":12,"ok":true}));
    assert_eq!(typed["rows"][0]["plain"], "{\"not\":\"typed\"}");

    let update = host
        .call("mysql", sql("UPDATE pets SET name = 'new'"))
        .await;
    assert_eq!(update.ok()["columns"], json!(["affectedRows"]));
    assert_eq!(update.ok()["rows"], json!([{"affectedRows":7}]));

    let schema = host
        .discover("mysql", json!({"connectionString":server.dsn}))
        .await;
    let schema = schema.ok();
    assert_eq!(schema["tables"][0]["name"], "pets");
    assert_eq!(
        schema["tables"][0]["columns"][0],
        json!({"name":"id","type":"int","primaryKey":true})
    );
    assert_eq!(schema["tables"][0]["columns"][1]["primaryKey"], false);

    let broken = host.call("mysql", sql("BROKEN SQL")).await;
    assert_eq!(broken.error(), "fixture SQL syntax error");
    assert_eq!(broken["debug"]["error"]["errno"], 1064);
    assert_eq!(broken["debug"]["error"]["sqlState"], "42000");
    assert_eq!(broken["debug"]["request"]["sql"], "BROKEN SQL");
}
