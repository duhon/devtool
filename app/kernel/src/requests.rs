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
    /// `Timeout: 30`: seconds (1 to 3600, here in milliseconds) after which the console stops
    /// the run, like Stop.
    pub timeout: Option<u32>,
    /// `Transform: {{$name(args)}}`: the jq function call, as written, that reshapes the
    /// driver's answer before the module renders it ([`Transform`]).
    pub transform: Option<String>,
}

/// A `Transform` option: one jq function call whose input `.` is the driver's answer. Its
/// arguments resolve when the run is sent; it runs on the answer before the module renders
/// it, so the console, History and `Repeat-Until` see what it gives.
#[derive(Clone, Debug)]
pub struct Transform {
    /// The call as written, like `{{$even()}}`.
    pub call: String,
    name: String,
    args: Vec<Value>,
    defs: String,
    /// The module's data was transformed (`run.data`).
    pub applied: bool,
    /// What the module's data became, for Debug.
    pub shaped: Option<Value>,
}

impl Transform {
    /// The call `call` of `service` with its arguments resolved, and the secrets they hold.
    fn resolve(call: &str, service: Option<&str>) -> Result<(Self, Vec<String>), String> {
        let defs = crate::modules::registry::function_defs(service);
        let root = crate::modules::registry::root();
        let (name, args_text) = services_sdk::refs::scan(call)
            .into_iter()
            .find_map(|found| match found.reference {
                services_sdk::refs::Ref::Function {
                    name, args_text, ..
                } => Some((name, args_text)),
                _ => None,
            })
            .ok_or("Transform takes a jq function call")?;
        if !services_jq::names(&defs)
            .iter()
            .any(|(known, _)| *known == name)
        {
            return Err(format!("Transform {call}: Unknown function ${name}"));
        }
        // The call runs on the response; the functions in its arguments run now.
        let mut outputs = Vec::new();
        let (args, secrets) = services_sdk::refs::call_arguments(
            &args_text,
            &root,
            &mut |_, _| Err("Transform takes a jq function, not a template".into()),
            &mut crate::editor::functions::runner(&defs, &root, &mut outputs),
        )
        .map_err(|error| format!("Transform {call}: {error}"))?;
        let transform = Self {
            call: call.to_owned(),
            name,
            args,
            defs,
            applied: false,
            shaped: None,
        };
        Ok((transform, secrets))
    }

    /// `answer` reshaped: the one output, several as an array, none as `null`.
    pub fn apply(&self, answer: &Value) -> Result<Value, String> {
        let mut outputs = crate::editor::functions::run(&self.defs, &self.name, &self.args, answer)
            .map_err(|error| format!("Transform {}: {error}", self.call))?;
        Ok(match outputs.len() {
            0 => Value::Null,
            1 => outputs.pop().expect("one output"),
            _ => Value::Array(outputs),
        })
    }

    /// The text a stopped run showed, reshaped (its module cannot hand data once stopped):
    /// `.` is the text; a string shows as it is, strings as lines, anything else as JSON
    /// (`true`).
    pub fn text(&self, text: &str) -> Result<(String, bool), String> {
        Ok(match self.apply(&json!(text))? {
            Value::String(text) => (text, false),
            Value::Array(lines) if lines.iter().all(Value::is_string) => {
                let lines: Vec<&str> = lines.iter().filter_map(Value::as_str).collect();
                (lines.join("\n"), false)
            }
            other => (
                serde_json::to_string_pretty(&other).expect("JSON values serialize"),
                true,
            ),
        })
    }
}

/// Whether `value` is exactly one jq function call `{{$name(args)}}`.
fn function_call(value: &str) -> bool {
    matches!(
        services_sdk::refs::scan(value).as_slice(),
        [found] if found.start == 0
            && found.end == value.len()
            && matches!(found.reference, services_sdk::refs::Ref::Function { .. })
    )
}

/// A placeholder line for the `n`th kept `Transform` line, free of `{{ }}`.
fn transform_mark(n: usize) -> String {
    format!("\u{1}transform {n}\u{1}")
}

/// The check text with each `#options` `Transform:` line swapped for a placeholder, and
/// those lines: the call runs on the response later, so the text keeps it as written.
fn keep_transforms(source: &str) -> (String, Vec<String>) {
    let mut options = false;
    let mut kept = Vec::new();
    let lines: Vec<String> = source
        .lines()
        .map(|line| {
            if let Some(marker) = line.trim_start().strip_prefix('#') {
                let name = marker.split_whitespace().next().unwrap_or("");
                options = name.eq_ignore_ascii_case("options");
            } else if options
                && line
                    .split_once(':')
                    .is_some_and(|(key, _)| key.trim().eq_ignore_ascii_case("transform"))
            {
                kept.push(line.to_owned());
                return transform_mark(kept.len() - 1);
            }
            line.to_owned()
        })
        .collect();
    (lines.join("\n"), kept)
}

/// The resolved text with the kept `Transform` lines back in place.
fn restore_transforms(text: &str, kept: &[String]) -> String {
    kept.iter()
        .enumerate()
        .fold(text.to_owned(), |text, (n, line)| {
            text.replacen(&transform_mark(n), line, 1)
        })
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
            "timeout" => {
                options.timeout = Some(
                    value
                        .parse::<u32>()
                        .ok()
                        .filter(|seconds| (1..=3600).contains(seconds))
                        .ok_or_else(|| {
                            format!("Timeout takes seconds from 1 to 3600, not {value:?}")
                        })?
                        * 1000,
                );
            }
            "transform" if function_call(value) => options.transform = Some(value.to_owned()),
            "transform" => {
                return Err(format!(
                    "Transform takes one jq function call, like {{{{$name(args)}}}}, not {value:?}"
                ));
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
            "Unknown option {option:?}; {name} takes Repeat, Repeat-Until, Timeout and Transform"
        ));
    }
    Ok(options)
}

