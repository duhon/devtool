use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use services_kernel::document;
use services_kernel::modules::{Effect, Instance, Modules};
use services_sdk::wire::{self, event};
use services_sdk::{
    Check, Chunk, Cmd, DriverCall, Event, Failure, Linked, Manifest, Module, Rendered,
    Subscriptions, cmd, http, ui,
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

fn modules() -> Modules {
    Modules::new("3.7.0", drivers(), Box::new(|| 0.0))
}

/// A service: `#endpoint METHOD URL`, renders the status and body.
#[derive(Default)]
struct Rest;

impl Module for Rest {
    type Msg = ();
    fn build(&self, check: &Check) -> Result<DriverCall, String> {
        let (method, url) = check
            .text_of("endpoint")
            .split_once(' ')
            .ok_or("#endpoint needs a method and a URL")?;
        Ok(http::Request::new(method, url).call())
    }
    fn render(&self, chunk: &Chunk) -> Result<Rendered, String> {
        let response = http::Response::parse(&chunk.response())?;
        Ok(Rendered {
            text: format!("{} {}", response.status, response.body.unwrap_or_default()),
            language: "text".into(),
        })
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
                cmd::status("fetching", "busy"),
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
    fn before_request(&self, mut call: DriverCall) -> Result<DriverCall, String> {
        let mut request = call.request();
        let url = request["url"].as_str().unwrap_or("").to_owned();
        if url.contains("{{missing}}") {
            return Err("Unknown variable {{missing}}".into());
        }
        request["url"] = json!(url.replace("{{host}}", "example.test"));
        call.set_request(&request);
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
            } => Some((tag.as_str(), request["url"].clone(), *stream)),
            _ => None,
        })
        .collect();
    assert_eq!(
        drivers,
        [
            ("fetch", json!("https://x"), false),
            ("live", json!("https://y"), true)
        ]
    );
    assert_eq!(
        modules.get("panel").unwrap().status,
        Some(("fetching".into(), "busy".into()))
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
    let check = document::check("rest", "#endpoint GET https://{{host}}/a", &Value::Null);
    let call = modules.prepare("rest", check).unwrap();
    assert_eq!(
        call.request()["headers"],
        json!([["X-Seen", "https://example.test/a"]])
    );

    let missing = document::check("rest", "#endpoint GET https://{{missing}}/", &Value::Null);
    assert_eq!(
        modules.prepare("rest", missing),
        Err("Unknown variable {{missing}}".into())
    );
    assert_eq!(
        modules.prepare("rest", document::check("rest", "#endpoint", &Value::Null)),
        Err("#endpoint needs a method and a URL".into())
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
        .highlight("rest", vec![7], vec!["a".into(), "b".into()])
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
