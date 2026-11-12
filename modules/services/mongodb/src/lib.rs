//! The MongoDB service: a `#target` connection string (`mongodb://…`), a `#collection`, an
//! `#action` (the command) and a JSON `#payload` with its fields, run through the Host
//! `mongodb` driver. Documents print as a table, other replies as JSON. Discovery feeds completion
//! with collections and fields.

pub mod action;
pub mod config;
pub mod discovery;
pub mod editor;
pub mod render;
pub mod value;

use config::Connection;
use discovery::{Entry, Scope};
use editor::Catalog;
use render::Display;
use serde_json::{Value, json};
use services_sdk::lang::resolve::Resolver;
use services_sdk::mongodb::{DRIVER, Request, Response};
use services_sdk::run;
use services_sdk::{
    Check, Cmd, Completions, DriverCall, Event, Failure, Line, LineColumns, Module, Subscriptions,
    cmd,
};
use std::collections::BTreeMap;

const RESOLVE: &str = ":resolve";
const CALL: &str = ":call";
const DISCOVER: &str = "discover:";

/// What a hints request needs found, and whether to find it again.
#[derive(Clone, Debug, Default, PartialEq)]
struct Need {
    collections: bool,
    fields: Option<String>,
    force: bool,
}

struct Pending {
    request: Value,
    uri: String,
    display: Display,
}

struct Job {
    uri: String,
    database: String,
    scope: Scope,
}

/// The raw text discovery resolves: the target and the options.
fn raw_of(check: &Check) -> String {
    format!(
        "{}\n{}",
        check.text_of("target").trim(),
        check.text_of("options")
    )
}

fn connection_of(text: &str) -> Result<Connection, String> {
    let (target, options) = text.split_once('\n').unwrap_or((text, ""));
    Connection::parse(target, options)
}

#[derive(Default)]
pub struct Mongodb {
    resolver: Resolver<Need>,
    resolving: BTreeMap<String, Check>,
    calls: BTreeMap<String, Pending>,
    cache: Vec<Entry>,
    jobs: BTreeMap<u64, Job>,
    count: u64,
}

impl Mongodb {
    fn entry(&self, uri: &str, database: &str) -> Option<&Entry> {
        self.cache
            .iter()
            .find(|entry| entry.uri == uri && entry.database == database)
    }

    fn entry_mut(&mut self, uri: &str, database: &str) -> &mut Entry {
        match self
            .cache
            .iter()
            .position(|entry| entry.uri == uri && entry.database == database)
        {
            Some(at) => {
                let entry = self.cache.remove(at);
                self.cache.push(entry);
            }
            None => {
                self.cache.push(Entry {
                    uri: uri.into(),
                    database: database.into(),
                    ..Entry::default()
                });
                if self.cache.len() > discovery::ENTRIES {
                    self.cache.remove(0);
                }
            }
        }
        self.cache.last_mut().expect("an entry")
    }

    fn catalog(&self, check: &Check) -> (String, Catalog) {
        let raw = raw_of(check);
        let known = self
            .resolver
            .get(&raw)
            .and_then(|text| connection_of(text).ok())
            .and_then(|connection| {
                self.entry(&connection.uri, &connection.database)
                    .map(|entry| (connection.database.clone(), entry.catalog()))
            });
        known.unwrap_or_default()
    }

    fn hints(&mut self, check: &Check, pos: usize, force: bool) -> Vec<Cmd> {
        let need = if force {
            Need {
                collections: true,
                fields: None,
                force,
            }
        } else {
            match editor::needs(check, pos) {
                Some((collections, fields)) => Need {
                    collections,
                    fields,
                    force,
                },
                None => return vec![],
            }
        };
        let raw = raw_of(check);
        if raw.contains("{{") {
            return match self.resolver.get(&raw) {
                Some(text) => {
                    let text = text.to_owned();
                    self.discover(&text, need)
                }
                None if self.resolver.pending(&raw) => vec![],
                None => vec![self.resolver.request(&raw, need)],
            };
        }
        self.discover(&raw, need)
    }

