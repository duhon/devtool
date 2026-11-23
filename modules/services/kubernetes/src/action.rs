//! The `#action` (a kubectl-style command) and the runs of the module: they resolve the check's
//! variables, learn the server's resources once per context, and then send the batches of driver
//! calls of the action's [`flow::Flow`] until it has a result to show.
//!
//! Driver calls of a run have the reply tags `{run}:call:<n>`; `{run}:resolve` is the
//! resolution of variables.

mod flow;
mod logs;
mod read;
mod text;
mod write;

use crate::config::{self, Config};
use crate::render::{self, Display};
use crate::resources::{self, Parsed, Resource};
use crate::target::{self, Target};
use flow::{Answer, Flow, Step, status_error};
use serde_json::{Value, json};
use services_sdk::kubernetes::{Event as StreamEvent, Request, Response};
use services_sdk::{Check, Cmd, Failure, cmd, run};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

pub const RESOLVE: &str = ":resolve";
pub const CALL: &str = ":call";
/// How long the resources of a context are kept.
const CATALOG_MS: f64 = 300_000.0;

/// The commands of the module, with what each takes.
pub const COMMANDS: [(&str, &str); 18] = [
    ("get", "get <resource>[/<name>]"),
    ("watch", "watch <resource>[/<name>]"),
    ("describe", "describe <resource>/<name>"),
    ("logs", "logs <pod or workload>/<name>"),
    ("events", "events [<resource>/<name>]"),
    ("top", "top pods|nodes"),
    ("api-resources", "api-resources"),
    ("auth can-i", "auth can-i <verb> <resource>[/<name>]"),
    ("rollout status", "rollout status <resource>/<name>"),
    ("rollout history", "rollout history deployment/<name>"),
    ("rollout restart", "rollout restart <resource>/<name>"),
    ("rollout undo", "rollout undo deployment/<name>"),
    ("scale", "scale <resource>/<name>"),
    ("apply", "apply"),
    ("create", "create"),
    ("patch", "patch <resource>/<name>"),
    ("delete", "delete <resource>/<name>"),
    ("label", "label <resource>/<name> <key>=<value>… or <key>-"),
];
const UNSUPPORTED: [&str; 5] = ["exec", "cp", "edit", "port-forward", "attach"];

/// A parsed `#action`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Action {
    /// The command: `get`, `rollout status`, `auth can-i`, `api-resources`…
    pub verb: String,
    /// What the user typed for the resource (`deploy`, `pods`, `deployment.apps`); empty when the
    /// command takes none.
    pub resource: String,
    pub name: Option<String>,
    /// The `k=v` and `k-` of `label` and `annotate`; the verb of `auth can-i`.
    pub args: Vec<String>,
}

/// A word of a line with its byte offset.
pub type Word<'a> = (usize, &'a str);

