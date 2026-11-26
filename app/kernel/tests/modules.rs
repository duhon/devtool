use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use services_kernel::document;
use services_kernel::modules::{Effect, Instance, Modules, registry};
use services_sdk::wire::{self, event};
use services_sdk::{
    Check, Chunk, Cmd, DriverCall, Event, Failure, Linked, Manifest, Module, Rendered,
    Subscriptions, cmd, http, tcp, ui,
};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::rc::Rc;

fn manifest(id: &str, kind: &str, order: u32) -> Manifest {
    Manifest::parse(&format!(
        r#"{{"id":"{id}","kind":"{kind}","name":"{id}","version":"1.0.0","abi":{},"app":">=3.0","drivers":{{"http":"1"}},"order":{order}}}"#,
        services_sdk::ABI
    ))
    .unwrap()
}

fn drivers() -> BTreeMap<String, String> {
    BTreeMap::from([("http".to_owned(), "1".to_owned())])
}

/// The App links its modules before the Kernel orders panels; tests link only Settings' slot.
fn modules() -> Modules {
    registry::link(registry::Link {
        builtin: || {
            (
                manifest("settings", "panel", 0),
                Box::new(Linked::<Later>::default()),
            )
        },
        linked: Vec::new,
    });
    Modules::new("3.7.0", drivers(), Box::new(|| 0.0))
}

/// A service: `#target` URL, `#action` method (GET), renders the status and body.
#[derive(Default)]
struct Rest;

impl Module for Rest {
    type Msg = ();
    fn build(&self, check: &Check) -> Result<DriverCall, String> {
        let url = check.text_of("target");
        if url.is_empty() {
            return Err("#target needs a URL".into());
        }
        let method = Some(check.text_of("action")).filter(|m| !m.is_empty());
        Ok(http::Request::new(method.unwrap_or("GET"), url).call())
    }
    fn render(&self, chunk: &Chunk) -> Result<Rendered, String> {
        let response = http::Response::parse(&chunk.response())?;
        Ok(Rendered {
            text: format!("{} {}", response.status, response.body.unwrap_or_default()),
            language: "text".into(),
            outcome: "OK".into(),
            columns: Vec::new(),
        })
    }
}

/// A service that discovered a schema for one address only.
#[derive(Default)]
struct Discovered;

