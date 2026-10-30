//! Host functions modules call (`host.proto`): HTTP through the shared reqwest clients, the data
//! directory and logging. Work runs on the host's tokio runtime; modules may await the answers from
//! any thread or runtime.

use services_module_api::proto::host::{
    self, Context, Empty, Header, HttpFailure, HttpRequest, HttpResponse, HttpStreamKind,
    HttpStreamMessage, LogRequest,
};
use services_module_api::stabby::{
    future::DynFutureUnsync, result::Result as SResult, string::String as SString,
};
use services_module_api::transport::{
    self, CancelArc, Endpoint, EndpointArc, Frame, FrameSinkBox, FrameSinkDyn, FrameSourceBox,
    Status, code,
};
use std::{
    path::PathBuf,
    sync::OnceLock,
    time::{Duration, Instant},
};
use tokio::runtime::Handle;

pub struct HostEndpoint {
    /// Indexed by `no_redirects * 2 + no_retries`.
    clients: [OnceLock<reqwest::Client>; 4],
    data_dir: PathBuf,
    runtime: Option<Handle>,
}

impl HostEndpoint {
    pub fn new(data_dir: PathBuf) -> crate::Result<Self> {
        let host = Self {
            clients: Default::default(),
            data_dir,
            runtime: Handle::try_current().ok(),
        };
        // REST / GraphQL and Splunk clients; a TLS setup failure stops startup as before.
        for (no_redirects, no_retries) in [(false, false), (true, true)] {
            host.client(no_redirects, no_retries)
                .map_err(|error| crate::Error::new(format!("Request failed: {error}")))?;
        }
        Ok(host)
    }

    pub fn arc(self) -> EndpointArc {
        services_module_api::stabby::sync::Arc::new(self).into()
    }

    fn client(&self, no_redirects: bool, no_retries: bool) -> reqwest::Result<reqwest::Client> {
        let slot = &self.clients[usize::from(no_redirects) * 2 + usize::from(no_retries)];
        if let Some(client) = slot.get() {
            return Ok(client.clone());
        }
        let mut builder = reqwest::Client::builder();
        if no_redirects {
            builder = builder.redirect(reqwest::redirect::Policy::none());
        }
        if no_retries {
            builder = builder.retry(reqwest::retry::never());
        }
        let client = builder.build()?;
        Ok(slot.get_or_init(|| client).clone())
    }

    fn spawn<F>(&self, future: F) -> DynFutureUnsync<'static, SResult<Frame, Status>>
    where
        F: Future<Output = Result<Frame, Status>> + Send + 'static,
    {
        match self.runtime.clone().or_else(|| Handle::try_current().ok()) {
            Some(runtime) => transport::spawn(&runtime, future),
            None => transport::ready(Err(Status::new(
                code::INTERNAL,
                "The host runtime is not available",
            ))),
        }
    }

    fn prepare(&self, request: &Frame) -> Result<(HttpRequest, reqwest::Client), Status> {
        let request: HttpRequest = request.decode()?;
        let client = self
            .client(request.no_redirects, request.no_retries)
            .map_err(|error| failure(&error))?;
        Ok((request, client))
    }
}

impl Endpoint for HostEndpoint {
    extern "C" fn unary<'a>(
        &'a self,
        method: SString,
        request: Frame,
        _cancel: CancelArc,
    ) -> DynFutureUnsync<'a, SResult<Frame, Status>> {
        match &*method {
            host::HTTP => match self.prepare(&request) {
                Ok((request, client)) => self.spawn(http(client, request)),
                Err(status) => transport::ready(Err(status)),
            },
            host::CONTEXT => transport::ready(Ok(Frame::message(&Context {
                data_dir: self.data_dir.to_string_lossy().into_owned(),
                host_version: env!("CARGO_PKG_VERSION").into(),
            }))),
            host::LOG => {
                let reply = request.decode::<LogRequest>().map(|log| {
                    eprintln!("[{}] {}", log.level, log.message);
                    Frame::message(&Empty {})
                });
                transport::ready(reply)
            }
            other => transport::ready(Err(Status::unimplemented(other))),
        }
    }

    extern "C" fn server_stream<'a>(
        &'a self,
        method: SString,
        request: Frame,
        sink: FrameSinkBox,
        _cancel: CancelArc,
    ) -> DynFutureUnsync<'a, SResult<Frame, Status>> {
        if &*method != host::HTTP_STREAM {
            return transport::ready(Err(Status::unimplemented(&method)));
        }
        match self.prepare(&request) {
            Ok((request, client)) => self.spawn(http_stream(client, request, sink)),
            Err(status) => transport::ready(Err(status)),
        }
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

