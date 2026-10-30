//! A MySQL wire-protocol server that answers fixed queries: `SELECT`,
//! `UPDATE`, `BROKEN`, `large_fixture`, `typed_fixture`, `dates_fixture` and
//! the schema query used by discovery.

use std::sync::Arc;
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
pub async fn connection(stream: TcpStream) -> std::io::Result<()> {
    connection_controlled(stream, 123, None).await
}
/// Lets a test block a query (`blocked_fixture`, `unrelated_fixture`) and
/// observe `KILL QUERY`.
#[derive(Default)]
pub struct CancellationFixture {
    pub running:
        tokio::sync::Mutex<std::collections::HashMap<u32, tokio_util::sync::CancellationToken>>,
    pub started: tokio::sync::Notify,
    pub killed: tokio::sync::Mutex<Vec<u32>>,
    pub release: tokio_util::sync::CancellationToken,
}
pub async fn connection_controlled(
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

/// A running wire server; aborted on drop.
pub struct Server {
    pub dsn: String,
    task: tokio::task::JoinHandle<()>,
}

impl Server {
    /// Every connection answers the fixed queries.
    pub async fn start() -> Self {
        Self::spawn(None).await
    }

    /// Connections get ids from 123 and honor the cancellation control.
    pub async fn controlled(control: Arc<CancellationFixture>) -> Self {
        Self::spawn(Some(control)).await
    }

    async fn spawn(control: Option<Arc<CancellationFixture>>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let mut next = 123;
            let mut tasks = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accept = listener.accept() => {
                        let (stream, _) = accept.unwrap();
                        let control = control.clone();
                        let id = next;
                        if control.is_some() {
                            next += 1;
                        }
                        tasks.spawn(async move {
                            let _ = connection_controlled(stream, id, control).await;
                        });
                    }
                    _ = tasks.join_next(), if !tasks.is_empty() => {}
                }
            }
        });
        Self {
            dsn: format!("mysql://fixture@{addr}/fixture?prefer_socket=false"),
            task,
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