/// The words of `line` with their offsets.
pub fn words(line: &str) -> Vec<Word<'_>> {
    let mut found = Vec::new();
    let mut start = None;
    for (at, c) in line.char_indices() {
        match (c.is_whitespace(), start) {
            (false, None) => start = Some(at),
            (true, Some(from)) => {
                found.push((from, &line[from..at]));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(from) = start {
        found.push((from, &line[from..]));
    }
    found
}

fn known(verb: &str) -> bool {
    verb == "annotate" || COMMANDS.iter().any(|(name, _)| *name == verb)
}

/// Whether the verb is followed by a `resource[/name]`; `apply` and `create` take an optional
/// resource word, too.
pub fn takes_resource(verb: &str) -> bool {
    known(verb) && verb != "api-resources"
}

/// Whether the object may also be named as the word after the resource (`get pods web`).
pub fn takes_spaced_name(verb: &str) -> bool {
    takes_resource(verb) && !matches!(verb, "auth can-i" | "apply" | "create")
}

/// An `#action` line read leniently, the way both the run ([`parse`]) and the editor read it: the
/// editor takes what is typed so far, the run then refuses what is incomplete.
#[derive(Debug, Default)]
pub struct Shape<'a> {
    /// The command in lower case: `get`, `rollout status`; one word until the second is typed.
    pub verb: String,
    pub verb_words: usize,
    /// The words after the verb.
    pub rest: Vec<Word<'a>>,
    /// The resource word, without the `/name`.
    pub resource: Option<Word<'a>>,
    /// The object name, after the `/` or as the next word. `None` while it is not typed.
    pub name: Option<Word<'a>>,
    /// The word holding the resource and the `/name`.
    pub object: Option<Word<'a>>,
    /// The resource word has a `/`, typed name or not.
    pub slash: bool,
    /// The `k=v` and `k-` of `label` and `annotate`; the verb of `auth can-i`.
    pub args: Vec<Word<'a>>,
}

/// The `resource/name` split of one word.
fn split_object(word: Word<'_>) -> (Option<Word<'_>>, Option<Word<'_>>, bool) {
    let (start, text) = word;
    let (resource, name) = match text.split_once('/') {
        Some((resource, name)) => (resource, Some((start + resource.len() + 1, name))),
        None => (text, None),
    };
    (
        Some((start, resource)).filter(|(_, text)| !text.is_empty()),
        name.filter(|(_, text)| !text.is_empty()),
        text.contains('/'),
    )
}

pub fn shape(line: &str) -> Shape<'_> {
    let all = words(line);
    let Some((_, first)) = all.first() else {
        return Shape::default();
    };
    let first = first.to_ascii_lowercase();
    let (verb, rest) = match all.get(1) {
        Some((_, second)) if matches!(first.as_str(), "rollout" | "auth") => (
            format!("{first} {}", second.to_ascii_lowercase()),
            &all[2..],
        ),
        _ => (first, &all[1..]),
    };
    let mut shape = Shape {
        verb_words: verb.split(' ').count(),
        rest: rest.to_vec(),
        ..Shape::default()
    };
    if takes_resource(&verb) {
        match verb.as_str() {
            "apply" | "create" => shape.resource = rest.first().copied(),
            "auth can-i" => {
                shape.args = rest.iter().take(1).copied().collect();
                if let Some(word) = rest.get(1) {
                    shape.object = Some(*word);
                    (shape.resource, shape.name, shape.slash) = split_object(*word);
                }
            }
            _ => {
                if let Some(word) = rest.first() {
                    shape.object = Some(*word);
                    (shape.resource, shape.name, shape.slash) = split_object(*word);
                    let spaced = rest.get(1).copied().filter(|_| !shape.slash);
                    shape.name = shape.name.or(spaced);
                }
                if matches!(verb.as_str(), "label" | "annotate") {
                    let from = if shape.slash { 1 } else { 2 };
                    shape.args = rest.get(from..).unwrap_or_default().to_vec();
                }
            }
        }
    }
    shape.verb = verb;
    shape
}

/// The object of `shape` as the run needs it: both parts when it has a `/`.
fn object(shape: &Shape) -> Result<(String, Option<String>), String> {
    let text = |word: Option<Word>| word.map(|(_, text)| text.to_owned());
    if shape.slash {
        let bad = |shown: String| format!("Invalid object {shown:?}; use <resource>/<name>");
        let (Some(resource), Some(name)) = (text(shape.resource), text(shape.name)) else {
            let token = shape.object.map_or("", |(_, token)| token);
            return Err(bad(token.to_owned()));
        };
        if name.contains('/') {
            return Err(bad(format!("{resource}/{name}")));
        }
        return Ok((resource, Some(name)));
    }
    Ok((text(shape.resource).unwrap_or_default(), text(shape.name)))
}

/// The resource and the one name of the object: `deploy/feed` or `deploy feed`.
fn target_of(shape: &Shape) -> Result<(String, Option<String>), String> {
    let verb = &shape.verb;
    if shape.rest.is_empty() {
        return Err(format!(
            "{verb} needs a resource: {verb} <resource>[/<name>]"
        ));
    }
    let own = if shape.slash { 1 } else { 2 };
    if shape.rest.len() > own {
        let tokens: Vec<&str> = shape.rest.iter().map(|(_, text)| *text).collect();
        return Err(format!(
            "{verb} takes one object, not several: {}",
            tokens.join(" ")
        ));
    }
    object(shape)
}

