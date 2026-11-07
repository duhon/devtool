use serde_json::{Value, json};
use services_native::Runtime;
use std::{
    fmt,
    ops::Deref,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio_util::sync::CancellationToken;

static NEXT: AtomicU64 = AtomicU64::new(0);

/// An isolated data directory, deleted on drop. Every runtime opened on it
/// shares state, which is how tests model a restart or a second window.
pub struct DataDir(PathBuf);

impl DataDir {
    pub fn new() -> Self {
        let path = std::env::temp_dir()
            .join("services-devtools-tests")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("test data directory must be creatable");
        crate::packages::install(&path);
        Self(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn runtime(&self) -> Runtime {
        Runtime::with_data_dir(&self.0).expect("runtime must open its data directory")
    }
}

impl Default for DataDir {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for DataDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A module host with its own data. Requests use the extension protocol, so a
/// test written against `Host` keeps working when a module moves out of the
/// native binary.
pub struct Host {
    data: Arc<DataDir>,
    runtime: Runtime,
}

impl Host {
    pub fn new() -> Self {
        Self::on(DataDir::new())
    }

    /// A host started on data prepared beforehand, e.g. legacy files to migrate.
    pub fn on(data: DataDir) -> Self {
        let data = Arc::new(data);
        let runtime = data.runtime();
        Self { data, runtime }
    }

    /// A second host on the same data, as after a restart or from another window.
    pub fn reopen(&self) -> Self {
        Self {
            data: self.data.clone(),
            runtime: self.data.runtime(),
        }
    }

    pub fn path(&self) -> &Path {
        self.data.path()
    }

    pub fn runtime(&self) -> &Runtime {
        &self.runtime
    }

    pub async fn send(&self, message: Value) -> Reply {
        self.send_cancellable(message, CancellationToken::new())
            .await
    }

    pub async fn send_cancellable(&self, message: Value, cancel: CancellationToken) -> Reply {
        let reply = self.runtime.dispatch_cancellable(&message, cancel).await;
        Reply::new(message, reply)
    }

    /// A panel operation: `{"service", "op", ...fields}`.
    pub async fn op(&self, service: &str, op: &str, fields: Value) -> Reply {
        let mut message = json!({"service":service,"op":op});
        merge(&mut message, fields);
        self.send(message).await
    }
}

impl Default for Host {
    fn default() -> Self {
        Self::new()
    }
}

fn merge(target: &mut Value, fields: Value) {
    if let (Some(target), Value::Object(fields)) = (target.as_object_mut(), fields) {
        target.extend(fields);
    }
}

/// One protocol reply, remembered with its request for failure messages.
#[derive(Clone, Debug)]
pub struct Reply {
    request: Value,
    value: Value,
}

impl Reply {
    pub fn new(request: Value, value: Value) -> Self {
        Self { request, value }
    }

    pub fn is_ok(&self) -> bool {
        self.value["ok"] == true
    }

    /// The `result` of a successful reply.
    #[track_caller]
    pub fn ok(&self) -> &Value {
        assert!(self.is_ok(), "expected success\n{self}");
        &self.value["result"]
    }

    /// The message of a failed reply.
    #[track_caller]
    pub fn error(&self) -> &str {
        assert!(!self.is_ok(), "expected an error\n{self}");
        self.value["error"]
            .as_str()
            .unwrap_or_else(|| panic!("error replies must carry a message\n{self}"))
    }

    pub fn value(&self) -> &Value {
        &self.value
    }

    pub fn into_value(self) -> Value {
        self.value
    }
}

impl Deref for Reply {
    type Target = Value;

    fn deref(&self) -> &Value {
        &self.value
    }
}

impl fmt::Display for Reply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "request: {}\nreply:   {}",
            redacted(&self.request),
            redacted(&self.value)
        )
    }
}

/// Variables expose the process environment as the `env` profile; failure
/// output must not leak it into logs.
fn redacted(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| match (key.as_str(), value) {
                    ("env", Value::Object(_)) => (key.clone(), json!("<process environment>")),
                    _ => (key.clone(), redacted(value)),
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(redacted).collect()),
        other => other.clone(),
    }
}
