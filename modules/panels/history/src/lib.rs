//! The History panel: every finished run with a check is saved through `history.insert`,
//! its source, title and response with secrets masked — the values of sensitive variables
//! the run used, sensitive fields and headers of the request, and URL credentials. The
//! panel lists the runs of the current check, newest first, and shows the one picked
//! read-only.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use services_sdk::time::{date, time};
use services_sdk::{Cmd, DriverCall, Event, Module, Subscriptions, cmd, ui};

const MASK: &str = "[redacted]";

/// Names whose values never show in history.
pub fn sensitive_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [
        "password",
        "passwd",
        "pwd",
        "token",
        "secret",
        "credential",
        "api_key",
        "apikey",
        "api-key",
        "authorization",
        "cookie",
        "private_key",
    ]
    .iter()
    .any(|part| name.contains(part))
}

/// Masks known secrets, then URL credentials (`scheme://user:pass@`).
pub struct Redactor {
    secrets: Vec<String>,
}

const KEYS: [&str; 10] = [
    "authorization",
    "set-cookie",
    "cookie",
    "password",
    "passwd",
    "pwd",
    "token",
    "api_key",
    "api-key",
    "secret",
];

/// The `key: value` / `key=value` assignments of sensitive keys in `text` (case
/// insensitive; `apikey` too): their values, quoted or up to a line end, `,`, `}` or `&`.
fn assignments(text: &str) -> Vec<String> {
    let lower = text.to_ascii_lowercase();
    let mut keys: Vec<&str> = KEYS.to_vec();
    keys.push("apikey");
    let mut values = Vec::new();
    for key in keys {
        let mut from = 0;
        while let Some(found) = lower[from..].find(key) {
            let start = from + found + key.len();
            from = start;
            let rest = &text[start..];
            let skipped = rest
                .find(|c: char| !(c.is_whitespace() || c == '"' || c == '\''))
                .unwrap_or(rest.len());
            let rest = &rest[skipped..];
            let Some(rest) = rest.strip_prefix([':', '=']) else {
                continue;
            };
            let rest = rest.trim_start();
            let value = match rest.chars().next() {
                Some(quote @ ('"' | '\'')) => rest[1..].split(quote).next().unwrap_or(""),
                _ => rest.split(['\r', '\n', ',', '}', '&']).next().unwrap_or(""),
            };
            values.push(value.trim().to_owned());
        }
    }
    values
}

/// The `user:pass` of each `scheme://user:pass@` in `text`.
fn credentials(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(at) = text[from..].find("://") {
        let start = from + at + 3;
        from = start;
        let scheme_ok = text[..start - 3]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric() || "+.-".contains(c));
        let rest = &text[start..];
        let end = rest
            .find(|c: char| c.is_whitespace() || c == '/' || c == '@')
            .unwrap_or(rest.len());
        if scheme_ok && end > 0 && rest[end..].starts_with('@') {
            found.push(rest[..end].to_owned());
        }
    }
    found
}

impl Redactor {
    /// Secrets of a run: `secrets` (values of sensitive variables), the values of sensitive
    /// keys and of sensitive assignments in the `request` and the `source`, URL credentials.
    pub fn new(request: &Value, source: &str, mut secrets: Vec<String>) -> Self {
        fn collect(value: &Value, secrets: &mut Vec<String>, leaves: &mut Vec<String>) {
            match value {
                Value::Object(fields) => {
                    for (key, value) in fields {
                        if sensitive_name(key)
                            && let Some(text) = value.as_str()
                        {
                            secrets.push(text.to_owned());
                        }
                        collect(value, secrets, leaves);
                    }
                }
                Value::Array(values) => {
                    // A `[name, value]` header pair.
                    if let [Value::String(name), Value::String(value)] = values.as_slice()
                        && sensitive_name(name)
                    {
                        secrets.push(value.clone());
                    }
                    for value in values {
                        collect(value, secrets, leaves);
                    }
                }
                Value::String(text) => {
                    if let Ok(json) = serde_json::from_str::<Value>(text)
                        && !json.is_string()
                    {
                        collect(&json, secrets, leaves);
                    }
                    leaves.push(text.clone());
                }
                _ => {}
            }
        }
        let mut leaves = vec![source.to_owned()];
        collect(request, &mut secrets, &mut leaves);
        for leaf in &leaves {
            for value in assignments(leaf).into_iter().chain(credentials(leaf)) {
                if let Some(token) = value.strip_prefix("Bearer ") {
                    secrets.push(token.to_owned());
                }
                if let Some((_, password)) = value.split_once(':') {
                    secrets.push(password.to_owned());
                }
                secrets.push(value);
            }
        }
        let mut parts = Vec::new();
        for secret in &secrets {
            if let Some(token) = secret.strip_prefix("Bearer ") {
                parts.push(token.to_owned());
            }
            for cookie in secret.split(';') {
                if let Some((_, value)) = cookie.split_once('=') {
                    parts.push(value.trim().to_owned());
                }
            }
        }
        secrets.extend(parts);
        secrets.retain(|value| !value.is_empty());
        secrets.sort_by_key(|value| std::cmp::Reverse(value.len()));
        secrets.dedup();
        Self { secrets }
    }

