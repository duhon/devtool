//! TEMPORARY: `window.__servicesMountEditor(host, options)` mounts a standalone console
//! editor for the table-performance browser scenario.
//! Remove it once that scenario is rewritten after the editor rewrite.
use crate::language::Language;
use services_kernel::document::{ResponseRegion, identity::Identity};
use services_widgets::dom::{json, set, window};
use services_widgets::editor::Handle;
use services_widgets::editor::model::Setup;
use std::rc::Rc;
use wasm_bindgen::prelude::*;

pub fn install() {
    let mount = Closure::<dyn Fn(web_sys::Element, JsValue) -> JsValue>::new(mount);
    set(&window(), "__servicesMountEditor", mount.as_ref());
    mount.forget();
}

/// Options: `value`, `service`, `responses` (response regions) and `checks` (lines).
/// Returns the editor's facade; its `destroy()` unmounts it.
fn mount(host: web_sys::Element, options: JsValue) -> JsValue {
    let options = json(options);
    let text = options["value"].as_str().unwrap_or_default();
    let service = options["service"].as_str().unwrap_or("mysql").to_owned();
    let regions: Vec<ResponseRegion> =
        serde_json::from_value(options["responses"].clone()).unwrap_or_default();
    let checks: Vec<Identity> = options["checks"]
        .as_array()
        .map(|lines| {
            lines
                .iter()
                .filter_map(|line| line.as_u64())
                .map(|line| Identity::new(line as usize))
                .collect()
        })
        .unwrap_or_default();
    let setup = Setup {
        mode: format!("console-{service}"),
        scope: "performance".into(),
        document: true,
        ..Setup::default()
    };
    let language = Rc::new(Language::console(&service, "performance"));
    let handle = Handle::with_extension(setup, language.clone(), text);
    language.set_document(&regions, &checks);
    handle.with(|session| {
        session.sync(text, None, js_sys::Date::now());
        session.take_events();
    });
    handle.mount(host, "performance-editor", "Performance")
}
