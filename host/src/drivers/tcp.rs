//! The tcp driver: connect (plain or TLS), write the request bytes, read until the peer closes,
//! `maxBytes` or `idleMs`. A session instead writes each `input` when it comes and reads until
//! the peer closes, `maxBytes`, `idleMs` or it is cancelled.

use super::{CONNECT_MS, Failure, INPUT_QUEUE, Input, emit, http::complete_text, micros, millis};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use rustls::{
    ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, ServerName, UnixTime, pem::PemObject},
};
use serde_json::{Value, json};
use services_sdk::{
    driver::Stage,
    http::Remote,
    tcp::{Closed, Request, Response, Timing},
};
use std::{
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    sync::mpsc,
};

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// With `events`, the stream sends `connected` and the data as `chunk` / `chunkBase64`; the
/// answer carries no data.
pub async fn run(request: Request, events: Option<&mpsc::Sender<Value>>) -> Result<Value, Failure> {
    if request.session {
        return Err(Failure::new(Stage::Send, "A tcp session is a stream"));
    }
    let work = exchange(&request, events);
    match request.timeout_ms {
        Some(ms) => tokio::time::timeout(millis(ms), work)
            .await
            .unwrap_or_else(|_| Err(Failure::new(Stage::Timeout, "The exchange timed out"))),
        None => work.await,
    }
}

async fn exchange(
    request: &Request,
    events: Option<&mpsc::Sender<Value>>,
) -> Result<Value, Failure> {
    let started = Instant::now();
    let data = data(request)?;
    let (mut stream, remote, mut timing) = open(request, started).await?;
    if let Some(events) = events {
        emit(events, json!({"connected": {"remote": remote}})).await?;
    }
    let sent = async {
        stream.write_all(&data).await?;
        stream.flush().await?;
        if request.close_write {
            stream.shutdown().await?;
        }
        Ok::<_, std::io::Error>(())
    };
    sent.await
        .map_err(|error| Failure::chain(Stage::Send, &error))?;

    let mut kept = Vec::new();
    let mut pending = Vec::new();
    let mut bytes = 0u64;
    let mut buf = vec![0u8; 16 * 1024];
    let closed = loop {
        let wanted = wanted(request, bytes, buf.len());
        if wanted == 0 {
            break Closed::Limit;
        }
        let read = stream.read(&mut buf[..wanted]);
        let read = match request.idle_ms {
            Some(ms) => match tokio::time::timeout(Duration::from_millis(ms), read).await {
                Ok(read) => read,
                Err(_) => break Closed::Idle,
            },
            None => read.await,
        };
        let n = match read {
            Ok(0) => break Closed::Peer,
            Ok(n) => n,
            // A TLS peer that closes without close_notify.
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break Closed::Peer,
            Err(error) => return Err(Failure::chain(Stage::Decode, &error)),
        };
        if bytes == 0 {
            timing.first_byte_us = micros(started);
        }
        bytes += n as u64;
        let chunk = &buf[..n];
        let Some(events) = events else {
            kept.extend_from_slice(chunk);
            continue;
        };
        let event = if request.text {
            pending.extend_from_slice(chunk);
            let text = complete_text(&mut pending);
            if text.is_empty() {
                continue;
            }
            json!({ "chunk": text })
        } else {
            json!({ "chunkBase64": B64.encode(chunk) })
        };
        emit(events, event).await?;
    };
    if let Some(events) = events.filter(|_| !pending.is_empty()) {
        let text = String::from_utf8_lossy(&pending).into_owned();
        emit(events, json!({ "chunk": text })).await?;
    }
    timing.total_us = micros(started);
    let mut response = Response {
        bytes,
        closed,
        remote,
        timing,
        ..Response::default()
    };
    if events.is_none() {
        if request.text {
            response.data = Some(String::from_utf8_lossy(&kept).into_owned());
        } else {
            response.data_base64 = Some(B64.encode(&kept));
        }
    }
    Ok(json!(response))
}

