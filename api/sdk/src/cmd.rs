//! Commands a module asks the Kernel to run. Each carries the module's `tag`; the answer
//! comes back as `Event::Reply` with that tag (a stream: several replies, then `End`).

use crate::wire::{Cmd, DriverCall, json};
use serde::Serialize;
use serde_json::{Value, json};

/// Every command the Kernel knows; anything else is answered with an error.
pub const NAMES: &[&str] = &[
    "driver.call",
    "driver.stream",
    "driver.cancel",
    "kv.get",
    "kv.put",
    "kv.delete",
    "kv.watch",
    "history.insert",
    "history.search",
    "os.env",
    "clipboard.read",
    "clipboard.write",
    "sources.read",
    "document.read",
    "document.insert",
    "document.replace",
    "document.select",
    "document.run",
    "workspace.snapshot",
    "workspace.restore",
    "page.eval",
    "agent.turn",
    "agent.stop",
    "agent.tools",
    "modules.install",
    "modules.remove",
    "modules.update",
    "app.update",
    "app.reload",
    "lock.acquire",
    "lock.release",
    "timer.after",
    "status.set",
    "notify",
    "log",
    "diagnostics",
];

impl Cmd {
    pub fn new(tag: impl Into<String>, name: &str, args: &impl Serialize) -> Self {
        Self {
            tag: tag.into(),
            name: name.into(),
            args: json(args),
        }
    }
}

/// Runs a driver call; a streaming call answers with several replies and an end.
pub fn driver(tag: impl Into<String>, call: &DriverCall) -> Cmd {
    let name = if call.stream {
        "driver.stream"
    } else {
        "driver.call"
    };
    Cmd::new(
        tag,
        name,
        &json!({"driver": call.driver, "request": call.request()}),
    )
}

/// Cancels the driver call or stream started with `target`.
pub fn cancel(target: &str) -> Cmd {
    Cmd::new(target, "driver.cancel", &json!({}))
}

pub fn kv_get(tag: impl Into<String>, key: &str) -> Cmd {
    Cmd::new(tag, "kv.get", &json!({"key": key}))
}

/// Stores a value under `key` in the module's own kv; `session` keys go into session snapshots.
pub fn kv_put(tag: impl Into<String>, key: &str, value: &Value, session: bool) -> Cmd {
    Cmd::new(
        tag,
        "kv.put",
        &json!({"key": key, "value": value, "session": session}),
    )
}

pub fn kv_delete(tag: impl Into<String>, key: &str) -> Cmd {
    Cmd::new(tag, "kv.delete", &json!({"key": key}))
}

/// `Event::Storage(key)` arrives when another place changes the key.
pub fn kv_watch(key: &str) -> Cmd {
    Cmd::new("", "kv.watch", &json!({"key": key}))
}

/// Replies once after `ms` milliseconds.
pub fn timer(tag: impl Into<String>, ms: u32) -> Cmd {
    Cmd::new(tag, "timer.after", &json!({"ms": ms}))
}

/// Replies when the lock is held; shared by every App window of the extension.
pub fn lock(tag: impl Into<String>, name: &str) -> Cmd {
    Cmd::new(tag, "lock.acquire", &json!({"name": name}))
}

pub fn unlock(name: &str) -> Cmd {
    Cmd::new("", "lock.release", &json!({"name": name}))
}

/// The module's own status line and indicator (`idle`, `busy`, `ok`, `warning`, `error`).
pub fn status(text: &str, indicator: &str) -> Cmd {
    Cmd::new(
        "",
        "status.set",
        &json!({"text": text, "indicator": indicator}),
    )
}

pub fn notify(text: &str, level: &str) -> Cmd {
    Cmd::new("", "notify", &json!({"text": text, "level": level}))
}

/// A line in the module's Debug log.
pub fn log(level: &str, text: &str) -> Cmd {
    Cmd::new("", "log", &json!({"level": level, "text": text}))
}
