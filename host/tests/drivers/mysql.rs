use serde_json::{Value, json};
use services_sdk::mysql::{self, Event};
use services_testkit::{
    DataDir,
    mysql::{CancellationFixture, Server},
    native::Pipe,
};
use std::{sync::Arc, time::Duration};

fn call(id: u32, op: &str, request: &mysql::Request) -> Value {
    super::call("mysql", id, op, json!(request))
}

#[tokio::test]
async fn mysql_call_answers_sets_typed_cells_and_server_errors() {
    let (data, server) = (DataDir::new(), Server::start().await);
    let runtime = data.runtime();
    let reply = runtime
        .dispatch(&call(
            1,
            "call",
            &mysql::Request::new(&server.dsn, "SELECT typed_fixture"),
        ))
        .await;
    assert_eq!(reply["ok"], true, "{reply}");
    let response = mysql::Response::parse(&reply["result"]).unwrap();
    assert_eq!(response.thread_id, 123);
    let set = &response.sets[0];
    let kinds: Vec<&str> = set.columns.iter().map(|c| c.kind.as_str()).collect();
    assert_eq!(
        kinds,
        [
            "long",
            "newdecimal",
            "tiny",
            "datetime",
            "json",
            "var_string"
        ]
    );
    assert_eq!(set.columns[2].length, 1);
    assert_eq!(
        set.rows,
        [vec![
            json!(42),
            json!("123456789012345678.123456789"),
            json!(1),
            json!("2026-01-15 12:34:56"),
            json!("{\"n\":12,\"ok\":true}"),
            json!("{\"not\":\"typed\"}"),
        ]]
    );

    let reply = runtime
        .dispatch(&call(
            2,
            "call",
            &mysql::Request::new(&server.dsn, "UPDATE pets"),
        ))
        .await;
    let response = mysql::Response::parse(&reply["result"]).unwrap();
    assert_eq!(response.sets[0].summary.affected_rows, 7);
    assert!(response.sets[0].columns.is_empty());

    let reply = runtime
        .dispatch(&call(
            3,
            "call",
            &mysql::Request::new(&server.dsn, "BROKEN"),
        ))
        .await;
    assert_eq!(reply["ok"], true, "a server error is an answer: {reply}");
    let error = mysql::Response::parse(&reply["result"])
        .unwrap()
        .error
        .unwrap();
    assert_eq!((error.code, error.state.as_str()), (1064, "42000"));
    assert_eq!(error.message, "fixture SQL syntax error");

    let mut large = mysql::Request::new(&server.dsn, "SELECT large_fixture");
    large.max_rows = Some(10);
    let reply = runtime.dispatch(&call(4, "call", &large)).await;
    let set = &mysql::Response::parse(&reply["result"]).unwrap().sets[0];
    assert_eq!(set.rows.len(), 10);
    assert!(set.summary.truncated);
}

#[tokio::test]
async fn mysql_stream_sends_columns_row_batches_and_done() {
    let (data, server) = (DataDir::new(), Server::start().await);
    let mut pipe = Pipe::start(data.runtime());
    let request = mysql::Request::new(&server.dsn, "SELECT large_fixture");
    pipe.send(&call(1, "stream", &request)).await;
    let (events, exit) = super::events(&mut pipe).await;
    let events: Vec<Event> = events.iter().map(|e| Event::parse(e).unwrap()).collect();
    assert!(matches!(&events[0], Event::Columns(columns) if columns[0].name == "payload"));
    let rows: usize = events
        .iter()
        .map(|event| match event {
            Event::Rows(rows) => rows.len(),
            _ => 0,
        })
        .sum();
    assert_eq!(rows, 2048);
    assert!(events.len() > 3, "rows come in batches");
    assert!(matches!(events.last(), Some(Event::Done(summary)) if !summary.truncated));
    let response = mysql::Response::parse(&exit["info"]).unwrap();
    assert!(response.sets.is_empty());
}

#[tokio::test]
async fn mysql_cancel_and_timeout_kill_the_query() {
    let control = Arc::new(CancellationFixture::default());
    let (data, server) = (DataDir::new(), Server::controlled(control.clone()).await);
    let mut pipe = Pipe::start(data.runtime());
    let request = mysql::Request::new(&server.dsn, "SELECT blocked_fixture");
    pipe.send(&call(1, "stream", &request)).await;
    control.started.notified().await;
    pipe.send(&json!({"id": 1, "type": "stop"})).await;
    let (_, reply) = super::events(&mut pipe).await;
    assert_eq!(reply["failure"]["stage"], "cancelled", "{reply}");
    assert_eq!(*control.killed.lock().await, [123]);
    let causes = reply["failure"]["causes"].to_string();
    assert!(causes.contains("KILL QUERY 123 completed"), "{causes}");

    let mut slow = request.clone();
    slow.timeout_ms = Some(100);
    let reply = tokio::time::timeout(
        Duration::from_secs(10),
        data.runtime().dispatch(&call(2, "call", &slow)),
    )
    .await
    .unwrap();
    assert_eq!(reply["failure"]["stage"], "timeout", "{reply}");
    assert_eq!(control.killed.lock().await.len(), 2);
}

#[tokio::test]
async fn mysql_failures_name_their_stage() {
    let data = DataDir::new();
    let runtime = data.runtime();
    let reply = runtime
        .dispatch(&call(
            1,
            "call",
            &mysql::Request::new("postgres://x@127.0.0.1/db", "SELECT 1"),
        ))
        .await;
    assert_eq!(reply["failure"]["stage"], "send", "{reply}");
    let reply = runtime
        .dispatch(&call(
            2,
            "call",
            &mysql::Request::new("mysql://x@127.0.0.1:1/db", "SELECT 1"),
        ))
        .await;
    assert_eq!(reply["failure"]["stage"], "connect", "{reply}");
}
