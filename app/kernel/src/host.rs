//! The link to the Host. [`Reply`] is how it answers on every target; in the browser
//! [`Transport`] talks to it over `chrome.runtime.connectNative` (port.rs): one request per
//! id, answers assembled from chunks, Stop, timeouts, and the health check of the protocol
//! version.

use serde_json::Value;

#[cfg(target_arch = "wasm32")]
mod port;
#[cfg(target_arch = "wasm32")]
pub use port::Transport;

/// An answer of the Host: the result, a streamed event, a process exit, or an error.
#[derive(Clone, Debug)]
pub enum Reply {
    Result(Value),
    Data(Value),
    Exit(Value),
    Error(Value),
}