    fn discover(&mut self, text: &str, need: Need) -> Vec<Cmd> {
        let connection = match connection_of(text) {
            Ok(connection) => connection,
            Err(message) => {
                return vec![cmd::status(
                    &format!("Discovery · WARNING · {message}"),
                    "warning",
                )];
            }
        };
        let now = services_sdk::now();
        let mut scopes = Vec::new();
        if need.collections {
            scopes.push(Scope::Collections);
        }
        if let Some(name) = need.fields {
            scopes.push(Scope::Fields(name));
        }
        let mut cmds = Vec::new();
        for scope in scopes {
            if self.jobs.values().any(|job| {
                job.uri == connection.uri
                    && job.database == connection.database
                    && job.scope == scope
            }) {
                continue;
            }
            let entry = self.entry_mut(&connection.uri, &connection.database);
            if need.force {
                entry.failed = None;
                entry.fields.clear();
            } else if entry.fresh(&scope, now) || entry.cooling(now) {
                continue;
            }
            self.count += 1;
            let request = discovery::request(&scope, &connection.uri, &connection.database);
            cmds.push(cmd::status("Discovery…", "busy"));
            cmds.push(cmd::driver(
                format!("{DISCOVER}{}", self.count),
                &DriverCall::new(DRIVER, &request),
            ));
            self.jobs.insert(
                self.count,
                Job {
                    uri: connection.uri.clone(),
                    database: connection.database.clone(),
                    scope,
                },
            );
        }
        cmds
    }

    fn discovered(&mut self, id: u64, result: Result<Value, Failure>) -> Vec<Cmd> {
        let Some(job) = self.jobs.remove(&id) else {
            return vec![];
        };
        let now = services_sdk::now();
        let found = result
            .map_err(|failure| failure.message)
            .and_then(|value| discovery::found(&job.scope, &Response::parse(&value)?));
        let entry = self.entry_mut(&job.uri, &job.database);
        match found {
            Ok(names) => {
                let count = names.len();
                entry.store(&job.scope, now, names);
                let what = match &job.scope {
                    Scope::Collections => "collections".to_owned(),
                    Scope::Fields(name) => format!("fields of {name}"),
                };
                vec![
                    cmd::status_with("Discovery · OK", "ok", &json!({what.clone(): count})),
                    cmd::log("info", &format!("MongoDB discovery found {count} {what}")),
                    cmd::relight(),
                ]
            }
            Err(message) => {
                entry.failed = Some(now);
                let message = config::scrub(&message, &job.uri);
                vec![
                    cmd::status(&format!("Discovery · ERROR · {message}"), "error"),
                    cmd::log("error", &format!("MongoDB discovery failed: {message}")),
                ]
            }
        }
    }

    fn run(&mut self, run: String, check: Check) -> Vec<Cmd> {
        let (target, options) = (check.text_of("target"), check.text_of("options"));
        if !target.contains("{{")
            && !options.contains("{{")
            && let Err(error) = Connection::parse(target, options)
        {
            return vec![cmd::fail(&run, &error, &Value::Null)];
        }
        let sections = json!({"target": target.trim(), "options": options,
            "collection": check.text_of("collection"), "action": check.text_of("action"),
            "payload": check.text_of("payload")});
        self.resolving.insert(run.clone(), check);
        vec![run::resolve_value(format!("{run}{RESOLVE}"), &sections)]
    }

    fn resolved(&mut self, run: &str, result: Result<Value, Failure>) -> Vec<Cmd> {
        let Some(check) = self.resolving.remove(run) else {
            return vec![];
        };
        let fail = |error: String| vec![cmd::fail(run, &error, &Value::Null)];
        let sections = match run::resolved_value(result) {
            Ok(sections) => run::with_sections(&check, &sections),
            Err(error) => return fail(error),
        };
        let connection =
            match Connection::parse(sections.text_of("target"), sections.text_of("options")) {
                Ok(connection) => connection,
                Err(error) => return fail(error),
            };
        let statement = match action::build(
            sections.text_of("collection").trim(),
            sections.text_of("action").trim(),
            sections.text_of("payload").trim(),
        ) {
            Ok(statement) => statement,
            Err(error) => return fail(error),
        };
        let database = statement
            .database
            .clone()
            .unwrap_or_else(|| connection.database.clone());
        let request = Request {
            max_documents: Some(connection.max_documents),
            batch_size: connection.batch_size,
            timeout_ms: Some(connection.timeout_ms),
            connect_ms: connection.connect_ms,
            ..Request::new(&connection.uri, &database, statement.command)
        };
        let mut shown = serde_json::to_value(&request).expect("a request");
        shown["uri"] = json!(config::redact(&connection.uri));
        self.calls.insert(
            run.to_owned(),
            Pending {
                request: shown,
                uri: connection.uri,
                display: Display {
                    view: Some(statement.view),
                    format: connection.format,
                    columns_type: connection.columns_type,
                },
            },
        );
        vec![cmd::driver(format!("{run}{CALL}"), &request.call())]
    }

