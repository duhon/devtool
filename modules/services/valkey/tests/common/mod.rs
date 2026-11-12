use prost::Message;
use serde_json::{Value, json};
use services_module_valkey::Valkey;
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
        service: "valkey".into(),
        text: text.into(),
        sections,
        ..Default::default()
    }
}

pub struct Harness {
    linked: Linked<Valkey>,
    wasm: Option<Wasm>,
    pub now: f64,
}

impl Default for Harness {
    fn default() -> Self {
        Self {
            linked: Linked::default(),
            wasm: None,
            now: 1_000.0,
        }
    }
}

impl Harness {
    pub fn wasm() -> Self {
        Self {
            wasm: Some(Wasm::new()),
            ..Self::default()
        }
    }

    pub fn call(&mut self, kind: request::Kind) -> answer::Kind {
        let bytes = wire::Request { kind: Some(kind) }.encode_to_vec();
        let bytes = match &mut self.wasm {
            Some(wasm) => wasm.call(&bytes),
            None => self.linked.call(&bytes),
        };
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

pub fn resolve(harness: &mut Harness, cmds: &[Cmd], vars: &[(&str, &str)]) -> Vec<Cmd> {
    let resolve = cmds.iter().find(|cmd| cmd.name == "vars.resolve").unwrap();
    let args = resolve.args();
    assert_eq!(args["mode"], "json");
    let mut text = args["text"].as_str().unwrap().to_owned();
    for (name, value) in vars {
        let escaped = serde_json::to_string(value).unwrap();
        text = text.replace(&format!("{{{{{name}}}}}"), &escaped[1..escaped.len() - 1]);
    }
    harness.reply(&resolve.tag, json!(text))
}

pub fn write(cmds: &[Cmd]) -> Vec<u8> {
    let cmd = cmds
        .iter()
        .find(|cmd| cmd.name == "driver.write")
        .unwrap_or_else(|| panic!("No write: {cmds:?}"));
    services_sdk::driver::decode(cmd.args()["dataBase64"].as_str().unwrap()).unwrap()
}

pub fn frame(harness: &mut Harness, tag: &str, bytes: &[u8]) -> Vec<Cmd> {
    harness.reply(
        tag,
        json!({"chunkBase64": services_sdk::driver::encode(bytes)}),
    )
}

struct Wasm {
    store: wasmi::Store<()>,
    memory: wasmi::Memory,
    alloc: wasmi::TypedFunc<i32, i32>,
    call: wasmi::TypedFunc<(i32, i32), i32>,
    free: wasmi::TypedFunc<i32, ()>,
}

impl Wasm {
    fn new() -> Self {
        let engine = wasmi::Engine::default();
        let path = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("../../../target/wasm32-unknown-unknown/release/services_module_valkey.wasm");
        let bytes = std::fs::read(path).expect("Run scripts/test-integration.sh");
        let module = wasmi::Module::new(&engine, bytes.as_slice()).unwrap();
        let mut store = wasmi::Store::new(&engine, ());
        let instance = wasmi::Linker::new(&engine)
            .instantiate(&mut store, &module)
            .unwrap()
            .start(&mut store)
            .unwrap();
        assert_eq!(
            instance
                .get_typed_func::<(), i32>(&store, "sdk_abi")
                .unwrap()
                .call(&mut store, ())
                .unwrap(),
            services_sdk::ABI as i32
        );
        Self {
            memory: instance.get_memory(&store, "memory").unwrap(),
            alloc: instance.get_typed_func(&store, "sdk_alloc").unwrap(),
            call: instance.get_typed_func(&store, "sdk_call").unwrap(),
            free: instance.get_typed_func(&store, "sdk_free").unwrap(),
            store,
        }
    }

    fn call(&mut self, bytes: &[u8]) -> Vec<u8> {
        let pointer = self
            .alloc
            .call(&mut self.store, bytes.len() as i32)
            .unwrap();
        self.memory
            .write(&mut self.store, pointer as usize, bytes)
            .unwrap();
        let answer = self
            .call
            .call(&mut self.store, (pointer, bytes.len() as i32))
            .unwrap();
        let mut length = [0; 4];
        self.memory
            .read(&self.store, answer as usize, &mut length)
            .unwrap();
        let mut bytes = vec![0; u32::from_le_bytes(length) as usize];
        self.memory
            .read(&self.store, answer as usize + 4, &mut bytes)
            .unwrap();
        self.free.call(&mut self.store, answer).unwrap();
        bytes
    }
}
