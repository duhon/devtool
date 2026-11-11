//! The request pipeline: a check becomes its module's request (`before_request` → the
//! driver call or the module's own `Run`) with the options the console runs it with (Repeat,
//! Repeat-Until); the answer becomes the text the console shows ([`format`]).

pub mod format;

use serde_json::{Value, json};
use std::collections::BTreeMap;

/// The `#marker` sections of a service: what its module completes, then the console's
/// `#options` unless the module has its own.
pub fn markers(service: &str) -> Vec<String> {
    if !crate::modules::registry::is_service(service) {
        return Vec::new();
    }
    let mut markers = crate::modules::registry::markers(service);
    if !markers.iter().any(|marker| marker == "options") {
        markers.push("options".into());
    }
    markers
}

pub fn is_marker(service: &str, marker: &str) -> bool {
    markers(service)
        .iter()
        .any(|known| known.eq_ignore_ascii_case(marker))
}

/// A check's sections by marker; text before any marker is `leftover`. The module owns the
/// section shapes, so a section runs to the next marker.
pub fn sections(service: &str, source: &str) -> BTreeMap<String, String> {
    let mut result: BTreeMap<String, String> = BTreeMap::new();
    let mut current = String::from("leftover");
    for raw in source.lines() {
        if let Some((marker, _)) = crate::document::marker(raw)
            && is_marker(service, &marker)
        {
            current = marker.to_ascii_lowercase();
            result.entry(current.clone()).or_default();
            continue;
        }
        let entry = result.entry(current.clone()).or_default();
        if !entry.is_empty() {
            entry.push('\n');
        }
        entry.push_str(raw);
    }
    result.values_mut().for_each(|s| *s = s.trim().to_owned());
    result
}

/// The `#options` keys the console handles itself; the module never receives them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunOptions {
    /// `Repeat: 5s` (`s`, `m` or `h`): run again that long (here in milliseconds) after each
    /// response, until Stop.
    pub repeat: Option<u32>,
    /// `Repeat-Until: changed`: a repeat stops when the shown response changes.
    pub until_changed: bool,
}

/// The console's keys of an `#options` text and the lines left for the module.
fn console_options(text: &str, name: &str) -> Result<RunOptions, String> {
    let mut options = RunOptions::default();
    let mut rest = vec![];
    for line in text.lines() {
        let Some((name, value)) = line.split_once(':') else {
            if !line.trim().is_empty() {
                rest.push(line);
            }
            continue;
        };
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "repeat" => {
                options.repeat = Some(interval(value).ok_or_else(|| {
                    format!("Repeat takes a time from 1s to 24h, like 5s, 2m or 1h, not {value:?}")
                })?);
            }
            "repeat-until" if value.eq_ignore_ascii_case("changed") => options.until_changed = true,
            "repeat-until" => {
                return Err(format!("Repeat-Until takes \"changed\", not {value:?}"));
            }
            _ => rest.push(line),
        }
    }
    if options.until_changed && options.repeat.is_none() {
        return Err("Repeat-Until needs Repeat, like Repeat: 5s".into());
    }
    if let Some(line) = rest.first() {
        let option = line
            .split_once(':')
            .map_or(*line, |(option, _)| option)
            .trim();
        return Err(format!(
            "Unknown option {option:?}; {name} takes Repeat and Repeat-Until"
        ));
    }
    Ok(options)
}

/// A Kernel service's check (the text after its `#check` line) as its module gets it, and
/// the options the console runs it with. The console owns Repeat and Repeat-Until; a module
/// that runs its checks itself gets the rest of `#options`, others none.
pub fn kernel_run(
    service: &str,
    source: &str,
) -> Result<(services_sdk::Check, RunOptions), String> {
    if !crate::modules::registry::loaded(service) {
        return Err(crate::modules::registry::unavailable(service));
    }
    let name = crate::modules::registry::manifest(service)
        .map_or_else(|| service.to_owned(), |manifest| manifest.name);
    let runs = crate::modules::registry::with(|modules| modules.runs(service));
    let mut text = Vec::new();
    let mut options = Vec::new();
    let mut in_options = false;
    for line in source.lines() {
        if let Some((marker, _)) = crate::document::marker(line) {
            in_options = marker == "options";
            if in_options && !runs {
                continue;
            }
            text.push(line);
            continue;
        }
        let console = line.split_once(':').is_some_and(|(name, _)| {
            matches!(
                name.trim().to_ascii_lowercase().as_str(),
                "repeat" | "repeat-until"
            )
        });
        if in_options && (console || !runs) {
            options.push(line);
        } else {
            text.push(line);
        }
    }
    let run = console_options(&options.join("\n"), &name)?;
    let check = crate::document::check(service, &text.join("\n"), &serde_json::json!({}));
    Ok((check, run))
}

