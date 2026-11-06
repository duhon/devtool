//! BASELINE: the 3.0 performance reference.
//! Ignored by default; run with
//! `cargo test -p services-native --release --test baseline -- --ignored --nocapture --test-threads 1`.
//! Every metric prints one `BASELINE <name> median=<v> p90=<v> <unit>` line; compare
//! runs before and after a change (+10% is the regression threshold).
//! `SERVICES_BASELINE_BIN` runs another host binary, so both sides of an A/B start from the
//! same directory. Memory is the dirty footprint; `rss.*` is informational.

use axum::{Router, body::Bytes, http::HeaderMap, response::IntoResponse};
use serde_json::{Value, json};
use services_native::adapters::framing::FrameReader;
use services_testkit::{Host, http, mysql};
use std::{
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, ChildStdout, Command},
};
use tokio_util::sync::CancellationToken;

const WARMUP: usize = 5;
const SAMPLES: usize = 40;
/// Sub-millisecond medians drift by up to 30% between runs at 40 samples.
const FAST_SAMPLES: usize = 400;

/// How many samples a metric needs, judged by its last warmup round.
fn sample_count(warm: Duration) -> usize {
    if warm < Duration::from_millis(1) {
        FAST_SAMPLES
    } else {
        SAMPLES
    }
}

fn report(name: &str, unit: &str, mut samples: Vec<f64>) {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let at = |q: f64| samples[((samples.len() - 1) as f64 * q).round() as usize];
    eprintln!(
        "BASELINE {name} median={:.3} p90={:.3} {unit} n={}",
        at(0.5),
        at(0.9),
        samples.len()
    );
}

fn ms(elapsed: Duration) -> f64 {
    elapsed.as_secs_f64() * 1000.0
}

/// A MySQL fixture on its own thread, so serving it does not share the measured loop.
fn isolated_mysql() -> String {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let server = mysql::Server::start().await;
            tx.send(server.dsn.clone()).unwrap();
            std::future::pending::<()>().await;
        });
    });
    rx.recv().unwrap()
}

async fn latency<F, Fut>(name: &str, mut run: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let mut warm = Duration::ZERO;
    for _ in 0..WARMUP {
        let started = Instant::now();
        run().await;
        warm = started.elapsed();
    }
    let count = sample_count(warm);
    let mut samples = Vec::with_capacity(count);
    for _ in 0..count {
        let started = Instant::now();
        run().await;
        samples.push(ms(started.elapsed()));
    }
    report(name, "ms", samples);
}

#[tokio::test]
#[ignore = "performance baseline; run with --release --ignored"]
async fn service_latency() {
    let host = Host::new();
    let (server, _) = http::services().await;
    let url = server.url.clone();
    latency("rest.echo", || async {
        host.call("rest", json!({"method":"post","url":format!("{url}/echo"),"headers":{"X-Token":"secret"},"body":"hello"}))
            .await
            .ok();
    })
    .await;
    latency("graphql.echo", || async {
        host.call(
            "graphql",
            json!({"url":format!("{url}/graphql"),"query":"query Echo { echo }","variables":{}}),
        )
        .await
        .ok();
    })
    .await;
    // Splunk allows one export per origin every 13 s (`splunk_rate_limit.rs`),
    // persisted in the data directory, so every sample uses a fresh host.
    let mut samples = vec![];
    for round in 0..WARMUP + SAMPLES {
        let fresh = Host::new();
        let started = Instant::now();
        fresh.call("splunk", json!({"baseUrl":url,"search":"index=fixture","app":"search","count":"0","earliestTime":"-1h","latestTime":"now","headers":{"Authorization":"Splunk fixture-token"}}))
            .await
            .ok();
        if round >= WARMUP {
            samples.push(ms(started.elapsed()));
        }
    }
    report("splunk.export", "ms", samples);
    let dsn = isolated_mysql();
    latency("mysql.select", || async {
        host.call(
            "mysql",
            json!({"connectionString":dsn,"sql":"SELECT answer_fixture"}),
        )
        .await
        .ok();
    })
    .await;
    latency("mysql.large_2048_rows", || async {
        host.call(
            "mysql",
            json!({"connectionString":dsn,"sql":"SELECT large_fixture"}),
        )
        .await
        .ok();
    })
    .await;
    let grpc = grpc_echo().await;
    latency("grpc.unary", || async {
        host.call("grpc", json!({"endpoint":format!("{}/fixture.Echo/Call",grpc.url),"proto":GRPC_PROTO,"requestMessage":"{\"text\":\"hello\"}"}))
            .await
            .ok();
    })
    .await;
    latency("terminal.short", || async {
        let streamed = host
            .stream(
                json!({"service":"terminal","params":{"endpoint":"sh -c","env":{},"command":"echo fixture"}}),
                CancellationToken::new(),
            )
            .await;
        assert!(streamed.result.is_ok(), "{:?}", streamed.result);
    })
    .await;
}

