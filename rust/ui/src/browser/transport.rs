use super::{call, error, get, js, json, window};
use serde_json::{Value, json};
use services_contracts::native_messaging::ResponseAssembler;
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::spawn_local;
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
pub struct Transport {
    session: RefCell<Option<super::sessions::Shared>>,
    in_flight: Cell<usize>,
    background: Cell<usize>,
    port: RefCell<Option<JsValue>>,
    next: Cell<u32>,
    pending: RefCell<BTreeMap<u32, Pending>>,
    disconnected: RefCell<Option<String>>,
    status: Callback<Option<String>>,
    native: bool,
    generation: Cell<u32>,
}
impl Transport {
    pub fn bind_session(&self, session: super::sessions::Shared) {
        *self.session.borrow_mut() = Some(session);
    }
    pub fn busy(&self) -> bool {
        // Background reads such as the Copilot model catalog are triggered by opening UI chrome,
        // not by user work, and must not block session changes or updates.
        self.in_flight.get() > self.background.get()
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
            background: Cell::new(0),
            port: RefCell::new(None),
            next: Cell::new(1),
            pending: RefCell::new(BTreeMap::new()),
            disconnected: RefCell::new(None),
            status,
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
            } else if crate::services::contains(service) || service == "agent" {
                if !message["params"].is_object() {
                    message["params"] = json!({});
                }
                message["params"]["__consoleVariables"] = variables;
            }
        }
        self.in_flight.set(self.in_flight.get() + 1);
        let is_background = message["service"] == "agent_models";
        if is_background {
            self.background.set(self.background.get() + 1);
        }
        let weak = Rc::downgrade(self);
        let save_variables = message["service"] == "vars" && message["op"] == "save";
        let completed = Cell::new(false);
        let callback = Callback::from(move |mut reply: Reply| {
            if let Some(this) = weak.upgrade() {
                if !matches!(reply, Reply::Data(_)) && !completed.replace(true) {
                    this.in_flight.set(this.in_flight.get().saturating_sub(1));
                    if is_background {
                        this.background.set(this.background.get().saturating_sub(1));
                    }
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
                let timeout_seconds =
                    if message["service"] == "updates" && message["op"] == "install" {
                        600
                    } else {
                        message["service"]
                            .as_str()
                            .and_then(crate::services::get)
                            .and_then(|service| service.console.timeout)
                            .unwrap_or(120)
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
