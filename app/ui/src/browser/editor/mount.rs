//! TEMPORARY: `window.__servicesMountEditor(host, options)` mounts a standalone console
//! editor for the table-performance browser scenario.
//! Remove it once that scenario is rewritten after the editor rewrite.
use super::view::Props;
use super::{Handle, View, handle::now, set, window};
use crate::browser::json;
use crate::checks::Identity;
use crate::document::ResponseRegion;
use crate::editor::session::Setup;
use wasm_bindgen::prelude::*;
use yew::Callback;

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
        .map(|lines| lines.iter().filter_map(|line| line.as_u64()).map(|line| Identity::new(line as usize)).collect())
        .unwrap_or_default();
    let setup = Setup {
        mode: format!("console-{service}"),
        service,
        console: true,
        check_scope: "performance".into(),
        ..Setup::default()
    };
    let handle = Handle::new(setup, text);
    handle.with(|session| {
        session.sync(text, &regions, &checks, None, now());
        session.take_events();
    });
    let weak = handle.weak();
    let on = Callback::from(move |msg| {
        if let Some(shared) = weak.upgrade() {
            Handle(shared).update(msg);
        }
    });
    let props = Props {
        handle: handle.clone(),
        on,
        testid: "performance-editor".into(),
        label: "Performance".into(),
        placeholder: Default::default(),
        class: Default::default(),
    };
    let app = yew::Renderer::<View>::with_root_and_props(host, props).render();
    let facade = handle.facade();
    let destroy = Closure::once_into_js(move || {
        app.destroy();
        drop(handle);
    });
    set(&facade, "destroy", &destroy);
    facade
}