/// `Repeat`'s time in milliseconds: a whole number of seconds, minutes or hours, 1s to 24h.
fn interval(value: &str) -> Option<u32> {
    let digits = value.find(|c: char| !c.is_ascii_digit())?;
    let count: u32 = value[..digits].parse().ok()?;
    let unit = match value[digits..].trim().to_ascii_lowercase().as_str() {
        "s" => 1000,
        "m" => 60_000,
        "h" => 3_600_000,
        _ => return None,
    };
    count
        .checked_mul(unit)
        .filter(|ms| (1000..=86_400_000).contains(ms))
}

pub fn parse(service: &str, source: &str) -> Result<Value, String> {
    parse_run(service, source).map(|(params, _)| params)
}

/// The module request of a check and the options the console runs it with.
pub fn parse_run(service: &str, source: &str) -> Result<(Value, RunOptions), String> {
    let (check, options) = kernel_run(service, source)?;
    if crate::modules::registry::with(|modules| modules.runs(service)) {
        // The module checks it when it runs; `target` shows in approvals.
        let endpoint = check.text_of("target").to_owned();
        return Ok((json!({"check": check.text, "target": endpoint}), options));
    }
    let call = crate::modules::registry::with(|modules| modules.prepare(service, check))?;
    Ok((call_value(&call), options))
}

/// The command `service` exports checks as and imports them from (`curl`, `grpcurl`), if any.
pub fn transfer(service: &str) -> Option<String> {
    crate::modules::registry::manifest(service).and_then(|manifest| manifest.transfer)
}

pub fn import(service: &str, command: &str) -> Result<String, String> {
    crate::modules::registry::with(|modules| modules.import(service, command.to_owned()))
}

/// The check as a command (curl, grpcurl…) with its variables resolved, secrets included:
/// the command runs elsewhere, where the variables don't exist.
pub fn export(service: &str, source: &str) -> Result<String, String> {
    let source = crate::modules::registry::with(|modules| modules.resolve(source, false))
        .map_err(|error| format!("Variables could not be resolved: {error}"))?
        .text;
    let (check, _) = kernel_run(service, &source)?;
    crate::modules::registry::with(|modules| modules.export(service, check))
}

/// A driver call as the console keeps it in a run's `params`: `{driver, request, stream}`.
pub fn call_value(call: &services_sdk::DriverCall) -> Value {
    serde_json::json!({"driver": call.driver, "request": call.request(), "stream": call.stream})
}

/// The driver call of [`call_value`], with its `request` as given.
pub fn call_of(value: &Value) -> services_sdk::DriverCall {
    let call =
        services_sdk::DriverCall::new(value["driver"].as_str().unwrap_or(""), &value["request"]);
    if value["stream"] == true {
        call.streaming()
    } else {
        call
    }
}

/// HTTP cannot take a request back once it is sent.
pub const STOPPED: &str = "Client request stopped. The remote server may still finish processing; committed changes are not rolled back.";

/// A Host answer without what the transport adds for Debug.
pub fn strip(mut value: Value) -> Value {
    if let Some(map) = value.as_object_mut() {
        map.remove("transportDebug");
    }
    value
}

/// How a run ended.
pub enum Finish {
    Stopped,
    Failed(String, Value),
    /// The driver answered the call the panels prepared.
    Answered(Value),
    /// The module finished its run (`run.result`).
    Ran(Value),
}

/// A run of a Kernel service: the driver call the panels prepared, or the check its module
/// runs itself.
pub struct Run {
    /// What the run sends: `params` (the driver call, or the `check` its module runs) and
    /// `history`.
    pub message: Value,
    /// The source and title of the check with their variables resolved, and the values
    /// of the sensitive ones: History masks them. `None` when they did not resolve.
    resolved: Option<(String, String, Vec<String>)>,
    /// The module runs the check itself (`Run`): it has started.
    pub started: bool,
    /// The module's status text for the run, like `3 rows`.
    pub summary: Option<String>,
}