pub fn parse(text: &str) -> Result<Action, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("The action is empty; write a command like get pods".into());
    }
    if text.contains('\n') {
        return Err("The action is one line: a command and its resource".into());
    }
    let first = text.split_whitespace().next().unwrap_or_default();
    let first = first.to_ascii_lowercase();
    if UNSUPPORTED.contains(&first.as_str()) {
        return Err(format!(
            "{first} is interactive and not supported here; use a Terminal with kubectl"
        ));
    }
    let shape = shape(text);
    let verb = shape.verb.clone();
    if matches!(first.as_str(), "rollout" | "auth") {
        if shape.verb_words < 2 {
            return Err(format!("{first} needs a subcommand"));
        }
        if !known(&verb) {
            return Err(format!("Unknown command {verb}"));
        }
    } else if !known(&verb) {
        return Err(format!("Unknown command {first:?}"));
    }
    let rest = &shape.rest;
    let mut action = Action {
        verb: verb.clone(),
        ..Action::default()
    };
    match verb.as_str() {
        "api-resources" => {
            if !rest.is_empty() {
                return Err("api-resources takes no arguments".into());
            }
        }
        "apply" | "create" => {
            if rest.len() > 1 {
                return Err(format!("{verb} takes the objects from #payload"));
            }
            action.resource = rest
                .first()
                .map_or_else(String::new, |(_, kind)| (*kind).to_owned());
        }
        "events" => {
            if !rest.is_empty() {
                (action.resource, action.name) = target_of(&shape)?;
            }
        }
        "auth can-i" => {
            let (Some((_, what)), Some(_)) = (rest.first(), rest.get(1)) else {
                return Err("auth can-i <verb> <resource>[/<name>]".into());
            };
            if rest.len() > 2 {
                return Err("auth can-i takes a verb and one resource".into());
            }
            action.args = vec![(*what).to_owned()];
            (action.resource, action.name) = object(&shape)?;
        }
        "label" | "annotate" => {
            if rest.is_empty() {
                return Err(format!("{verb} <resource>/<name> <key>=<value>…"));
            }
            let (resource, name) = object(&shape)?;
            if shape.args.is_empty() {
                return Err(format!(
                    "{verb} needs <key>=<value> or <key>- after the object"
                ));
            }
            for (_, pair) in &shape.args {
                let valid = pair.contains('=') && !pair.starts_with('=')
                    || pair.len() > 1 && pair.ends_with('-');
                if !valid {
                    return Err(format!(
                        "Invalid {verb} {pair:?}; use <key>=<value> or <key>-"
                    ));
                }
            }
            action.resource = resource;
            action.name = name;
            action.args = shape
                .args
                .iter()
                .map(|(_, pair)| (*pair).to_owned())
                .collect();
        }
        _ => {
            (action.resource, action.name) = target_of(&shape)?;
        }
    }
    if let Some(name) = &action.name {
        resources::check_name(name)?;
    }
    let name_needed = matches!(
        verb.as_str(),
        "describe"
            | "scale"
            | "patch"
            | "label"
            | "annotate"
            | "rollout status"
            | "rollout history"
            | "rollout restart"
            | "rollout undo"
    );
    if name_needed && action.name.is_none() {
        return Err(format!(
            "{verb} needs one object: {verb} {}/<name>",
            action.resource
        ));
    }
    Ok(action)
}

/// What a run knows once its sections are resolved.
struct Setup {
    target: Target,
    action: Action,
    config: Config,
    payload: String,
    display: Rc<Display>,
}

fn setup(check: &Check) -> Result<Setup, String> {
    let target = target::parse(check.text_of("target"))?;
    let action = parse(check.text_of("action"))?;
    let config = config::parse(check.text_of("metadata"), check.text_of("options"))?;
    config.validate(&action.verb)?;
    let display = Display::new(&config.options)?;
    if matches!(action.verb.as_str(), "apply" | "create") {
        let objects = crate::payload::objects(check.text_of("payload"), &target.namespace)?;
        if objects.is_empty() {
            return Err(format!("{} needs the objects in #payload", action.verb));
        }
    }
    Ok(Setup {
        target,
        action,
        config,
        payload: check.text_of("payload").to_owned(),
        display: Rc::new(display),
    })
}

/// What a run waits for.
enum Stage {
    Resolving(Check),
    /// The first or second batch of discovery.
    Discovering(Vec<Part>),
    Flow(Box<Flow>),
}

/// A piece of the resources: known, or the legacy paths to fetch.
enum Part {
    Known(Vec<Resource>),
    Fetch(Vec<String>),
}

