//! `chrome.runtime.connectNative`: the Host's port in the browser.

use super::Reply;
use crate::Callback;
use crate::js::{call, error, get, js, json, window};
use serde_json::{Value, json};
use services_contracts::native_messaging::ResponseAssembler;
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};

struct Pending {
    callback: Callback<Reply>,
    trace: Vec<Value>,
    stream: bool,
    chunks: ResponseAssembler,
    label: String,
    started: f64,
}
pub struct Transport {
    session: RefCell<Option<crate::workspace::store::Shared>>,
    in_flight: Cell<usize>,
    background: Cell<usize>,
    port: RefCell<Option<JsValue>>,
    next: Cell<u32>,
    pending: RefCell<BTreeMap<u32, Pending>>,
    disconnected: RefCell<Option<String>>,
    status: Callback<Option<String>>,
    onbusy: Callback<bool>,
    native: bool,
    generation: Cell<u32>,
}
impl Transport {
    pub fn bind_session(&self, session: crate::workspace::store::Shared) {
        *self.session.borrow_mut() = Some(session);
    }
    pub fn busy(&self) -> bool {
        // Background reads such as the Copilot model catalog are triggered by opening UI chrome,
        // not by user work, and must not block session changes or updates.
        self.in_flight.get() > self.background.get()
    }
    /// Reports a change of the requests the link is busy with, so `busy_reason()` shown by the
    /// Shell stays current; requests while idle report nothing.
    fn notify_busy(&self, was_busy: bool) {
        let busy = self.busy();
        if busy || was_busy {
            self.onbusy.emit(busy);
        }
    }
    /// Why `busy()` holds: the request counters and the native requests still waiting.
    pub fn busy_reason(&self) -> String {
        let now = js_sys::Date::now();
        let waiting = self
            .pending
            .borrow()
            .values()
            .map(|p| format!("{} {:.1}s", p.label, (now - p.started) / 1000.0))
            .collect::<Vec<_>>();
        format!(
            "native in flight {}, background {} [{}]",
            self.in_flight.get(),
            self.background.get(),
            waiting.join(", ")
        )
    }
    pub fn is_native(&self) -> bool {
        self.native
    }
    pub fn new(status: Callback<Option<String>>, onbusy: Callback<bool>) -> Rc<Self> {
        let chrome = get(&js_sys::global(), "chrome");
        let runtime = get(&chrome, "runtime");
        let native = get(&runtime, "connectNative").is_function();
        let this = Rc::new(Self {
            session: RefCell::new(None),
            in_flight: Cell::new(0),
            background: Cell::new(0),
            port: RefCell::new(None),
            next: Cell::new(1),
            pending: RefCell::new(BTreeMap::new()),
            disconnected: RefCell::new(None),
            status,
            onbusy,
            native,
            generation: Cell::new(0),
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
        // Requests of the previous host process get no answer from the new one.
        let pending = std::mem::take(&mut *self.pending.borrow_mut());
        for (_, entry) in pending {
            entry.callback.emit(Reply::Error(
                json!({"error":"The native host was restarted","transportDebug":entry.trace}),
            ));
        }
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
            if service == "agent" {
                // The Agent masks the values of the session's sensitive variables (Variables);
                // the Host adds the Dictionary's from the Local source.
                let modules = session.borrow().library.snapshot().modules.clone();
                let stored = |module: &str, key: &str| {
                    modules
                        .get(module)
                        .and_then(|keys| keys.get(key))
                        .cloned()
                        .or_else(|| {
                            web_sys::window()
                                .and_then(|window| window.local_storage().ok().flatten())
                                .and_then(|storage| {
                                    storage.get_item(&format!("module:{module}:{key}")).ok()
                                })
                                .flatten()
                                .and_then(|text| serde_json::from_str(&text).ok())
                        })
                        .unwrap_or_else(|| json!({}))
                };
                let variables = json!({
                    "variables": stored("variables", "var"),
                });
                if !message["params"].is_object() {
                    message["params"] = json!({});
                }
                message["params"]["__consoleVariables"] = variables;
            }
        }
        let was_busy = self.busy();
        self.in_flight.set(self.in_flight.get() + 1);
        let is_background = message["service"] == "agent_models";
        if is_background {
            self.background.set(self.background.get() + 1);
        }
        self.notify_busy(was_busy);
        let weak = Rc::downgrade(self);
        let completed = Cell::new(false);
        let callback = Callback::from(move |reply: Reply| {
            if let Some(this) = weak.upgrade()
                && !matches!(reply, Reply::Data(_))
                && !completed.replace(true)
            {
                let was_busy = this.busy();
                this.in_flight.set(this.in_flight.get().saturating_sub(1));
                if is_background {
                    this.background.set(this.background.get().saturating_sub(1));
                }
                this.notify_busy(was_busy);
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
                    label: [&message["service"], &message["op"], &message["module"]]
                        .iter()
                        .filter_map(|part| part.as_str())
                        .collect::<Vec<_>>()
                        .join("/"),
                    started: js_sys::Date::now(),
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
                let timeout_seconds =
                    if message["service"] == "updates" && message["op"] == "install" {
                        600
                    } else {
                        // A driver call's own deadline, with a margin for the Host to report it.
                        message["request"]["timeoutMs"]
                            .as_u64()
                            .map_or(120, |ms| (ms.div_ceil(1000) + 5).max(120))
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
        } else {
            callback.emit(Reply::Error(json!({
                "error":"SERVER_OFFLINE: The native host is unavailable."
            })));
        }
        id
    }
    pub fn stop_all(&self) -> Result<(), String> {
        let ids: Vec<_> = self.pending.borrow().keys().copied().collect();
        let mut errors = Vec::new();
        for id in ids {
            if let Err(error) = self.stop(id) {
                errors.push(error);
            }
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
        if let Some(port) = self.port.borrow_mut().take()
            && let Err(error) = call(&port, "disconnect", &[])
        {
            web_sys::console::error_1(&error);
        }
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
            Err("The native host is unavailable.".into())
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