    pub fn text(&self, text: &str) -> String {
        let mut text = text.to_owned();
        for secret in &self.secrets {
            text = text.replace(secret, MASK);
        }
        for credential in credentials(&text) {
            text = text.replace(&format!("://{credential}@"), &format!("://{MASK}@"));
        }
        text
    }
}

const DOCUMENT: &str = "document";
const LIST: &str = "list";
const MORE: &str = "more";
const SNAPSHOT: &str = "snapshot";
/// Runs read at a time.
const PAGE: u32 = 25;

#[derive(Serialize, Deserialize)]
pub enum Msg {
    Select(u64),
    /// The list scrolled to `from:to`.
    Range,
    More,
}

/// A run of the current check as the list shows it.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub id: u64,
    pub started_at: f64,
    pub ended_at: f64,
    pub duration_ms: f64,
    pub status: String,
    pub title: String,
}

#[derive(Default)]
pub struct History {
    /// Minutes the local time is ahead of UTC (`Init` settings `timezoneOffset`).
    offset: f64,
    service: String,
    check: String,
    entries: Vec<Entry>,
    next_before: Option<u64>,
    total: u64,
    bytes: u64,
    selected: Option<u64>,
    /// The selected run's text as read so far, and what the viewer shows once it is whole.
    text: String,
    shown: Option<String>,
    /// The read the replies belong to; older ones are dropped.
    epoch: u64,
    error: Option<String>,
}

impl History {
    fn list(&mut self, more: bool) -> Vec<Cmd> {
        if self.check.is_empty() {
            return vec![self.status()];
        }
        self.epoch += 1;
        let mut query = json!({"checkId": self.check, "limit": PAGE});
        let tag = if more {
            query["before"] = json!(self.next_before);
            MORE
        } else {
            LIST
        };
        vec![
            cmd::history_search(format!("{tag}:{}", self.epoch), &query),
            cmd::status("History", "busy"),
        ]
    }

    fn read(&mut self, id: u64, offset: u64) -> Cmd {
        cmd::history_search(
            format!("{SNAPSHOT}:{}:{id}", self.epoch),
            &json!({"op": "get", "entryId": id, "offset": offset}),
        )
    }

    fn status(&self) -> Cmd {
        match &self.error {
            Some(error) => cmd::status(&format!("ERROR · {error}"), "error"),
            None => cmd::status(
                &format!(
                    "{} calls · {:.1} / 100 MiB",
                    self.total,
                    self.bytes as f64 / 1_048_576.0
                ),
                "idle",
            ),
        }
    }

    fn page(&mut self, page: &Value, more: bool) -> Result<(), String> {
        let entries: Vec<Entry> = serde_json::from_value(page["entries"].clone())
            .map_err(|e| format!("Invalid History page: {e}"))?;
        if !more {
            self.entries.clear();
        }
        self.entries.extend(entries);
        self.next_before = page["nextBefore"].as_u64();
        self.total = page["total"].as_u64().unwrap_or(0);
        self.bytes = page["bytes"].as_u64().unwrap_or(0);
        if self
            .selected
            .is_some_and(|id| !self.entries.iter().any(|entry| entry.id == id))
        {
            self.selected = None;
            self.shown = None;
        }
        Ok(())
    }