impl Run {
    pub fn new(message: Value) -> Self {
        Self {
            message,
            resolved: None,
            started: false,
            summary: None,
        }
    }

    /// Resolves the variables of the source and title History gets.
    pub fn resolve(&mut self) -> Result<(), String> {
        let history = &self.message["history"];
        let resolve = |text: &str| {
            crate::modules::registry::with(|modules| modules.resolve(text, false))
                .map(|resolution| (resolution.text, resolution.secrets))
        };
        let resolved =
            resolve(history["source"].as_str().unwrap_or("")).and_then(|(source, mut secrets)| {
                let (title, more) = resolve(history["title"].as_str().unwrap_or(""))?;
                secrets.extend(more);
                Ok((source, title, secrets))
            });
        self.resolved =
            Some(resolved.map_err(|error| format!("Variables could not be resolved: {error}"))?);
        Ok(())
    }

    /// The end of the run: its module renders the answer and the panels hear of it
    /// (`after_response`: History saves it). The reply is what the console shows — `Ok` a
    /// result, `Err` an error; `stdout` is what the module's own run printed, `started` and
    /// `ended` the run's clock (milliseconds).
    pub fn finish(
        &self,
        service: &str,
        finish: Finish,
        stdout: &str,
        started: f64,
        ended: f64,
    ) -> (Result<Value, Value>, Vec<crate::Effect>) {
        let begun = self.message["history"]["startedAt"]
            .as_f64()
            .unwrap_or(started);
        let call = self.message["params"].clone();
        // A check its module ran itself has no driver call of its own.
        let driver_call = if call["check"].is_string() {
            services_sdk::DriverCall::default()
        } else {
            call_of(&call)
        };
        let (reply, status, outcome, text) = match finish {
            Finish::Stopped => {
                let mut result = json!({"cancelled":true,"warning":STOPPED,"request":call});
                if self.started && !stdout.is_empty() {
                    result["body"] = json!(stdout);
                }
                (
                    Ok(result),
                    "stopped",
                    "Stopped".to_owned(),
                    STOPPED.to_owned(),
                )
            }
            Finish::Ran(result) => {
                let debug = &result["debug"];
                let request = if debug["request"].is_null() {
                    call.clone()
                } else {
                    debug["request"].clone()
                };
                if let Some(error) = result["error"].as_str() {
                    (
                        Err(json!({"error":error,"request":request,"debug":debug,
                            "durationMs":ended - started})),
                        "error",
                        "ERROR".to_owned(),
                        error.to_owned(),
                    )
                } else {
                    let outcome = result["outcome"].as_str().unwrap_or("OK").to_owned();
                    let body = result["body"].as_str().unwrap_or("").to_owned();
                    let mut reply = json!({"body":body,"outcome":outcome,
                        "language":result["language"],"durationMs":ended - started,
                        "request":request,"debug":debug,"columns":result["columns"]});
                    if let Some(summary) = &self.summary {
                        reply["summary"] = json!(summary);
                    }
                    let status = if outcome == "ERROR" {
                        "error"
                    } else {
                        "success"
                    };
                    (Ok(reply), status, outcome, body)
                }
            }
            Finish::Failed(error, details) => (
                Err(json!({"error":error,"request":call,"reply":details,
                    "durationMs":ended - started})),
                "error",
                "ERROR".to_owned(),
                error,
            ),
            Finish::Answered(response) => {
                let chunk = services_sdk::Chunk {
                    call: Some(driver_call.clone()),
                    response: serde_json::to_vec(&response).unwrap_or_default(),
                    index: 0,
                    last: true,
                };
                match crate::modules::registry::with(|modules| modules.render(service, chunk)) {
                    Ok(rendered) => {
                        let status = if rendered.outcome == "ERROR" {
                            "error"
                        } else {
                            "success"
                        };
                        (
                            Ok(json!({"body":rendered.text,"outcome":rendered.outcome,
                                "language":rendered.language,"durationMs":ended - started,
                                "request":call,"response":response,
                                "columns":rendered.columns.iter()
                                    .map(|column| [&column.name, &column.kind])
                                    .collect::<Vec<_>>()})),
                            status,
                            rendered.outcome,
                            rendered.text,
                        )
                    }
                    Err(error) => {
                        let error = format!("The response could not be shown: {error}");
                        (
                            Err(json!({"error":error,"request":call,"response":response,
                                "durationMs":ended - started})),
                            "error",
                            "ERROR".to_owned(),
                            error,
                        )
                    }
                }
            }
        };
        let history = &self.message["history"];
        let mut record = json!({"service":service,"started":begun as u64,"ended":ended as u64,
            "status":status,"outcome":outcome,"text":text,"defer":history["defer"] == true});
        if history.is_object() {
            record["checkId"] = history["checkId"].clone();
            record["sessionId"] = history["sessionId"].clone();
        }
        // Without the resolved source the panels get none: it may hold secrets.
        if let Some((source, title, secrets)) = &self.resolved {
            record["source"] = json!(source);
            record["title"] = json!(title);
            record["secrets"] = json!(secrets);
        }
        let effects =
            crate::modules::registry::with(|modules| modules.finished(&driver_call, &record));
        (reply, effects)
    }
}

