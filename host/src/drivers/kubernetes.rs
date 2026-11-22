//! The kubernetes driver: an authenticated transport to the API server of a kubeconfig context
//! (contract in `services_sdk::kubernetes`). One `kube::Client` per kubeconfig and context,
//! reused across calls and rebuilt once when the server answers 401.
//!
//! Exec credential plugins (`kubelogin`…) are run by the driver, not by `kube`: the native host's
//! stdin is the browser's pipe, so a plugin must never inherit it, and a prompt on an inherited
//! stderr is invisible. The plugin runs without stdin, with its output captured, under a
//! deadline and is killed with it; its token or certificate then becomes the client's static
//! credential until it expires. A plugin that needs the user fails the call with `auth` and its
//! scrubbed stderr: the user logs in once in a terminal. Secrets are redacted before any answer
//! or event leaves the host. Tokens are never put in a failure.

use super::{CONNECT_MS, Failure, emit, micros, millis};
use base64::Engine;
use http_body_util::BodyExt;
use k8s_openapi::jiff::Timestamp;
use kube::{
    Client, Config,
    client::Body,
    config::{ExecConfig, ExecInteractiveMode, KubeConfigOptions, Kubeconfig, KubeconfigError},
};
use regex::Regex;
use serde_json::{Map, Value, json};
use services_sdk::{
    driver::Stage,
    http::Timing,
    kubernetes::{Context, Op, Request, Response, Status},
};
use std::{
    collections::HashMap,
    ffi::OsString,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant, SystemTime},
};
use tokio::{io::AsyncReadExt, sync::mpsc};
use tokio_util::sync::CancellationToken;

const REDACTED: &str = "<redacted>";
const LAST_APPLIED: &str = "kubectl.kubernetes.io/last-applied-configuration";
/// Time a credential plugin and the client build get when the call sets no sooner deadline.
const AUTH_MS: u64 = 30_000;
/// A credential is renewed this long before it expires.
const EXPIRY_SKEW: Duration = Duration::from_secs(10);
/// Characters of a failed plugin's stderr kept for the user.
const STDERR_KEPT: usize = 2_000;
const STOPPED: &str = "The request was dropped; a write the server received is not rolled back.";
const LOGIN: &str = "Log in once in a terminal (for example run kubectl with this context) \
                     and retry.";

type Files = Vec<(PathBuf, Option<SystemTime>, u64)>;
/// The kubeconfig files read (and what changed in them), and the context.
type Key = (Files, String);

struct Cached {
    client: Client,
    expires: Option<SystemTime>,
}

#[derive(Clone, Default)]
pub struct Kubernetes {
    /// A `KUBECONFIG` value (a path list) that replaces the environment and the default file;
    /// tests set it so that they never reach the user's kubeconfig.
    kubeconfig: Option<OsString>,
    clients: Arc<Mutex<HashMap<Key, Cached>>>,
}

impl Kubernetes {
    /// This driver reading `kubeconfig` (a `KUBECONFIG`-style path list) instead of the
    /// environment and `~/.kube/config`.
    pub fn with_kubeconfig(mut self, kubeconfig: impl Into<OsString>) -> Self {
        self.kubeconfig = Some(kubeconfig.into());
        self
    }

    /// Runs `request`; with `events` it streams watch events and followed log lines.
    pub async fn run(
        &self,
        request: Request,
        cancel: CancellationToken,
        events: Option<&mpsc::Sender<Value>>,
    ) -> Result<Value, Failure> {
        let deadline = request.timeout_ms.map(|ms| Instant::now() + millis(ms));
        // A stream ends by its deadline; any other call fails with it.
        let streaming = events.is_some() && request.stream;
        let expired = async {
            match deadline {
                Some(at) if !streaming => tokio::time::sleep_until(at.into()).await,
                _ => std::future::pending().await,
            }
        };
        let result = tokio::select! {
            result = self.execute(&request, events, &cancel, deadline) => result,
            () = expired => Err(Failure::new(Stage::Timeout, format!("The request timed out. {STOPPED}"))),
            () = cancel.cancelled() => Err(Failure::new(Stage::Cancelled, format!("Cancelled. {STOPPED}"))),
        };
        result.map(|answer| json!(answer))
    }