#[tokio::test]
#[ignore = "performance baseline; run with --release --ignored"]
async fn large_payloads() {
    const MIB: usize = 1024 * 1024;
    let host = Host::new();
    let body = "fixture-🦀\"\\\n".repeat(10 * MIB / 16);
    let size = body.len();
    let server = http::Server::new(Router::new().route(
        "/large",
        axum::routing::get(move || {
            let body = body.clone();
            async move { body }
        }),
    ))
    .await;
    let url = format!("{}/large", server.url);
    latency("rest.large_10mib", || async {
        let reply = host.call("rest", json!({"method":"get","url":url})).await;
        assert_eq!(reply.ok()["body"].as_str().map(str::len), Some(size));
    })
    .await;
    let mut samples = vec![];
    for round in 0..WARMUP + 10 {
        let started = Instant::now();
        let streamed = host
            .stream(
                json!({"service":"terminal","params":{"endpoint":"sh -c","env":{},
                    "command":"head -c 20971520 /dev/zero | tr '\\0' a"}}),
                CancellationToken::new(),
            )
            .await;
        let elapsed = started.elapsed();
        assert!(streamed.result.is_ok(), "{:?}", streamed.result);
        let bytes: usize = streamed
            .events
            .iter()
            .filter_map(|e| e["data"].as_str())
            .map(str::len)
            .sum();
        assert_eq!(bytes, 20 * MIB);
        if round >= WARMUP {
            samples.push(20.0 / elapsed.as_secs_f64());
        }
    }
    report("terminal.stream_20mib", "MiB/s", samples);
}

const GRPC_PROTO: &str = r#"syntax = "proto3";
package fixture;
message Text { string text = 1; }
service Echo { rpc Call(Text) returns (Text); }
"#;

struct GrpcEcho {
    url: String,
    _server: http::Server,
}

/// A plaintext HTTP/2 server that echoes every unary gRPC frame back with an
/// OK status, so the measurement covers the client, not a real service.
async fn grpc_echo() -> GrpcEcho {
    async fn echo(body: Bytes) -> impl IntoResponse {
        let mut trailers = HeaderMap::new();
        trailers.insert("grpc-status", "0".parse().unwrap());
        let frames = futures_util::stream::iter([
            Ok::<_, std::convert::Infallible>(http_body::Frame::data(body)),
            Ok(http_body::Frame::trailers(trailers)),
        ]);
        (
            [("content-type", "application/grpc")],
            axum::body::Body::new(http_body_util::StreamBody::new(frames)),
        )
    }
    let server = http::Server::new(Router::new().fallback(echo)).await;
    GrpcEcho {
        url: server.url.replace("http://", "grpc://"),
        _server: server,
    }
}

/// The real host binary speaking native messaging over pipes, as Chrome runs it.
struct Native {
    child: Child,
    input: ChildStdin,
    output: ChildStdout,
    reader: FrameReader,
    assembler: services_contracts::native_messaging::ResponseAssembler,
    pending: std::collections::VecDeque<Value>,
    next: u64,
}

impl Native {
    fn spawn(data: &Path, mode: &str) -> Self {
        let binary = std::env::var_os("SERVICES_BASELINE_BIN")
            .unwrap_or_else(|| env!("CARGO_BIN_EXE_services-devtools").into());
        let mut child = Command::new(binary)
            .arg(mode)
            .env("SERVICES_DATA_DIR", data)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("the host binary must start");
        Self {
            input: child.stdin.take().unwrap(),
            output: child.stdout.take().unwrap(),
            child,
            reader: FrameReader::default(),
            assembler: Default::default(),
            pending: Default::default(),
            next: 0,
        }
    }

    async fn request(&mut self, mut message: Value) -> Value {
        self.next += 1;
        let id = self.next;
        message["id"] = json!(id);
        let body = serde_json::to_vec(&message).unwrap();
        let mut frame = (body.len() as u32).to_le_bytes().to_vec();
        frame.extend(body);
        self.input.write_all(&frame).await.unwrap();
        self.input.flush().await.unwrap();
        loop {
            while let Some(message) = self.pending.pop_front() {
                let value = if message["type"] == "nativeChunk" {
                    match self.assembler.push(&message).unwrap() {
                        Some(value) => value,
                        None => continue,
                    }
                } else {
                    message
                };
                if value["id"] == json!(id) && value.get("ok").is_some() {
                    return value;
                }
            }
            let mut buffer = vec![0; 64 * 1024];
            let read = tokio::time::timeout(Duration::from_secs(30), self.output.read(&mut buffer))
                .await
                .expect("the host must answer")
                .unwrap();
            assert!(read > 0, "the host closed its output");
            self.pending
                .extend(self.reader.feed(&buffer[..read]).unwrap());
        }
    }

    async fn health(&mut self) -> Value {
        self.request(json!({"type":"health","capabilities":{"chunkedResponses":true}}))
            .await
    }

