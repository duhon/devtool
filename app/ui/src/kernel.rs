//! Modules the App runs through the Kernel — console services and panels: their manifests
//! (the Host's `wasm` list) and one [`Modules`] holding their running instances. The Kernel lives on the UI
//! thread; [`with`] lends it out for one synchronous call, so effects come back to the
//! caller to run outside the borrow.

use serde_json::Value;
use services_kernel::{Effect, Instance, Modules};
use services_sdk::wire::{self, event};
use services_sdk::{Check, Manifest};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};

/// The App version modules declare compatibility with.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

thread_local! {
    static KERNEL: RefCell<Option<Modules>> = const { RefCell::new(None) };
    static DECLARED: RefCell<Vec<Manifest>> = RefCell::new(seed(services_sdk::Kind::Service));
    static PANELS: RefCell<Vec<Manifest>> = RefCell::new(seed(services_sdk::Kind::Panel));
    static STYLES: RefCell<HashMap<String, &'static str>> = RefCell::new(HashMap::new());
    static DRIVERS: RefCell<Option<BTreeMap<String, String>>> = const { RefCell::new(None) };
    /// The digest of each declared module's wasm, and of each one running.
    static DIGESTS: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
    static LOADED: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
    /// Why a declared module could not start.
    static FAILED: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
}

/// Natively the SDK modules are linked, so tests run them without a browser.
#[cfg(not(target_arch = "wasm32"))]
fn linked() -> Vec<(Manifest, Box<dyn Instance>)> {
    use services_sdk::Linked;
    let manifest = |json: &str| Manifest::parse(json).expect("linked module manifest");
    macro_rules! linked {
        ($($path:literal => $module:ty),* $(,)?) => {
            vec![$((
                manifest(include_str!(concat!("../../../modules/", $path, "/manifest.json"))),
                Box::new(Linked::<$module>::default()) as Box<dyn Instance>,
            )),*]
        };
    }
    linked![
        "services/rest" => services_module_rest::Rest,
        "services/mysql" => services_module_mysql::Mysql,
        "services/graphql" => services_module_graphql::Graphql,
        "services/grpc" => services_module_grpc::Grpc,
        "services/splunk" => services_module_splunk::Splunk,
        "services/terminal" => services_module_terminal::Terminal,
        "services/artemis" => services_module_artemis::Artemis,
        "services/opensearch" => services_module_opensearch::OpenSearch,
        "panels/variables" => services_module_variables::Variables,
        "panels/history" => services_module_history::History,
    ]
}

fn seed(kind: services_sdk::Kind) -> Vec<Manifest> {
    #[cfg(not(target_arch = "wasm32"))]
    return linked()
        .into_iter()
        .map(|(manifest, _)| manifest)
        .filter(|manifest| manifest.kind == kind)
        .collect();
    #[cfg(target_arch = "wasm32")]
    {
        let _ = kind;
        Vec::new()
    }
}

fn now() -> f64 {
    #[cfg(target_arch = "wasm32")]
    return js_sys::Date::now();
    #[cfg(not(target_arch = "wasm32"))]
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64() * 1000.0)
}

fn start(drivers: BTreeMap<String, String>) -> Modules {
    #[allow(unused_mut)]
    let mut modules = Modules::new(VERSION, drivers, Box::new(now));
    #[cfg(not(target_arch = "wasm32"))]
    for (manifest, instance) in linked() {
        let id = manifest.id.clone();
        modules
            .load(manifest, instance, Value::Null)
            .unwrap_or_else(|error| panic!("{id} loads: {error:?}"));
    }
    modules
}

fn native_drivers() -> BTreeMap<String, String> {
    #[cfg(not(target_arch = "wasm32"))]
    return ["grpc", "http", "mysql", "process", "tcp"]
        .into_iter()
        .map(|driver| (driver.into(), "1".into()))
        .collect();
    #[cfg(target_arch = "wasm32")]
    BTreeMap::new()
}

/// Lends the Kernel for one call; never call `with` again inside `f`.
pub fn with<R>(f: impl FnOnce(&mut Modules) -> R) -> R {
    KERNEL.with(|kernel| {
        let mut kernel = kernel.borrow_mut();
        f(kernel.get_or_insert_with(|| start(native_drivers())))
    })
}

