use serde_json::json;
use services_native::services::mysql::Mysql;
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

async fn packet(stream: &mut TcpStream, sequence: u8, payload: &[u8]) -> std::io::Result<()> {
    let len = payload.len() as u32;
    stream
        .write_all(&[len as u8, (len >> 8) as u8, (len >> 16) as u8, sequence])
        .await?;
    stream.write_all(payload).await
}
async fn read(stream: &mut TcpStream) -> std::io::Result<Vec<u8>> {
    let mut header = [0; 4];
    stream.read_exact(&mut header).await?;
    let len = header[0] as usize | ((header[1] as usize) << 8) | ((header[2] as usize) << 16);
    let mut bytes = vec![0; len];
    stream.read_exact(&mut bytes).await?;
    Ok(bytes)
}
fn lenstr(bytes: &[u8]) -> Vec<u8> {
    assert!(bytes.len() < 251);
    let mut out = vec![bytes.len() as u8];
    out.extend(bytes);
    out
}
fn column(name: &str, kind: u8) -> Vec<u8> {
    let mut out = vec![];
    for value in ["def", "fixture", "", "", name, name] {
        out.extend(lenstr(value.as_bytes()));
    }
    out.extend([
        12,
        45,
        0,
        if kind == 1 { 1 } else { 255 },
        0,
        0,
        0,
        kind,
        0,
        0,
        0,
        0,
        0,
    ]);
    out
}
async fn rows(
    stream: &mut TcpStream,
    columns: &[(&str, u8)],
    rows: &[Vec<&str>],
) -> std::io::Result<()> {
    packet(stream, 1, &[columns.len() as u8]).await?;
    let mut seq: u8 = 2;
    for (name, kind) in columns {
        packet(stream, seq, &column(name, *kind)).await?;
        seq = seq.wrapping_add(1);
    }
    packet(stream, seq, &[0xfe, 0, 0, 2, 0]).await?;
    seq = seq.wrapping_add(1);
    for row in rows {
        let bytes = row
            .iter()
            .flat_map(|s| lenstr(s.as_bytes()))
            .collect::<Vec<_>>();
        packet(stream, seq, &bytes).await?;
        seq = seq.wrapping_add(1);
    }
    packet(stream, seq, &[0xfe, 0, 0, 2, 0]).await
}
async fn connection(stream: TcpStream) -> std::io::Result<()> {
    connection_controlled(stream, 123, None).await
}
#[derive(Default)]
struct CancellationFixture {
    running:
        tokio::sync::Mutex<std::collections::HashMap<u32, tokio_util::sync::CancellationToken>>,
    started: tokio::sync::Notify,
    killed: tokio::sync::Mutex<Vec<u32>>,
    release: tokio_util::sync::CancellationToken,
}
async fn connection_controlled(
    mut stream: TcpStream,
    id: u32,
    control: Option<std::sync::Arc<CancellationFixture>>,
) -> std::io::Result<()> {
    let caps: u32 = 1 | 4 | 8 | 0x200 | 0x2000 | 0x8000 | 0x20000 | 0x80000;
    let mut handshake = vec![10];
    handshake.extend(b"8.0.36-rust-fixture\0");
    handshake.extend(id.to_le_bytes());
    handshake.extend(b"12345678\0");
    handshake.extend((caps as u16).to_le_bytes());
    handshake.push(45);
    handshake.extend(2u16.to_le_bytes());
    handshake.extend(((caps >> 16) as u16).to_le_bytes());
    handshake.push(21);
    handshake.extend([0; 10]);
    handshake.extend(b"abcdefghijkl\0mysql_native_password\0");
    packet(&mut stream, 0, &handshake).await?;
    let _auth = read(&mut stream).await?;
    packet(&mut stream, 2, &[0, 0, 0, 2, 0, 0, 0]).await?;
    loop {
        let bytes = read(&mut stream).await?;
        match bytes.first() {
            Some(1) => return Ok(()),
            Some(3) => {
                let sql = String::from_utf8_lossy(&bytes[1..]);
                if let Some(control) = &control {
                    if let Some(target) = sql
                        .strip_prefix("KILL QUERY ")
                        .and_then(|id| id.parse::<u32>().ok())
                    {
                        control.killed.lock().await.push(target);
                        control.running.lock().await.get(&target).unwrap().cancel();
                        packet(&mut stream, 1, &[0, 0, 0, 2, 0, 0, 0]).await?;
                        continue;
                    }
                    if sql.contains("blocked_fixture") {
                        let token = tokio_util::sync::CancellationToken::new();
                        control.running.lock().await.insert(id, token.clone());
                        control.started.notify_one();
                        token.cancelled().await;
                        let mut error = vec![0xff, 0x25, 0x05];
                        error.extend(b"#70100Query execution was interrupted");
                        packet(&mut stream, 1, &error).await?;
                        continue;
                    }
                    if sql.contains("unrelated_fixture") {
                        control.started.notify_one();
                        control.release.cancelled().await;
                    }
                }
                if sql.contains("@@max_allowed_packet") {
                    rows(
                        &mut stream,
                        &[("@@max_allowed_packet", 8), ("@@wait_timeout", 8)],
                        &[vec!["16777216", "28800"]],
                    )
                    .await?;
                } else if sql.contains("information_schema.columns") {
                    rows(
                        &mut stream,
                        &[
                            ("tableName", 253),
                            ("columnName", 253),
                            ("columnType", 253),
                            ("columnKey", 253),
                        ],
                        &[
                            vec!["pets", "id", "int", "PRI"],
                            vec!["pets", "name", "varchar(20)", ""],
                        ],
                    )
                    .await?;
                } else if sql.contains("large_fixture") {
                    let payload = "fixture-🦀\\\"\n".repeat(16);
                    rows(
                        &mut stream,
                        &[("payload", 253)],
                        &vec![vec![payload.as_str()]; 2048],
                    )
                    .await?;
                } else if sql.contains("typed_fixture") {
                    rows(
                        &mut stream,
                        &[
                            ("id_alias", 3),
                            ("amount", 246),
                            ("enabled", 1),
                            ("created", 12),
                            ("document", 245),
                            ("plain", 253),
                        ],
                        &[vec![
                            "42",
                            "123456789012345678.123456789",
                            "1",
                            "2026-01-15 12:34:56",
                            "{\"n\":12,\"ok\":true}",
                            "{\"not\":\"typed\"}",
                        ]],
                    )
                    .await?;
                } else if sql.contains("dates_fixture") {
                    rows(
                        &mut stream,
                        &[
                            ("winter", 12),
                            ("summer", 12),
                            ("calendar", 10),
                            ("overlap", 12),
                            ("gap", 7),
                        ],
                        &[vec![
                            "2026-01-15 12:34:56.123456",
                            "2026-07-15 12:34:56.123456",
                            "2026-01-01",
                            "2026-11-01 01:30:00",
                            "2026-03-08 02:30:00",
                        ]],
                    )
                    .await?;
                } else if sql.starts_with("SELECT") {
                    rows(
                        &mut stream,
                        &[("answer", 3), ("label", 253)],
                        &[vec!["42", "fixture"]],
                    )
                    .await?;
                } else if sql.starts_with("UPDATE") {
                    packet(&mut stream, 1, &[0, 7, 0, 2, 0, 0, 0]).await?;
                } else if sql.starts_with("BROKEN") {
                    let mut error = vec![0xff, 0x28, 0x04];
                    error.extend(b"#42000fixture SQL syntax error");
                    packet(&mut stream, 1, &error).await?;
                } else {
                    packet(&mut stream, 1, &[0, 0, 0, 2, 0, 0, 0]).await?;
                }
            }
            _ => packet(&mut stream, 1, &[0, 0, 0, 2, 0, 0, 0]).await?,
        }
    }
}