impl Module for Discovered {
    type Msg = ();
    fn schema(&self, check: &Check) -> Option<Value> {
        (check.text_of("target") == "known").then(|| json!({"types": {"Pet": {}}}))
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
enum PanelMsg {
    Fetch,
    Rename,
}

/// A panel: fetches on a click, ticks a timer, rewrites `{{host}}` and records history.
#[derive(Default)]
struct Panel {
    shown: u32,
    body: String,
    name: String,
    errors: Vec<String>,
    ended: Vec<String>,
}

impl Module for Panel {
    type Msg = PanelMsg;
    fn update(&mut self, event: Event<PanelMsg>) -> Vec<Cmd> {
        match event {
            Event::Shown => {
                self.shown += 1;
                vec![cmd::timer("tick", 1000), cmd::log("info", "shown")]
            }
            Event::Ui {
                msg: PanelMsg::Fetch,
                ..
            } => vec![
                cmd::driver("fetch", &http::Request::new("GET", "https://x").call()),
                cmd::driver(
                    "live",
                    &http::Request::new("GET", "https://y").call().streaming(),
                ),
                Cmd::new("bad", "nope", &json!({})),
                Cmd::new("install", "modules.install", &json!({})),
                cmd::status_stoppable("fetching", "busy", &json!("stop"), "panel-stop"),
            ],
            Event::Ui {
                msg: PanelMsg::Rename,
                value,
            } => {
                self.name = value.unwrap_or_default();
                Vec::new()
            }
            Event::Reply { tag, result } => {
                match result {
                    Ok(value) if tag == "fetch" => {
                        self.body = value["body"].as_str().unwrap_or("").into()
                    }
                    Ok(_) => {}
                    Err(failure) => self.errors.push(format!("{tag}: {}", failure.message)),
                }
                Vec::new()
            }
            Event::End { tag } => {
                self.ended.push(tag);
                Vec::new()
            }
            _ => Vec::new(),
        }
    }
    fn view(&self) -> Option<ui::Node> {
        Some(ui::panel(
            "Panel",
            vec![
                ui::text(format!("{} {} {}", self.shown, self.body, self.name)),
                ui::text(self.errors.join("; ")),
                ui::text(self.ended.join(",")),
                ui::button("Fetch", &PanelMsg::Fetch),
                ui::input(None, "name", &PanelMsg::Rename),
            ],
            Vec::new(),
        ))
    }
    fn subscriptions(&self) -> Subscriptions {
        Subscriptions {
            requests: true,
            ..Subscriptions::default()
        }
    }
    fn before_request(&self, call: DriverCall) -> Result<DriverCall, String> {
        Ok(call)
    }
    fn after_response(&self, call: &DriverCall, response: &Value) -> Vec<Cmd> {
        vec![Cmd::new(
            "",
            "history.insert",
            &json!({"url": call.request()["url"], "status": response["status"]}),
        )]
    }
}

/// A second panel that runs after the first and only adds a header.
#[derive(Default)]
struct Later;

impl Module for Later {
    type Msg = ();
    fn before_request(&self, mut call: DriverCall) -> Result<DriverCall, String> {
        let mut request = call.request();
        request["headers"] = json!([["X-Seen", request["url"]]]);
        call.set_request(&request);
        Ok(call)
    }
}

fn texts(node: &ui::Node) -> Vec<String> {
    let Some(ui::node::Kind::PanelFrame(frame)) = &node.kind else {
        panic!("a panel frame")
    };
    frame
        .body
        .iter()
        .filter_map(|node| match &node.kind {
            Some(ui::node::Kind::Text(text)) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn handler(node: &ui::Node, index: usize) -> Vec<u8> {
    let Some(ui::node::Kind::PanelFrame(frame)) = &node.kind else {
        panic!("a panel frame")
    };
    match &frame.body[index].kind {
        Some(ui::node::Kind::Button(button)) => button.on_click.clone().unwrap(),
        Some(ui::node::Kind::Input(input)) => input.on_change.clone().unwrap(),
        other => panic!("no handler: {other:?}"),
    }
}

fn load(modules: &mut Modules) {
    let all: [(&str, &str, u32, Box<dyn Instance>); 3] = [
        ("rest", "service", 1, Box::new(Linked::<Rest>::default())),
        ("panel", "panel", 1, Box::new(Linked::<Panel>::default())),
        ("later", "panel", 2, Box::new(Linked::<Later>::default())),
    ];
    for (id, kind, order, instance) in all {
        modules
            .load(manifest(id, kind, order), instance, json!({}))
            .unwrap();
    }
}

#[test]
fn a_shown_panel_runs_its_commands_and_redraws_from_replies() {
    let mut modules = modules();
    load(&mut modules);
    assert!(modules.view("panel").is_none());

    let effects = modules.show("panel");
    assert!(effects.contains(&Effect::Timer {
        module: "panel".into(),
        tag: "tick".into(),
        ms: 1000
    }));
    let view = modules.view("panel").unwrap().clone();
    assert_eq!(texts(&view)[0], "1  ");
    assert_eq!(modules.get("panel").unwrap().log.back().unwrap().1, "shown");

    let effects = modules.ui("panel", handler(&view, 3), None);
    let drivers: Vec<_> = effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Driver {
                tag,
                request,
                stream,
                ..
            } => Some((tag.as_str(), request["headers"].clone(), *stream)),
            _ => None,
        })
        .collect();
    // Calls outside runs pass the panels' `before_request` too.
    assert_eq!(
        drivers,
        [
            ("fetch", json!([["X-Seen", "https://x"]]), false),
            ("live", json!([["X-Seen", "https://y"]]), true)
        ]
    );
    assert_eq!(
        modules.get("panel").unwrap().status,
        Some(services_kernel::Status {
            text: "fetching".into(),
            indicator: "busy".into(),
            detail: Value::Null,
            since: Some(0.0),
            stop: Some(services_kernel::Stop {
                msg: serde_json::to_vec(&json!("stop")).unwrap(),
                testid: "panel-stop".into(),
                disabled: false,
            }),
        })
    );
    let errors = &texts(modules.view("panel").unwrap())[1];
    assert!(errors.contains("bad: Unknown command \"nope\""), "{errors}");
    assert!(
        errors.contains("install: Only built-in modules"),
        "{errors}"
    );

    modules.reply("panel", "fetch", Ok(json!({"status": 200, "body": "hi"})));
    modules.reply("panel", "live", Ok(json!({"chunk": "a"})));
    modules.end("panel", "live");
    // The single call is done already: a late end changes nothing.
    assert!(modules.end("panel", "fetch").is_empty());
    let view = modules.view("panel").unwrap().clone();
    assert_eq!(texts(&view)[0], "1 hi ");
    assert_eq!(texts(&view)[2], "live");

    modules.ui("panel", handler(&view, 4), Some("renamed".into()));
    assert_eq!(texts(modules.view("panel").unwrap())[0], "1 hi renamed");
}

#[test]
fn a_hidden_panel_stops_its_work_and_gets_no_events() {
    let mut modules = modules();
    load(&mut modules);
    modules.show("panel");
    let view = modules.view("panel").unwrap().clone();
    modules.ui("panel", handler(&view, 3), None);

    let effects = modules.hide("panel");
    let cancelled: Vec<_> = effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Cancel { tag, .. } => Some(tag.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(cancelled, ["fetch", "live", "tick"]);
    assert!(modules.view("panel").is_none());

    let request = wire::Event {
        kind: Some(event::Kind::Request(wire::RequestEvent {
            id: "1".into(),
            service: "rest".into(),
            finished: true,
        })),
        now: 0.0,
    };
    assert!(modules.broadcast(request.clone()).is_empty());
    assert!(
        modules
            .reply("panel", "fetch", Ok(json!({"body": "late"})))
            .is_empty()
    );

    modules.show("panel");
    assert_eq!(texts(modules.view("panel").unwrap())[0], "2  ");
    assert!(modules.broadcast(request).contains(&Effect::Changed {
        module: "panel".into()
    }));
}

#[test]
fn the_request_pipeline_runs_the_service_then_panels_in_order() {
    let mut modules = modules();
    load(&mut modules);
    contribute(
        "host",
        json!({"first": [["var", {"host": "example.test"}]]}),
    );
    let check = document::check("rest", "#target https://{{:host}}/a", &Value::Null);
    let call = modules.prepare("rest", check).unwrap();
    assert_eq!(
        call.request()["headers"],
        json!([["X-Seen", "https://example.test/a"]])
    );

    let missing = document::check("rest", "#target https://{{:missing}}/", &Value::Null);
    let error = modules.prepare("rest", missing).unwrap_err();
    assert!(error.contains("Unknown variable"), "{error}");
    assert_eq!(
        modules.prepare("rest", document::check("rest", "#target", &Value::Null)),
        Err("#target needs a URL".into())
    );

    let response = json!({"status": 201, "body": "made"});
    let effects = modules.finished(&call, &response);
    let Effect::Command { module, cmd } = &effects[0] else {
        panic!("{effects:?}")
    };
    assert_eq!(module, "panel");
    assert_eq!(cmd.name, "history.insert");
    assert_eq!(cmd.args()["status"], 201);

    let rendered = modules
        .render(
            "rest",
            Chunk {
                call: Some(call),
                response: serde_json::to_vec(&response).unwrap(),
                index: 0,
                last: true,
            },
        )
        .unwrap();
    assert_eq!(rendered.text, "201 made");
}

/// Traps on demand, like a wasm instance that hit `unreachable`.
struct Fragile {
    inner: Linked<Panel>,
    trap: Rc<Cell<bool>>,
}

impl Instance for Fragile {
    fn call(&self, request: &[u8]) -> Result<Vec<u8>, String> {
        if self.trap.replace(false) {
            return Err("unreachable".into());
        }
        Ok(self.inner.call(request))
    }
}

#[test]
fn a_trap_drops_running_work_and_the_module_starts_again() {
    let mut modules = modules();
    let trap = Rc::new(Cell::new(false));
    modules
        .load(
            manifest("panel", "panel", 1),
            Box::new(Fragile {
                inner: Linked::default(),
                trap: trap.clone(),
            }),
            json!({}),
        )
        .unwrap();
    modules.show("panel");
    trap.set(true);
    let effects = modules.reply("panel", "tick", Ok(Value::Null));
    assert!(effects.contains(&Effect::Failed {
        module: "panel".into(),
        message: "panel failed: unreachable".into()
    }));
    assert!(
        modules
            .get("panel")
            .unwrap()
            .log
            .iter()
            .any(|(level, _)| level == "error")
    );
    // The next event starts it again with Init first.
    modules.reply("panel", "fetch", Err(Failure::new("refused")));
    assert!(texts(modules.view("panel").unwrap())[1].contains("fetch: refused"));
}

#[test]
fn modules_without_their_drivers_do_not_load() {
    let mut modules = Modules::new("3.7.0", BTreeMap::new(), Box::new(|| 0.0));
    let error = modules
        .load(
            manifest("rest", "service", 1),
            Box::new(Linked::<Rest>::default()),
            json!({}),
        )
        .err()
        .unwrap();
    assert!(error.contains("needs the http driver"), "{error}");
}

#[test]
fn slow_synchronous_calls_are_noted_in_the_log() {
    let time = Rc::new(Cell::new(0.0));
    let step = Rc::new(Cell::new(4.0));
    let (clock, delta) = (time.clone(), step.clone());
    let mut modules = Modules::new(
        "3.7.0",
        drivers(),
        Box::new(move || {
            clock.set(clock.get() + delta.get());
            clock.get()
        }),
    );
    modules
        .load(
            manifest("rest", "service", 1),
            Box::new(Linked::<Rest>::default()),
            json!({}),
        )
        .unwrap();
    let lines = modules
        .highlight("rest", vec![7], vec!["a".into(), "b".into()], Vec::new())
        .unwrap();
    assert_eq!(lines[1].state, [7]);
    assert!(modules.get("rest").unwrap().log.is_empty());
    step.set(6.0);
    modules.fold("rest", Check::default()).unwrap();
    assert_eq!(
        modules.get("rest").unwrap().log.back().unwrap().1,
        "fold took 6.0 ms (budget 5 ms)"
    );
}

/// A service that notes the hints it hears in its status.
#[derive(Default)]
struct Hinted;

impl Module for Hinted {
    type Msg = ();
    fn build(&self, _: &Check) -> Result<DriverCall, String> {
        Err("no requests".into())
    }
    fn render(&self, _: &Chunk) -> Result<Rendered, String> {
        Err("no responses".into())
    }
    fn update(&mut self, event: Event<()>) -> Vec<Cmd> {
        match event {
            Event::Hints { check, pos } => {
                vec![cmd::status(&format!("{} {pos}", check.text), "ok")]
            }
            Event::Discover(check) => vec![cmd::resolve("url", &check.text)],
            Event::Reply { tag, result } => {
                let text = match result {
                    Ok(value) => format!("{tag} {}", value.as_str().unwrap_or("")),
                    Err(failure) => format!("{tag} failed: {}", failure.message),
                };
                vec![cmd::status(&text, "ok")]
            }
            _ => Vec::new(),
        }
    }
    fn subscriptions(&self) -> Subscriptions {
        Subscriptions {
            hints: true,
            discover: true,
            ..Subscriptions::default()
        }
    }
}

#[test]
fn hints_reach_only_the_subscribed_service_of_the_check() {
    let mut modules = modules();
    load(&mut modules);
    for id in ["one", "two"] {
        modules
            .load(
                manifest(id, "service", 1),
                Box::new(Linked::<Hinted>::default()),
                json!({}),
            )
            .unwrap();
    }
    let hints = |service: &str| wire::Event {
        kind: Some(event::Kind::Hints(wire::CompleteRequest {
            check: Some(Check {
                columns: Vec::new(),
                service: service.into(),
                text: "#target".into(),
                ..Check::default()
            }),
            pos: 3,
        })),
        now: 0.0,
    };
    let status = |modules: &Modules, id: &str| {
        modules
            .get(id)
            .unwrap()
            .status
            .as_ref()
            .map(|status| status.text.clone())
    };

    modules.broadcast(hints("one"));
    assert_eq!(status(&modules, "one").as_deref(), Some("#target 3"));
    assert_eq!(status(&modules, "two"), None);
    // `rest` does not subscribe to hints.
    assert!(modules.broadcast(hints("rest")).is_empty());
    assert_eq!(status(&modules, "rest"), None);
    assert_eq!(status(&modules, "two"), None);
}

/// A panel that keeps one variable and reads it on start.
#[derive(Default)]
struct Vars;

impl Module for Vars {
    type Msg = ();
    fn update(&mut self, event: Event<()>) -> Vec<Cmd> {
        match event {
            Event::Init { .. } => vec![cmd::kv_get("load", "host")],
            _ => Vec::new(),
        }
    }
}

fn contribute(panel: &str, tree: Value) {
    registry::contribute(panel, &json!({"tree": tree}));
}

/// The panels of a session: `var`, the sets `dev` and `stage`, then `env`.
fn tree() {
    // Undeclared panels are ordered by id.
    contribute(
        "1-vars",
        json!({
            "first": [["var", {"host": "x.test", "password": "p\"w",
                "token": "{{.var.password}}", "user": "{{:login}}", "who": "{{:who}}"}]],
            "last": [["env", {"HOME": "/home/me", "login": "env-login", "host": "env.test"}]],
        }),
    );
    registry::contribute(
        "2-dictionary",
        &json!({"tree": {"first": [
            ["dev", {"host": "dev.test", "hosts": {"catalog": "catalog.dev", "n": {"1": "deep"}}}],
            ["stage", {"host": "stage.test", "api_key": "k3y"}]]}}),
    );
}

#[test]
fn the_kernel_resolves_variables_from_the_tree_the_panels_contribute() {
    let mut modules = modules();
    modules
        .load(
            manifest("one", "service", 1),
            Box::new(Linked::<Hinted>::default()),
            json!({}),
        )
        .unwrap();
    let discover = |text: &str| wire::Event {
        kind: Some(event::Kind::Discover(Check {
            service: "one".into(),
            text: text.into(),
            ..Check::default()
        })),
        now: 0.0,
    };
    let status = |modules: &Modules| modules.get("one").unwrap().status.clone().unwrap().text;

    modules.broadcast(discover("http://{{.var.host}}/a"));
    assert_eq!(
        status(&modules),
        "url failed: Unknown variable {{.var.host}}"
    );

    // A closed panel still starts and reads what it holds.
    let effects = modules
        .load(
            manifest("vars", "panel", 1),
            Box::new(Linked::<Vars>::default()),
            json!({}),
        )
        .unwrap();
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::Command { cmd, .. } if cmd.name == "kv.get"))
    );
    assert!(modules.settling());
    modules.reply("vars", "load", Ok(json!(null)));
    assert!(!modules.settling());

    tree();
    modules.broadcast(discover("http://{{.var.host}}/a"));
    assert_eq!(status(&modules), "url http://x.test/a");
    assert_eq!(modules.get("one").unwrap().resolutions, 1);
    // The reply ended the command: a late end changes nothing.
    assert!(modules.end("one", "url").is_empty());
}

fn text(modules: &mut Modules, text: &str) -> Result<String, String> {
    modules
        .resolve(text, false)
        .map(|resolution| resolution.text)
}

#[test]
fn references_resolve_by_priority_var_then_sets_then_env() {
    let mut modules = modules();
    tree();
    assert_eq!(text(&mut modules, "{{:host}}").unwrap(), "x.test");
    assert_eq!(
        text(&mut modules, "{{.dev.host}} {{.stage.host}}").unwrap(),
        "dev.test stage.test"
    );
    // Only `stage` has it, only `env` has it.
    assert_eq!(
        text(&mut modules, "{{:api_key}} {{:HOME}}").unwrap(),
        "k3y /home/me"
    );
    assert_eq!(text(&mut modules, "{{.env.host}}").unwrap(), "env.test");
    // A value's own references resolve too, wherever the other panel is.
    assert_eq!(text(&mut modules, "{{:user}}").unwrap(), "env-login");
    assert!(
        text(&mut modules, "{{:nope}}")
            .unwrap_err()
            .contains("Unknown variable")
    );
    assert!(
        text(&mut modules, "{{:who}}")
            .unwrap_err()
            .contains("ircular")
    );
}

#[test]
fn a_hash_path_selects_a_string_and_a_hash_is_a_string_context_error() {
    let mut modules = modules();
    tree();
    assert_eq!(
        text(&mut modules, "{{:hosts.catalog}} {{:hosts.n.1}}").unwrap(),
        "catalog.dev deep"
    );
    let error = text(&mut modules, "{{:hosts}}").unwrap_err();
    assert!(error.contains("expected a string, got a hash"), "{error}");
}

#[test]
fn json_mode_escapes_values_and_secrets_are_returned() {
    let mut modules = modules();
    tree();
    let resolution = modules.resolve("{\"p\":\"{{.var.token}}\"}", true).unwrap();
    assert_eq!(resolution.text, "{\"p\":\"p\\\"w\"}");
    assert_eq!(resolution.secrets, ["p\"w"]);
    let resolution = modules.resolve("{{:api_key}}", false).unwrap();
    assert_eq!(resolution.secrets, ["k3y"]);
}

#[test]
fn live_template_calls_expand_at_resolve_time() {
    let mut modules = modules();
    tree();
    registry::contribute(
        "templates",
        &json!({"templates": {
            "*": {"*": "@tpl(name, kind = \"qa\") Url\nhttps://{{name}}.{{:host}}/{{kind}}"},
            "rest": {"target": "@own Own\nown-{{.dev.host}}"}}}),
    );
    assert_eq!(
        text(&mut modules, "GET {{@tpl(\"a\")}} {{@tpl(\"b\", \"x\")}}").unwrap(),
        "GET https://a.x.test/qa https://b.x.test/x"
    );
    assert_eq!(text(&mut modules, "{{@own}}").unwrap(), "own-dev.test");
    let error = text(&mut modules, "{{@nope}}").unwrap_err();
    assert!(error.contains("Unknown template"), "{error}");
    // A JSON plan: the call's quotes are read as written, its result is JSON-escaped.
    registry::contribute(
        "templates",
        &json!({"templates": {"*": {"*": "@q(type = \"qa\") Q\n\"{{type}}\"\nline"}}}),
    );
    let plan = json!({"command": "echo '{{@q(\"prod\")}}'", "n": 1}).to_string();
    let resolved: serde_json::Value =
        serde_json::from_str(&modules.resolve(&plan, true).unwrap().text).unwrap();
    assert_eq!(
        resolved,
        json!({"command": "echo '\"prod\"\nline'", "n": 1})
    );
}

#[test]
fn function_calls_run_on_the_variable_tree_at_resolve_time() {
    let mut modules = modules();
    tree();
    registry::contribute(
        "functions",
        &json!({"functions": {
            "*": "def pick(k): .var[k];",
            "rest": "def joined: \"a \" + .var.token;"}}),
    );
    registry::contribute(
        "templates",
        &json!({"templates": {"*": {"*": "@t(k) T\n<{{$pick(k)}}>"}}}),
    );
    assert_eq!(text(&mut modules, "{{$pick(\"host\")}}").unwrap(), "x.test");
    assert_eq!(text(&mut modules, "{{@t(\"host\")}}").unwrap(), "<x.test>");
    let resolution = modules.resolve("{\"a\":\"{{$joined}}\"}", true).unwrap();
    assert_eq!(resolution.text, "{\"a\":\"a p\\\"w\"}");
    assert_eq!(resolution.secrets, ["p\"w"]);
    let error = text(&mut modules, "{{$nope}}").unwrap_err();
    assert!(error.contains("Unknown function $nope"), "{error}");
}

/// A service that runs its checks: a lock, a call, output, then a result.
#[derive(Default)]
struct Runner {
    cancelled: Vec<String>,
}

impl Module for Runner {
    type Msg = ();
    fn update(&mut self, event: Event<()>) -> Vec<Cmd> {
        match event {
            Event::Run { tag, check, .. } => vec![
                cmd::lock(format!("{tag}:lock"), "pace"),
                cmd::output(&tag, &format!("at {} ", services_sdk::now())),
                Cmd::new(&tag, "log", &json!({"level":"info","text":check.text})),
            ],
            Event::Reply { tag, .. } if tag.ends_with(":lock") => {
                let run = tag.trim_end_matches(":lock");
                vec![
                    cmd::driver(
                        format!("{run}:call"),
                        &http::Request::new("GET", "https://x").call(),
                    ),
                    cmd::timer(format!("{run}:idle"), 1000),
                ]
            }
            Event::Reply { tag, result } if tag.ends_with(":call") => {
                let run = tag.trim_end_matches(":call");
                vec![
                    cmd::run_status(run, "1 row"),
                    cmd::result(
                        run,
                        &Rendered {
                            text: result.unwrap()["body"].as_str().unwrap().into(),
                            language: "text".into(),
                            outcome: "OK".into(),
                            columns: Vec::new(),
                        },
                        &json!({}),
                    ),
                ]
            }
            Event::Cancel { tag } => {
                self.cancelled.push(tag.clone());
                vec![cmd::result(&tag, &Rendered::default(), &json!({}))]
            }
            _ => Vec::new(),
        }
    }
    fn subscriptions(&self) -> Subscriptions {
        Subscriptions {
            run: true,
            ..Subscriptions::default()
        }
    }
}

#[test]
fn a_run_owns_its_calls_and_locks_until_it_finishes_or_stops() {
    let mut modules = Modules::new("3.7.0", drivers(), Box::new(|| 42.0));
    modules
        .load(
            manifest("runner", "service", 1),
            Box::new(Linked::<Runner>::default()),
            json!({}),
        )
        .unwrap();
    assert!(modules.runs("runner"));
    let check = Check {
        columns: Vec::new(),
        service: "runner".into(),
        text: "go".into(),
        ..Check::default()
    };
    let effects = modules.start_run("runner", "r1", check.clone(), false);
    assert_eq!(
        effects,
        vec![
            Effect::Lock {
                module: "runner".into(),
                tag: "r1:lock".into(),
                name: "pace".into()
            },
            Effect::Output {
                module: "runner".into(),
                run: "r1".into(),
                text: "at 42 ".into()
            },
        ]
    );
    let effects = modules.reply("runner", "r1:lock", Ok(json!({})));
    assert!(matches!(&effects[0], Effect::Driver { tag, .. } if tag == "r1:call"));
    let effects = modules.reply("runner", "r1:call", Ok(json!({"body":"done"})));
    assert_eq!(
        effects,
        vec![
            Effect::RunStatus {
                module: "runner".into(),
                run: "r1".into(),
                text: "1 row".into()
            },
            // The finished run's timer and lock go with it.
            Effect::Cancel {
                module: "runner".into(),
                tag: "r1:idle".into()
            },
            Effect::Unlock {
                module: "runner".into(),
                name: "pace".into()
            },
            Effect::Finished {
                module: "runner".into(),
                run: "r1".into(),
                result: json!({"body":"done","language":"text","outcome":"OK","debug":{},"columns":[]})
            },
        ]
    );

    modules.start_run("runner", "r2", check, false);
    modules.reply("runner", "r2:lock", Ok(json!({})));
    let effects = modules.cancel_run("runner", "r2");
    assert_eq!(
        effects,
        vec![
            Effect::Cancel {
                module: "runner".into(),
                tag: "r2:call".into()
            },
            Effect::Cancel {
                module: "runner".into(),
                tag: "r2:idle".into()
            },
            Effect::Unlock {
                module: "runner".into(),
                name: "pace".into()
            },
        ],
        "a stopped run's result is dropped"
    );
    assert!(modules.reply("runner", "r2:call", Ok(json!({}))).is_empty());
}

/// A chat panel: watches its kv key and runs an Agent turn on Shown.
#[derive(Default)]
struct Chat {
    heard: Vec<String>,
}

impl Module for Chat {
    type Msg = ();
    fn update(&mut self, event: Event<()>) -> Vec<Cmd> {
        match event {
            Event::Init { .. } => vec![cmd::kv_watch("chats")],
            Event::Shown => vec![cmd::agent_turn("turn", &json!({"prompt": "hi"}))],
            Event::Storage { key } => {
                self.heard.push(key);
                Vec::new()
            }
            Event::ToolCall { name, .. } => {
                self.heard.push(name);
                Vec::new()
            }
            _ => Vec::new(),
        }
    }
    fn view(&self) -> Option<ui::Node> {
        Some(ui::panel(
            "Chat",
            vec![ui::text(self.heard.join(","))],
            Vec::new(),
        ))
    }
}

#[test]
fn a_chat_panel_hears_its_watched_keys_and_its_turn_stops_when_hidden() {
    let mut modules = modules();
    modules
        .load(
            manifest("chat", "panel", 1),
            Box::new(Linked::<Chat>::default()),
            json!({}),
        )
        .unwrap();
    let effects = modules.show("chat");
    assert!(effects.iter().any(|effect| matches!(effect,
        Effect::Command { cmd, .. } if cmd.name == "agent.turn")));
    assert!(modules.storage("chat", "other").is_empty());
    modules.storage("chat", "chats");
    modules.tool_call("chat", "call", "page_read", &json!({}));
    assert_eq!(texts(modules.view("chat").unwrap()), ["chats,page_read"]);

    // A closed panel is off: its running turn is cancelled.
    let effects = modules.hide("chat");
    assert!(effects.contains(&Effect::Cancel {
        module: "chat".into(),
        tag: "turn".into()
    }));
}

#[test]
fn a_service_module_answers_for_the_schema_it_discovered() {
    let mut modules = modules();
    modules
        .load(
            manifest("found", "service", 1),
            Box::new(Linked::<Discovered>::default()),
            json!({}),
        )
        .unwrap();
    let check =
        |target: &str| document::check("found", &format!("#target\n{target}"), &Value::Null);
    assert_eq!(
        modules.schema("found", check("known")).unwrap(),
        Some(json!({"types": {"Pet": {}}}))
    );
    assert_eq!(modules.schema("found", check("other")).unwrap(), None);
    assert!(modules.schema("missing", check("known")).is_err());
}

/// Opens a session and a plain stream when shown, writes to the tag a `Ui` value names and
/// logs the errors it hears.
#[derive(Default)]
struct Sessions;

impl Module for Sessions {
    type Msg = ();
    fn update(&mut self, event: Event<()>) -> Vec<Cmd> {
        match event {
            Event::Shown => vec![
                cmd::driver("s", &tcp::Request::session("db", 6379).call()),
                cmd::driver("plain", &tcp::Request::new("db", 6379).call().streaming()),
            ],
            Event::Ui { value, .. } => vec![cmd::write(&value.unwrap(), b"PING\r\n")],
            Event::Reply {
                tag,
                result: Err(failure),
            } => vec![cmd::log("error", &format!("{tag}: {}", failure.message))],
            _ => Vec::new(),
        }
    }
}

#[test]
fn a_module_writes_only_to_its_own_running_sessions() {
    let mut modules = modules();
    // `other` is a service, never shown: it opens no session of its own.
    for (id, kind) in [("mine", "panel"), ("other", "service")] {
        modules
            .load(
                manifest(id, kind, 1),
                Box::new(Linked::<Sessions>::default()),
                json!({}),
            )
            .unwrap();
    }
    modules.show("mine");
    let write = |modules: &mut Modules, id: &str, tag: &str| {
        let msg = serde_json::to_vec(&()).unwrap();
        let effects = modules.ui(id, msg, Some(tag.into()));
        effects
            .into_iter()
            .filter(|effect| matches!(effect, Effect::Write { .. }))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        write(&mut modules, "mine", "s"),
        [Effect::Write {
            module: "mine".into(),
            tag: "s".into(),
            data: "UElORw0K".into()
        }]
    );
    for (id, tag) in [("mine", "plain"), ("mine", "none"), ("other", "s")] {
        assert!(write(&mut modules, id, tag).is_empty(), "{id} {tag}");
    }
    modules.end("mine", "s");
    assert!(write(&mut modules, "mine", "s").is_empty());
    let errors = |id: &str| -> Vec<String> {
        let log = &modules.get(id).unwrap().log;
        log.iter().map(|(_, text)| text.clone()).collect()
    };
    let refused = "No session stream with this tag is running";
    assert_eq!(
        errors("mine"),
        [
            format!("plain: {refused}"),
            format!("none: {refused}"),
            format!("s: {refused}")
        ]
    );
    assert_eq!(errors("other"), [format!("s: {refused}")]);
}

#[test]
fn the_agent_reads_the_templates_and_functions_of_a_service_and_the_global_ones() {
    let _modules = modules();
    registry::contribute(
        "templates",
        &json!({"templates": {
            "*": {"check": "@g Global check\n#target\n/g"},
            "rest": {"target": "@own(name) Own\n/{{name}}"},
            "graphql": {"target": "@other Other\n/o"}}}),
    );
    registry::contribute(
        "functions",
        &json!({"functions": {"*": "def up: ascii_upcase;", "rest": "def rows: .sets;"}}),
    );
    assert_eq!(
        registry::library("templates", "rest").unwrap(),
        "# Templates of rest\n\n## target\n@own(name) Own\n/{{name}}\n\n# Global templates\n\n\
         ## check\n@g Global check\n#target\n/g\n\n\
         Built in: the check template @copy(check = \"prev\") of every service."
    );
    assert_eq!(
        registry::library("functions", "mysql").unwrap(),
        "# Functions of mysql\n\n(none)\n\n# Global functions\n\ndef up: ascii_upcase;\n\n\
         Built in: the jq function copy($n) used by @copy."
    );
    assert!(registry::library("hints", "rest").is_err());
}