/// A Kernel service's check (the text after its `#check` line) as its module gets it, and
/// the options the console runs it with. The console owns Repeat, Repeat-Until, Timeout and
/// Transform; a module that runs its checks itself gets the rest of `#options`, others none.
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
                "repeat" | "repeat-until" | "timeout" | "transform"
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
    let source =
        crate::modules::registry::with(|modules| modules.resolve_in(source, false, Some(service)))
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
    /// The `Transform` of the run (`message["transform"]`), once resolved.
    pub transform: Option<Transform>,
}

impl Run {
    pub fn new(message: Value) -> Self {
        Self {
            message,
            resolved: None,
            started: false,
            summary: None,
            transform: None,
        }
    }

    /// Resolves the variables of the source and title History gets, and the arguments of
    /// the run's `Transform`.
    pub fn resolve(&mut self) -> Result<(), String> {
        let history = &self.message["history"];
        let service = self.message["service"].as_str();
        let resolve = |text: &str| {
            crate::modules::registry::with(|modules| modules.resolve_in(text, false, service))
                .map(|resolution| (resolution.text, resolution.secrets))
        };
        let (masked, kept) = keep_transforms(history["source"].as_str().unwrap_or(""));
        let resolved = resolve(&masked)
            .map(|(source, secrets)| (restore_transforms(&source, &kept), secrets))
            .and_then(|(source, mut secrets)| {
                let (title, more) = resolve(history["title"].as_str().unwrap_or(""))?;
                secrets.extend(more);
                Ok((source, title, secrets))
            });
        let (source, title, mut secrets) =
            resolved.map_err(|error| format!("Variables could not be resolved: {error}"))?;
        if let Some(call) = self.message["transform"].as_str() {
            let (transform, more) = Transform::resolve(call, service)?;
            secrets.extend(more);
            secrets.retain(|secret| !secret.is_empty());
            secrets.sort();
            secrets.dedup();
            self.transform = Some(transform);
        }
        self.resolved = Some((source, title, secrets));
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
                    if let Some(transform) = self
                        .transform
                        .as_ref()
                        .filter(|transform| !transform.applied)
                    {
                        match transform.text(stdout) {
                            Ok((text, json)) => {
                                result["body"] = json!(text);
                                if json {
                                    result["language"] = json!("json");
                                }
                            }
                            Err(error) => result["warning"] = json!(format!("{error}. {STOPPED}")),
                        }
                    }
                }
                (
                    Ok(result),
                    "stopped",
                    "Stopped".to_owned(),
                    STOPPED.to_owned(),
                )
            }
            Finish::Ran(mut result) => {
                if let Some(transform) = &self.transform {
                    transformed_run(transform, &mut result);
                }
                let shaped = self
                    .transform
                    .as_ref()
                    .and_then(|transform| transform.shaped.clone());
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
                    if let Some(shaped) = shaped {
                        reply["transformed"] = shaped;
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
                let shaped = self
                    .transform
                    .as_ref()
                    .map(|transform| transform.apply(&response))
                    .transpose();
                let rendered = shaped.clone().and_then(|shaped| {
                    let chunk = services_sdk::Chunk {
                        call: Some(driver_call.clone()),
                        response: serde_json::to_vec(shaped.as_ref().unwrap_or(&response))
                            .unwrap_or_default(),
                        index: 0,
                        last: true,
                    };
                    crate::modules::registry::with(|modules| modules.render(service, chunk))
                        .map_err(|error| match &self.transform {
                            Some(transform) => format!(
                                "The response could not be shown after Transform {}: {error}",
                                transform.call
                            ),
                            None => format!("The response could not be shown: {error}"),
                        })
                });
                match rendered {
                    Ok(rendered) => {
                        let status = if rendered.outcome == "ERROR" {
                            "error"
                        } else {
                            "success"
                        };
                        let mut reply = json!({"body":rendered.text,"outcome":rendered.outcome,
                            "language":rendered.language,"durationMs":ended - started,
                            "request":call,"response":response,
                            "columns":rendered.columns.iter()
                                .map(|column| [&column.name, &column.kind])
                                .collect::<Vec<_>>()});
                        if let Ok(Some(shaped)) = shaped {
                            reply["transformed"] = shaped;
                        }
                        (Ok(reply), status, rendered.outcome, rendered.text)
                    }
                    Err(error) => {
                        let mut reply = json!({"error":error,"request":call,"response":response,
                            "durationMs":ended - started});
                        if let Ok(Some(shaped)) = shaped {
                            reply["transformed"] = shaped;
                        }
                        (Err(reply), "error", "ERROR".to_owned(), error)
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

/// The result of a module's own run with a `Transform`: an error its module gave after the
/// transformed data says so.
fn transformed_run(transform: &Transform, result: &mut Value) {
    if let Some(error) = result["error"].as_str()
        && transform.applied
        && !error.starts_with("Transform")
    {
        result["error"] = json!(format!("{error} (after Transform {})", transform.call));
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
        "agent.mcp" => request(json!({"service":"agent_models","op":"mcp"})),
        "agent.system" => request(json!({"service":"agent_models","op":"system"})),
        "agent.models" => Some(Hosted::Request(
            json!({"service":"agent_models"}),
            After::Models,
        )),
        _ => None,
    }
}
