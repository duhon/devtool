//! Modules the App runs through the Kernel — console services and panels: their manifests
//! (the Host's `wasm` list) and one [`Modules`] holding their running instances. The Kernel lives on the UI
//! thread; [`with`] lends it out for one synchronous call, so effects come back to the
//! caller to run outside the borrow.

use crate::{Effect, Instance, Modules};
use serde_json::Value;
use services_sdk::Manifest;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

/// The App version modules declare compatibility with.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

thread_local! {
    static KERNEL: RefCell<Option<Modules>> = const { RefCell::new(None) };
    static DECLARED: RefCell<Vec<Manifest>> = RefCell::new(seed(services_sdk::Kind::Service));
    static PANELS: RefCell<Vec<Manifest>> = RefCell::new(seed(services_sdk::Kind::Panel));
    static DRIVERS: RefCell<Option<BTreeMap<String, String>>> = const { RefCell::new(None) };
    /// The digest of each declared module's wasm, and of each one running.
    static DIGESTS: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
    static LOADED: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
    /// Why a declared module could not start.
    static FAILED: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
    /// The first effects of the modules the Kernel starts with, until the App runs them.
    static BOOT: RefCell<Vec<Effect>> = const { RefCell::new(Vec::new()) };
    /// What each module adds to the editors (`editor.contribute`), by key.
    static CONTRIBUTED: RefCell<BTreeMap<String, serde_json::Map<String, Value>>> =
        const { RefCell::new(BTreeMap::new()) };
}

/// Records what `module` adds to the editors; `true` when it changed.
pub fn contribute(module: &str, contribution: &Value) -> bool {
    let Some(fields) = contribution.as_object() else {
        return false;
    };
    CONTRIBUTED.with(|contributed| {
        let mut contributed = contributed.borrow_mut();
        let own = contributed.entry(module.to_owned()).or_default();
        let mut changed = false;
        for (key, value) in fields {
            if own.get(key) != Some(value) {
                own.insert(key.clone(), value.clone());
                changed = true;
            }
        }
        changed
    })
}

/// The contributions under `key` of every module, panels by `order`.
pub fn contributions(key: &str) -> Vec<Value> {
    let order: Vec<String> = panels().into_iter().map(|panel| panel.id).collect();
    CONTRIBUTED.with(|contributed| {
        let contributed = contributed.borrow();
        let mut ids: Vec<&String> = contributed.keys().collect();
        ids.sort_by_key(|id| {
            order
                .iter()
                .position(|panel| panel == *id)
                .unwrap_or(usize::MAX)
        });
        ids.into_iter()
            .filter_map(|id| contributed[id].get(key).cloned())
            .collect()
    })
}

/// The `{{…}}` references the editors complete, with their values.
pub fn variables() -> Vec<(String, String)> {
    contributions("variables")
        .iter()
        .flat_map(|list| {
            serde_json::from_value::<Vec<(String, String)>>(list.clone()).unwrap_or_default()
        })
        .collect()
}

/// The modules that are part of the App rather than declared by the Host: the built-in
/// Settings panel on every target and, natively, every module linked in (tests run them
/// without a browser). The Shell links them before the Kernel starts.
#[derive(Clone, Copy)]
pub struct Link {
    pub builtin: fn() -> Module,
    pub linked: fn() -> Vec<Module>,
}

/// A module the App links in: its manifest and the instance that runs it.
pub type Module = (Manifest, Box<dyn Instance>);

static LINK: OnceLock<Link> = OnceLock::new();

/// Sets the modules of the App; the first call wins.
pub fn link(modules: Link) {
    let _ = LINK.set(modules);
}

fn linked() -> &'static Link {
    LINK.get()
        .expect("the App links its modules (services_shell::link) before the Kernel starts")
}

fn builtin() -> Module {
    (linked().builtin)()
}

fn seed(kind: services_sdk::Kind) -> Vec<Manifest> {
    (linked().linked)()
        .into_iter()
        .map(|(manifest, _)| manifest)
        .filter(|manifest| manifest.kind == kind)
        .collect()
}

fn now() -> f64 {
    #[cfg(target_arch = "wasm32")]
    return js_sys::Date::now();
    #[cfg(not(target_arch = "wasm32"))]
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64() * 1000.0)
}

/// What every module starts with: `timezoneOffset`, minutes the local time is ahead of UTC,
/// and `instance`, an id unique to this start (the module builds its own ids from it).
fn settings() -> Value {
    #[cfg(target_arch = "wasm32")]
    let offset = -js_sys::Date::new_0().get_timezone_offset();
    #[cfg(not(target_arch = "wasm32"))]
    let offset = 0.0;
    serde_json::json!({
        "timezoneOffset": offset,
        "instance": uuid::Uuid::now_v7().simple().to_string(),
    })
}

fn start(drivers: BTreeMap<String, String>) -> Modules {
    let mut modules = Modules::new(VERSION, drivers, Box::new(now));
    let (manifest, instance) = builtin();
    let effects = modules
        .load(manifest, instance, settings())
        .expect("Settings loads");
    BOOT.with(|boot| *boot.borrow_mut() = effects);
    for (manifest, instance) in (linked().linked)() {
        let id = manifest.id.clone();
        modules
            .load(manifest, instance, settings())
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

/// The first effects of the modules the Kernel started with, once.
pub fn boot() -> Vec<Effect> {
    // Starts the Kernel if it has not yet.
    with(|_| ());
    BOOT.with(|boot| std::mem::take(&mut *boot.borrow_mut()))
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
        effects.extend(modules.load(manifest, instance, settings())?);
        Ok::<_, String>(effects)
    })?;
    LOADED.with(|loaded| loaded.borrow_mut().insert(id.clone(), digest.to_owned()));
    FAILED.with(|failed| failed.borrow_mut().remove(&id));
    Ok(effects)
}

/// Why the module of `id` could not start.
pub fn failure(id: &str) -> Option<String> {
    FAILED.with(|failed| failed.borrow().get(id).cloned())
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
    let mut effects = boot();
    effects.extend(with(|modules| {
        running
            .keys()
            .filter(|id| !declared.iter().any(|manifest| &manifest.id == *id))
            .flat_map(|id| modules.unload(id))
            .collect::<Vec<_>>()
    }));
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
    let panels = panels
        .into_iter()
        .filter(|panel| panel.id != builtin().0.id)
        .collect();
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

/// The panel modules, by `order`: the declared ones and the built-in Settings.
pub fn panels() -> Vec<Manifest> {
    let mut panels = PANELS.with(|panels| panels.borrow().clone());
    panels.push(builtin().0);
    panels.sort_by_key(|panel| panel.order.unwrap_or(u32::MAX));
    panels
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
pub fn status(id: &str) -> Option<crate::Status> {
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

/// The section markers the module completes in an empty check (`endpoint`, `headers`, …).
pub fn markers(id: &str) -> Vec<String> {
    if !loaded(id) {
        return Vec::new();
    }
    let completions =
        with(|modules| modules.complete(id, crate::document::check(id, "#", &Value::Null), 1));
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
