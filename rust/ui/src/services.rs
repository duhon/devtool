//! Console services: the module manifests the console renders tabs, sections and
//! templates from. The console knows no module itself: the cached or the host's list
//! fills the registry (the host's list always wins), and each module's console part
//! comes with its package.

use serde_json::Value;
use services_contracts::manifest::{DiscoveryKind, Manifest, View};
use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock, RwLock};

pub use services_module_ui::{Imported, Sections, ServiceUi};

/// Console parts of packaged modules attached at run time (their `ui.wasm`).
static ATTACHED: LazyLock<RwLock<BTreeMap<String, &'static dyn ServiceUi>>> =
    LazyLock::new(Default::default);

/// Attaches a packaged module's console part; it replaces an earlier one.
pub fn attach(id: &str, ui: &'static dyn ServiceUi) {
    ATTACHED
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .insert(id.to_owned(), ui);
}

/// Packaged modules linked natively, so tests run their console parts without a browser.
#[cfg(not(target_arch = "wasm32"))]
fn linked() -> [(&'static str, &'static dyn ServiceUi); 7] {
    [
        (
            services_module_rest_ui::MANIFEST,
            &services_module_rest_ui::Rest,
        ),
        (
            services_module_mysql_ui::MANIFEST,
            &services_module_mysql_ui::Mysql,
        ),
        (
            services_module_graphql_ui::MANIFEST,
            &services_module_graphql_ui::Graphql,
        ),
        (
            services_module_grpc_ui::MANIFEST,
            &services_module_grpc_ui::Grpc,
        ),
        (
            services_module_splunk_ui::MANIFEST,
            &services_module_splunk_ui::Splunk,
        ),
        (
            services_module_terminal_ui::MANIFEST,
            &services_module_terminal_ui::Terminal,
        ),
        (
            services_module_artemis_ui::MANIFEST,
            &services_module_artemis_ui::Artemis,
        ),
    ]
}

/// What a service does in the console beyond its manifest data.
pub fn ui(id: &str) -> Option<&'static dyn ServiceUi> {
    if let Some(ui) = ATTACHED.read().unwrap_or_else(|e| e.into_inner()).get(id) {
        return Some(*ui);
    }
    #[cfg(not(target_arch = "wasm32"))]
    for (manifest, ui) in linked() {
        if serde_json::from_str::<Value>(manifest).is_ok_and(|m| m["id"] == id) {
            return Some(ui);
        }
    }
    None
}

/// The console part of a registered module, or why it is unavailable.
pub fn ui_of(manifest: &Manifest) -> Result<&'static dyn ServiceUi, String> {
    ui(&manifest.id).ok_or_else(|| match manifest.ui {
        Some(_) => format!("The {} console part is still loading", manifest.name),
        None => format!("Unsupported console service: {}", manifest.id),
    })
}

static REGISTRY: LazyLock<RwLock<Arc<Vec<Manifest>>>> =
    LazyLock::new(|| RwLock::new(Arc::new(seed())));

/// The registry before a module list arrives: empty in the browser; natively, the linked
/// packages, in host order.
pub fn seed() -> Vec<Manifest> {
    #[allow(unused_mut)]
    let mut seed: Vec<Manifest> = Vec::new();
    #[cfg(not(target_arch = "wasm32"))]
    seed.extend(
        linked().map(|(manifest, _)| serde_json::from_str(manifest).expect("package manifest")),
    );
    seed.sort_by_key(|manifest| manifest.console.order.unwrap_or(u32::MAX));
    seed
}

/// A module list from the host reply or the cache: `{"modules":[...]}`.
pub fn parse(value: &Value) -> Result<Vec<Manifest>, String> {
    let manifests: Vec<Manifest> = serde_json::from_value(value["modules"].clone())
        .map_err(|error| format!("Invalid module list: {error}"))?;
    validate(&manifests)?;
    Ok(manifests)
}

