//! The Terminal service: a `#endpoint` command line (a local shell or an ssh connection)
//! runs the `#command` text as its last argument through the Host `process` driver, with
//! `#env` exported first.

use serde::Deserialize;
use serde_json::{Value, json};
use services_sdk::lang::check::At;
use services_sdk::lang::{finish, headers, item, marker, shell};
use services_sdk::process::{self, Exit, Request};
use services_sdk::{
    Check, Cmd, Completions, Event, Failure, Line, Module, Rendered, Subscriptions, Token, cmd, run,
};
use std::collections::BTreeMap;

pub const SECTIONS: [&str; 3] = ["endpoint", "command", "env"];

const ENDPOINTS: [&str; 3] = ["bash -c", "zsh -c", "ssh "];
const ENV: [&str; 6] = [
    "PATH: ",
    "HOME: ",
    "USER: ",
    "LANG: en_US.UTF-8",
    "NODE_ENV: production",
    "DEBUG: *",
];

pub const INCOMPLETE: &str = "Output capture ended before stdout/stderr closed. Detached background processes may still be running; close their resources explicitly.";
pub const TRUNCATED: &str = "Output past the Host's limit was dropped.";

/// The words of an endpoint as a POSIX shell splits them: quotes and backslashes.
pub fn parse_argv(input: &str) -> Result<Vec<String>, String> {
    let mut chars = input.chars().peekable();
    let mut out = vec![];
    loop {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        if chars.peek().is_none() {
            return Ok(out);
        }
        let mut token = String::new();
        while let Some(c) = chars.next_if(|c| !c.is_whitespace()) {
            match c {
                '\'' => loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => token.push(c),
                        None => return Err("Unterminated single quote in endpoint".into()),
                    }
                },
                '"' => loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') if matches!(chars.peek(), Some('"' | '\\')) => {
                            token.extend(chars.next());
                        }
                        Some(c) => token.push(c),
                        None => return Err("Unterminated double quote in endpoint".into()),
                    }
                },
                '\\' if chars.peek().is_some() => token.extend(chars.next()),
                c => token.push(c),
            }
        }
        out.push(token);
    }
}

/// The command with `env` exported first: `export NAME='value' …; command`.
pub fn build_command(command: &str, env: &[(String, String)]) -> String {
    if env.is_empty() {
        return command.into();
    }
    let exports: Vec<_> = env
        .iter()
        .map(|(name, value)| format!("{name}='{}'", value.replace('\'', "'\\''")))
        .collect();
    format!("export {}; {command}", exports.join(" "))
}

/// The endpoint line and the command: `#command`, or the lines after the endpoint.
fn parts(check: &Check) -> (&str, &str) {
    let endpoint = check.text_of("endpoint");
    let (line, rest) = endpoint.split_once('\n').unwrap_or((endpoint, ""));
    let command = match check.section("command") {
        Some(command) => command,
        None if !rest.trim().is_empty() => rest.trim(),
        None => check.text_of("leftover"),
    };
    (line.trim(), command)
}

/// The `process` driver request of a check: the endpoint's program and arguments, then the
/// command with `#env` exported.
pub fn request(check: &Check) -> Result<Request, String> {
    plan(check).and_then(|plan| resolved(&plan))
}

/// What a run resolves before it builds its request: the endpoint's words, the command and
/// the `#env` pairs. Words are split first, so a value with spaces stays one argument.
pub fn plan(check: &Check) -> Result<Value, String> {
    run::no_options(check, "Terminal")?;
    let (endpoint, command) = parts(check);
    let argv = parse_argv(endpoint)?;
    if argv.is_empty() {
        return Err("endpoint must not be empty".into());
    }
    if command.trim().is_empty() {
        return Err("command is required".into());
    }
    let env = headers::pairs(check.text_of("env"));
    Ok(json!({"argv": argv, "command": command, "env": env}))
}