/// Restarts the Kernel for the drivers the Host offers; `true` unless it already had them
/// (its modules then need loading again).
pub fn configure(drivers: BTreeMap<String, String>) -> bool {
    if DRIVERS.with(|known| known.borrow().as_ref() == Some(&drivers)) {
        return false;
    }
    DRIVERS.with(|known| *known.borrow_mut() = Some(drivers.clone()));
    KERNEL.with(|kernel| *kernel.borrow_mut() = Some(start(drivers)));
    LOADED.with(|loaded| loaded.borrow_mut().clear());
    true
}

/// Starts (or restarts) a module from the wasm of `digest`; its first effects come back.
pub fn install(
    manifest: Manifest,
    digest: &str,
    instance: Box<dyn Instance>,
) -> Result<Vec<Effect>, String> {
    let id = manifest.id.clone();
    let effects = with(|modules| {
        let mut effects = modules.unload(&id);
        effects.extend(modules.load(manifest, instance, Value::Null)?);
        Ok::<_, String>(effects)
    })?;
    LOADED.with(|loaded| loaded.borrow_mut().insert(id.clone(), digest.to_owned()));
    FAILED.with(|failed| failed.borrow_mut().remove(&id));
    Ok(effects)
}

/// Records why the module of `id` could not start.
pub fn fail(id: &str, message: String) {
    FAILED.with(|failed| failed.borrow_mut().insert(id.to_owned(), message));
}

/// Stops the running modules no longer declared; the declared ones not running at their
/// digest come back to load.
pub fn retain() -> (Vec<Effect>, Vec<(Manifest, String)>) {
    let mut declared = declared();
    declared.extend(panels());
    let digests = DIGESTS.with(|digests| digests.borrow().clone());
    let running = LOADED.with(|loaded| loaded.borrow().clone());
    let effects = with(|modules| {
        running
            .keys()
            .filter(|id| !declared.iter().any(|manifest| &manifest.id == *id))
            .flat_map(|id| modules.unload(id))
            .collect()
    });
    LOADED.with(|loaded| {
        loaded
            .borrow_mut()
            .retain(|id, _| declared.iter().any(|manifest| &manifest.id == id))
    });
    let wanted = declared
        .into_iter()
        .filter_map(|manifest| {
            let digest = digests.get(&manifest.id)?.clone();
            (running.get(&manifest.id) != Some(&digest)).then_some((manifest, digest))
        })
        .collect();
    (effects, wanted)
}

/// The Host's `wasm` list: SDK manifests with the `digest` of each module's wasm.
pub fn parse(value: &Value) -> Result<Vec<(Manifest, String)>, String> {
    let Some(list) = value["wasm"].as_array() else {
        return Ok(Vec::new());
    };
    list.iter()
        .map(|entry| {
            let mut fields = entry.clone();
            let digest = fields
                .as_object_mut()
                .and_then(|map| map.remove("digest"))
                .and_then(|digest| digest.as_str().map(str::to_owned))
                .ok_or("A wasm module without a digest")?;
            let manifest: Manifest = serde_json::from_value(fields)
                .map_err(|error| format!("Invalid wasm module manifest: {error}"))?;
            Ok((manifest, digest))
        })
        .collect()
}

/// Replaces the declared modules — the services the console shows tabs for and the
/// panels — with the digest of each one's wasm; `true` when the services changed.
pub fn declare(modules: Vec<(Manifest, String)>) -> bool {
    let (modules, digests): (Vec<Manifest>, HashMap<String, String>) = modules
        .into_iter()
        .map(|(manifest, digest)| {
            let id = manifest.id.clone();
            (manifest, (id, digest))
        })
        .unzip();
    let (services, panels): (Vec<Manifest>, Vec<Manifest>) = modules
        .into_iter()
        .partition(|manifest| manifest.kind == services_sdk::Kind::Service);
    DIGESTS.with(|known| *known.borrow_mut() = digests);
    PANELS.with(|declared| *declared.borrow_mut() = panels);
    DECLARED.with(|declared| {
        let mut declared = declared.borrow_mut();
        let changed = *declared != services;
        *declared = services;
        changed
    })
}

pub fn declared() -> Vec<Manifest> {
    DECLARED.with(|declared| declared.borrow().clone())
}

/// The declared panel modules.
pub fn panels() -> Vec<Manifest> {
    PANELS.with(|panels| panels.borrow().clone())
}

