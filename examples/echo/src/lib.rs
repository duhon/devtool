//! Echo: the smallest module. It answers each check with its text, the way an HTTP service
//! answers, so the console shows it in the body/headers view.

use serde_json::{Value, json};
use services_module_api::error::{Result, required};
use services_module_api::sdk::{self, Call, Manifest, Service, Threads};
use services_module_api::transport::{EndpointArc, EndpointBox, Status};

pub struct Echo;

impl Service for Echo {
    fn manifest(&self) -> Manifest {
        Manifest {
            execute: true,
            ..Manifest::from_json(include_str!("../manifest.json"), env!("CARGO_PKG_VERSION"))
        }
    }

    async fn execute(&self, call: Call) -> Result<Value> {
        let endpoint = required(&call.params, "endpoint")?;
        let text = required(&call.params, "text")?;
        Ok(json!({
            "status": 200,
            "headers": {"content-type": "text/plain", "x-echo-endpoint": endpoint},
            "body": text,
        }))
    }
}

/// The module's entry: the host calls it once after loading the library.
pub fn endpoint(host: EndpointArc) -> std::result::Result<EndpointBox, Status> {
    sdk::serve(Echo, host, Threads::Host)
}

services_module_api::export!(endpoint);