    /// A chunk of the selected run's text; the next one is read until the text is whole.
    fn chunk(&mut self, id: u64, chunk: &Value) -> Result<Option<Cmd>, String> {
        if chunk["id"].as_u64() != Some(id) {
            return Err("History snapshot identity mismatch".into());
        }
        let text = chunk["text"]
            .as_str()
            .ok_or("History snapshot has no text")?;
        let next = match &chunk["nextOffset"] {
            Value::Null => None,
            offset => Some(offset.as_u64().ok_or("Invalid History offset")?),
        };
        if next.is_some_and(|offset| {
            text.is_empty() || offset != (self.text.len() + text.len()) as u64
        }) {
            return Err("Invalid History offset".into());
        }
        self.text.push_str(text);
        if let Some(offset) = next {
            return Ok(Some(self.read(id, offset)));
        }
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.id == id)
            .ok_or("History execution metadata is missing")?;
        self.shown = Some(document(&self.text, &time(entry.ended_at, self.offset)));
        Ok(None)
    }
}

/// The run as the viewer shows it: the request, then `#responce <time> · <outcome>` and the
/// response.
pub fn document(snapshot: &str, time: &str) -> String {
    let headers = [
        ("\n\n#response success\n", "OK"),
        ("\n\n#response error\n", "ERROR"),
        ("\n\n#response stopped\n", "Stopped"),
        ("\n\n#responce OK\n", "OK"),
        ("\n\n#responce WARNING\n", "WARNING"),
        ("\n\n#responce ERROR\n", "ERROR"),
        ("\n\n#responce NOTICE\n", "NOTICE"),
        ("\n\n#responce Stopped\n", "Stopped"),
    ];
    let Some((offset, marker, outcome)) = headers
        .into_iter()
        .filter_map(|(marker, outcome)| {
            snapshot
                .find(marker)
                .map(|offset| (offset, marker, outcome))
        })
        .min_by_key(|(offset, _, _)| *offset)
    else {
        return snapshot.into();
    };
    let request = snapshot[..offset].trim_end_matches('\n');
    let body = &snapshot[offset + marker.len()..];
    format!("{request}\n#responce {time} · {outcome}\n{body}")
}

/// A duration like `850ms`, `1.2s`, `1.5min`.
fn duration(ms: f64) -> String {
    if ms < 1000.0 {
        return format!("{ms:.0}ms");
    }
    let (value, unit) = if ms < 60_000.0 {
        (ms / 1000.0, "s")
    } else {
        (ms / 60_000.0, "min")
    };
    let value = format!("{value:.1}");
    format!(
        "{}{unit}",
        value.trim_end_matches('0').trim_end_matches('.')
    )
}

impl Module for History {
    type Msg = Msg;

    fn subscriptions(&self) -> Subscriptions {
        Subscriptions {
            document: true,
            workspace: true,
            ..Default::default()
        }
    }