/// Builds the request exactly as the built-in services did with reqwest.
fn builder(
    client: &reqwest::Client,
    request: &mut HttpRequest,
) -> Result<reqwest::RequestBuilder, Status> {
    let method = reqwest::Method::from_bytes(request.method.as_bytes())
        .map_err(|error| Status::new(code::INVALID_ARGUMENT, &error.to_string()))?;
    let mut builder = client.request(method, request.url.as_str());
    for header in &request.headers {
        builder = builder.header(header.name.as_str(), header.value.as_str());
    }
    if let Some(body) = request.body.take() {
        builder = builder.body(body);
    }
    if request.timeout_ms > 0 {
        builder = builder.timeout(Duration::from_millis(request.timeout_ms));
    }
    Ok(builder)
}

async fn send(
    client: &reqwest::Client,
    request: &mut HttpRequest,
) -> Result<(reqwest::Response, HttpResponse), Status> {
    let started = Instant::now();
    let response = builder(client, request)?
        .send()
        .await
        .map_err(|error| failure(&error))?;
    let headers_us = started.elapsed().as_micros() as u64;
    let response = if request.error_for_status {
        response
            .error_for_status()
            .map_err(|error| failure(&error))?
    } else {
        response
    };
    let status = response.status();
    let remote = response.remote_addr();
    let head = HttpResponse {
        status: u32::from(status.as_u16()),
        status_text: status.canonical_reason().unwrap_or("").into(),
        http_version: format!("{:?}", response.version()),
        headers: response
            .headers()
            .iter()
            .map(|(name, value)| Header {
                name: name.to_string(),
                value: value.to_str().unwrap_or("").into(),
            })
            .collect(),
        body: Vec::new(),
        headers_us,
        remote_address: remote.map(|remote| remote.ip().to_string()),
        remote_port: remote.map_or(0, |remote| u32::from(remote.port())),
        remote_family: match remote {
            Some(remote) if remote.is_ipv4() => "IPv4".into(),
            Some(_) => "IPv6".into(),
            None => String::new(),
        },
    };
    Ok((response, head))
}

async fn http(client: reqwest::Client, mut request: HttpRequest) -> Result<Frame, Status> {
    let (response, mut head) = send(&client, &mut request).await?;
    let body = Vec::from(response.bytes().await.map_err(|error| failure(&error))?);
    head.body = if request.text {
        // One validation pass; only invalid input pays for the lossy copy.
        String::from_utf8(body)
            .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned())
            .into_bytes()
    } else {
        body
    };
    Ok(Frame::message(&head))
}

async fn http_stream(
    client: reqwest::Client,
    mut request: HttpRequest,
    sink: FrameSinkBox,
) -> Result<Frame, Status> {
    let (mut response, head) = send(&client, &mut request).await?;
    let message = |kind| Frame::message(&HttpStreamMessage { kind: Some(kind) });
    if !sink.send(message(HttpStreamKind::Head(head))).await {
        return Ok(Frame::new(&[], &[]));
    }
    while let Some(chunk) = response.chunk().await.map_err(|error| failure(&error))? {
        if !sink
            .send(message(HttpStreamKind::Chunk(Vec::from(chunk))))
            .await
        {
            break;
        }
    }
    Ok(Frame::new(&[], &[]))
}

/// A reqwest error with its cause chain, as the console shows it.
fn failure(error: &reqwest::Error) -> Status {
    let mut causes = vec![error.to_string()];
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source.filter(|_| causes.len() < 9) {
        causes.push(cause.to_string());
        source = cause.source();
    }
    let details = HttpFailure {
        causes,
        connect: error.is_connect(),
        builder: error.is_builder(),
        timeout: error.is_timeout(),
        status: error.status().map(|status| u32::from(status.as_u16())),
        decode: error.is_decode(),
    };
    Status::new(code::UNAVAILABLE, &format!("Request failed: {error}")).with_details(&details)
}