/// A declared panel has neither started nor failed yet: what it resolves or keeps for
/// runs is not there.
pub fn panels_loading() -> bool {
    panels().iter().any(|panel| {
        !loaded(&panel.id) && !FAILED.with(|failed| failed.borrow().contains_key(&panel.id))
    })
}

/// Panels still read what their hooks need, or have not started: a run waits.
pub fn settling() -> bool {
    panels_loading() || with(|modules| modules.settling())
}

pub fn manifest(id: &str) -> Option<Manifest> {
    DECLARED.with(|declared| declared.borrow().iter().find(|m| m.id == id).cloned())
}

/// A console service the Kernel runs.
pub fn is_service(id: &str) -> bool {
    DECLARED.with(|declared| declared.borrow().iter().any(|m| m.id == id))
}

/// The module of `id` is running.
pub fn loaded(id: &str) -> bool {
    with(|modules| modules.get(id).is_some())
}

/// The declared module of `id` has neither started nor failed yet.
pub fn loading(id: &str) -> bool {
    is_service(id) && !loaded(id) && !FAILED.with(|failed| failed.borrow().contains_key(id))
}

/// Why a declared service cannot answer yet.
pub fn unavailable(id: &str) -> String {
    if let Some(failure) = FAILED.with(|failed| failed.borrow().get(id).cloned()) {
        return failure;
    }
    let name = manifest(id).map_or_else(|| id.to_owned(), |manifest| manifest.name);
    format!("The {name} module is still loading")
}

/// The module's status line and indicator (`Discovery · OK`, …).
pub fn status(id: &str) -> Option<services_kernel::Status> {
    with(|modules| modules.get(id).and_then(|loaded| loaded.status.clone()))
}

/// The module's latest log lines, the newest last.
pub fn log(id: &str, lines: usize) -> Vec<(String, String)> {
    with(|modules| {
        modules.get(id).map_or_else(Vec::new, |loaded| {
            let skip = loaded.log.len().saturating_sub(lines);
            loaded.log.iter().skip(skip).cloned().collect()
        })
    })
}

/// A check as the module sees it: the text after the `#check` line.
pub fn check(service: &str, text: &str, options: &Value) -> Check {
    services_kernel::document::check(service, text, options)
}

pub fn document(check: Check) -> wire::Event {
    wire::Event {
        kind: Some(event::Kind::Document(check)),
        now: 0.0,
    }
}

pub fn discover(check: Check) -> wire::Event {
    wire::Event {
        kind: Some(event::Kind::Discover(check)),
        now: 0.0,
    }
}

/// The user asked for hints at `pos`, a UTF-8 offset in the check text.
pub fn hints(check: Check, pos: u32) -> wire::Event {
    wire::Event {
        kind: Some(event::Kind::Hints(wire::CompleteRequest {
            check: Some(check),
            pos,
        })),
        now: 0.0,
    }
}

/// A kv key the modules may watch changed.
pub fn storage(key: &str) -> wire::Event {
    wire::Event {
        kind: Some(event::Kind::Storage(key.into())),
        now: 0.0,
    }
}

/// Sends an event to the running modules subscribed to it.
pub fn broadcast(event: wire::Event) -> Vec<Effect> {
    with(|modules| modules.broadcast(event))
}

/// The section markers the module completes in an empty check (`endpoint`, `headers`, …).
pub fn markers(id: &str) -> Vec<String> {
    if !loaded(id) {
        return Vec::new();
    }
    let completions = with(|modules| modules.complete(id, check(id, "#", &Value::Null), 1));
    completions
        .map(|completions| {
            completions
                .items
                .into_iter()
                .filter_map(|item| {
                    let text = if item.insert.is_empty() {
                        item.label
                    } else {
                        item.insert
                    };
                    let name = text.strip_prefix('#')?.trim().replace("$0", "");
                    (!name.is_empty()).then_some(name)
                })
                .collect()
        })
        .unwrap_or_default()
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

/// A style name as the editor keeps it.
pub fn style(kind: &str) -> &'static str {
    STYLES.with(|styles| {
        *styles
            .borrow_mut()
            .entry(kind.to_owned())
            .or_insert_with(|| Box::leak(kind.to_owned().into_boxed_str()))
    })
}
