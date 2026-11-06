//! Transport layer: gRPC-like calls over the dylib boundary, in both directions.
//!
//! The ABI only knows unary, server streaming and duplex calls plus cancellation, metadata and a
//! structured status. Method names and payloads (`host.proto`, `module.proto`) belong to the
//! application layer, so new methods or fields never change this ABI.

use stabby::boxed::Box;
use stabby::future::DynFutureUnsync;
use stabby::option::Option as SOption;
use stabby::result::Result;
use stabby::string::String;
use stabby::sync::Arc;
use stabby::vec::Vec;

#[stabby::stabby]
pub struct MetaEntry {
    pub key: String,
    pub value: String,
}

/// One message: metadata (headers / trailers) plus an opaque body.
#[stabby::stabby]
pub struct Frame {
    pub metadata: Vec<MetaEntry>,
    pub body: Vec<u8>,
}

/// Structured error. `code` is a plain number (gRPC codes) so new codes do not change the ABI.
#[stabby::stabby]
pub struct Status {
    pub code: u32,
    pub message: String,
    pub details: Vec<u8>,
}

/// gRPC status codes.
pub mod code {
    pub const CANCELLED: u32 = 1;
    pub const UNKNOWN: u32 = 2;
    pub const INVALID_ARGUMENT: u32 = 3;
    pub const DEADLINE_EXCEEDED: u32 = 4;
    pub const NOT_FOUND: u32 = 5;
    pub const FAILED_PRECONDITION: u32 = 9;
    pub const UNIMPLEMENTED: u32 = 12;
    pub const INTERNAL: u32 = 13;
    pub const UNAVAILABLE: u32 = 14;
}

/// Cooperative cancellation (e.g. MySQL `KILL QUERY`): the caller signals, the callee cleans up and
/// still returns a result. Dropping the call future stays the hard abort.
#[stabby::stabby(checked)]
pub trait Cancel {
    extern "C" fn is_cancelled(&self) -> bool;
    /// Runtime-agnostic: may be awaited on any runtime.
    extern "C" fn cancelled<'a>(&'a self) -> DynFutureUnsync<'a, ()>;
}

#[stabby::stabby(checked)]
pub trait FrameSink {
    /// Waits while the receiver is full (backpressure); `false` once the receiver is gone.
    extern "C" fn send<'a>(&'a self, frame: Frame) -> DynFutureUnsync<'a, bool>;
}

#[stabby::stabby(checked)]
pub trait FrameSource {
    /// `None` once the sender closed its side.
    extern "C" fn next<'a>(&'a self) -> DynFutureUnsync<'a, SOption<Frame>>;
}

pub type CancelArc = stabby::dynptr!(Arc<dyn Cancel + Send + Sync>);
pub type FrameSinkBox = stabby::dynptr!(Box<dyn FrameSink + Send + Sync>);
pub type FrameSourceBox = stabby::dynptr!(Box<dyn FrameSource + Send + Sync>);

/// Implemented by modules (the host calls them) and by the host (modules call it).
#[stabby::stabby(checked)]
pub trait Endpoint {
    extern "C" fn unary<'a>(
        &'a self,
        method: String,
        request: Frame,
        cancel: CancelArc,
    ) -> DynFutureUnsync<'a, Result<Frame, Status>>;
    /// Responses go to `sink`; the returned frame carries trailers.
    extern "C" fn server_stream<'a>(
        &'a self,
        method: String,
        request: Frame,
        sink: FrameSinkBox,
        cancel: CancelArc,
    ) -> DynFutureUnsync<'a, Result<Frame, Status>>;
    /// `open` carries the call metadata; requests arrive on `input`, responses go to `sink`.
    extern "C" fn duplex<'a>(
        &'a self,
        method: String,
        open: Frame,
        input: FrameSourceBox,
        sink: FrameSinkBox,
        cancel: CancelArc,
    ) -> DynFutureUnsync<'a, Result<Frame, Status>>;
}

pub type EndpointArc = stabby::dynptr!(Arc<dyn Endpoint + Send + Sync>);
pub type EndpointBox = stabby::dynptr!(Box<dyn Endpoint + Send + Sync>);

impl Frame {
    pub fn new(metadata: &[(&str, &str)], body: &[u8]) -> Self {
        let mut entries = Vec::new();
        for (key, value) in metadata {
            entries.push(MetaEntry {
                key: (*key).into(),
                value: (*value).into(),
            });
        }
        Frame {
            metadata: entries,
            body: body.into(),
        }
    }