/// A session: sends `connected`, writes the request data and then each `input` as it comes,
/// each followed by `written`, and the bytes read as `chunkBase64` as they arrive. Reading waits
/// while `events` is full. It ends with the [`Response`] when the peer closes, at `maxBytes` or
/// `idleMs` (none by default), or fails when input overflows; dropping it (cancel) closes the
/// connection.
pub async fn session(
    request: Request,
    events: &mpsc::Sender<Value>,
    input: Input,
) -> Result<Value, Failure> {
    tokio::select! {
        result = converse(request, events, input.messages) => result,
        () = input.overflow.cancelled() => Err(Failure::new(
            Stage::Write,
            format!("More than {INPUT_QUEUE} writes were waiting"),
        )),
    }
}

async fn converse(
    request: Request,
    events: &mpsc::Sender<Value>,
    mut input: mpsc::Receiver<Value>,
) -> Result<Value, Failure> {
    if request.timeout_ms.is_some() || request.close_write {
        return Err(Failure::new(
            Stage::Send,
            "A tcp session takes neither timeoutMs nor closeWrite",
        ));
    }
    let started = Instant::now();
    let first = data(&request)?;
    let (mut stream, remote, mut timing) = open(&request, started).await?;
    emit(events, json!({"connected": {"remote": remote}})).await?;
    let mut written = 0u64;
    if !first.is_empty() {
        written += write(&mut stream, &first, events).await?;
    }
    let idle = request.idle_ms.map(millis);
    let mut deadline = idle.map(|idle| tokio::time::Instant::now() + idle);
    let mut inputs = true;
    let mut bytes = 0u64;
    let mut buf = vec![0u8; 16 * 1024];
    let closed = loop {
        let wanted = wanted(&request, bytes, buf.len());
        if wanted == 0 {
            break Closed::Limit;
        }
        tokio::select! {
            read = stream.read(&mut buf[..wanted]) => {
                let n = match read {
                    Ok(0) => break Closed::Peer,
                    Ok(n) => n,
                    Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                        break Closed::Peer;
                    }
                    Err(error) => return Err(Failure::chain(Stage::Read, &error)),
                };
                if bytes == 0 {
                    timing.first_byte_us = micros(started);
                }
                bytes += n as u64;
                emit(events, json!({ "chunkBase64": B64.encode(&buf[..n]) })).await?;
                deadline = idle.map(|idle| tokio::time::Instant::now() + idle);
            }
            next = input.recv(), if inputs => match next {
                Some(message) => {
                    let data = message["dataBase64"]
                        .as_str()
                        .ok_or("no dataBase64".to_owned())
                        .and_then(|data| B64.decode(data).map_err(|error| error.to_string()))
                        .map_err(|error| Failure::new(Stage::Write, format!("Invalid input: {error}")))?;
                    written += write(&mut stream, &data, events).await?;
                }
                // No more input (overflow or the Host is closing); what ends the session follows.
                None => inputs = false,
            },
            () = tokio::time::sleep_until(deadline.unwrap_or_else(tokio::time::Instant::now)),
                if deadline.is_some() => break Closed::Idle,
        }
    };
    timing.total_us = micros(started);
    Ok(json!(Response {
        bytes,
        written,
        closed,
        remote,
        timing,
        ..Response::default()
    }))
}

/// Writes `data` whole, then says so.
async fn write(
    stream: &mut Box<dyn Io>,
    data: &[u8],
    events: &mpsc::Sender<Value>,
) -> Result<u64, Failure> {
    let sent = async {
        stream.write_all(data).await?;
        stream.flush().await
    };
    sent.await
        .map_err(|error| Failure::chain(Stage::Write, &error))?;
    emit(events, json!({"written": {"bytes": data.len()}})).await?;
    Ok(data.len() as u64)
}

/// The request data: `data`, else `dataBase64`.
fn data(request: &Request) -> Result<Vec<u8>, Failure> {
    match (&request.data, &request.data_base64) {
        (Some(data), _) => Ok(data.clone().into_bytes()),
        (None, Some(data)) => B64
            .decode(data)
            .map_err(|error| Failure::new(Stage::Send, format!("Invalid dataBase64: {error}"))),
        (None, None) => Ok(Vec::new()),
    }
}

/// How much the next read may take before `maxBytes`.
fn wanted(request: &Request, bytes: u64, buffer: usize) -> usize {
    request
        .max_bytes
        .map_or(buffer, |max| buffer.min(max.saturating_sub(bytes) as usize))
}