    fn update(&mut self, event: Event<Msg>) -> Vec<Cmd> {
        match event {
            Event::Init { settings } => {
                self.offset = settings["timezoneOffset"].as_f64().unwrap_or(0.0);
                Vec::new()
            }
            Event::Shown | Event::Document(_) | Event::Workspace | Event::Modules => {
                vec![cmd::document_read(DOCUMENT)]
            }
            Event::Reply { tag, result } => {
                let mut parts = tag.splitn(3, ':');
                let (kind, epoch) = (parts.next().unwrap_or(""), parts.next());
                let current = epoch.and_then(|epoch| epoch.parse().ok()) == Some(self.epoch);
                match (kind, result) {
                    (DOCUMENT, Ok(document)) => {
                        self.service = document["service"].as_str().unwrap_or_default().into();
                        let check = document["checkId"].as_str().unwrap_or_default();
                        if check != self.check {
                            self.check = check.into();
                            self.entries.clear();
                            self.selected = None;
                            self.shown = None;
                            self.text.clear();
                            self.total = 0;
                            self.next_before = None;
                        }
                        self.list(false)
                    }
                    (LIST | MORE, Ok(page)) if current => {
                        self.error = self.page(&page, kind == MORE).err();
                        vec![self.status()]
                    }
                    (SNAPSHOT, Ok(chunk)) if current => {
                        let id = parts.next().and_then(|id| id.parse().ok());
                        if id != self.selected || id.is_none() {
                            return Vec::new();
                        }
                        match self.chunk(id.unwrap_or_default(), &chunk) {
                            Ok(next) => {
                                self.error = None;
                                next.into_iter().chain([self.status()]).collect()
                            }
                            Err(error) => {
                                self.error = Some(error);
                                vec![self.status()]
                            }
                        }
                    }
                    (LIST | MORE | SNAPSHOT, Err(failure)) if current => {
                        self.error = Some(failure.message);
                        vec![self.status()]
                    }
                    // A run was saved (`history.insert` under its check): the list reads it.
                    (check, Ok(_))
                        if !check.is_empty() && check == self.check && epoch.is_none() =>
                    {
                        self.list(false)
                    }
                    (_, Err(failure)) if epoch.is_none() && kind != DOCUMENT => {
                        self.error = Some(failure.message);
                        vec![self.status()]
                    }
                    _ => Vec::new(),
                }
            }
            Event::Ui {
                msg: Msg::Select(id),
                ..
            } => {
                self.text.clear();
                self.shown = None;
                self.epoch += 1;
                if self.selected == Some(id) {
                    self.selected = None;
                    vec![self.status()]
                } else {
                    self.selected = Some(id);
                    vec![self.read(id, 0), cmd::status("History snapshot", "busy")]
                }
            }
            Event::Ui { msg: Msg::More, .. } if self.next_before.is_some() => self.list(true),
            Event::Ui {
                msg: Msg::Range,
                value: Some(range),
            } => {
                let to: usize = range
                    .split_once(':')
                    .and_then(|(_, to)| to.parse().ok())
                    .unwrap_or(0);
                if to > self.entries.len() && self.next_before.is_some() {
                    self.list(true)
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        }
    }

    fn view(&self) -> Option<ui::Node> {
        let mut body = Vec::new();
        if self.check.is_empty() {
            body.push(ui::text("Select a check to see its history.").variant("muted"));
        } else if self.entries.is_empty() {
            body.push(ui::text("No executions recorded for this check.").variant("muted"));
        }
        let rows = self
            .entries
            .iter()
            .map(|entry| {
                let label = format!(
                    "{} · {} · {} · {}",
                    date(entry.started_at, self.offset),
                    entry.status,
                    duration(entry.duration_ms),
                    entry.title
                );
                let button = ui::button(label, &Msg::Select(entry.id))
                    .key(format!("history-entry-{}", entry.id));
                if self.selected == Some(entry.id) {
                    button.variant("primary")
                } else {
                    button
                }
            })
            .collect();
        let total = self.total.max(self.entries.len() as u64) as u32;
        body.push(ui::list(total, 0, rows, &Msg::Range).key("history-entries"));
        if self.next_before.is_some() {
            body.push(ui::button("Older executions", &Msg::More).key("history-more"));
        }
        if let Some(text) = &self.shown {
            body.push(
                ui::viewer(format!("history/{}", self.service), text.clone()).key("history-editor"),
            );
        }
        Some(ui::panel("History", body, Vec::new()).key("history-panel"))
    }

    /// `response` is the run record (`Modules::finished`); a run without a check is not
    /// kept, nor one whose source did not resolve (it may hold secrets unmasked).
    fn after_response(&self, call: &DriverCall, response: &Value) -> Vec<Cmd> {
        let (Some(source), Some(check), Some(session)) = (
            response["source"].as_str(),
            response["checkId"].as_str(),
            response["sessionId"].as_str(),
        ) else {
            return Vec::new();
        };
        let secrets = response["secrets"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        let request = if call.driver.is_empty() {
            Value::Null
        } else {
            call.request()
        };
        let redactor = Redactor::new(&request, source, secrets);
        let text = format!(
            "{}\n\n#responce {}\n{}",
            redactor.text(source),
            response["outcome"].as_str().unwrap_or(""),
            response["text"].as_str().unwrap_or("")
        );
        vec![cmd::history_insert(
            check,
            &json!({
                "checkId": check,
                "sessionId": session,
                "title": redactor.text(response["title"].as_str().unwrap_or("")),
                "module": response["service"],
                "started": response["started"],
                "ended": response["ended"],
                "status": response["status"],
                "text": text,
                "defer": response["defer"] == true,
            }),
        )]
    }
}

services_sdk::export!(History);