struct Job {
    stage: Stage,
    setup: Option<Setup>,
    /// The next call number.
    counter: u32,
    /// The slots of the open batch: call number → index in `answers`.
    slots: BTreeMap<u32, usize>,
    answers: Vec<Option<Answer>>,
    requests: Vec<Request>,
    /// The calls and answers shown as the run's debug info.
    debug: Vec<Value>,
    catalog: Option<Rc<Vec<Resource>>>,
}

impl Job {
    fn new(stage: Stage) -> Self {
        Self {
            stage,
            setup: None,
            counter: 0,
            slots: BTreeMap::new(),
            answers: Vec::new(),
            requests: Vec::new(),
            debug: Vec::new(),
            catalog: None,
        }
    }

    fn open(&mut self, run: &str, requests: Vec<Request>) -> Vec<Cmd> {
        self.slots.clear();
        self.answers = requests.iter().map(|_| None).collect();
        let mut cmds = Vec::new();
        for (slot, request) in requests.iter().enumerate() {
            self.counter += 1;
            self.slots.insert(self.counter, slot);
            let call = if request.stream {
                request.call().streaming()
            } else {
                request.call()
            };
            cmds.push(cmd::driver(format!("{run}{CALL}:{}", self.counter), &call));
        }
        self.requests = requests;
        cmds
    }
}

#[derive(Default)]
pub struct Runs {
    jobs: BTreeMap<String, Job>,
    catalogs: BTreeMap<String, (f64, Rc<Vec<Resource>>)>,
    /// The runs with a `Transform`: their data goes to it before it is shown.
    transforms: BTreeSet<String>,
    /// Display, outcome and notes of the runs waiting for their data back.
    shaping: run::Shaping<(Rc<Display>, &'static str, Vec<String>)>,
}

fn failed(run: &str, job: &Job, error: &str) -> Vec<Cmd> {
    vec![cmd::fail(run, error, &Value::Array(job.debug.clone()))]
}

impl Runs {
    pub fn run(&mut self, run: String, check: Check, transform: bool) -> Vec<Cmd> {
        if transform {
            self.transforms.insert(run.clone());
        }
        let sections = ["target", "action", "metadata", "options", "payload"];
        if !sections
            .iter()
            .any(|name| check.text_of(name).contains("{{"))
            && let Err(error) = setup(&check)
        {
            return vec![cmd::fail(&run, &error, &Value::Null)];
        }
        let resolving = json!({
            "target": check.text_of("target").trim(), "action": check.text_of("action"),
            "metadata": check.text_of("metadata"), "options": check.text_of("options"),
            "payload": check.text_of("payload")});
        self.jobs
            .insert(run.clone(), Job::new(Stage::Resolving(check)));
        vec![run::resolve_value(format!("{run}{RESOLVE}"), &resolving)]
    }

    pub fn cancel(&mut self, run: &str) -> Vec<Cmd> {
        self.jobs.remove(run);
        self.transforms.remove(run);
        self.shaping.cancel(run);
        vec![]
    }

    pub fn end(&mut self, tag: &str) -> Vec<Cmd> {
        let Some((run, number)) = call_of(tag) else {
            return vec![];
        };
        self.landed(run, number, None)
    }

    pub fn reply(&mut self, tag: &str, result: Result<Value, Failure>) -> Vec<Cmd> {
        if let Some(run) = tag.strip_suffix(RESOLVE) {
            return self.resolved(run, result);
        }
        if self.shaping.owns(tag) {
            let shown = self
                .shaping
                .reply(tag, result, |(display, outcome, notes), data| {
                    let (rendered, status) = render::shaped(&display, data, outcome, &notes)?;
                    Ok(run::Answer {
                        rendered,
                        status: Some(status),
                    })
                });
            return shown.unwrap_or_default();
        }
        let Some((run, number)) = call_of(tag) else {
            return vec![];
        };
        self.landed(run, number, Some(result))
    }

