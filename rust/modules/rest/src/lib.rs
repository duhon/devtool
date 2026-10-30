//! REST: HTTP requests and OpenAPI discovery, all I/O through the host.

mod openapi;

use serde_json::Value;
use services_module_api::error::{Result, headers, required, string, text};
use services_module_api::sdk::{self, Call, Manifest, Service, Threads};
use services_module_api::transport::{EndpointArc, EndpointBox, Status};

pub struct Rest;

impl Service for Rest {
    fn manifest(&self) -> Manifest {
        Manifest {
            execute: true,
            discover: true,
            stream: false,
            ..Manifest::from_json(include_str!("../manifest.json"), env!("CARGO_PKG_VERSION"))
        }
    }

    async fn execute(&self, call: Call) -> Result<Value> {
        send(&call.host, &call.params).await
    }

    async fn discover(&self, call: Call) -> Result<Value> {
        openapi::discover(&call.host, &call.params).await
    }
}

pub fn endpoint(host: EndpointArc) -> std::result::Result<EndpointBox, Status> {
    sdk::serve(Rest, host, Threads::Host)
}

services_module_api::export!(endpoint);

pub async fn send(host: &sdk::Host, params: &Value) -> Result<Value> {
    let method = required(params, "method")?;
    let url = text(params, "url");
    let headers = headers(params)?;
    let body = params.get("body").filter(|v| !v.is_null()).map(string);
    services_module_api::http::request(host, &method, &url, &headers, body).await
}