    fn rss_mib(&self) -> f64 {
        let pid = self.child.id().unwrap().to_string();
        let output = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &pid])
            .output()
            .unwrap();
        let kib: f64 = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .unwrap();
        kib / 1024.0
    }

    /// Dirty memory the process owns (physical footprint, as Activity Monitor shows it); unlike
    /// RSS it does not count clean code pages the system can drop.
    fn footprint_mib(&self) -> f64 {
        let pid = self.child.id().unwrap() as libc::c_int;
        // SAFETY: `rusage_info_v2` is plain data; the kernel fills it for `RUSAGE_INFO_V2`.
        let mut info: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::proc_pid_rusage(
                pid,
                libc::RUSAGE_INFO_V2,
                (&raw mut info).cast::<libc::rusage_info_t>(),
            )
        };
        assert_eq!(result, 0, "proc_pid_rusage failed");
        info.ri_phys_footprint as f64 / (1024.0 * 1024.0)
    }
}

#[tokio::test]
#[ignore = "performance baseline; run with --release --ignored"]
async fn host_process() {
    let data = services_testkit::DataDir::new();
    for (name, mode, rounds) in [
        ("startup.bare", "__native-host-shell", SAMPLES),
        ("startup.login_shell", "native-host", 15),
    ] {
        let mut samples = vec![];
        for round in 0..WARMUP + rounds {
            let started = Instant::now();
            let mut native = Native::spawn(data.path(), mode);
            let reply = native.health().await;
            let elapsed = started.elapsed();
            assert_eq!(reply["ok"], true, "{reply}");
            if round >= WARMUP {
                samples.push(ms(elapsed));
            }
        }
        report(name, "ms", samples);
    }
    let (server, _) = http::services().await;
    let large = "fixture-🦀\"\\\n".repeat(5 * 1024 * 1024 / 16);
    let large_server = http::Server::new(Router::new().route(
        "/large",
        axum::routing::get(move || {
            let body = large.clone();
            async move { body }
        }),
    ))
    .await;
    let (mut idle, mut idle_rss) = (vec![], vec![]);
    let (mut loaded, mut loaded_rss) = (vec![], vec![]);
    for _ in 0..5 {
        let mut native = Native::spawn(data.path(), "__native-host-shell");
        native.health().await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        idle.push(native.footprint_mib());
        idle_rss.push(native.rss_mib());
        for _ in 0..50 {
            let reply = native
                .request(json!({"service":"rest","params":{"method":"post","url":format!("{}/echo",server.url),"body":"hello"}}))
                .await;
            assert_eq!(reply["ok"], true, "{reply}");
        }
        for _ in 0..5 {
            let reply = native
                .request(json!({"service":"rest","params":{"method":"get","url":format!("{}/large",large_server.url)}}))
                .await;
            assert_eq!(reply["ok"], true, "{reply}");
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        loaded.push(native.footprint_mib());
        loaded_rss.push(native.rss_mib());
    }
    report("footprint.idle", "MiB", idle);
    report("footprint.after_workload", "MiB", loaded);
    report("rss.idle", "MiB", idle_rss);
    report("rss.after_workload", "MiB", loaded_rss);
}

/// Answers as Chrome receives them: through the host process, framed and chunked.
#[tokio::test]
#[ignore = "performance baseline; run with --release --ignored"]
async fn host_large_response() {
    let data = services_testkit::DataDir::new();
    let body = "fixture-🦀\"\\\n".repeat(10 * 1024 * 1024 / 16);
    let size = body.len();
    let server = http::Server::new(Router::new().route(
        "/large",
        axum::routing::get(move || {
            let body = body.clone();
            async move { body }
        }),
    ))
    .await;
    let mut native = Native::spawn(data.path(), "__native-host-shell");
    native.health().await;
    let url = format!("{}/large", server.url);
    let mut samples = vec![];
    for round in 0..WARMUP + SAMPLES {
        let started = Instant::now();
        let reply = native
            .request(json!({"service":"rest","params":{"method":"get","url":url}}))
            .await;
        let elapsed = started.elapsed();
        assert_eq!(reply["result"]["body"].as_str().map(str::len), Some(size));
        if round >= WARMUP {
            samples.push(ms(elapsed));
        }
    }
    report("host.rest_large_10mib", "ms", samples);
    report("footprint.after_large", "MiB", vec![native.footprint_mib()]);
    report("rss.after_large", "MiB", vec![native.rss_mib()]);
    let dsn = isolated_mysql();
    for (name, sql) in [
        ("host.mysql_select", "SELECT answer_fixture"),
        ("host.mysql_large_2048_rows", "SELECT large_fixture"),
    ] {
        let (mut samples, mut count, mut round) = (vec![], SAMPLES, 0);
        while samples.len() < count {
            let started = Instant::now();
            let reply = native
                .request(json!({"service":"mysql","params":{"connectionString":dsn,"sql":sql}}))
                .await;
            let elapsed = started.elapsed();
            assert_eq!(reply["ok"], true, "{reply}");
            if round >= WARMUP {
                samples.push(ms(elapsed));
            } else if round == WARMUP - 1 {
                count = sample_count(elapsed);
            }
            round += 1;
        }
        report(name, "ms", samples);
    }
}