    async fn execute(
        &self,
        request: &Request,
        events: Option<&mpsc::Sender<Value>>,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> Result<Response, Failure> {
        match request.op {
            Op::Contexts => self.contexts(),
            Op::Request => self.request(request, events, cancel, deadline).await,
        }
    }

    fn paths(&self) -> Vec<PathBuf> {
        let value = self
            .kubeconfig
            .clone()
            .or_else(|| std::env::var_os("KUBECONFIG").filter(|value| !value.is_empty()));
        match value {
            Some(value) => std::env::split_paths(&value)
                .filter(|path| !path.as_os_str().is_empty())
                .collect(),
            None => std::env::var_os("HOME")
                .map(|home| vec![PathBuf::from(home).join(".kube").join("config")])
                .unwrap_or_default(),
        }
    }

    /// The merged kubeconfig; a listed file that does not exist is skipped, like kubectl does.
    fn load(&self) -> Result<(Kubeconfig, Files), Failure> {
        let mut merged = Kubeconfig::default();
        let mut files = vec![];
        for path in self.paths() {
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            files.push((path.clone(), meta.modified().ok(), meta.len()));
            let config = Kubeconfig::read_from(&path).map_err(|error| {
                Failure::new(
                    Stage::Auth,
                    format!("Cannot read the kubeconfig {}: {error}", path.display()),
                )
            })?;
            merged = merged.merge(config).map_err(|error| {
                Failure::new(
                    Stage::Auth,
                    format!("Cannot merge the kubeconfigs: {error}"),
                )
            })?;
        }
        Ok((merged, files))
    }

    /// The contexts of the kubeconfig; no cluster is contacted.
    fn contexts(&self) -> Result<Response, Failure> {
        let started = Instant::now();
        let (config, _) = self.load()?;
        let server = |cluster: &str| {
            config
                .clusters
                .iter()
                .find(|named| named.name == cluster)
                .and_then(|named| named.cluster.as_ref())
                .and_then(|cluster| cluster.server.clone())
                .unwrap_or_default()
        };
        let contexts = config
            .contexts
            .iter()
            .map(|named| {
                let context = named.context.clone().unwrap_or_default();
                Context {
                    name: named.name.clone(),
                    server: server(&context.cluster),
                    cluster: context.cluster,
                    namespace: context.namespace.filter(|namespace| !namespace.is_empty()),
                }
            })
            .collect();
        Ok(Response {
            contexts,
            current: config
                .current_context
                .clone()
                .filter(|name| !name.is_empty()),
            timing: Timing {
                first_byte_us: micros(started),
                total_us: micros(started),
            },
            ..Response::default()
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Key, Cached>> {
        self.clients.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The client of `context` and whether it was just built.
    async fn client(
        &self,
        context: &str,
        deadline: Option<Instant>,
    ) -> Result<(Key, Client, bool), Failure> {
        let (config, files) = self.load()?;
        let key = (files, context.to_owned());
        if let Some(cached) = self.lock().get(&key) {
            let valid = cached
                .expires
                .is_none_or(|expires| expires > SystemTime::now() + EXPIRY_SKEW);
            if valid {
                return Ok((key, cached.client.clone(), false));
            }
        }
        if !config.contexts.iter().any(|named| named.name == context) {
            return Err(Failure::new(
                Stage::Auth,
                format!("The context \"{context}\" is not in the kubeconfig"),
            ));
        }
        // The credentials get most of the call's time, so that their failure explains itself
        // before the call times out.
        let remaining = deadline.map(|at| at.saturating_duration_since(Instant::now()));
        let budget = remaining
            .map_or(millis(AUTH_MS), |left| left / 10 * 9)
            .min(millis(AUTH_MS));
        // The plugin's own timeout (inside `build`) explains a hang, so this one, bounding the
        // rest of the build, ends a little later.
        let margin = remaining.map_or(Duration::from_secs(1), |left| {
            (left / 20).min(Duration::from_secs(1))
        });
        let built = tokio::time::timeout(budget + margin, build(config, context, budget))
            .await
            .map_err(|_| {
                Failure::new(
                    Stage::Auth,
                    format!(
                        "Getting the credentials for the context \"{context}\" timed out. {LOGIN}"
                    ),
                )
            })??;
        let client = built.client.clone();
        self.lock().insert(key.clone(), built);
        Ok((key, client, true))
    }

    async fn request(
        &self,
        request: &Request,
        events: Option<&mpsc::Sender<Value>>,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> Result<Response, Failure> {
        let started = Instant::now();
        let context = request
            .context
            .as_deref()
            .filter(|context| !context.is_empty())
            .ok_or_else(|| Failure::new(Stage::Send, "A request needs a context"))?;
        let (key, client, fresh) = self.client(context, deadline).await?;
        let mut response = send(&client, request).await?;
        if response.status() == 401 && !fresh {
            self.lock().remove(&key);
            let (_, client, _) = self.client(context, deadline).await?;
            response = send(&client, request).await?;
        }
        let status = response.status().as_u16();
        let ok = response.status().is_success();
        let first_byte_us = micros(started);
        let json_body = response
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.contains("json"));
        let secrets = is_secrets_path(&request.path);
        let mut answer = Response {
            status,
            ..Response::default()
        };
        let body = response.into_body();
        match events.filter(|_| request.stream && ok) {
            Some(events) => {
                let watching = matches!(
                    request.query.get("watch").map(String::as_str),
                    Some("true" | "1")
                );
                let lines = Lines {
                    max: request.max_bytes,
                    watching,
                    secrets,
                    deadline,
                };
                answer.truncated = stream(body, &lines, events, cancel).await?;
            }
            None => {
                let (bytes, truncated) = collect(body, request.max_bytes).await?;
                answer.truncated = truncated;
                let sensitive = secrets || mentions_secret(&bytes);
                answer.body = if !truncated && (json_body || sensitive) {
                    match serde_json::from_slice::<Value>(&bytes) {
                        Ok(mut value) => {
                            redact(&mut value, secrets);
                            value
                        }
                        Err(_) if sensitive => Value::String(withheld("it is not valid JSON")),
                        Err(error) => {
                            return Err(Failure::new(
                                Stage::Decode,
                                format!("The answer is not valid JSON: {error}"),
                            ));
                        }
                    }
                } else if sensitive {
                    Value::String(withheld("the answer was cut at maxBytes"))
                } else {
                    Value::String(String::from_utf8_lossy(&bytes).into_owned())
                };
                if !ok {
                    answer.error = Some(status_of(status, &answer.body));
                }
            }
        }
        answer.timing = Timing {
            first_byte_us,
            total_us: micros(started),
        };
        Ok(answer)
    }
}

/// The client for `context`, its exec plugin already run.
async fn build(kubeconfig: Kubeconfig, context: &str, budget: Duration) -> Result<Cached, Failure> {
    let options = KubeConfigOptions {
        context: Some(context.into()),
        ..KubeConfigOptions::default()
    };
    let mut config = Config::from_custom_kubeconfig(kubeconfig, &options)
        .await
        .map_err(|error: KubeconfigError| {
            Failure::new(
                Stage::Auth,
                format!("The kubeconfig context \"{context}\" is unusable: {error}"),
            )
        })?;
    config.connect_timeout.get_or_insert(millis(CONNECT_MS));
    let mut expires = None;
    if let Some(mut exec) = config.auth_info.exec.take() {
        exec.interactive_mode = Some(ExecInteractiveMode::Never);
        let credential = run_plugin(&exec, budget).await?;
        expires = credential.expires;
        let auth = &mut config.auth_info;
        auth.token = credential.token.map(Into::into);
        auth.token_file = None;
        if let (Some(cert), Some(key)) = (credential.certificate, credential.key) {
            let base64 = base64::engine::general_purpose::STANDARD;
            auth.client_certificate_data = Some(base64.encode(cert));
            auth.client_key_data = Some(base64.encode(key).into());
            auth.client_certificate = None;
            auth.client_key = None;
        }
    }
    // Legacy auth providers may run a command synchronously while the client is built.
    let client = tokio::task::spawn_blocking(move || Client::try_from(config))
        .await
        .map_err(|error| Failure::new(Stage::Auth, format!("Building the client failed: {error}")))?
        .map_err(|error| Failure::chain(Stage::Auth, &error))?;
    Ok(Cached { client, expires })
}

struct Credential {
    token: Option<String>,
    certificate: Option<String>,
    key: Option<String>,
    expires: Option<SystemTime>,
}

/// Runs the plugin without stdin, its output captured, killed when `budget` ends.
async fn run_plugin(exec: &ExecConfig, budget: Duration) -> Result<Credential, Failure> {
    let auth = |message: String| Failure::new(Stage::Auth, message);
    let Some(command) = &exec.command else {
        return Err(auth("The credential plugin names no command".into()));
    };
    let mut cmd = tokio::process::Command::new(command);
    cmd.args(exec.args.iter().flatten())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    for env in exec.env.iter().flatten() {
        if let (Some(name), Some(value)) = (env.get("name"), env.get("value")) {
            cmd.env(name, value);
        }
    }
    for name in exec.drop_env.iter().flatten() {
        cmd.env_remove(name);
    }
    let mut info = json!({
        "apiVersion": exec.api_version.as_deref().unwrap_or("client.authentication.k8s.io/v1"),
        "kind": "ExecCredential",
        "spec": {"interactive": false},
    });
    if exec.provide_cluster_info
        && let Some(cluster) = &exec.cluster
    {
        info["spec"]["cluster"] = serde_json::to_value(cluster).unwrap_or_default();
    }
    cmd.env("KUBERNETES_EXEC_INFO", info.to_string());
    let mut child = cmd.spawn().map_err(|error| {
        let hint = exec.install_hint.as_deref().unwrap_or_default();
        auth(format!(
            "Cannot start the credential plugin `{command}`: {error}. {hint}"
        ))
    })?;
    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");
    let read = async {
        let (mut out, mut err) = (vec![], vec![]);
        let (mut stdout, mut stderr) = (stdout.take(1 << 20), stderr.take(1 << 16));
        let (a, b) = tokio::join!(stdout.read_to_end(&mut out), stderr.read_to_end(&mut err));
        a.and(b)?;
        let status = child.wait().await?;
        Ok::<_, std::io::Error>((status, out, err))
    };
    let (status, out, err) = match tokio::time::timeout(budget, read).await {
        Ok(Ok(done)) => done,
        Ok(Err(error)) => {
            return Err(auth(format!(
                "The credential plugin `{command}` failed: {error}"
            )));
        }
        Err(_) => {
            return Err(auth(format!(
                "The credential plugin `{command}` did not finish in {} s; it probably waits \
                 for you to log in. {LOGIN}",
                budget.as_secs()
            )));
        }
    };
    if !status.success() {
        return Err(auth(format!(
            "The credential plugin `{command}` failed ({status}). {LOGIN} Plugin output: {}",
            scrub(&String::from_utf8_lossy(&err)).trim()
        )));
    }
    let bad = |what: &str| auth(format!("The credential plugin `{command}` printed {what}"));
    let value: Value =
        serde_json::from_slice(&out).map_err(|_| bad("no valid ExecCredential JSON"))?;
    let text = |name: &str| {
        value["status"][name]
            .as_str()
            .filter(|text| !text.is_empty())
            .map(String::from)
    };
    let credential = Credential {
        token: text("token"),
        certificate: text("clientCertificateData"),
        key: text("clientKeyData"),
        expires: text("expirationTimestamp")
            .and_then(|at| at.parse::<Timestamp>().ok())
            .map(SystemTime::from),
    };
    if credential.token.is_none() && credential.certificate.is_none() {
        return Err(bad("neither a token nor a client certificate"));
    }
    Ok(credential)
}

/// Text without anything that looks like a token or a secret.
fn scrub(text: &str) -> String {
    static PATTERNS: OnceLock<[Regex; 3]> = OnceLock::new();
    let [jwt, named, long] = PATTERNS.get_or_init(|| {
        [
            Regex::new(r"eyJ[\w-]{8,}(\.[\w-]+)*").unwrap(),
            Regex::new(r"(?i)\b(bearer|token|secret|password)(\W{1,3})\S+").unwrap(),
            Regex::new(r"[A-Za-z0-9+/_-]{40,}={0,2}").unwrap(),
        ]
    });
    let text = jwt.replace_all(text, REDACTED);
    let text = named.replace_all(&text, "$1$2<redacted>");
    let text = long.replace_all(&text, REDACTED);
    text.chars().take(STDERR_KEPT).collect()
}

async fn send(client: &Client, request: &Request) -> Result<http::Response<Body>, Failure> {
    let method = http::Method::from_bytes(request.method.to_ascii_uppercase().as_bytes())
        .ok()
        .filter(|method| ["GET", "POST", "PUT", "PATCH", "DELETE"].contains(&method.as_str()))
        .ok_or_else(|| {
            Failure::new(
                Stage::Send,
                "The method must be GET, POST, PUT, PATCH or DELETE",
            )
        })?;
    if !request.path.starts_with('/') {
        return Err(Failure::new(Stage::Send, "The path must start with /"));
    }
    let mut uri = request.path.clone();
    if !request.query.is_empty() {
        uri.push(if uri.contains('?') { '&' } else { '?' });
        uri.push_str(
            &url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs(&request.query)
                .finish(),
        );
    }
    let mut builder = http::Request::builder().method(method).uri(uri).header(
        http::header::ACCEPT,
        request.accept.as_deref().unwrap_or("application/json"),
    );
    let body = match &request.body {
        Some(body) => {
            builder = builder.header(
                http::header::CONTENT_TYPE,
                request
                    .content_type
                    .as_deref()
                    .unwrap_or("application/json"),
            );
            Body::from(body.clone().into_bytes())
        }
        None => Body::from(Vec::new()),
    };
    let message = builder
        .body(body)
        .map_err(|error| Failure::new(Stage::Send, format!("Invalid request: {error}")))?;
    client
        .send(message)
        .await
        .map_err(|error| Failure::chain(Stage::Connect, &error))
}

/// The body up to `max` bytes, and whether the server had more.
async fn collect(mut body: Body, max: Option<u64>) -> Result<(Vec<u8>, bool), Failure> {
    let mut bytes = vec![];
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|error| Failure::chain(Stage::Decode, &error))?;
        let Ok(data) = frame.into_data() else {
            continue;
        };
        if let Some(max) = max
            && bytes.len() as u64 + data.len() as u64 > max
        {
            bytes.extend_from_slice(&data[..(max - bytes.len() as u64) as usize]);
            return Ok((bytes, true));
        }
        bytes.extend_from_slice(&data);
    }
    Ok((bytes, false))
}

struct Lines {
    max: Option<u64>,
    /// Lines are watch events (JSON), else log lines.
    watching: bool,
    secrets: bool,
    deadline: Option<Instant>,
}

/// Sends the lines of the body as events until it ends or the deadline passes (`false`) or
/// `max` bytes were sent (`true`); a cancel is a failure.
async fn stream(
    mut body: Body,
    lines: &Lines,
    events: &mpsc::Sender<Value>,
    cancel: &CancellationToken,
) -> Result<bool, Failure> {
    let (mut pending, mut sent) = (Vec::<u8>::new(), 0u64);
    let expired = async {
        match lines.deadline {
            Some(at) => tokio::time::sleep_until(at.into()).await,
            None => std::future::pending().await,
        }
    };
    tokio::pin!(expired);
    loop {
        let frame = tokio::select! {
            frame = body.frame() => frame,
            () = &mut expired => return Ok(false),
            () = cancel.cancelled() => {
                return Err(Failure::new(Stage::Cancelled, format!("Cancelled. {STOPPED}")));
            }
        };
        let last = match frame {
            None => true,
            Some(frame) => {
                let frame = frame.map_err(|error| Failure::chain(Stage::Decode, &error))?;
                if let Ok(data) = frame.into_data() {
                    pending.extend_from_slice(&data);
                }
                false
            }
        };
        while let Some(end) = pending
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|at| at + 1)
            .or((last && !pending.is_empty()).then_some(pending.len()))
        {
            let line: Vec<u8> = pending.drain(..end).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\n', '\r']);
            sent += line.len() as u64 + 1;
            if lines.max.is_some_and(|max| sent > max) {
                return Ok(true);
            }
            if !lines.watching {
                emit(events, json!({"line": line})).await?;
            } else if !line.trim().is_empty() {
                let mut object: Value = serde_json::from_str(line).map_err(|error| {
                    Failure::new(Stage::Decode, format!("A watch event is not JSON: {error}"))
                })?;
                redact(&mut object, lines.secrets);
                emit(events, json!({"object": object})).await?;
            }
        }
        if last {
            return Ok(false);
        }
    }
}

/// The `Status` of a non-2xx answer.
fn status_of(code: u16, body: &Value) -> Status {
    let text = |name: &str| body[name].as_str().map(String::from).unwrap_or_default();
    if body["kind"] == "Status" {
        return Status {
            reason: text("reason"),
            message: text("message"),
            code: body["code"].as_u64().map_or(code, |code| code as u16),
        };
    }
    let message = match body {
        Value::String(text) => text.chars().take(500).collect(),
        other => other.to_string().chars().take(500).collect(),
    };
    Status {
        reason: http::StatusCode::from_u16(code)
            .ok()
            .and_then(|code| code.canonical_reason())
            .unwrap_or_default()
            .replace(' ', ""),
        message,
        code,
    }
}

/// Whether `path` addresses secrets (`/api/v1/namespaces/ns/secrets/x`): their metadata-only
/// answers (a `Table`) carry the last-applied annotation too.
/// Whether the raw answer may hold a Secret, which is only safe to return once parsed and redacted.
fn mentions_secret(bytes: &[u8]) -> bool {
    bytes.windows(6).any(|window| window == b"Secret")
}

fn withheld(reason: &str) -> String {
    format!("<redacted: {reason}, so it cannot be checked for Secret values>")
}

fn is_secrets_path(path: &str) -> bool {
    let path = path.split('?').next().unwrap_or_default();
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    matches!(
        segments.as_slice(),
        ["api", "v1", "secrets", ..] | ["api", "v1", "namespaces", _, "secrets", ..]
    )
}

/// Replaces the values of every Secret in `value`; with `secrets` every object is one.
pub fn redact(value: &mut Value, secrets: bool) {
    match value {
        Value::Object(object) => {
            let kind = object.get("kind").and_then(Value::as_str);
            let list = kind == Some("SecretList");
            if secrets || kind == Some("Secret") {
                redact_secret(object);
            }
            for (name, child) in object.iter_mut() {
                redact(child, secrets || (list && name == "items"));
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|item| redact(item, secrets)),
        _ => {}
    }
}

fn redact_secret(object: &mut Map<String, Value>) {
    for field in ["data", "stringData"] {
        if let Some(Value::Object(values)) = object.get_mut(field) {
            values
                .values_mut()
                .for_each(|value| *value = Value::String(REDACTED.into()));
        }
    }
    if let Some(annotation) = object
        .get_mut("metadata")
        .and_then(|metadata| metadata.get_mut("annotations"))
        .and_then(|annotations| annotations.get_mut(LAST_APPLIED))
    {
        *annotation = Value::String(REDACTED.into());
    }
}
