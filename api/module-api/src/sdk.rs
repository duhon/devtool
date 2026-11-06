//! Module side: implement [`Service`], hand it to [`serve`], call the host through [`Host`].

use crate::error::{Error, Result, history_text};
use crate::proto::{host, module};
use crate::transport::{
    self, CancelArc, CancelDyn, EndpointArc, EndpointBox, EndpointDyn, Frame, FrameSinkBox,
    FrameSinkDyn, FrameSourceBox, Status, code,
};
use serde_json::{Value, json};
use stabby::future::DynFutureUnsync;
use stabby::result::Result as SResult;
use stabby::string::String as SString;
use std::future::Future;
use std::pin::{Pin, pin};
use std::sync::{Arc, OnceLock};
use std::task::Poll;
use tokio_util::sync::CancellationToken;

pub use crate::proto::module::{Manifest, RunStatus};

pub const CANCELLED_WARNING: &str = "Client request stopped. The remote server may still finish processing; committed changes are not rolled back.";

/// One console request.
pub struct Call {
    pub params: Value,
    pub host: Host,
    /// Cancelled when the user stops the request.
    pub cancel: CancellationToken,
}

/// Events of a streaming call (Terminal output); the host forwards each one to the console.
pub type Events = tokio::sync::mpsc::Sender<Value>;

/// A console service.
pub trait Service: Send + Sync + 'static {
    fn manifest(&self) -> Manifest;

    fn execute(&self, call: Call) -> impl Future<Output = Result<Value>> + Send {
        let _ = call;
        async { Err(Error::new("execute is not supported")) }
    }

    fn discover(&self, call: Call) -> impl Future<Output = Result<Value>> + Send {
        let _ = call;
        async { Err(Error::new("discover is not supported")) }
    }

    fn stream(&self, call: Call, events: Events) -> impl Future<Output = Result<Value>> + Send {
        let _ = (call, events);
        async { Err(Error::new("stream is not supported")) }
    }

    /// `true`: the service watches `Call::cancel` and returns its own result (MySQL `KILL QUERY`).
    /// `false`: a stop drops the request and answers `{"cancelled":true,…}` right away.
    fn soft_cancel(&self) -> bool {
        false
    }

    /// History status of a successful call.
    fn status(&self, response: &Value) -> RunStatus {
        status(response)
    }

    /// `OK` / `WARNING` / `ERROR` / `NOTICE` / `Stopped` shown in history.
    fn outcome(&self, response: &Value) -> &'static str {
        outcome(response)
    }
}

impl Manifest {
    /// A manifest from the module's `manifest.json` (id, name, description, params, console);
    /// the code adds the version and the capabilities.
    pub fn from_json(spec_json: &str, version: &str) -> Self {
        let spec: Value = serde_json::from_str(spec_json).unwrap_or_default();
        let text = |key: &str| spec[key].as_str().unwrap_or_default().to_owned();
        Self {
            id: text("id"),
            name: text("name"),
            version: version.into(),
            spec_json: spec_json.into(),
            ..Default::default()
        }
    }
}

/// Default history status: stopped, failed (an `error`, a non-zero `code`, HTTP status >= 400) or success.
pub fn status(response: &Value) -> RunStatus {
    if response["cancelled"] == true {
        RunStatus::Stopped
    } else if response.get("error").is_some()
        || response["code"].as_i64().is_some_and(|code| code != 0)
        || response["status"].as_u64().is_some_and(|code| code >= 400)
    {
        RunStatus::Error
    } else {
        RunStatus::Success
    }
}

/// Default outcome: by HTTP status.
pub fn outcome(response: &Value) -> &'static str {
    if let Some(outcome) = common_outcome(response) {
        return outcome;
    }
    match response["status"].as_u64() {
        Some(200..=299) => "OK",
        Some(400..=499) => "WARNING",
        Some(500..) => "ERROR",
        _ => "NOTICE",
    }
}

/// `Stopped` / `ERROR` shared by every service.
pub fn common_outcome(response: &Value) -> Option<&'static str> {
    if response["cancelled"] == true {
        Some("Stopped")
    } else if response.get("error").is_some() {
        Some("ERROR")
    } else {
        None
    }
}

/// Client of the host functions (`host.proto`).
#[derive(Clone)]
pub struct Host(Arc<EndpointArc>);

impl Host {
    pub fn new(endpoint: EndpointArc) -> Self {
        Self(Arc::new(endpoint))
    }

    pub async fn unary<R: prost::Message + Default>(
        &self,
        method: &str,
        request: &impl prost::Message,
    ) -> std::result::Result<R, Status> {
        let reply: std::result::Result<Frame, Status> = self
            .0
            .unary(
                method.into(),
                Frame::message(request),
                transport::never_cancelled(),
            )
            .await
            .into();
        reply?.decode()
    }

    pub async fn http(&self, request: &host::HttpRequest) -> Result<host::HttpResponse> {
        self.unary(host::HTTP, request)
            .await
            .map_err(Error::from_host)
    }

