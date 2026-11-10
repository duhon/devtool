//! Apache ActiveMQ Artemis over its Jolokia management API: `#action` commands run as one or
//! more Jolokia calls through the Host `http` driver, their rows shown as a table. Discovery
//! reads the broker's queue and address names for `#action` completion.

pub mod action;
pub mod jolokia;
pub mod language;

use jolokia::{Error, Reply, Request};
use serde_json::{Map, Value, json};
use services_sdk::lang::headers;
use services_sdk::lang::table::{self, Column, Format, Kind};
use services_sdk::{
    Check, Cmd, Completions, Event, Failure, Line, Module, Rendered, Subscriptions, cmd, run,
};
use std::collections::BTreeMap;

/// Commands that send `#payload`.
const WITH_BODY: [&str; 3] = ["SEND", "PUBLISH", "JOLOKIA"];
/// `#options` names and the params they fill.
const OPTIONS: [(&str, &str); 3] = [
    ("persistent", "persistent"),
    ("columns-type", "columnsType"),
    ("columns-size", "columnsSize"),
];
/// Options of the earlier format and where their meaning moved.
const MOVED: [(&str, &str); 5] = [
    ("count", "LIMIT n in #action"),
    ("filter", "QUEUES text (names containing it) in #action"),
    ("selector", "WHERE in #action"),
    ("durable", "Persistent"),
    ("jolokia-path", "the #target path"),
];
const DISCOVER: &str = "discover:";
/// Connections whose names are kept, the most recent last.
const DISCOVERED: usize = 16;

/// The parts of a check a run reads, to resolve their variables at once; how it shows.
struct Plan {
    sections: Value,
    format: Option<Format>,
}

/// The check checked as written: one broker URL, a known command, a body only where the
/// command sends one and known options.
fn plan(check: &Check) -> Result<Plan, String> {
    let endpoint = check.text_of("target");
    let words: Vec<_> = endpoint.split_whitespace().collect();
    if let Some(command) = words
        .first()
        .map(|word| word.to_ascii_uppercase())
        .filter(|word| action::COMMANDS.iter().any(|c| c.name() == word))
    {
        return Err(format!(
            "#target is only the broker URL now; move {command} to #action"
        ));
    }
    if words.len() != 1 {
        return Err("#target must be the broker's http:// URL".into());
    }
    let action = check.text_of("action");
    let command = action
        .split_whitespace()
        .next()
        .unwrap_or("QUEUES")
        .to_ascii_uppercase();
    if !action::COMMANDS.iter().any(|c| c.name() == command) {
        let names: Vec<_> = action::COMMANDS.iter().map(|c| c.name()).collect();
        return Err(format!(
            "#action starts with a command: {}",
            names.join(", ")
        ));
    }
    let body = check.text_of("payload").trim();
    if !body.is_empty() && !WITH_BODY.contains(&command.as_str()) {
        return Err(format!("{command} takes no #payload"));
    }
    let mut options = Map::new();
    let mut format = None;
    for (name, value) in headers::pairs(check.text_of("options")) {
        let name = name.to_ascii_lowercase();
        if name == "format" {
            format = Some(Format::parse(&value)?);
            continue;
        }
        if matches!(name.as_str(), "repeat" | "repeat-until") {
            continue;
        }
        let Some(key) = OPTIONS
            .iter()
            .find(|(option, _)| *option == name)
            .map(|(_, key)| *key)
        else {
            return Err(match MOVED.iter().find(|(option, _)| *option == name) {
                Some((_, place)) => format!("Option {name:?} is now {place}"),
                None => format!(
                    "Unknown Artemis option {name:?}; use Persistent, Columns-Type, Columns-Size or Format"
                ),
            });
        };
        if !value.is_empty() {
            options.insert(key.into(), json!(value));
        }
    }
    Ok(Plan {
        sections: json!({
            "url": words[0],
            "action": action,
            "headers": header_map(check),
            "payload": body,
            "options": options,
        }),
        format,
    })
}