    /// Encodes straight into the frame body (one copy of large payloads less).
    pub fn message(message: &impl prost::Message) -> Self {
        let mut body = Vec::with_capacity(message.encoded_len());
        message
            .encode(&mut BodyBuf(&mut body))
            .expect("the body grows on demand");
        Frame {
            metadata: Vec::new(),
            body,
        }
    }

    pub fn meta(&self, key: &str) -> core::option::Option<&str> {
        self.metadata
            .iter()
            .find(|entry| &*entry.key == key)
            .map(|entry| &*entry.value)
    }

    pub fn with(mut self, key: &str, value: &str) -> Self {
        self.metadata.push(MetaEntry {
            key: key.into(),
            value: value.into(),
        });
        self
    }

    pub fn decode<M: prost::Message + Default>(&self) -> core::result::Result<M, Status> {
        M::decode(&*self.body).map_err(invalid_message)
    }

    /// Like [`Frame::decode`], but `Bytes` fields share the body instead of copying it.
    pub fn into_message<M: prost::Message + Default>(self) -> core::result::Result<M, Status> {
        M::decode(prost::bytes::Bytes::from_owner(self.body)).map_err(invalid_message)
    }
}

fn invalid_message(error: prost::DecodeError) -> Status {
    Status::new(code::INVALID_ARGUMENT, &format!("invalid message: {error}"))
}

/// `BufMut` over a frame body.
struct BodyBuf<'a>(&'a mut Vec<u8>);

// SAFETY: `chunk_mut` exposes only the spare capacity, `advance_mut` is called for bytes written.
unsafe impl prost::bytes::BufMut for BodyBuf<'_> {
    fn remaining_mut(&self) -> usize {
        isize::MAX as usize - self.0.len()
    }

    unsafe fn advance_mut(&mut self, count: usize) {
        let len = self.0.len() + count;
        assert!(len <= self.0.capacity());
        // SAFETY: the caller initialized `count` bytes of spare capacity.
        unsafe { self.0.set_len(len) };
    }

    fn chunk_mut(&mut self) -> &mut prost::bytes::buf::UninitSlice {
        if self.0.capacity() == self.0.len() {
            self.0.reserve(64);
        }
        let len = self.0.len();
        let spare = self.0.capacity() - len;
        // SAFETY: `capacity` bytes are allocated; the slice starts right after the initialized ones.
        unsafe {
            let start = self.0.as_slice_mut().as_mut_ptr().add(len);
            prost::bytes::buf::UninitSlice::from_raw_parts_mut(start, spare)
        }
    }

    fn put_slice(&mut self, src: &[u8]) {
        self.0.copy_extend(src);
    }
}

impl Status {
    pub fn new(code: u32, message: &str) -> Self {
        Status {
            code,
            message: message.into(),
            details: Vec::new(),
        }
    }

    pub fn with_details(mut self, details: &impl prost::Message) -> Self {
        self.details = details.encode_to_vec().as_slice().into();
        self
    }

    pub fn unimplemented(method: &str) -> Self {
        Self::new(code::UNIMPLEMENTED, &format!("unknown method {method}"))
    }
}

impl core::fmt::Debug for Status {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Status({}, {})", self.code, &*self.message)
    }
}

impl core::fmt::Display for Status {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.message)
    }
}

pub fn panic_message(payload: &(dyn std::any::Any + Send)) -> std::string::String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        format!("module panicked: {text}")
    } else if let Some(text) = payload.downcast_ref::<std::string::String>() {
        format!("module panicked: {text}")
    } else {
        "module panicked".into()
    }
}

/// Guards a synchronous entry point; a panic becomes `INTERNAL` instead of a host abort.
pub fn catch<T>(body: impl FnOnce() -> core::result::Result<T, Status>) -> Result<T, Status>
where
    T: stabby::abi::IDeterminantProvider<Status>,
{
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) {
        Ok(result) => result.into(),
        Err(payload) => Result::Err(Status::new(code::INTERNAL, &panic_message(&*payload))),
    }
}

/// An already finished call.
pub fn ready<'a>(
    result: core::result::Result<Frame, Status>,
) -> DynFutureUnsync<'a, Result<Frame, Status>> {
    let result: Result<Frame, Status> = result.into();
    Box::new(core::future::ready(result)).into()
}