/// Connects within `connectMs`, with the TLS handshake if asked.
async fn open(
    request: &Request,
    started: Instant,
) -> Result<(Box<dyn Io>, Option<Remote>, Timing), Failure> {
    let server_name = request.server_name.as_deref().unwrap_or(&request.host);
    let server_name = ServerName::try_from(server_name.to_owned()).map_err(|_| {
        Failure::new(
            Stage::Send,
            format!("Invalid server name \"{server_name}\""),
        )
    })?;
    let connect_budget = millis(request.connect_ms.unwrap_or(CONNECT_MS));
    let (tcp, remote) = tokio::time::timeout(connect_budget, connect(request))
        .await
        .map_err(|_| Failure::new(Stage::Timeout, "Connecting timed out"))??;
    let mut timing = Timing {
        connect_us: micros(started),
        ..Timing::default()
    };
    if !request.tls {
        return Ok((Box::new(tcp), remote, timing));
    }
    let connector = tokio_rustls::TlsConnector::from(config(request)?);
    let left = connect_budget.saturating_sub(started.elapsed());
    let tls = tokio::time::timeout(left, connector.connect(server_name, tcp))
        .await
        .map_err(|_| Failure::new(Stage::Timeout, "The TLS handshake timed out"))?
        .map_err(|error| Failure::at(Stage::Tls, &error))?;
    timing.tls_us = Some(micros(started));
    Ok((Box::new(tls), remote, timing))
}

/// Tries every address the name resolves to.
async fn connect(request: &Request) -> Result<(TcpStream, Option<Remote>), Failure> {
    let addresses = tokio::net::lookup_host((request.host.as_str(), request.port))
        .await
        .map_err(|error| Failure::at(Stage::Dns, &error))?;
    let mut last = None;
    for address in addresses {
        match TcpStream::connect(address).await {
            Ok(stream) => {
                let remote = stream.peer_addr().ok().map(|remote| Remote {
                    address: remote.ip().to_string(),
                    port: remote.port(),
                    family: if remote.is_ipv4() { "IPv4" } else { "IPv6" }.into(),
                });
                return Ok((stream, remote));
            }
            Err(error) => last = Some(error),
        }
    }
    Err(match last {
        Some(error) => Failure::at(Stage::Connect, &error),
        None => Failure::new(Stage::Dns, format!("No address for \"{}\"", request.host)),
    })
}

/// System roots plus `caPem`, or no checks for `insecure`; built once each without `caPem`.
fn config(request: &Request) -> Result<Arc<ClientConfig>, Failure> {
    static VERIFIED: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    static INSECURE: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    let verified = |roots| {
        Arc::new(
            ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth(),
        )
    };
    if request.insecure {
        return Ok(INSECURE
            .get_or_init(|| {
                Arc::new(
                    ClientConfig::builder()
                        .dangerous()
                        .with_custom_certificate_verifier(Arc::new(AnyCertificate))
                        .with_no_client_auth(),
                )
            })
            .clone());
    }
    let Some(pem) = &request.ca_pem else {
        if let Some(config) = VERIFIED.get() {
            return Ok(config.clone());
        }
        if system_roots().is_empty() {
            return Err(Failure::new(Stage::Tls, "No system root certificates"));
        }
        return Ok(VERIFIED
            .get_or_init(|| verified(system_roots().clone()))
            .clone());
    };
    let invalid = |error: String| Failure::new(Stage::Tls, format!("Invalid caPem: {error}"));
    let mut roots = system_roots().clone();
    let mut added = 0;
    for cert in CertificateDer::pem_slice_iter(pem.as_bytes()) {
        let cert = cert.map_err(|error| invalid(error.to_string()))?;
        roots
            .add(cert)
            .map_err(|error| invalid(error.to_string()))?;
        added += 1;
    }
    if added == 0 {
        return Err(invalid("no certificate".into()));
    }
    Ok(verified(roots))
}

fn system_roots() -> &'static RootCertStore {
    static ROOTS: OnceLock<RootCertStore> = OnceLock::new();
    ROOTS.get_or_init(|| {
        let mut roots = RootCertStore::empty();
        roots.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
        roots
    })
}

#[derive(Debug)]
struct AnyCertificate;

impl ServerCertVerifier for AnyCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::CryptoProvider::get_default()
            .map(|provider| {
                provider
                    .signature_verification_algorithms
                    .supported_schemes()
            })
            .unwrap_or_default()
    }
}