fn header_map(check: &Check) -> Map<String, Value> {
    headers::pairs(check.text_of("headers"))
        .into_iter()
        .map(|(name, value)| (name, Value::String(value)))
        .collect()
}

/// A request from resolved sections; the options are not resolved.
fn request(mut resolved: Value, options: &Value) -> Result<Request, Error> {
    for (key, value) in options.as_object().into_iter().flatten() {
        resolved[key] = value.clone();
    }
    Request::parse(&resolved)
}

/// The Jolokia calls of a command or discovery: the replies so far and the call in flight.
#[derive(Debug)]
struct Calls {
    request: Request,
    replies: Vec<Reply>,
    started: f64,
    sent: f64,
}

impl Calls {
    fn new(request: Request) -> Self {
        let now = services_sdk::now();
        Self {
            request,
            replies: vec![],
            started: now,
            sent: now,
        }
    }

    fn answered(&mut self, result: Result<Value, Failure>) {
        self.replies.push(Reply {
            sent: self.sent,
            answered: services_sdk::now(),
            result,
        });
    }

    /// The next call's command, tagged `{prefix}:call:{n}`.
    fn call(&mut self, prefix: &str, next: &services_sdk::http::Request) -> Cmd {
        self.sent = services_sdk::now();
        cmd::driver(
            format!("{prefix}:call:{}", self.replies.len()),
            &next.call(),
        )
    }
}

#[derive(Debug)]
enum Stage {
    Resolving {
        options: Value,
        format: Option<Format>,
    },
    Calling {
        calls: Box<Calls>,
        format: Option<Format>,
    },
}