/// What the App does once the Host answered a module's command.
#[derive(Clone)]
pub enum After {
    Reply,
    /// A History save: a failure is a notice.
    History,
    /// Modules were installed or removed (the one removed with its data).
    Modules(Option<String>),
    /// What sources apply changed: panels read their files again.
    Sources,
    /// The model catalog: the options new chats get come with it.
    Models,
}

/// A module command the Host serves: the request it sends and what follows the answer.
pub enum Hosted {
    Request(Value, After),
    /// Answered at once.
    Done(Value),
}

/// The Host request of a module command the Host serves; `native`: the App talks to the
/// native Host (only it checks module sources). `None` for the commands the App serves.
pub fn hosted(name: &str, mut args: Value, native: bool) -> Option<Hosted> {
    let request = |request: Value| Some(Hosted::Request(request, After::Reply));
    match name {
        "history.insert" => {
            args["service"] = json!("history");
            args["op"] = json!("insert");
            Some(Hosted::Request(args, After::History))
        }
        "history.search" => {
            args["service"] = json!("history");
            if args["op"].is_null() {
                args["op"] = json!("list");
            }
            request(args)
        }
        "os.env" => request(json!({"service":"env"})),
        "sources.read" => request(json!({"service":"sources","op":"read","name":args["name"]})),
        "sources.write" => request(
            json!({"service":"sources","op":"write","name":args["name"],"text":args["text"]}),
        ),
        "modules.check" if !native => Some(Hosted::Done(Value::Null)),
        "modules.check" => {
            request(json!({"service":"sources","op":"check","force":args["force"] == true}))
        }
        "modules.install" => Some(Hosted::Request(
            json!({"service":"modules","op":"install","source":args["source"],
                "module":args["module"],"version":args["version"]}),
            After::Modules(None),
        )),
        "modules.remove" => {
            let module = args["module"].as_str().unwrap_or_default().to_owned();
            let forget = args["forget"] == true;
            Some(Hosted::Request(
                json!({"service":"modules","op":"remove","module":args["module"],"forget":forget}),
                After::Modules(forget.then_some(module)),
            ))
        }
        "sources.list" => request(json!({"service":"sources","op":"get"})),
        "sources.save" => Some(Hosted::Request(
            json!({"service":"sources","op":"save","sources":args["sources"]}),
            After::Sources,
        )),
        "sources.apply" => Some(Hosted::Request(
            json!({"service":"sources","op":"apply","source":args["source"]}),
            After::Sources,
        )),
        "sources.sync" => Some(Hosted::Request(
            json!({"service":"sources","op":"sync","source":args["source"]}),
            After::Sources,
        )),
        "sources.resolve" => Some(Hosted::Request(
            json!({"service":"sources","op":"resolve","source":args["source"],
                "file":args["file"],"text":args["text"]}),
            After::Sources,
        )),
        "sources.open" => request(json!({"service":"sources","op":"open","source":args["source"]})),
        "agent.models" => Some(Hosted::Request(
            json!({"service":"agent_models"}),
            After::Models,
        )),
        _ => None,
    }
}
