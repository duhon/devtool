mod agent_markdown;
mod agent_panel;
mod app;
mod check_rail;
mod console_variables;
mod editor;
mod history;
mod page_agent;
mod pane_resizer;
mod session_database;
mod sessions;
mod templates;
mod transport;
mod updates;
mod views;

use js_sys::{Function, Reflect};
use wasm_bindgen::{JsCast, JsValue};
pub fn get(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &key.into()).unwrap_or(JsValue::UNDEFINED)
}
pub fn set(value: &JsValue, key: &str, v: &JsValue) {
    if let Err(e) = Reflect::set(value, &key.into(), v) {
        web_sys::console::error_1(&e);
    }
}
pub fn call(value: &JsValue, key: &str, args: &[JsValue]) -> Result<JsValue, JsValue> {
    let f = get(value, key)
        .dyn_into::<Function>()
        .map_err(|_| JsValue::from_str(&format!("Missing browser API: {key}")))?;
    let a = js_sys::Array::new();
    for arg in args {
        a.push(arg);
    }
    f.apply(value, &a)
}
pub fn js(value: &serde_json::Value) -> JsValue {
    use serde::Serialize;
    value
        .serialize(&serde_wasm_bindgen::Serializer::json_compatible())
        .unwrap_or(JsValue::NULL)
}
pub fn json(value: JsValue) -> serde_json::Value {
    serde_wasm_bindgen::from_value(value).unwrap_or(serde_json::Value::Null)
}
pub fn error(value: JsValue) -> String {
    get(&value, "message")
        .as_string()
        .or_else(|| value.as_string())
        .unwrap_or_else(|| format!("{value:?}"))
}
pub fn window() -> web_sys::Window {
    web_sys::window().expect("browser window")
}
pub fn start() -> Result<(), JsValue> {
    let root = window()
        .document()
        .ok_or("No document")?
        .get_element_by_id("app")
        .ok_or("Missing #app mount element")?;
    editor::register_modes()?;
    yew::Renderer::<sessions::Host>::with_root(root).render();
    Ok(())
}
