mod app;
mod kernel_modules;
mod session_database;
mod sessions;
mod transport;
mod updates;

use services_widgets::dom::{call, error, get, js, json, set, window};
use wasm_bindgen::JsValue;
const MODULES: &str = "servicesConsole.modules";

/// The module list of the last host reply; the built-in one stays without a valid cache.
fn restore_modules() -> bool {
    let cached: Option<serde_json::Value> = window()
        .local_storage()
        .ok()
        .flatten()
        .and_then(|storage| storage.get_item(MODULES).ok().flatten())
        .and_then(|raw| serde_json::from_str(&raw).ok());
    let Some(cached) = cached else {
        return false;
    };
    crate::kernel::parse(&cached).is_ok_and(|modules| {
        crate::kernel::declare(modules);
        true
    })
}

/// Installs and caches the host's module list and the modules the Kernel runs; `true` when
/// either changed.
pub fn accept_modules(value: &serde_json::Value) -> Result<bool, String> {
    let modules = crate::kernel::parse(value)?;
    if let Some(storage) = window().local_storage().ok().flatten() {
        let cached = serde_json::json!({"wasm": value["wasm"]}).to_string();
        let _ = storage.set_item(MODULES, &cached);
    }
    Ok(crate::kernel::declare(modules))
}

pub fn start() -> Result<(), JsValue> {
    let restored = restore_modules();
    crate::language::install();
    services_widgets::editor::mount::install();
    let root = window()
        .document()
        .ok_or("No document")?
        .get_element_by_id("app")
        .ok_or("Missing #app mount element")?;
    yew::Renderer::<sessions::Host>::with_root_and_props(root, sessions::HostProps { restored })
        .render();
    Ok(())
}