    fn answered(&mut self, run: &str, result: Result<Value, Failure>) -> Vec<Cmd> {
        let Some(pending) = self.calls.remove(run) else {
            return vec![];
        };
        let uri = pending.uri.as_str();
        match result {
            Ok(value) => {
                let debug = json!({"request": pending.request, "response": value});
                let request: Request =
                    serde_json::from_value(pending.request.clone()).expect("a request");
                match Response::parse(&value)
                    .and_then(|response| render::render(&pending.display, &request, &response))
                {
                    Ok((rendered, status)) => {
                        vec![
                            cmd::run_status(run, &status),
                            cmd::result(run, &rendered, &debug),
                        ]
                    }
                    Err(error) => {
                        let error = format!("The response could not be shown: {error}");
                        vec![cmd::fail(run, &error, &debug)]
                    }
                }
            }
            Err(failure) => {
                let message = config::scrub(&failure.message, uri);
                let details = config::scrub(&failure.details().to_string(), uri);
                let debug = json!({"request": pending.request, "reply": details});
                vec![cmd::fail(run, &message, &debug)]
            }
        }
    }

    fn reply(&mut self, tag: &str, result: Result<Value, Failure>) -> Vec<Cmd> {
        if let Some(run) = tag.strip_suffix(RESOLVE) {
            return self.resolved(run, result);
        }
        if let Some(run) = tag.strip_suffix(CALL) {
            return self.answered(run, result);
        }
        if let Some(id) = tag.strip_prefix(DISCOVER) {
            return id
                .parse()
                .map_or_else(|_| vec![], |id| self.discovered(id, result));
        }
        let Some(resolved) = self.resolver.reply(tag, result) else {
            return vec![];
        };
        match resolved.result {
            Ok(text) => self.discover(&text, resolved.purpose),
            Err(message) => {
                let message =
                    format!("Variables of the connection could not be resolved: {message}");
                let mut cmds = vec![cmd::log("warning", &message)];
                if resolved.purpose.force {
                    cmds.insert(
                        0,
                        cmd::status(&format!("Discovery · ERROR · {message}"), "error"),
                    );
                }
                cmds
            }
        }
    }
}

impl Module for Mongodb {
    type Msg = ();

    fn update(&mut self, event: Event<()>) -> Vec<Cmd> {
        match event {
            Event::Hints { check, pos } => self.hints(&check, pos as usize, false),
            Event::Discover(check) => self.hints(&check, 0, true),
            Event::Reply { tag, result } => self.reply(&tag, result),
            Event::Run { tag, check } => self.run(tag, check),
            Event::Cancel { tag } => {
                self.resolving.remove(&tag);
                self.calls.remove(&tag);
                vec![]
            }
            Event::Workspace => {
                self.cache.clear();
                self.jobs.clear();
                self.resolver = Resolver::default();
                vec![]
            }
            _ => vec![],
        }
    }

    fn subscriptions(&self) -> Subscriptions {
        Subscriptions {
            hints: true,
            discover: true,
            run: true,
            workspace: true,
            ..Subscriptions::default()
        }
    }

    fn highlight(&self, state: &[u8], lines: &[String], columns: &[LineColumns]) -> Vec<Line> {
        editor::highlight(state, lines, columns)
    }

    fn fold(&self, check: &Check) -> Vec<services_sdk::Fold> {
        editor::folds(check)
    }

    fn complete(&self, check: &Check, pos: u32) -> Completions {
        editor::complete(check, pos as usize, &self.catalog(check).1)
    }

    fn schema(&self, check: &Check) -> Option<Value> {
        let (database, catalog) = self.catalog(check);
        (!catalog.collections.is_empty()).then(|| editor::schema(&database, &catalog))
    }
}

services_sdk::export!(Mongodb);