/// The request of a [`plan`] whose variables are resolved: `#env` values are quoted only
/// now, so a value is never resolved inside the quotes.
pub fn resolved(plan: &Value) -> Result<Request, String> {
    #[derive(Deserialize)]
    struct Plan {
        argv: Vec<String>,
        command: String,
        env: Vec<(String, String)>,
    }
    let plan = Plan::deserialize(plan).map_err(|error| error.to_string())?;
    let Some((program, args)) = plan.argv.split_first() else {
        return Err("endpoint must not be empty".into());
    };
    let mut request = Request::new(program, &[]);
    request.args = args.to_vec();
    request.args.push(build_command(&plan.command, &plan.env));
    Ok(request)
}

/// OK for exit 0, WARNING when it also wrote to stderr or output went missing, else ERROR.
pub fn outcome(exit: &Exit) -> &'static str {
    let stderr = exit.stderr.as_deref().unwrap_or("");
    match exit.code {
        Some(0) if exit.output_incomplete || exit.truncated || !stderr.trim().is_empty() => {
            "WARNING"
        }
        Some(0) => "OK",
        _ => "ERROR",
    }
}

/// A driver answer as console text: stdout, then stderr, then why output may be missing.
/// A piece of a stream is its text.
pub fn render(response: &Value) -> Result<Rendered, String> {
    let rendered = |text: String, outcome: &str| Rendered {
        text,
        language: "text".into(),
        outcome: outcome.into(),
    };
    if let Ok(event) = process::Event::parse(response) {
        return Ok(match event {
            process::Event::Stdout(text) | process::Event::Stderr(text) => rendered(text, "OK"),
            _ => rendered(String::new(), "OK"),
        });
    }
    let exit = Exit::parse(response)?;
    let mut text = format!(
        "{}{}",
        exit.stdout.as_deref().unwrap_or(""),
        exit.stderr.as_deref().unwrap_or("")
    );
    warn(&exit, &mut text);
    Ok(rendered(text, outcome(&exit)))
}

/// Appends why output may be missing.
fn warn(exit: &Exit, text: &mut String) {
    for (missing, warning) in [
        (exit.truncated, TRUNCATED),
        (exit.output_incomplete, INCOMPLETE),
    ] {
        if missing {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str("Warning: ");
            text.push_str(warning);
        }
    }
}

/// The part of a check a line is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum Area {
    Other,
    Shell,
    Env,
    Response,
}

impl Area {
    fn of(byte: Option<&u8>) -> Self {
        match byte {
            Some(1) => Area::Shell,
            Some(2) => Area::Env,
            Some(3) => Area::Response,
            _ => Area::Other,
        }
    }
}

/// Tokens of a marker line: the marker as a comment; the rest of an `#endpoint` line as
/// shell, of other lines as part of the comment.
fn marker_tokens(line: &str, name: &str, rest: &str, quote: &mut shell::State) -> Vec<Token> {
    let head = line.len() - line.trim_start().len() + 1 + name.len();
    if name != "endpoint" || rest.is_empty() {
        return vec![Token {
            start: 0,
            len: line.trim_end().len() as u32,
            kind: "comment".into(),
        }];
    }
    let at = (line.trim_end().len() - rest.len()) as u32;
    let mut tokens = vec![Token {
        start: 0,
        len: head as u32,
        kind: "comment".into(),
    }];
    tokens.extend(shell::tokens(rest, quote).into_iter().map(|t| Token {
        start: t.start + at,
        ..t
    }));
    tokens
}

