use super::{
    error::{Error, Result},
    variables::VariableStore,
};
use crate::services;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
#[derive(Clone)]
pub struct Registry {
    pub variables: VariableStore,
    pub http: reqwest::Client,
    splunk_http: reqwest::Client,
    pub mysql: services::mysql::Mysql,
}
impl Registry {
    pub fn new(variables: VariableStore) -> Result<Self> {
        if rustls::crypto::CryptoProvider::get_default().is_none() {
            let installed = rustls::crypto::ring::default_provider().install_default();
            // A concurrent runtime may have installed the process-wide provider first.
            if installed.is_err() && rustls::crypto::CryptoProvider::get_default().is_none() {
                return Err(Error::new("Failed to initialize the TLS crypto provider"));
            }
        }
        Ok(Self {
            variables,
            http: reqwest::Client::builder().build().map_err(Error::from)?,
            splunk_http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .build()
                .map_err(Error::from)?,
            mysql: Default::default(),
        })
    }
    pub fn resolve(&self, service: &str, params: &Value) -> Result<Value> {
        self.variables
            .resolve_params(service, params)
            .map_err(|e| e.with_debug(params.clone()))
    }
    pub async fn call(&self, service: &str, params: &Value) -> Result<Value> {
        self.call_cancellable(service, params, CancellationToken::new())
            .await
    }
    pub async fn call_cancellable(
        &self,
        service: &str,
        params: &Value,
        cancel: CancellationToken,
    ) -> Result<Value> {
        let params = self.resolve(service, params)?;
        self.call_resolved(service, params, cancel).await
    }
    pub async fn call_resolved(
        &self,
        service: &str,
        params: Value,
        cancel: CancellationToken,
    ) -> Result<Value> {
        if cancel.is_cancelled() {
            return Ok(json!({"cancelled":true,"phase":"before-dispatch"}));
        }
        let columns = if matches!(service, "mysql" | "splunk") {
            Some(services_contracts::columns::Options::parse(&params).map_err(Error::new)?)
        } else {
            None
        };
        let request = async {
            match service {
                "rest" => services::http::send(&self.http, &params).await,
                "graphql" => services::graphql::send(&self.http, &params).await,
                "splunk" => {
                    services::splunk::send(
                        &self.splunk_http,
                        &params,
                        &self.variables.path.with_file_name(".splunk-rate-limits"),
                    )
                    .await
                }
                "grpc" => services::grpc::send(&params).await,
                "mysql" => self.mysql.send_cancellable(&params, cancel.clone()).await,
                "terminal" => Err(Error::new("Service \"terminal\" is streaming-only")),
                _ => Err(Error::new(format!("Unknown service \"{service}\""))),
            }
        };
        let result = if service == "mysql" {
            request.await
        } else {
            tokio::select! {
                result=request=>result,
                _=cancel.cancelled()=>Ok(json!({"cancelled":true,"warning":"Client request stopped. The remote server may still finish processing; committed changes are not rolled back."})),
            }
        };
        result
            .and_then(|mut result| {
                if let Some(columns) = columns {
                    columns.apply(&mut result).map_err(Error::new)?;
                }
                Ok(result)
            })
            .map_err(|e| {
                if e.debug.is_none() {
                    e.with_debug(params)
                } else {
                    e
                }
            })
    }
    pub async fn discover(&self, service: &str, params: &Value) -> Result<Value> {
        self.discover_cancellable(service, params, CancellationToken::new())
            .await
    }
    pub async fn discover_cancellable(
        &self,
        service: &str,
        params: &Value,
        cancel: CancellationToken,
    ) -> Result<Value> {
        let params = self.resolve(service, params)?;
        if cancel.is_cancelled() {
            return Ok(json!({"cancelled":true}));
        }
        let discovery = async {
            match service {
                "rest" => services::openapi::discover(&self.http, &params).await,
                "graphql" => services::graphql::discover(&self.http, &params).await,
                "grpc" => services::grpc::discover(&params).await,
                "mysql" => {
                    self.mysql
                        .discover_cancellable(&params, cancel.clone())
                        .await
                }
                _ => Err(Error::new(format!(
                    "Service \"{service}\" doesn't support discovery"
                ))),
            }
        };
        let result = if service == "mysql" {
            discovery.await
        } else {
            tokio::select! {
                result=discovery=>result,
                _=cancel.cancelled()=>Ok(json!({"cancelled":true})),
            }
        };
        result.map_err(|e| {
            if e.debug.is_none() {
                e.with_debug(params)
            } else {
                e
            }
        })
    }
    pub async fn stream(
        &self,
        service: &str,
        params: &Value,
        cancel: CancellationToken,
        events: mpsc::Sender<Value>,
    ) -> Result<Value> {
        let params = self.resolve(service, params)?;
        self.stream_resolved(service, params, cancel, events).await
    }
    pub async fn stream_resolved(
        &self,
        service: &str,
        params: Value,
        cancel: CancellationToken,
        events: mpsc::Sender<Value>,
    ) -> Result<Value> {
        if service != "terminal" {
            return Err(Error::new(format!(
                "Service \"{service}\" is not streaming"
            )));
        }
        services::terminal::run(&params, cancel, events)
            .await
            .map_err(|e| {
                if e.debug.is_none() {
                    e.with_debug(params)
                } else {
                    e
                }
            })
    }
}