/// Wraps a future crossing the boundary so a panic while polling becomes `INTERNAL`.
pub fn guarded<'a, F>(future: F) -> DynFutureUnsync<'a, Result<Frame, Status>>
where
    F: core::future::Future<Output = core::result::Result<Frame, Status>> + Send + 'a,
{
    use core::pin::Pin;
    use core::task::{Context, Poll};

    struct CatchUnwind<F>(Pin<std::boxed::Box<F>>);

    impl<F: core::future::Future> core::future::Future for CatchUnwind<F> {
        type Output = core::result::Result<F::Output, std::string::String>;

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            let inner = self.0.as_mut();
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| inner.poll(cx))) {
                Ok(Poll::Pending) => Poll::Pending,
                Ok(Poll::Ready(value)) => Poll::Ready(Ok(value)),
                Err(payload) => Poll::Ready(Err(panic_message(&*payload))),
            }
        }
    }

    let caught = CatchUnwind(std::boxed::Box::pin(future));
    Box::new(async move {
        match caught.await {
            Ok(result) => result.into(),
            Err(panic) => Result::Err(Status::new(code::INTERNAL, &panic)),
        }
    })
    .into()
}

/// Runs `future` on `runtime` and answers through a runtime-agnostic future: the caller may poll it
/// from any thread or runtime. Dropping the answer aborts the task.
pub fn spawn<F>(
    runtime: &tokio::runtime::Handle,
    future: F,
) -> DynFutureUnsync<'static, Result<Frame, Status>>
where
    F: core::future::Future<Output = core::result::Result<Frame, Status>> + Send + 'static,
{
    struct Task(tokio::task::JoinHandle<core::result::Result<Frame, Status>>);

    impl Drop for Task {
        fn drop(&mut self) {
            self.0.abort();
        }
    }

    let mut task = Task(runtime.spawn(future));
    guarded(async move {
        match (&mut task.0).await {
            Ok(result) => result,
            Err(error) if error.is_panic() => Err(Status::new(
                code::INTERNAL,
                &panic_message(&*error.into_panic()),
            )),
            Err(error) => Err(Status::new(code::CANCELLED, &error.to_string())),
        }
    })
}

/// A [`Cancel`] backed by a token; usable on either side of the boundary.
pub struct TokenCancel(pub tokio_util::sync::CancellationToken);

impl Cancel for TokenCancel {
    extern "C" fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }

    extern "C" fn cancelled<'a>(&'a self) -> DynFutureUnsync<'a, ()> {
        Box::new(self.0.cancelled()).into()
    }
}

pub fn cancel_arc(token: tokio_util::sync::CancellationToken) -> CancelArc {
    Arc::new(TokenCancel(token)).into()
}

pub fn never_cancelled() -> CancelArc {
    cancel_arc(tokio_util::sync::CancellationToken::new())
}

/// Bounded sink: `send` waits while `capacity` frames are unread.
struct ChannelSink(tokio::sync::mpsc::Sender<Frame>);

impl FrameSink for ChannelSink {
    extern "C" fn send<'a>(&'a self, frame: Frame) -> DynFutureUnsync<'a, bool> {
        Box::new(async move { self.0.send(frame).await.is_ok() }).into()
    }
}

/// A sink whose frames arrive on the returned receiver; dropping the receiver closes the sink.
pub fn frame_channel(capacity: usize) -> (FrameSinkBox, tokio::sync::mpsc::Receiver<Frame>) {
    let (sender, receiver) = tokio::sync::mpsc::channel(capacity);
    (Box::new(ChannelSink(sender)).into(), receiver)
}

struct ChannelSource(tokio::sync::Mutex<tokio::sync::mpsc::Receiver<Frame>>);

impl FrameSource for ChannelSource {
    extern "C" fn next<'a>(&'a self) -> DynFutureUnsync<'a, SOption<Frame>> {
        Box::new(async move { self.0.lock().await.recv().await.into() }).into()
    }
}

/// Input stream for duplex calls; dropping the sender closes it.
pub fn frame_source(capacity: usize) -> (FrameSourceBox, tokio::sync::mpsc::Sender<Frame>) {
    let (sender, receiver) = tokio::sync::mpsc::channel(capacity);
    let source = ChannelSource(tokio::sync::Mutex::new(receiver));
    (Box::new(source).into(), sender)
}
