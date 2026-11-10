//! The browser's [`Platform`]: the Host port, window timers, Web Locks, the session store
//! and `localStorage`, the clipboard and the inspected page.

use super::{Event, Kernel, Local, Platform, Started};
use crate::Callback;
use crate::host::{Reply, Transport};
use crate::js::{call, error, get, window};
use crate::locks::Locks;
use crate::workspace::store::Shared;
use serde_json::Value;
use services_sdk::{Cmd, Failure};
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::{JsCast, closure::Closure};
use wasm_bindgen_futures::spawn_local;

struct Browser {
    transport: Rc<Transport>,
    sessions: Shared,
    locks: RefCell<Locks>,
}

impl Kernel {
    /// The Kernel of an App window on the session store `sessions`.
    pub fn browser(sessions: Shared, on_event: Callback<Event>) -> Self {
        Self::new(on_event, |post| {
            let connection = post.clone();
            let transport = Transport::new(
                Callback::from(move |status| connection.emit(Event::Connection(status))),
                Callback::from(move |_| post.emit(Event::Busy)),
            );
            transport.bind_session(sessions.clone());
            Rc::new(Browser {
                transport,
                sessions,
                locks: RefCell::default(),
            })
        })
    }
}

impl Platform for Browser {
    fn request(&self, message: Value, stream: bool, reply: Callback<Reply>) -> u32 {
        self.transport.request(message, stream, reply)
    }

    fn stop(&self, id: u32) -> Result<(), String> {
        self.transport.stop(id)
    }

    fn stop_all(&self) -> Result<(), String> {
        self.transport.stop_all()
    }

    fn agent_reply(&self, id: u32, call: &str, result: Value) -> Result<(), String> {
        self.transport.agent_reply(id, call, result)
    }

    fn native(&self) -> bool {
        self.transport.is_native()
    }

    fn connect(&self) {
        self.transport.connect();
    }

    fn busy(&self) -> Option<String> {
        self.transport.busy().then(|| self.transport.busy_reason())
    }

    fn now(&self) -> f64 {
        js_sys::Date::now()
    }

    fn time(&self) -> String {
        js_sys::Date::new_0()
            .to_locale_time_string("en-US")
            .as_string()
            .unwrap_or_default()
    }

    fn timeout(&self, ms: u32, done: Callback<()>) -> Result<i32, String> {
        let callback = Closure::once_into_js(move || done.emit(()));
        window()
            .set_timeout_with_callback_and_timeout_and_arguments_0(
                callback.unchecked_ref(),
                ms.min(i32::MAX as u32) as i32,
            )
            .map_err(error)
    }

    fn clear(&self, timer: i32) {
        window().clear_timeout_with_handle(timer);
    }

    fn lock(&self, module: &str, name: &str, granted: Callback<()>) -> Result<(), String> {
        self.locks.borrow_mut().acquire(module, name, granted)
    }

    fn unlock(&self, module: &str, name: &str) {
        self.locks
            .borrow_mut()
            .release(module.to_owned(), name.to_owned());
    }

    fn modules(&self, list: &Value) -> Result<bool, String> {
        crate::modules::loader::accept(list)
    }

    fn load(&self, first: &str, health: bool, loaded: Callback<Started>) {
        crate::modules::loader::prepare(&self.transport, first, health, loaded);
    }

    fn agent_workspace_id(&self) -> String {
        crate::storage::agent_workspace_id()
    }

    fn agent_options(&self, options: &Value) -> Result<Value, Failure> {
        crate::storage::agent_options(options)
    }

    fn local(
        &self,
        module: &str,
        cmd: &Cmd,
        done: Callback<Result<Value, Failure>>,
    ) -> Option<Local> {
        let args = cmd.args();
        Some(match cmd.name.as_str() {
            "kv.get" | "kv.put" | "kv.delete" => {
                let result = crate::storage::kv(&self.sessions, module, &cmd.name, &args);
                // A session write answers once the saved session has it: a write over another
                // window's change fails there.
                if cmd.name != "kv.get"
                    && let Ok(Value::Bool(true)) = result
                    && !cmd.tag.is_empty()
                {
                    let shared = self.sessions.clone();
                    spawn_local(async move {
                        let saved = crate::workspace::store::Store::flush(&shared).await;
                        let saved = saved.and_then(|()| match shared.borrow().save_error() {
                            Some(error) => Err(error),
                            None => Ok(()),
                        });
                        done.emit(saved.map(|()| Value::Null).map_err(Failure::new));
                    });
                    return Some(Local::Later);
                }
                Local::Now(if cmd.name == "kv.get" {
                    result
                } else {
                    result.map(|_| Value::Null)
                })
            }
            "clipboard.write" => {
                let text = args["text"].as_str().unwrap_or_default().to_owned();
                let clipboard = get(window().navigator().as_ref(), "clipboard");
                let _ = call(&clipboard, "writeText", &[text.into()]);
                Local::Now(Ok(Value::Null))
            }
            "page.eval" | "page.network" => {
                let expression = args["expression"].as_str().unwrap_or_default().to_owned();
                let network = cmd.name == "page.network";
                spawn_local(async move {
                    let result = if network {
                        crate::page::network().await
                    } else {
                        crate::page::eval(&expression).await
                    };
                    done.emit(result.map_err(Failure::new));
                });
                Local::Later
            }
            _ => return None,
        })
    }
}