    fn resolved(&mut self, run: &str, result: Result<Value, Failure>) -> Vec<Cmd> {
        let Some(Job {
            stage: Stage::Resolving(check),
            ..
        }) = self.jobs.remove(run)
        else {
            return vec![];
        };
        let sections = match run::resolved_value(result) {
            Ok(sections) => run::with_sections(&check, &sections),
            Err(error) => return vec![cmd::fail(run, &error, &Value::Null)],
        };
        let setup = match setup(&sections) {
            Ok(setup) => setup,
            Err(error) => return vec![cmd::fail(run, &error, &Value::Null)],
        };
        let mut job = Job::new(Stage::Discovering(Vec::new()));
        let context = setup.target.context.clone();
        let cached = self
            .catalogs
            .get(&context)
            .filter(|(at, _)| services_sdk::now() - at < CATALOG_MS)
            .map(|(_, catalog)| catalog.clone());
        job.setup = Some(setup);
        let cmds = match cached {
            Some(catalog) => {
                job.catalog = Some(catalog.clone());
                self.begin(run, &mut job, catalog)
            }
            None => job.open(run, resources::requests(&context)),
        };
        if !job.slots.is_empty() {
            self.jobs.insert(run.to_owned(), job);
        }
        cmds
    }

    /// Starts the flow of `job` over `catalog`.
    fn begin(&mut self, run: &str, job: &mut Job, catalog: Rc<Vec<Resource>>) -> Vec<Cmd> {
        let setup = job.setup.as_ref().expect("a setup");
        let started = Flow::new(
            &setup.target,
            &setup.action,
            &setup.config,
            &setup.payload,
            catalog,
            services_sdk::now(),
            self.transforms.contains(run),
        );
        match started {
            Ok((flow, step)) => {
                job.stage = Stage::Flow(Box::new(flow));
                self.advance(run, job, step)
            }
            Err(error) => {
                job.stage = Stage::Discovering(Vec::new());
                job.requests.clear();
                job.slots.clear();
                failed(run, job, &error)
            }
        }
    }

    fn advance(&mut self, run: &str, job: &mut Job, step: Step) -> Vec<Cmd> {
        match step {
            Step::Calls(requests) => job.open(run, requests),
            Step::Done(done) => {
                job.requests.clear();
                job.slots.clear();
                let display = job.setup.as_ref().expect("a setup").display.clone();
                if self.transforms.remove(run) {
                    let (data, notes) = render::data(&display, &done.content);
                    let debug = Value::Array(job.debug.clone());
                    let keep = (display, done.outcome, notes);
                    return vec![self.shaping.hand(run, &data, debug, keep)];
                }
                match render::render(&display, done.content, done.outcome) {
                    Ok((rendered, status)) => vec![
                        cmd::run_status(run, &status),
                        cmd::result(run, &rendered, &Value::Array(job.debug.clone())),
                    ],
                    Err(error) => failed(
                        run,
                        job,
                        &format!("The response could not be shown: {error}"),
                    ),
                }
            }
        }
    }

    fn landed(
        &mut self,
        run: &str,
        number: u32,
        result: Option<Result<Value, Failure>>,
    ) -> Vec<Cmd> {
        let Some(mut job) = self.jobs.remove(run) else {
            return vec![];
        };
        let Some(&slot) = job.slots.get(&number) else {
            self.jobs.insert(run.to_owned(), job);
            return vec![];
        };
        let streamed = job.requests.get(slot).is_some_and(|request| request.stream);
        let mut cmds = Vec::new();
        match result {
            Some(Err(failure)) => {
                job.debug
                    .push(json!({"request": job.requests.get(slot), "reply": failure.details()}));
                return vec![cmd::fail(
                    run,
                    &failure.message,
                    &Value::Array(job.debug.clone()),
                )];
            }
            Some(Ok(value)) if streamed => {
                let answer = job.answers[slot].get_or_insert_with(Answer::default);
                if let Ok(event) = StreamEvent::parse(&value) {
                    cmds.extend(live(&mut job, slot, &event, run));
                    if let Some(answer) = job.answers[slot].as_mut() {
                        answer.events.push(event);
                    }
                } else if let Ok(response) = Response::parse(&value) {
                    answer.response = response;
                }
                self.jobs.insert(run.to_owned(), job);
                return cmds;
            }
            Some(Ok(value)) => match Response::parse(&value) {
                Ok(response) => {
                    job.debug
                        .push(json!({"request": job.requests.get(slot), "response": value}));
                    job.answers[slot] = Some(Answer {
                        response,
                        events: Vec::new(),
                    });
                }
                Err(error) => return failed(run, &job, &error),
            },
            None => {
                let answer = job.answers[slot].get_or_insert_with(Answer::default);
                let shown = json!({"status": answer.response.status, "events": answer.events.len(),
                    "error": answer.response.error});
                job.debug
                    .push(json!({"request": job.requests.get(slot), "response": shown}));
            }
        }
        job.slots.remove(&number);
        if !job.slots.is_empty() {
            self.jobs.insert(run.to_owned(), job);
            return cmds;
        }
        let answers: Vec<Answer> = job
            .answers
            .drain(..)
            .map(Option::unwrap_or_default)
            .collect();
        let cmds = self.settled(run, &mut job, answers);
        if !job.slots.is_empty() {
            self.jobs.insert(run.to_owned(), job);
        }
        cmds
    }