/// Highlighting of a Terminal check: the endpoint and command as shell, `#env` as pairs.
/// The output stays plain.
pub fn highlight(state: &[u8], lines: &[String]) -> Vec<Line> {
    let mut area = Area::of(state.first());
    let mut quote = shell::State::decode(state.get(1..).unwrap_or(&[]));
    lines
        .iter()
        .map(|line| {
            let tokens = match marker(line) {
                Some((name, rest))
                    if area != Area::Response
                        || matches!(name.as_str(), "responce" | "response") =>
                {
                    quote = shell::State::default();
                    area = match name.as_str() {
                        "endpoint" | "command" => Area::Shell,
                        "env" => Area::Env,
                        "responce" | "response" => Area::Response,
                        _ => Area::Other,
                    };
                    marker_tokens(line, &name, rest, &mut quote)
                }
                _ => match area {
                    Area::Shell => shell::tokens(line, &mut quote),
                    Area::Env => headers::tokens(line),
                    Area::Other | Area::Response => vec![],
                },
            };
            let mut state = vec![area as u8];
            quote.encode(&mut state);
            Line { tokens, state }
        })
        .collect()
}

/// Completion in a Terminal check: markers, endpoint shells and `#env` names.
pub fn complete(check: &Check, pos: usize) -> Completions {
    let at = At::new(&check.text, pos);
    if at.in_placeholder() || at.in_response() {
        return at.none();
    }
    if let Some(markers) = at.markers(&SECTIONS) {
        return markers;
    }
    let line_start = at.line_start(at.line);
    let first = at.section.as_ref().is_some_and(|(i, _)| at.line == i + 1);
    match at.name() {
        Some("endpoint") if first => {
            let value = at.before.trim_start();
            let from = line_start + at.before.len() - value.len();
            let items = ENDPOINTS
                .iter()
                .map(|text| item(*text, "Endpoint", "keyword"))
                .collect();
            finish(from, at.pos, value, items)
        }
        Some("endpoint") => at.next_section(&SECTIONS).unwrap_or_else(|| at.none()),
        Some("env") => {
            let used = headers::used(at.other_lines());
            let items = headers::items(&ENV, &[], &used);
            let mut result = finish(line_start, at.pos, at.before.trim(), items);
            if at.before.trim().is_empty() && !used.is_empty() {
                let mut items = finish(line_start, at.pos, "", at.unused(&SECTIONS)).items;
                items.append(&mut result.items);
                result.items = items;
            }
            result
        }
        _ => at.none(),
    }
}

/// Characters of a command's last stderr line shown in its status.
const STATUS_STDERR_CHARS: usize = 160;