/// A command's grid as text: the columns with their kinds and the `Columns-*` overrides.
fn render(request: &Request, grid: &Value, format: Option<Format>) -> Result<Rendered, String> {
    let names: Vec<&str> = grid["columns"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let mut columns: Vec<Column> = names
        .iter()
        .map(|name| {
            let kind = grid["columnTypes"][*name]
                .as_str()
                .and_then(Kind::parse)
                .unwrap_or_default();
            Column::new(*name, kind)
        })
        .collect();
    request.columns.apply(&mut columns)?;
    let rows: Vec<Vec<Value>> = grid["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|row| names.iter().map(|name| row[*name].clone()).collect())
        .collect();
    Ok(Rendered {
        text: table::show(&columns, &rows, format, false),
        columns: table::types(&columns),
        language: if format == Some(Format::Json) {
            "json".into()
        } else {
            "artemis".into()
        },
        outcome: if grid["warning"].is_string() {
            "WARNING".into()
        } else {
            "OK".into()
        },
    })
}

fn rows(count: usize) -> String {
    format!("{count} {}", if count == 1 { "row" } else { "rows" })
}

/// A discovery: the connection it is for and its calls once resolved.
#[derive(Debug)]
struct Discovery {
    id: u64,
    key: String,
    calls: Option<Calls>,
}

#[derive(Debug, Default)]
pub struct Artemis {
    runs: BTreeMap<String, Stage>,
    /// `(connection, names)` of discovered brokers, the most recent last.
    discovered: Vec<(String, Value)>,
    /// Connections discovered or tried, so hints do not retry them.
    attempted: Vec<String>,
    discovery: Option<Discovery>,
    discoveries: u64,
}

/// The connection of a check as written: its endpoint and headers.
fn connection(check: &Check) -> String {
    format!(
        "{}\n{}",
        check.text_of("target").trim(),
        check.text_of("headers").trim()
    )
}

/// An endpoint discovery can use: one `http://` or `https://` URL with a host.
fn discoverable(check: &Check) -> bool {
    let endpoint = check.text_of("target").trim();
    let rest = ["http://", "https://"].iter().find_map(|scheme| {
        endpoint
            .get(..scheme.len())
            .filter(|head| head.eq_ignore_ascii_case(scheme))
            .map(|_| &endpoint[scheme.len()..])
    });
    rest.is_some_and(|rest| !rest.is_empty() && !rest.starts_with(['/', ':']))
        && !endpoint.contains(char::is_whitespace)
}

impl Artemis {
    /// The names discovered for the check's connection; `null` when not discovered.
    pub fn names(&self, check: &Check) -> Value {
        let key = connection(check);
        self.discovered
            .iter()
            .rev()
            .find(|(known, _)| *known == key)
            .map_or(Value::Null, |(_, names)| names.clone())
    }

    /// The run and the step a reply `tag` belongs to.
    fn owner(&self, tag: &str) -> Option<(String, String)> {
        self.runs.keys().find_map(|run| {
            let step = tag.strip_prefix(run.as_str())?.strip_prefix(':')?;
            Some((run.clone(), step.to_owned()))
        })
    }

    fn start(&mut self, run: String, check: &Check) -> Vec<Cmd> {
        let plan = match plan(check) {
            Ok(plan) => plan,
            Err(error) => return vec![cmd::fail(&run, &error, &Value::Null)],
        };
        let mut sections = plan.sections;
        let options = sections["options"].take();
        self.runs.insert(
            run.clone(),
            Stage::Resolving {
                options,
                format: plan.format,
            },
        );
        vec![run::resolve_value(format!("{run}:resolve"), &sections)]
    }

    fn step(&mut self, run: &str, step: &str, result: Result<Value, Failure>) -> Vec<Cmd> {
        let Some(stage) = self.runs.remove(run) else {
            return vec![];
        };
        let (mut calls, format) = match (stage, step) {
            (Stage::Resolving { options, format }, "resolve") => {
                let resolved = match run::resolved_value(result) {
                    Ok(resolved) => resolved,
                    Err(error) => return vec![cmd::fail(run, &error, &Value::Null)],
                };
                match request(resolved, &options) {
                    Ok(request) => (Box::new(Calls::new(request)), format),
                    Err(error) => return vec![cmd::fail(run, error.message(), &Value::Null)],
                }
            }
            (Stage::Calling { mut calls, format }, step)
                if step == format!("call:{}", calls.replies.len()) =>
            {
                calls.answered(result);
                (calls, format)
            }
            (stage, _) => {
                self.runs.insert(run.to_owned(), stage);
                return vec![];
            }
        };
        let (result, mut debug) = jolokia::run(&calls.request, &calls.replies, calls.started);
        match result {
            Err(Error::Pending(next)) => {
                let command = calls.call(run, &next);
                self.runs
                    .insert(run.to_owned(), Stage::Calling { calls, format });
                vec![command]
            }
            Err(error) => vec![cmd::fail(
                run,
                error.message(),
                &jolokia::debugged(&error, debug),
            )],
            Ok(grid) => match render(&calls.request, &grid, format) {
                Ok(rendered) => {
                    if let Some(warning) = grid.get("warning") {
                        debug["warning"] = warning.clone();
                    }
                    let count = grid["rows"].as_array().map_or(0, Vec::len);
                    vec![
                        cmd::run_status(run, &rows(count)),
                        cmd::result(run, &rendered, &debug),
                    ]
                }
                Err(error) => vec![cmd::fail(run, &error, &debug)],
            },
        }
    }

    fn hints(&mut self, check: &Check) -> Vec<Cmd> {
        let key = connection(check);
        if !discoverable(check) || self.attempted.contains(&key) || self.discovery.is_some() {
            return vec![];
        }
        self.discover(check)
    }

    fn discover(&mut self, check: &Check) -> Vec<Cmd> {
        if !discoverable(check) {
            return vec![cmd::status(
                "Discovery · WARNING · Invalid or incomplete service address for discovery",
                "warning",
            )];
        }
        self.discoveries += 1;
        let id = self.discoveries;
        let key = connection(check);
        self.attempted.retain(|tried| *tried != key);
        self.attempted.push(key.clone());
        self.discovery = Some(Discovery {
            id,
            key,
            calls: None,
        });
        let sections = json!({
            "url": check.text_of("target").trim(),
            "action": "QUEUES",
            "headers": header_map(check),
        });
        vec![
            cmd::status("Discovery…", "busy"),
            run::resolve_value(format!("{DISCOVER}{id}:resolve"), &sections),
        ]
    }

    fn discovering(&mut self, step: &str, result: Result<Value, Failure>) -> Vec<Cmd> {
        let Some((id, step)) = step.split_once(':') else {
            return vec![];
        };
        let Some(mut discovery) = self
            .discovery
            .take_if(|discovery| discovery.id.to_string() == id)
        else {
            return vec![];
        };
        let found = match (discovery.calls.as_mut(), step) {
            (None, "resolve") => run::resolved_value(result)
                .map_err(Error::new)
                .and_then(|resolved| Request::parse(&resolved))
                .map(|request| {
                    discovery.calls = Some(Calls::new(request));
                }),
            (Some(calls), step) if step == format!("call:{}", calls.replies.len()) => {
                calls.answered(result);
                Ok(())
            }
            _ => {
                self.discovery = Some(discovery);
                return vec![];
            }
        }
        .and_then(|()| {
            let calls = discovery.calls.as_ref().expect("resolved");
            jolokia::discover(&calls.request, &calls.replies, calls.started)
        });
        match found {
            Err(Error::Pending(next)) => {
                let prefix = format!("{DISCOVER}{}", discovery.id);
                let command = discovery
                    .calls
                    .as_mut()
                    .expect("resolved")
                    .call(&prefix, &next);
                self.discovery = Some(discovery);
                vec![command]
            }
            Err(error) => {
                let message = error.message();
                vec![
                    cmd::status(&format!("Discovery · ERROR · {message}"), "error"),
                    cmd::log("error", &format!("Artemis discovery failed: {message}")),
                ]
            }
            Ok(names) => {
                let count = |list: &str| names["names"][list].as_array().map_or(0, Vec::len);
                let (queues, addresses) = (count("queues"), count("addresses"));
                self.discovered.retain(|(known, _)| *known != discovery.key);
                self.discovered.push((discovery.key, names));
                if self.discovered.len() > DISCOVERED {
                    self.discovered.remove(0);
                }
                vec![
                    cmd::status_with(
                        "Discovery · OK",
                        "ok",
                        &json!({"queues": queues, "addresses": addresses}),
                    ),
                    cmd::log(
                        "info",
                        &format!(
                            "Artemis discovery found {queues} queues and {addresses} addresses"
                        ),
                    ),
                ]
            }
        }
    }

    fn reply(&mut self, tag: &str, result: Result<Value, Failure>) -> Vec<Cmd> {
        if let Some(step) = tag.strip_prefix(DISCOVER) {
            return self.discovering(step, result);
        }
        match self.owner(tag) {
            Some((run, step)) => self.step(&run, &step, result),
            None => vec![],
        }
    }
}

impl Module for Artemis {
    type Msg = ();

    fn update(&mut self, event: Event<()>) -> Vec<Cmd> {
        match event {
            Event::Run { tag, check } => self.start(tag, &check),
            Event::Reply { tag, result } => self.reply(&tag, result),
            Event::Cancel { tag } => {
                self.runs.remove(&tag);
                vec![]
            }
            Event::Hints { check, .. } => self.hints(&check),
            Event::Discover(check) => self.discover(&check),
            _ => vec![],
        }
    }

    fn subscriptions(&self) -> Subscriptions {
        Subscriptions {
            run: true,
            hints: true,
            discover: true,
            ..Subscriptions::default()
        }
    }

    fn highlight(
        &self,
        state: &[u8],
        lines: &[String],
        columns: &[services_sdk::LineColumns],
    ) -> Vec<Line> {
        language::highlight(state, lines, columns)
    }

    fn fold(&self, check: &Check) -> Vec<services_sdk::Fold> {
        services_sdk::lang::table::folds(&check.text, &check.columns)
    }

    fn complete(&self, check: &Check, pos: u32) -> Completions {
        language::complete(check, pos as usize, &self.names(check))
    }

    fn schema(&self, check: &Check) -> Option<Value> {
        Some(self.names(check)).filter(|names| !names.is_null())
    }
}

services_sdk::export!(Artemis);