    /// The commands after a whole batch has answered.
    fn settled(&mut self, run: &str, job: &mut Job, answers: Vec<Answer>) -> Vec<Cmd> {
        match std::mem::replace(&mut job.stage, Stage::Discovering(Vec::new())) {
            Stage::Flow(mut flow) => {
                let step = flow.next(answers);
                job.stage = Stage::Flow(flow);
                match step {
                    Ok(step) => self.advance(run, job, step),
                    Err(error) => failed(run, job, &error),
                }
            }
            Stage::Discovering(parts) => self.discovered(run, job, parts, answers),
            Stage::Resolving(_) => vec![],
        }
    }

    fn discovered(
        &mut self,
        run: &str,
        job: &mut Job,
        mut parts: Vec<Part>,
        answers: Vec<Answer>,
    ) -> Vec<Cmd> {
        let first = parts.is_empty();
        let mut fetch = Vec::new();
        if first {
            for (index, answer) in answers.iter().enumerate() {
                match &answer.response.error {
                    Some(_) if index > 0 => continue,
                    _ => {}
                }
                if let Some(error) = status_error(&answer.response) {
                    if index == 0 {
                        return failed(run, job, &error);
                    }
                    continue;
                }
                match resources::parse(&answer.response.body) {
                    Ok(Parsed::Resources(found)) => parts.push(Part::Known(found)),
                    Ok(Parsed::Legacy(paths)) => parts.push(Part::Fetch(paths)),
                    Err(error) => return failed(run, job, &error),
                }
            }
            for part in &parts {
                if let Part::Fetch(paths) = part {
                    fetch.extend(paths.iter().cloned());
                }
            }
            if !fetch.is_empty() {
                let context = job.setup.as_ref().expect("a setup").target.context.clone();
                let requests = fetch
                    .iter()
                    .map(|path| Request::get(&context, path))
                    .collect();
                job.stage = Stage::Discovering(parts);
                return job.open(run, requests);
            }
        }
        let mut lists = answers.iter().map(|answer| {
            if answer.response.status < 300 && answer.response.error.is_none() {
                resources::parse_list(&answer.response.body)
            } else {
                Vec::new()
            }
        });
        let mut catalog = Vec::new();
        for part in parts {
            match part {
                Part::Known(found) => catalog.extend(found),
                Part::Fetch(paths) if first => {
                    let _ = paths;
                }
                Part::Fetch(paths) => {
                    for _ in paths {
                        catalog.extend(lists.next().unwrap_or_default());
                    }
                }
            }
        }
        let catalog = Rc::new(catalog);
        let context = job.setup.as_ref().expect("a setup").target.context.clone();
        self.catalogs
            .insert(context, (services_sdk::now(), catalog.clone()));
        job.catalog = Some(catalog.clone());
        self.begin(run, job, catalog)
    }
}

/// The text a streamed event shows while a run goes on.
fn live(job: &mut Job, slot: usize, event: &StreamEvent, run: &str) -> Vec<Cmd> {
    let Stage::Flow(flow) = &mut job.stage else {
        return vec![];
    };
    let text = match event {
        StreamEvent::Line(line) if flow.action.verb == "logs" => Some(flow.live_log(slot, line)),
        _ => flow.live(event),
    };
    match text {
        Some(text) if job.setup.as_ref().is_none_or(|s| s.display.keeps(&text)) => {
            vec![cmd::output(run, &format!("{text}\n"))]
        }
        _ => vec![],
    }
}

/// The run and call number of a call tag `{run}:call:<n>` (or `{run}:call`, call 1).
fn call_of(tag: &str) -> Option<(&str, u32)> {
    if let Some((run, number)) = tag.rsplit_once(":call:") {
        return Some((run, number.parse().ok()?));
    }
    tag.strip_suffix(CALL).map(|run| (run, 1))
}