/// Rejects a list the console cannot render: empty or repeated ids, or a console without
/// `#endpoint`, with repeated sections or an undeclared payload. An empty list is valid:
/// every module can be removed in the settings.
pub fn validate(manifests: &[Manifest]) -> Result<(), String> {
    for (index, manifest) in manifests.iter().enumerate() {
        let id = &manifest.id;
        if id.is_empty() || manifests[..index].iter().any(|other| &other.id == id) {
            return Err(format!("Invalid or repeated module id \"{id}\""));
        }
        let sections = &manifest.console.sections;
        for (at, section) in sections.iter().enumerate() {
            if section.name.is_empty() || sections[..at].iter().any(|s| s.name == section.name) {
                return Err(format!(
                    "{id}: invalid or repeated section \"{}\"",
                    section.name
                ));
            }
        }
        if manifest.section("endpoint").is_none() {
            return Err(format!("{id}: no #endpoint section"));
        }
        if manifest.section(&manifest.console.payload).is_none() {
            return Err(format!("{id}: undeclared payload section"));
        }
        if let Some(section) = sections.iter().find(|s| s.connection && s.param.is_none()) {
            return Err(format!(
                "{id}: connection section \"{}\" fills no param",
                section.name
            ));
        }
        if manifest.console.discovery.is_some()
            && !manifest.section("endpoint").is_some_and(|s| s.connection)
        {
            return Err(format!("{id}: discovery without a connection #endpoint"));
        }
    }
    Ok(())
}

/// Replaces the registry; `true` when the list changed.
pub fn install(manifests: Vec<Manifest>) -> Result<bool, String> {
    validate(&manifests)?;
    let mut registry = REGISTRY.write().unwrap_or_else(|e| e.into_inner());
    if **registry == manifests {
        return Ok(false);
    }
    *registry = Arc::new(manifests);
    Ok(true)
}

pub fn all() -> Arc<Vec<Manifest>> {
    REGISTRY.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// One registered module; keeps the list it came from alive.
pub struct Service {
    all: Arc<Vec<Manifest>>,
    index: usize,
}

impl std::ops::Deref for Service {
    type Target = Manifest;
    fn deref(&self) -> &Manifest {
        &self.all[self.index]
    }
}

pub fn get(id: &str) -> Option<Service> {
    let all = all();
    let index = all.iter().position(|manifest| manifest.id == id)?;
    Some(Service { all, index })
}

pub fn contains(id: &str) -> bool {
    all().iter().any(|manifest| manifest.id == id)
}

/// Module ids in load order.
pub fn ids() -> Vec<String> {
    all().iter().map(|manifest| manifest.id.clone()).collect()
}

/// The tab title: the manifest name, or the id of an unknown service.
pub fn label(id: &str) -> String {
    get(id).map_or_else(|| id.to_owned(), |service| service.name.clone())
}

/// Tab order without a saved one: by `console.order`, then the rest in load order.
pub fn default_order() -> Vec<String> {
    let all = all();
    let mut ordered: Vec<&Manifest> = all.iter().collect();
    ordered.sort_by_key(|manifest| manifest.console.order.unwrap_or(u32::MAX));
    ordered
        .into_iter()
        .map(|manifest| manifest.id.clone())
        .collect()
}

/// The service a new console opens on.
pub fn default_service() -> String {
    default_order().into_iter().next().unwrap_or_default()
}

/// Template library sections of a service: `check` plus its console sections.
pub fn template_sections(id: &str) -> Vec<String> {
    get(id)
        .map(|service| service.template_sections().map(str::to_owned).collect())
        .unwrap_or_default()
}

pub fn has_template_section(id: &str, section: &str) -> bool {
    get(id).is_some_and(|service| service.template_sections().any(|name| name == section))
}

/// What `id` discovers; `None` without discovery or for an unknown service.
pub fn discovery(id: &str) -> Option<DiscoveryKind> {
    get(id)?
        .console
        .discovery
        .as_ref()
        .map(|discovery| discovery.kind)
}

/// Discovery runs for the path a check edits, not when its connection changes.
pub fn lazy_discovery(id: &str) -> bool {
    discovery(id).is_some_and(DiscoveryKind::lazy)
}

/// The path of the `#endpoint` is completed from the discovered schema.
pub fn path_discovery(id: &str) -> bool {
    discovery(id).is_some_and(DiscoveryKind::paths)
}

/// How responses of `id` are shown.
pub fn view(id: &str) -> View {
    get(id)
        .map(|service| service.console.response)
        .unwrap_or_default()
}

/// The language of the section `area` of `id` (empty or unknown means the payload).
pub fn language(id: &str, area: &str) -> String {
    get(id)
        .map(|service| service.language(area).to_owned())
        .unwrap_or_default()
}

pub fn has_section(id: &str, section: &str) -> bool {
    get(id).is_some_and(|service| service.section(section).is_some())
}
