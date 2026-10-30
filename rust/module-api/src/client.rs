//! Host side: calls a module endpoint through `module.proto`.

use crate::proto::{host, module};
use crate::transport::{self, EndpointBox, EndpointDyn, Frame, Status, code};
use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Wake, Waker};
use tokio_util::sync::CancellationToken;

pub struct Module {
    endpoint: EndpointBox,
    pub manifest: module::Manifest,
}

impl Module {
    /// Reads the manifest; it is answered without a runtime, so this does not block on I/O.
    pub fn new(endpoint: EndpointBox) -> Result<Self, Status> {
        let manifest = block_on(async {
            let reply: Result<Frame, Status> = endpoint
                .unary(
                    module::MANIFEST.into(),
                    Frame::message(&host::Empty {}),
                    transport::never_cancelled(),
                )
                .await
                .into();
            reply?.decode::<module::Manifest>()
        })?;
        Ok(Self { endpoint, manifest })
    }

    pub async fn execute(
        &self,
        params_json: String,
        history: bool,
        cancel: CancellationToken,
    ) -> Result<module::ExecuteResponse, Status> {
        let request = module::ExecuteRequest {
            params_json,
            history,
        };
        let reply: Result<Frame, Status> = self
            .endpoint
            .unary(
                module::EXECUTE.into(),
                Frame::message(&request),
                transport::cancel_arc(cancel),
            )
            .await
            .into();
        reply?.into_message()
    }

    pub async fn discover(
        &self,
        params_json: String,
        cancel: CancellationToken,
    ) -> Result<String, Status> {
        let request = module::DiscoverRequest { params_json };
        let reply: Result<Frame, Status> = self
            .endpoint
            .unary(
                module::DISCOVER.into(),
                Frame::message(&request),
                transport::cancel_arc(cancel),
            )
            .await
            .into();
        Ok(reply?.decode::<module::DiscoverResponse>()?.result_json)
    }

    /// Calls `on_event` with the JSON of every event, then returns the final response.
    pub async fn stream<F, Fut>(
        &self,
        params_json: String,
        history: bool,
        cancel: CancellationToken,
        mut on_event: F,
    ) -> Result<module::ExecuteResponse, Status>
    where
        F: FnMut(String) -> Fut,
        Fut: Future<Output = ()>,
    {
        let request = module::ExecuteRequest {
            params_json,
            history,
        };
        let (sink, mut frames) = transport::frame_channel(128);
        let mut call = pin!(self.endpoint.server_stream(
            module::STREAM.into(),
            Frame::message(&request),
            sink,
            transport::cancel_arc(cancel),
        ));
        let trailer = loop {
            let frame = tokio::select! {
                biased;
                frame = frames.recv() => frame,
                trailer = &mut call => break trailer,
            };
            match frame {
                Some(frame) => on_event(frame.decode::<module::Event>()?.json).await,
                None => break call.await,
            }
        };
        while let Some(frame) = frames.recv().await {
            on_event(frame.decode::<module::Event>()?.json).await;
        }
        let trailer: Result<Frame, Status> = trailer.into();
        trailer?.into_message()
    }
}

struct ThreadWaker(std::thread::Thread);

impl Wake for ThreadWaker {
    fn wake(self: std::sync::Arc<Self>) {
        self.0.unpark();
    }
}

/// Minimal executor for calls that do not need a runtime.
pub fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let waker = Waker::from(std::sync::Arc::new(ThreadWaker(std::thread::current())));
    let mut context = Context::from_waker(&waker);
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return output;
        }
        std::thread::park();
    }
}

/// A module answered something the host cannot read.
pub fn invalid(message: &str) -> Status {
    Status::new(code::INTERNAL, message)
}