    /// Response headers first; the body follows chunk by chunk. Dropping the stream closes the
    /// connection.
    pub async fn http_stream(
        &self,
        request: &host::HttpRequest,
    ) -> Result<(host::HttpResponse, HttpStream)> {
        let (sink, frames) = transport::frame_channel(16);
        let endpoint = self.0.clone();
        let request = Frame::message(request);
        let call = Box::pin(async move {
            let result: std::result::Result<Frame, Status> = endpoint
                .server_stream(
                    host::HTTP_STREAM.into(),
                    request,
                    sink,
                    transport::never_cancelled(),
                )
                .await
                .into();
            result
        });
        let mut stream = HttpStream {
            call: Some(call),
            frames,
            failed: None,
        };
        match stream.next().await?.and_then(|message| message.kind) {
            Some(host::HttpStreamKind::Head(head)) => Ok((head, stream)),
            Some(host::HttpStreamKind::Chunk(_)) => {
                Err(Error::new("HTTP stream sent a body before the headers"))
            }
            None => Err(Error::new("HTTP stream ended before the headers")),
        }
    }

    pub async fn context(&self) -> Result<host::Context> {
        self.unary(host::CONTEXT, &host::Empty {})
            .await
            .map_err(Error::from_host)
    }

    pub async fn log(&self, level: &str, message: &str) {
        let request = host::LogRequest {
            level: level.into(),
            message: message.into(),
        };
        let _ = self.unary::<host::Empty>(host::LOG, &request).await;
    }
}

type CallFuture = Pin<Box<dyn Future<Output = std::result::Result<Frame, Status>> + Send>>;

/// Body of a [`Host::http_stream`] response.
pub struct HttpStream {
    call: Option<CallFuture>,
    frames: tokio::sync::mpsc::Receiver<Frame>,
    failed: Option<Status>,
}

impl HttpStream {
    /// The next body chunk; `None` at the end of the body.
    pub async fn chunk(&mut self) -> Result<Option<Vec<u8>>> {
        match self.next().await?.and_then(|message| message.kind) {
            Some(host::HttpStreamKind::Chunk(chunk)) => Ok(Some(chunk)),
            Some(host::HttpStreamKind::Head(_)) => {
                Err(Error::new("HTTP stream sent headers twice"))
            }
            None => Ok(None),
        }
    }

    async fn next(&mut self) -> Result<Option<host::HttpStreamMessage>> {
        enum Next {
            Frame(Option<Frame>),
            Done(std::result::Result<Frame, Status>),
        }
        loop {
            let next = match self.call.as_mut() {
                Some(call) => tokio::select! {
                    biased;
                    frame = self.frames.recv() => Next::Frame(frame),
                    result = call => Next::Done(result),
                },
                None => Next::Frame(self.frames.recv().await),
            };
            match next {
                Next::Frame(Some(frame)) => {
                    return frame.decode().map(Some).map_err(Error::from_host);
                }
                Next::Frame(None) => {
                    if let Some(call) = self.call.take() {
                        self.failed = call.await.err();
                    }
                    return match self.failed.take() {
                        Some(status) => Err(Error::from_host(status)),
                        None => Ok(None),
                    };
                }
                Next::Done(result) => {
                    self.call = None;
                    self.failed = result.err();
                }
            }
        }
    }
}

/// Where module futures run.
pub enum Threads {
    /// Polled on the host's threads; the module must not need its own tokio runtime
    /// (no `tokio::time`, sockets or processes). I/O goes through [`Host`].
    Host,
    /// A module-owned runtime (2 workers), created on first use.
    Own(&'static str),
}

/// The endpoint for `service`: the host calls it through `module.proto`.
pub fn serve<S: Service>(
    service: S,
    host: EndpointArc,
    threads: Threads,
) -> std::result::Result<EndpointBox, Status> {
    let served = Served {
        manifest: service.manifest(),
        service: Arc::new(service),
        host: Host::new(host),
        threads,
        runtime: OnceLock::new(),
    };
    Ok(stabby::boxed::Box::new(served).into())
}

struct Served<S> {
    manifest: Manifest,
    service: Arc<S>,
    host: Host,
    threads: Threads,
    runtime: OnceLock<std::result::Result<tokio::runtime::Runtime, String>>,
}

impl<S> Drop for Served<S> {
    fn drop(&mut self) {
        if let Some(Ok(runtime)) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

impl<S: Service> Served<S> {
    fn spawn<'a, F>(&'a self, task: F) -> DynFutureUnsync<'a, SResult<Frame, Status>>
    where
        F: Future<Output = std::result::Result<Frame, Status>> + Send + 'static,
    {
        let Threads::Own(name) = self.threads else {
            return transport::guarded(task);
        };
        let runtime = self.runtime.get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_name(name)
                .enable_all()
                .build()
                .map_err(|error| error.to_string())
        });
        match runtime {
            Ok(runtime) => transport::spawn(runtime.handle(), task),
            Err(error) => transport::ready(Err(Status::new(code::INTERNAL, error))),
        }
    }

