use prost::Message;
use serde_json::Value;
use services_module_kubernetes::Kubernetes;
use services_sdk::wire::{self, answer, event, request};
use services_sdk::{Check, Cmd, Linked, Section};

pub fn check(text: &str) -> Check {
    let mut sections: Vec<Section> = Vec::new();
    for (line, text) in text.lines().enumerate() {
        if let Some(name) = text.strip_prefix('#') {
            sections.push(Section {
                name: name.into(),
                text: String::new(),
                line: line as u32,
            });
        } else if let Some(section) = sections.last_mut() {
            if !section.text.is_empty() {
                section.text.push('\n');
            }
            section.text.push_str(text);
        }
    }
    Check {
        service: "kubernetes".into(),
        text: text.into(),
        sections,
        ..Default::default()
    }
}

pub struct Harness {
    linked: Linked<Kubernetes>,
    pub now: f64,
}

impl Default for Harness {
    fn default() -> Self {
        Self {
            linked: Linked::default(),
            now: 1_000.0,
        }
    }
}

impl Harness {
    pub fn call(&mut self, kind: request::Kind) -> answer::Kind {
        let bytes = wire::Request { kind: Some(kind) }.encode_to_vec();
        let bytes = self.linked.call(&bytes);
        wire::Answer::decode(bytes.as_slice())
            .unwrap()
            .kind
            .unwrap()
    }

    pub fn send(&mut self, kind: event::Kind) -> Vec<Cmd> {
        let answer::Kind::Updated(updated) =
            self.call(request::Kind::Update(wire::UpdateRequest {
                event: Some(wire::Event {
                    kind: Some(kind),
                    now: self.now,
                }),
                view: false,
            }))
        else {
            panic!("Expected Updated")
        };
        updated.cmds
    }

    pub fn reply(&mut self, tag: &str, value: Value) -> Vec<Cmd> {
        self.send(event::Kind::Reply(wire::Reply {
            tag: tag.into(),
            ok: Some(serde_json::to_vec(&value).unwrap()),
            error: None,
        }))
    }

    pub fn fail(&mut self, tag: &str, message: &str) -> Vec<Cmd> {
        self.send(event::Kind::Reply(wire::Reply {
            tag: tag.into(),
            ok: None,
            error: Some(services_sdk::Failure::new(message)),
        }))
    }

    pub fn run(&mut self, text: &str) -> Vec<Cmd> {
        self.send(event::Kind::Run(wire::RunEvent {
            tag: "run:1".into(),
            check: Some(check(text)),
        }))
    }

    pub fn complete(&mut self, text: &str, pos: usize) -> wire::Completions {
        let answer::Kind::Completions(items) =
            self.call(request::Kind::Complete(wire::CompleteRequest {
                check: Some(check(text)),
                pos: pos as u32,
            }))
        else {
            panic!("Expected completions")
        };
        items
    }
}
