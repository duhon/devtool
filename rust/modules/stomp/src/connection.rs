//! A broker connection: TCP or TLS carrying STOMP frames.

use crate::frame::{Decoder, Frame};
use rustls::pki_types::ServerName;
use std::sync::{Arc, OnceLock};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

pub struct Connection {
    io: Box<dyn Io>,
    decoder: Decoder,
    escapes: bool,
}

impl Connection {
    pub async fn open(host: &str, port: u16, tls: bool) -> Result<Self, String> {
        let tcp = TcpStream::connect((host, port))
            .await
            .map_err(|e| format!("Cannot connect to {host}:{port}: {e}"))?;
        let _ = tcp.set_nodelay(true);
        let io: Box<dyn Io> = if tls {
            let name = ServerName::try_from(host.to_owned())
                .map_err(|_| format!("Invalid TLS server name {host:?}"))?;
            let stream = tokio_rustls::TlsConnector::from(tls_config()?)
                .connect(name, tcp)
                .await
                .map_err(|e| format!("TLS handshake with {host}:{port} failed: {e}"))?;
            Box::new(stream)
        } else {
            Box::new(tcp)
        };
        Ok(Self {
            io,
            decoder: Decoder::default(),
            escapes: true,
        })
    }

    /// STOMP 1.0 has no header escaping.
    pub fn set_escapes(&mut self, escapes: bool) {
        self.escapes = escapes;
        self.decoder.escapes = escapes;
    }

    pub async fn send(&mut self, frame: &Frame) -> Result<(), String> {
        let bytes = frame.encode(self.escapes);
        let written = async {
            self.io.write_all(&bytes).await?;
            self.io.flush().await
        };
        written
            .await
            .map_err(|e| format!("Writing {} failed: {e}", frame.command))
    }

    /// The next frame. Cancel-safe: bytes already read stay in the decoder.
    pub async fn receive(&mut self) -> Result<Frame, String> {
        let mut chunk = [0u8; 16 * 1024];
        loop {
            if let Some(frame) = self.decoder.frame()? {
                return Ok(frame);
            }
            let n = self
                .io
                .read(&mut chunk)
                .await
                .map_err(|e| format!("Reading from the broker failed: {e}"))?;
            if n == 0 {
                return Err("The broker closed the connection".into());
            }
            self.decoder.push(&chunk[..n]);
        }
    }

    pub async fn close(mut self) {
        let _ = self.io.shutdown().await;
    }
}

/// System trust roots plus the Mozilla set; built once.
fn tls_config() -> Result<Arc<rustls::ClientConfig>, String> {
    static CONFIG: OnceLock<Result<Arc<rustls::ClientConfig>, String>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            for cert in rustls_native_certs::load_native_certs().certs {
                let _ = roots.add(cert);
            }
            let config = rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?
            .with_root_certificates(roots)
            .with_no_client_auth();
            Ok(Arc::new(config))
        })
        .clone()
}
