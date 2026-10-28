use super::{call, error, get, js, json, window};
use crate::descriptors;
use js_sys::Promise;
use serde_json::{Value, json};
use services_contracts::native_messaging::ResponseAssembler;
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{MessageEvent, RequestInit, Response, WebSocket};
use yew::Callback;

#[derive(Clone, Debug)]
pub enum Reply {
    Result(Value),
    Data(Value),
    Exit(Value),
    Error(Value),
}
struct Pending {
    callback: Callback<Reply>,
    trace: Vec<Value>,
    stream: bool,
    chunks: ResponseAssembler,
}
struct HttpScope {
    id: String,
    socket: WebSocket,
    ready: Promise,
}
#[derive(Default)]
struct HttpRequest {
    cancelled: Cell<bool>,
    dispatched: Cell<bool>,
}
pub struct Transport {
    session: RefCell<Option<super::sessions::Shared>>,
    in_flight: Cell<usize>,
    port: RefCell<Option<JsValue>>,
    next: Cell<u32>,
    pending: RefCell<BTreeMap<u32, Pending>>,
    disconnected: RefCell<Option<String>>,
    sockets: RefCell<BTreeMap<u32, WebSocket>>,
    status: Callback<Option<String>>,
    native: bool,
    generation: Cell<u32>,
    http_scope: RefCell<Option<HttpScope>>,
    http_requests: RefCell<BTreeMap<u32, Rc<HttpRequest>>>,
}
impl Transport {
    pub fn bind_session(&self, session: super::sessions::Shared) {
        *self.session.borrow_mut() = Some(session);
    }
    pub fn busy(&self) -> bool {
        self.in_flight.get() != 0 || !self.sockets.borrow().is_empty()
    }
    pub fn variable_drafts(&self) -> BTreeMap<String, String> {
        self.session
            .borrow()
            .as_ref()
            .map(|s| s.borrow().library.snapshot().variable_drafts.clone())
            .unwrap_or_default()
    }
    pub fn save_variable_drafts(&self, drafts: &BTreeMap<String, String>) -> Result<(), String> {
        let session = self
            .session
            .borrow()
            .clone()
            .ok_or("Session is unavailable")?;
        session
            .borrow_mut()
            .update(|snapshot| snapshot.variable_drafts = drafts.clone())
    }
    pub fn is_native(&self) -> bool {
        self.native
    }
    pub fn new(status: Callback<Option<String>>) -> Rc<Self> {
        let chrome = get(&js_sys::global(), "chrome");
        let runtime = get(&chrome, "runtime");
        let native = get(&runtime, "connectNative").is_function();
        let this = Rc::new(Self {
            session: RefCell::new(None),
            in_flight: Cell::new(0),
            port: RefCell::new(None),
            next: Cell::new(1),
            pending: RefCell::new(BTreeMap::new()),
            disconnected: RefCell::new(None),
            sockets: RefCell::new(BTreeMap::new()),
            status,
            native,
            generation: Cell::new(0),
            http_scope: RefCell::new(None),
            http_requests: RefCell::new(BTreeMap::new()),
        });
        let weak = Rc::downgrade(&this);
        let close = Closure::<dyn FnMut()>::new(move || {
            if let Some(this) = weak.upgrade() {
                this.shutdown();
            }
        });
        if let Err(error) =
            window().add_event_listener_with_callback("pagehide", close.as_ref().unchecked_ref())
        {
            web_sys::console::error_1(&error);
        }
        close.forget();
        if native {
            this.connect();
        }
        this
    }
    pub fn connect(self: &Rc<Self>) {
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        if let Some(old) = self.port.borrow_mut().take()
            && let Err(e) = call(&old, "disconnect", &[])
        {
            web_sys::console::warn_1(&e);
        }
        *self.disconnected.borrow_mut() = None;
        let runtime = get(&get(&js_sys::global(), "chrome"), "runtime");
        match call(
            &runtime,
            "connectNative",
            &[services_contracts::HOST_NAME.into()],
        ) {
            Ok(port) => {
                let weak = Rc::downgrade(self);
                let onmessage = Closure::<dyn FnMut(JsValue)>::new(move |message| {
                    if let Some(this) = weak.upgrade() {
                        this.receive(json(message));
                    }
                });
                let listener = get(&port, "onMessage");
                if let Err(e) = call(&listener, "addListener", &[onmessage.as_ref().clone()]) {
                    self.offline(error(e));
                    return;
                }
                onmessage.forget();
                let weak = Rc::downgrade(self);
                let disconnect = Closure::<dyn FnMut()>::new(move || {
                    // Chrome requires lastError to be read synchronously in this callback.
                    let runtime = get(&get(&js_sys::global(), "chrome"), "runtime");
                    let detail = get(&get(&runtime, "lastError"), "message")
                        .as_string()
                        .unwrap_or_else(|| "The native host disconnected".into());
                    if let Some(this) = weak.upgrade()
                        && this.generation.get() == generation
                    {
                        this.offline(detail);
                    }
                });
                if let Err(e) = call(
                    &get(&port, "onDisconnect"),
                    "addListener",
                    &[disconnect.as_ref().clone()],
                ) {
                    self.offline(error(e));
                    return;
                }
                disconnect.forget();
                *self.port.borrow_mut() = Some(port);
                let weak = Rc::downgrade(self);
                self.request(json!({"type":"health","capabilities":{"chunkedResponses":true}}),false,Callback::from(move|reply|{
                    if let Some(this)=weak.upgrade(){match reply{
                        Reply::Result(value)if value["protocolVersion"]==services_contracts::PROTOCOL_VERSION=>this.status.emit(None),
                        Reply::Result(value)=>this.offline(format!("Native protocol mismatch: expected {}, received {}. Reinstall the native host and reload this panel.",services_contracts::PROTOCOL_VERSION,value["protocolVersion"])),
                        Reply::Error(e)=>this.status.emit(Some(e["error"].as_str().unwrap_or("Native health check failed").into())),
                        _=>{},
                    }}
                }));
            }
            Err(e) => self.offline(error(e)),
        }
    }
    fn offline(&self, detail: String) {
        let message = format!(
            "SERVER_OFFLINE: {detail}. Install or update the Services DevTools native host, then reload the extension or click Retry connection."
        );
        *self.disconnected.borrow_mut() = Some(message.clone());
        self.port.borrow_mut().take();
        self.status.emit(Some(message.clone()));
        let pending = std::mem::take(&mut *self.pending.borrow_mut());
        for (_, entry) in pending {
            entry.callback.emit(Reply::Error(
                json!({"error":message,"transportDebug":entry.trace}),
            ));
        }
    }
    fn receive(&self, message: Value) {
        let Some(id) = message["id"].as_u64().map(|n| n as u32) else {
            return;
        };
        let message = if message["type"] == "nativeChunk" {
            let assembled = {
                let mut entries = self.pending.borrow_mut();
                let Some(entry) = entries.get_mut(&id) else {
                    return;
                };
                entry.chunks.push(&message)
            };
            match assembled {
                Ok(Some(value)) => value,
                Ok(None) => return,
                Err(error) => {
                    let pending = self.pending.borrow_mut().remove(&id);
                    if let Err(error) = self.stop(id) {
                        web_sys::console::error_1(&error.into());
                    }
                    if let Some(entry) = pending {
                        entry.callback.emit(Reply::Error(json!({
                            "error":error,"transportDebug":entry.trace,
                        })));
                    }
                    return;
                }
            }
        } else {
            message
        };
        let data = message["type"] == "data";
        let mut entries = self.pending.borrow_mut();
        let Some(entry) = entries.get_mut(&id) else {
            return;
        };
        if !entry.stream {
            entry
                .trace
                .push(json!({"direction":"inbound","type":"native-message","message":message}));
        }
        let callback = entry.callback.clone();
        let trace = entry.trace.clone();
        let reply = if data {
            Reply::Data(message["event"].clone())
        } else if message["type"] == "exit" {
            let mut info = message["info"].clone();
            if !info.is_object() {
                info = json!({});
            }
            info["transportDebug"] = json!(trace);
            Reply::Exit(info)
        } else if message["ok"] == true {
            let mut result = message["result"].clone();
            if result.is_object() {
                result["transportDebug"] = json!(trace);
            }
            Reply::Result(result)
        } else {
            let mut e = message.clone();
            e["transportDebug"] = json!(trace);
            Reply::Error(e)
        };
        if !data {
            entries.remove(&id);
        }
        drop(entries);
        callback.emit(reply);
    }
    pub fn request(
        self: &Rc<Self>,
        mut message: Value,
        stream: bool,
        callback: Callback<Reply>,
    ) -> u32 {
        let id = self.next.get();
        self.next.set(id.wrapping_add(1).max(1));
        message["id"] = json!(id);
        if let Some(session) = self.session.borrow().clone() {
            let service = message["service"].as_str().unwrap_or("");
            if service == "hints" {
                let name = message["serviceName"].as_str().unwrap_or("");
                let result = if message["op"] == "save" {
                    let text = message["text"].as_str().unwrap_or("");
                    crate::console::validate_hints(text)
                        .and_then(|_| {
                            session.borrow_mut().update(|s| {
                                s.hints.insert(name.into(), text.into());
                            })
                        })
                        .map(|_| json!({"text":text}))
                } else {
                    Ok(
                        json!({"text":session.borrow().library.snapshot().hints.get(name).cloned().unwrap_or_default()}),
                    )
                };
                let saving = message["op"] == "save";
                spawn_local(async move {
                    let result = match result {
                        Ok(value) if saving => {
                            super::sessions::Store::flush(&session).await.map(|_| value)
                        }
                        result => result,
                    };
                    callback.emit(match result {
                        Ok(value) => Reply::Result(value),
                        Err(error) => Reply::Error(json!({"error":error})),
                    });
                });
                return id;
            }
            let variables = session.borrow().library.snapshot().variables.clone();
            if service == "vars" || service == "dictionary" {
                message["sessionVariables"] = variables;
            } else if crate::console::SERVICES.contains(&service) || service == "agent" {
                if !message["params"].is_object() {
                    message["params"] = json!({});
                }
                message["params"]["__consoleVariables"] = variables;
            }
        }
        self.in_flight.set(self.in_flight.get() + 1);
        let weak = Rc::downgrade(self);
        let save_variables = message["service"] == "vars" && message["op"] == "save";
        let completed = Cell::new(false);
        let callback = Callback::from(move |mut reply: Reply| {
            if let Some(this) = weak.upgrade() {
                if !matches!(reply, Reply::Data(_)) && !completed.replace(true) {
                    this.in_flight.set(this.in_flight.get().saturating_sub(1));
                }
                if save_variables
                    && let Reply::Result(value) = &reply
                    && let Some(session) = this.session.borrow().clone()
                {
                    let result = session
                        .borrow_mut()
                        .update(|snapshot| snapshot.variables = value["profiles"]["var"].clone());
                    if let Err(error) = result {
                        reply = Reply::Error(json!({"error":error}));
                    } else {
                        let callback = callback.clone();
                        spawn_local(async move {
                            let reply = match super::sessions::Store::flush(&session).await {
                                Ok(()) => reply,
                                Err(error) => Reply::Error(json!({"error":error})),
                            };
                            callback.emit(reply);
                        });
                        return;
                    }
                }
            }
            callback.emit(reply);
        });
        if self.native {
            if let Some(detail) = self.disconnected.borrow().clone() {
                callback.emit(Reply::Error(json!({"error":detail})));
                return id;
            }
            self.pending.borrow_mut().insert(
                id,
                Pending {
                    callback: callback.clone(),
                    trace: if message["service"]=="agent" {Vec::new()}else{vec![
                        json!({"direction":"outbound","type":"native-message","message":message}),
                    ]},
                    stream,
                    chunks: ResponseAssembler::default(),
                },
            );
            let result = self
                .port
                .borrow()
                .as_ref()
                .ok_or(JsValue::from_str("Native port unavailable"))
                .and_then(|p| call(p, "postMessage", &[js(&message)]));
            if let Err(e) = result {
                self.pending.borrow_mut().remove(&id);
                callback.emit(Reply::Error(json!({"error":error(e),"message":message})));
            }
            if !stream {
                let timeout_seconds = if message["service"] == "splunk" {
                    services_contracts::SPLUNK_TIMEOUT_SECONDS + 5
                } else if message["service"] == "updates" && message["op"] == "install" {
                    600
                } else {
                    120
                };
                let weak = Rc::downgrade(self);
                let timeout = Closure::once_into_js(move || {
                    if let Some(this) = weak.upgrade() {
                        let pending = this.pending.borrow_mut().remove(&id);
                        if let Some(p) = pending {
                            if let Err(error) = this.stop(id) {
                                web_sys::console::error_1(&error.into());
                            }
                            p.callback.emit(Reply::Error(json!({"error":format!("Native request timed out after {timeout_seconds} seconds. Check Debug, the native host installation, and server connectivity."),"transportDebug":p.trace})));
                        }
                    }
                });
                if let Err(e) = window().set_timeout_with_callback_and_timeout_and_arguments_0(
                    timeout.unchecked_ref(),
                    (timeout_seconds * 1_000) as i32,
                ) {
                    web_sys::console::error_1(&e);
                }
            }
        } else if stream {
            self.stream_http(id, message, callback);
        } else {
            let this = self.clone();
            let owned =
                crate::console::SERVICES.contains(&message["service"].as_str().unwrap_or(""));
            let request = Rc::new(HttpRequest::default());
            if owned {
                self.http_requests.borrow_mut().insert(id, request.clone());
            }
            spawn_local(async move {
                let (url, method, body) = http_route(&message);
                let trace = json!({"direction":"outbound","type":"fetch","url":url,"method":method,"body":body});
                let response = async {
                    let scope = if owned {
                        let (scope, ready) =
                            this.http_scope().map_err(|error| json!({"error":error}))?;
                        JsFuture::from(ready)
                            .await
                            .map_err(|e| json!({"error":error(e)}))?;
                        if request.cancelled.get() {
                            return Ok(json!({"cancelled":true,"phase":"before-dispatch"}));
                        }
                        request.dispatched.set(true);
                        Some((scope, id))
                    } else {
                        None
                    };
                    fetch(&url, &method, body, scope).await
                }
                .await;
                this.http_requests.borrow_mut().remove(&id);
                match response {
                    Ok(mut value) => {
                        if value.is_object() {
                            value["transportDebug"] =
                                json!([trace,{"direction":"inbound","body":value.clone()}]);
                        }
                        callback.emit(Reply::Result(value));
                    }
                    Err(mut err) => {
                        err["transportDebug"] = json!([trace]);
                        callback.emit(Reply::Error(err));
                    }
                }
            });
        }
        id
    }
    fn http_scope(&self) -> Result<(String, Promise), String> {
        let mut stored = self.http_scope.borrow_mut();
        if let Some(scope) = stored.as_ref()
            && scope.socket.ready_state() <= WebSocket::OPEN
        {
            return Ok((scope.id.clone(), scope.ready.clone()));
        }
        let id = super::agent_panel::id()?;
        let location = window().location();
        let socket = WebSocket::new(&format!(
            "{}://{}/api/operations/{id}",
            if location.protocol().map_err(error)? == "https:" {
                "wss"
            } else {
                "ws"
            },
            location.host().map_err(error)?
        ))
        .map_err(error)?;
        let ready = Promise::new(&mut |resolve, reject| {
            let resolve = resolve.clone();
            let invalid = reject.clone();
            let message = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
                let value = event
                    .data()
                    .as_string()
                    .and_then(|text| serde_json::from_str::<Value>(&text).ok());
                let result = if value.as_ref().is_some_and(|value| value["ready"] == true) {
                    resolve.call0(&JsValue::NULL)
                } else {
                    let message = value
                        .as_ref()
                        .and_then(|value| value["error"].as_str())
                        .unwrap_or("Invalid Console lifecycle response");
                    invalid.call1(&JsValue::NULL, &message.into())
                };
                if let Err(error) = result {
                    web_sys::console::error_1(&error);
                }
            });
            socket.set_onmessage(Some(message.as_ref().unchecked_ref()));
            message.forget();
            let reject = reject.clone();
            let closed = Closure::<dyn FnMut()>::new(move || {
                if let Err(error) =
                    reject.call1(&JsValue::NULL, &"Console lifecycle disconnected".into())
                {
                    web_sys::console::error_1(&error);
                }
            });
            socket.set_onclose(Some(closed.as_ref().unchecked_ref()));
            socket.set_onerror(Some(closed.as_ref().unchecked_ref()));
            closed.forget();
        });
        *stored = Some(HttpScope {
            id: id.clone(),
            socket,
            ready: ready.clone(),
        });
        Ok((id, ready))
    }
    pub fn stop_all(&self) -> Result<(), String> {
        let ids: Vec<_> = if self.native {
            self.pending.borrow().keys().copied().collect()
        } else {
            self.sockets
                .borrow()
                .keys()
                .chain(self.http_requests.borrow().keys())
                .copied()
                .collect()
        };
        let mut errors = Vec::new();
        for id in ids {
            if let Err(error) = self.stop(id) {
                errors.push(error);
            }
        }
        if let Some(scope) = self.http_scope.borrow_mut().take()
            && let Err(error) = scope.socket.close()
        {
            errors.push(super::error(error));
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
    fn shutdown(&self) {
        if let Err(error) = self.stop_all() {
            web_sys::console::error_1(&error.into());
        }
        for socket in self.sockets.borrow().values() {
            if let Err(error) = socket.close() {
                web_sys::console::error_1(&error);
            }
        }
        if let Some(port) = self.port.borrow_mut().take()
            && let Err(error) = call(&port, "disconnect", &[])
        {
            web_sys::console::error_1(&error);
        }
    }
    fn stream_http(self: &Rc<Self>, id: u32, message: Value, callback: Callback<Reply>) {
        let location = window().location();
        let protocol = location.protocol().unwrap_or_default();
        let host = location.host().unwrap_or_default();
        let url = format!(
            "{}://{host}{}",
            if protocol == "https:" { "wss" } else { "ws" },
            if message["service"] == "agent" {
                "/api/agent/stream"
            } else {
                "/api/services/terminal/stream"
            }
        );
        let socket = match WebSocket::new(&url) {
            Ok(s) => s,
            Err(e) => {
                callback.emit(Reply::Error(json!({"error":error(e)})));
                return;
            }
        };
        let opened = socket.clone();
        let params = if message.get("history").is_some() {
            message.to_string()
        } else {
            message["params"].to_string()
        };
        let cb = callback.clone();
        let open = Closure::<dyn FnMut()>::new(move || {
            if let Err(e) = opened.send_with_str(&params) {
                cb.emit(Reply::Error(json!({"error":error(e)})));
            }
        });
        socket.set_onopen(Some(open.as_ref().unchecked_ref()));
        open.forget();
        let finished = Rc::new(Cell::new(false));
        let f = finished.clone();
        let cb = callback.clone();
        let weak = Rc::downgrade(self);
        let msg = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            match event
                .data()
                .as_string()
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            {
                Some(value) if value["type"] == "exit" => {
                    f.set(true);
                    if let Some(this) = weak.upgrade()
                        && let Some(socket) = this.sockets.borrow_mut().remove(&id)
                        && let Err(error) = socket.close()
                    {
                        web_sys::console::error_1(&error);
                    }
                    cb.emit(Reply::Exit(value));
                }
                Some(value) => cb.emit(Reply::Data(value)),
                None => cb.emit(Reply::Error(
                    json!({"error":"Invalid JSON from streaming server"}),
                )),
            }
        });
        socket.set_onmessage(Some(msg.as_ref().unchecked_ref()));
        msg.forget();
        let weak = Rc::downgrade(self);
        let f = finished;
        let close = Closure::<dyn FnMut()>::new(move || {
            if let Some(this) = weak.upgrade() {
                this.sockets.borrow_mut().remove(&id);
            }
            if !f.replace(true) {
                callback.emit(Reply::Error(
                    json!({"error":"SERVER_OFFLINE: streaming connection closed before exit"}),
                ));
            }
        });
        socket.set_onclose(Some(close.as_ref().unchecked_ref()));
        close.forget();
        self.sockets.borrow_mut().insert(id, socket);
    }
    pub fn agent_reply(&self, id: u32, call_id: &str, result: Value) -> Result<(), String> {
        let message = json!({"id":id,"type":"agentToolResult","callId":call_id,"result":result});
        if self.native {
            if !self.pending.borrow().contains_key(&id) {
                return Err("Agent turn is no longer connected".into());
            }
            self.port
                .borrow()
                .as_ref()
                .ok_or("Native host is disconnected".into())
                .and_then(|port| {
                    call(port, "postMessage", &[js(&message)])
                        .map(|_| ())
                        .map_err(error)
                })
        } else {
            self.sockets
                .borrow()
                .get(&id)
                .ok_or("Agent turn is no longer connected".into())
                .and_then(|socket| socket.send_with_str(&message.to_string()).map_err(error))
        }
    }
    pub fn stop(&self, id: u32) -> Result<(), String> {
        if self.native {
            let message = json!({"id":id,"type":"stop"});
            if let Some(entry) = self.pending.borrow_mut().get_mut(&id) {
                entry
                    .trace
                    .push(json!({"direction":"outbound","message":message}));
            }
            self.port
                .borrow()
                .as_ref()
                .ok_or("Native host is disconnected".into())
                .and_then(|p| {
                    call(p, "postMessage", &[js(&message)])
                        .map(|_| ())
                        .map_err(error)
                })
        } else if let Some(request) = self.http_requests.borrow().get(&id) {
            request.cancelled.set(true);
            if request.dispatched.get() {
                self.http_scope
                    .borrow()
                    .as_ref()
                    .ok_or("Console lifecycle disconnected")?
                    .socket
                    .send_with_str(&json!({"type":"stop","id":id}).to_string())
                    .map_err(error)
            } else {
                Ok(())
            }
        } else if let Some(socket) = self.sockets.borrow().get(&id) {
            socket.send_with_str("{\"type\":\"stop\"}").map_err(error)
        } else {
            Err("Stream is no longer running".into())
        }
    }
}
impl Drop for Transport {
    fn drop(&mut self) {
        self.shutdown();
    }
}
fn encode(value: &str) -> String {
    js_sys::encode_uri_component(value)
        .as_string()
        .unwrap_or_default()
}
fn http_route(m: &Value) -> (String, String, Option<Value>) {
    let service = m["service"].as_str().unwrap_or("");
    if service == "history" {
        return (
            "/api/console/history".into(),
            "POST".into(),
            Some(m.clone()),
        );
    }
    if m.get("history").is_some() {
        return ("/api/console".into(), "POST".into(), Some(m.clone()));
    }
    if service == "sessions" || m.get("sessionVariables").is_some() {
        return (
            "/api/console/session-context".into(),
            "POST".into(),
            Some(m.clone()),
        );
    }
    if m["type"] == "health" {
        return ("/api/health".into(), "GET".into(), None);
    }
    if m["type"] == "discover" {
        return (
            "/api/console/discover".into(),
            "POST".into(),
            Some(json!({"service":service,"params":m["params"]})),
        );
    }
    if service == "templates" {
        return ("/api/console/templates".into(), if m["op"]=="save" {"PUT"} else {"GET"}.into(),
            (m["op"]=="save").then(|| json!({"serviceName":m["serviceName"],"section":m["section"],"text":m["text"],"base":m["base"]})));
    }
    if service == "hints" {
        return (
            format!(
                "/api/console/hints/{}",
                encode(m["serviceName"].as_str().unwrap_or("rest"))
            ),
            if m["op"] == "save" { "PUT" } else { "GET" }.into(),
            if m["op"] == "save" {
                Some(json!({"text":m["text"]}))
            } else {
                None
            },
        );
    }
    if service == "vars" || service == "dictionary" {
        let name = encode(m["name"].as_str().unwrap_or(""));
        return match m["op"].as_str().unwrap_or("list") {
            "save" => (
                format!("/{service}/api/profiles/{name}"),
                "POST".into(),
                Some(json!({"variables":m["variables"]})),
            ),
            "delete" => (
                format!("/{service}/api/profiles/{name}"),
                "DELETE".into(),
                None,
            ),
            "rename" => (
                format!("/{service}/api/profiles/{name}/rename"),
                "POST".into(),
                Some(json!({"name":m["newName"]})),
            ),
            "reorder" => (
                format!("/{service}/api/order"),
                "POST".into(),
                Some(json!({"order":m["order"]})),
            ),
            _ => (format!("/{service}/api/profiles"), "GET".into(), None),
        };
    }
    (
        format!("/api/services/{service}/{}", descriptors::endpoint(service)),
        "POST".into(),
        Some(m["params"].clone()),
    )
}
async fn fetch(
    url: &str,
    method: &str,
    body: Option<Value>,
    scope: Option<(String, u32)>,
) -> Result<Value, Value> {
    let options = RequestInit::new();
    options.set_method(method);
    if let Some(body) = body {
        options.set_body(&body.to_string().into());
    }
    let headers = web_sys::Headers::new().map_err(|e| json!({"error":error(e)}))?;
    headers
        .set("Content-Type", "application/json")
        .map_err(|e| json!({"error":error(e)}))?;
    if let Some((scope, id)) = scope {
        headers
            .set("x-services-scope", &scope)
            .map_err(|e| json!({"error":error(e)}))?;
        headers
            .set("x-services-request", &id.to_string())
            .map_err(|e| json!({"error":error(e)}))?;
    }
    options.set_headers(&headers);
    let response = JsFuture::from(window().fetch_with_str_and_init(url, &options))
        .await
        .map_err(|e| json!({"error":format!("SERVER_OFFLINE: {}",error(e))}))?;
    let response: Response = response.dyn_into().map_err(|e| json!({"error":error(e)}))?;
    let promise: Promise = response.json().map_err(|e| json!({"error":error(e)}))?;
    let value = json(
        JsFuture::from(promise)
            .await
            .map_err(|e| json!({"error":format!("Invalid server response: {}",error(e))}))?,
    );
    if !response.ok() || value.get("error").is_some() {
        Err(if value.is_object() {
            value
        } else {
            json!({"error":format!("HTTP {}",response.status())})
        })
    } else {
        Ok(value)
    }
}