/// The run's status: a failed exit's code or signal, then the last line the command wrote
/// to stderr (also after a successful exit, where stderr makes it a warning).
pub fn exit_summary(exit: &Exit, stderr: &str) -> Option<String> {
    let mut parts = match (exit.code, exit.signal.as_deref()) {
        (Some(0), _) => vec![],
        (Some(code), _) => vec![format!("exit {code}")],
        (None, Some(signal)) => vec![signal.to_owned()],
        (None, None) => vec![],
    };
    if let Some(line) = stderr.lines().map(str::trim).rfind(|line| !line.is_empty()) {
        parts.push(if line.chars().count() > STATUS_STDERR_CHARS {
            let mut short: String = line.chars().take(STATUS_STDERR_CHARS - 1).collect();
            short.push('…');
            short
        } else {
            line.to_owned()
        });
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// A command running: its output as it came, stdout and stderr interleaved.
#[derive(Debug, Default)]
struct Running {
    request: Value,
    output: String,
    stderr: String,
    exit: Option<Exit>,
}

impl Running {
    fn finish(self, run: &str) -> Vec<Cmd> {
        let Some(mut exit) = self.exit else {
            let debug = json!({"request": self.request});
            return vec![cmd::fail(
                run,
                "The command ended without an exit status",
                &debug,
            )];
        };
        exit.stderr = Some(self.stderr.clone());
        // The exit's fields stay at the top, where Debug shows them unfolded.
        let mut debug = json!(exit);
        debug["request"] = self.request;
        let mut text = self.output;
        warn(&exit, &mut text);
        let rendered = Rendered {
            text,
            language: "text".into(),
            outcome: outcome(&exit).into(),
        };
        let mut cmds: Vec<Cmd> = exit_summary(&exit, &self.stderr)
            .iter()
            .map(|status| cmd::run_status(run, status))
            .collect();
        cmds.push(cmd::result(run, &rendered, &debug));
        cmds
    }
}

#[derive(Debug, Default)]
pub struct Terminal {
    runs: BTreeMap<String, Running>,
}

impl Terminal {
    /// The run and the step (`resolve`, `call`) of a reply `tag`.
    fn owner(&self, tag: &str) -> Option<(String, String)> {
        self.runs.keys().find_map(|run| {
            let step = tag.strip_prefix(run.as_str())?.strip_prefix(':')?;
            Some((run.clone(), step.to_owned()))
        })
    }

    fn start(&mut self, run: String, check: &Check) -> Vec<Cmd> {
        match plan(check) {
            Ok(plan) => {
                self.runs.insert(run.clone(), Running::default());
                vec![run::resolve_value(format!("{run}:resolve"), &plan)]
            }
            Err(error) => vec![cmd::fail(&run, &error, &Value::Null)],
        }
    }

    fn reply(&mut self, run: &str, step: &str, result: Result<Value, Failure>) -> Vec<Cmd> {
        let Some(running) = self.runs.get_mut(run) else {
            return vec![];
        };
        match (step, result) {
            ("resolve", result) => {
                match run::resolved_value(result).and_then(|plan| resolved(&plan)) {
                    Ok(request) => {
                        let call = request.call().streaming();
                        running.request = call.request();
                        vec![cmd::driver(format!("{run}:call"), &call)]
                    }
                    Err(error) => {
                        self.runs.remove(run);
                        vec![cmd::fail(run, &error, &Value::Null)]
                    }
                }
            }
            ("call", Ok(value)) => match process::Event::parse(&value) {
                Ok(process::Event::Stdout(text)) => {
                    running.output.push_str(&text);
                    vec![cmd::output(run, &text)]
                }
                Ok(process::Event::Stderr(text)) => {
                    running.output.push_str(&text);
                    running.stderr.push_str(&text);
                    vec![cmd::output(run, &text)]
                }
                Ok(_) => vec![],
                Err(_) => match Exit::parse(&value) {
                    Ok(exit) => {
                        running.exit = Some(exit);
                        vec![]
                    }
                    Err(error) => {
                        let debug = json!({"request": running.request, "response": value});
                        self.runs.remove(run);
                        vec![cmd::fail(run, &error, &debug)]
                    }
                },
            },
            ("call", Err(failure)) => {
                let debug = json!({"request": running.request, "reply": failure.details()});
                self.runs.remove(run);
                vec![cmd::fail(run, &failure.message, &debug)]
            }
            _ => vec![],
        }
    }
}

impl Module for Terminal {
    type Msg = ();

    fn update(&mut self, event: Event<()>) -> Vec<Cmd> {
        match event {
            Event::Run { tag, check } => self.start(tag, &check),
            Event::Reply { tag, result } => match self.owner(&tag) {
                Some((run, step)) => self.reply(&run, &step, result),
                None => vec![],
            },
            Event::End { tag } => match self.owner(&tag) {
                Some((run, step)) if step == "call" => self
                    .runs
                    .remove(&run)
                    .map(|running| running.finish(&run))
                    .unwrap_or_default(),
                _ => vec![],
            },
            Event::Cancel { tag } => {
                self.runs.remove(&tag);
                vec![]
            }
            _ => vec![],
        }
    }

    fn subscriptions(&self) -> Subscriptions {
        Subscriptions {
            run: true,
            ..Subscriptions::default()
        }
    }

    fn highlight(&self, state: &[u8], lines: &[String]) -> Vec<Line> {
        highlight(state, lines)
    }

    fn complete(&self, check: &Check, pos: u32) -> Completions {
        complete(check, pos as usize)
    }
}

services_sdk::export!(Terminal);