#[tokio::test]
async fn cancellation_kills_only_its_mysql_thread_and_does_not_reuse_the_connection() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let control = std::sync::Arc::new(CancellationFixture::default());
    let state = control.clone();
    let server = tokio::spawn(async move {
        let mut next = 123;
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let state = state.clone();
            let id = next;
            next += 1;
            tasks.spawn(async move {
                let _ = connection_controlled(stream, id, Some(state)).await;
            });
        }
    });
    let mysql = Mysql::default();
    let dsn = format!("mysql://fixture@{addr}/fixture?prefer_socket=false");
    let cancel = tokio_util::sync::CancellationToken::new();
    let db = mysql.clone();
    let token = cancel.clone();
    let params = json!({"connectionString":dsn,"sql":"SELECT blocked_fixture"});
    let request = tokio::spawn(async move { db.send_cancellable(&params, token).await });
    tokio::time::timeout(Duration::from_secs(3), control.started.notified())
        .await
        .unwrap();
    let db = mysql.clone();
    let params = json!({"connectionString":dsn,"sql":"SELECT unrelated_fixture"});
    let sibling = tokio::spawn(async move { db.send(&params).await });
    tokio::time::timeout(Duration::from_secs(3), control.started.notified())
        .await
        .unwrap();
    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(4), request)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(result["cancelled"], true);
    assert_eq!(result["threadId"], 123);
    assert_eq!(*control.killed.lock().await, vec![123]);
    assert!(
        !sibling.is_finished(),
        "Cancellation must not finish another connection's query"
    );
    control.release.cancel();
    let result = tokio::time::timeout(Duration::from_secs(3), sibling)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(result["rows"][0]["answer"], 42);
    let result = mysql
        .send(&json!({"connectionString":dsn,"sql":"SELECT 42"}))
        .await
        .unwrap();
    assert_ne!(
        result["debug"]["connection"]["threadId"], 123,
        "Cancelled connection must not return to the pool"
    );
    server.abort();
    let _ = server.await;
}
#[tokio::test]
async fn mysql_dates_use_child_process_host_timezone_without_global_environment_mutation() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accept=listener.accept()=>{
                    let (stream,_)=accept.unwrap();
                    tasks.spawn(async move{let _=connection(stream).await;});
                }
                _=tasks.join_next(),if !tasks.is_empty()=>{}
            }
        }
    });
    struct Data(std::path::PathBuf);
    impl Drop for Data {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let data = Data(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-data")
            .join(format!("mysql-timezone-{}", std::process::id())),
    );
    std::fs::create_dir_all(&data.0).unwrap();
    let dsn = format!("mysql://fixture@{addr}/fixture?prefer_socket=false");
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
                &dsn,
                "--sql",
                "SELECT dates_fixture",
            ])
            .env("TZ", zone)
            .env("SERVICES_DATA_DIR", &data.0)
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
    server.abort();
    let _ = server.await;
}
#[tokio::test]
async fn mysql_real_wire_query_write_pool_schema_and_large_native_response() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accept=listener.accept()=>{
                    let (stream,_)=accept.unwrap();
                    tasks.spawn(async move{let _=connection(stream).await;});
                }
                _=tasks.join_next(),if !tasks.is_empty()=>{}
            }
        }
    });
    let mysql = Mysql::default();
    let dsn = format!("mysql://fixture@{addr}/fixture?prefer_socket=false");
    let large = mysql
        .send(&json!({"connectionString":dsn,"sql":"SELECT large_fixture"}))
        .await
        .unwrap();
    let original = json!({"id":9,"ok":true,"result":large});
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
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        mysql
            .send(&json!({"connectionString":dsn,"sql":"SELECT 42 AS answer, 'fixture' AS label"})),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result["columns"], json!(["answer", "label"]));
    assert_eq!(result["rows"], json!([{"answer":42,"label":"fixture"}]));
    assert_eq!(
        result["columnTypes"],
        json!({"answer":"number","label":"text"})
    );
    assert_eq!(result["debug"]["connection"]["threadId"], 123);
    let typed = mysql
        .send(&json!({"connectionString":dsn,"sql":"SELECT typed_fixture"}))
        .await
        .unwrap();
    assert_eq!(
        typed["columnTypes"],
        json!({"id_alias":"number","amount":"number","enabled":"boolean","created":"datetime","document":"json","plain":"text"})
    );
    assert_eq!(typed["rows"][0]["amount"], "123456789012345678.123456789");
    assert_eq!(typed["rows"][0]["enabled"], 1);
    assert_eq!(typed["rows"][0]["document"], json!({"n":12,"ok":true}));
    assert_eq!(typed["rows"][0]["plain"], "{\"not\":\"typed\"}");
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        mysql.send(&json!({"connectionString":dsn,"sql":"UPDATE pets SET name = 'new'"})),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result["columns"], json!(["affectedRows"]));
    assert_eq!(result["rows"], json!([{"affectedRows":7}]));
    let schema = tokio::time::timeout(
        Duration::from_secs(5),
        mysql.discover(&json!({"connectionString":dsn})),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(schema["tables"][0]["name"], "pets");
    assert_eq!(
        schema["tables"][0]["columns"][0],
        json!({"name":"id","type":"int","primaryKey":true})
    );
    let error = mysql
        .send(&json!({"connectionString":dsn,"sql":"BROKEN SQL"}))
        .await
        .unwrap_err();
    assert_eq!(error.message, "fixture SQL syntax error");
    let debug = error.debug.unwrap();
    assert_eq!(debug["error"]["errno"], 1064);
    assert_eq!(debug["error"]["sqlState"], "42000");
    assert_eq!(debug["request"]["sql"], "BROKEN SQL");
    server.abort();
    let _ = server.await;
}