    fn call(&self, params_json: &str) -> std::result::Result<(Call, CancellationToken), Status> {
        let params = serde_json::from_str(params_json).map_err(|error| {
            Status::new(code::INVALID_ARGUMENT, &format!("invalid params: {error}"))
        })?;
        let cancel = CancellationToken::new();
        let call = Call {
            params,
            host: self.host.clone(),
            cancel: cancel.clone(),
        };
        Ok((call, cancel))
    }
}

impl<S: Service> transport::Endpoint for Served<S> {
    extern "C" fn unary<'a>(
        &'a self,
        method: SString,
        request: Frame,
        cancel: CancelArc,
    ) -> DynFutureUnsync<'a, SResult<Frame, Status>> {
        match &*method {
            module::MANIFEST => transport::ready(Ok(Frame::message(&self.manifest))),
            module::EXECUTE => {
                let request: module::ExecuteRequest = match request.decode() {
                    Ok(request) => request,
                    Err(status) => return transport::ready(Err(status)),
                };
                let (call, token) = match self.call(&request.params_json) {
                    Ok(call) => call,
                    Err(status) => return transport::ready(Err(status)),
                };
                let service = self.service.clone();
                self.spawn(async move {
                    let fallback = (!service.soft_cancel())
                        .then(|| json!({"cancelled":true,"warning":CANCELLED_WARNING}));
                    let result = watch(&cancel, &token, fallback, service.execute(call)).await;
                    respond(&*service, result, request.history)
                })
            }
            module::DISCOVER => {
                let request: module::DiscoverRequest = match request.decode() {
                    Ok(request) => request,
                    Err(status) => return transport::ready(Err(status)),
                };
                let (call, token) = match self.call(&request.params_json) {
                    Ok(call) => call,
                    Err(status) => return transport::ready(Err(status)),
                };
                let service = self.service.clone();
                self.spawn(async move {
                    let fallback = (!service.soft_cancel()).then(|| json!({"cancelled":true}));
                    let value = watch(&cancel, &token, fallback, service.discover(call))
                        .await
                        .map_err(Error::into_status)?;
                    let result_json = serde_json::to_string(&value)
                        .map_err(|error| Status::new(code::INTERNAL, &error.to_string()))?;
                    Ok(Frame::message(&module::DiscoverResponse { result_json }))
                })
            }
            other => transport::ready(Err(Status::unimplemented(other))),
        }
    }

    extern "C" fn server_stream<'a>(
        &'a self,
        method: SString,
        request: Frame,
        sink: FrameSinkBox,
        cancel: CancelArc,
    ) -> DynFutureUnsync<'a, SResult<Frame, Status>> {
        if &*method != module::STREAM {
            return transport::ready(Err(Status::unimplemented(&method)));
        }
        let request: module::ExecuteRequest = match request.decode() {
            Ok(request) => request,
            Err(status) => return transport::ready(Err(status)),
        };
        let (call, token) = match self.call(&request.params_json) {
            Ok(call) => call,
            Err(status) => return transport::ready(Err(status)),
        };
        let service = self.service.clone();
        self.spawn(async move {
            let (events, mut received) = tokio::sync::mpsc::channel::<Value>(128);
            let mut run = pin!(watch(&cancel, &token, None, service.stream(call, events)));
            let mut open = true;
            let result = loop {
                let event = tokio::select! {
                    biased;
                    result = &mut run => break result,
                    event = received.recv() => event,
                };
                match event {
                    Some(event) => forward(&sink, &event, &mut open).await,
                    None => break run.await,
                }
            };
            while let Some(event) = received.recv().await {
                forward(&sink, &event, &mut open).await;
            }
            respond(&*service, result, request.history)
        })
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

/// Runs `work`, cancelling `token` when the host cancels the call. With a `fallback` the call
/// stops right away and answers `fallback`.
async fn watch<F>(
    cancel: &CancelArc,
    token: &CancellationToken,
    mut fallback: Option<Value>,
    work: F,
) -> Result<Value>
where
    F: Future<Output = Result<Value>>,
{
    let mut work = pin!(work);
    let mut signal = pin!(cancel.cancelled());
    let mut signalled = false;
    std::future::poll_fn(move |cx| {
        if !signalled && signal.as_mut().poll(cx).is_ready() {
            signalled = true;
            token.cancel();
            if let Some(value) = fallback.take() {
                return Poll::Ready(Ok(value));
            }
        }
        work.as_mut().poll(cx)
    })
    .await
}

async fn forward(sink: &FrameSinkBox, event: &Value, open: &mut bool) {
    if *open {
        let event = module::Event {
            json: event.to_string(),
        };
        *open = sink.send(Frame::message(&event)).await;
    }
}

fn respond<S: Service>(
    service: &S,
    result: Result<Value>,
    history: bool,
) -> std::result::Result<Frame, Status> {
    let value = result.map_err(Error::into_status)?;
    let response = module::ExecuteResponse {
        status: service.status(&value) as i32,
        outcome: service.outcome(&value).into(),
        history_text: if history {
            history_text(&value).ok()
        } else {
            None
        },
        result_json: serde_json::to_string(&value)
            .map_err(|error| Status::new(code::INTERNAL, &error.to_string()))?
            .into(),
    };
    Ok(Frame::message(&response))
}
