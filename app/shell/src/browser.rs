mod app;
mod mount;
mod sessions;

use services_widgets::dom::{call, error, get, window};
use wasm_bindgen::JsValue;
/// A UI callback as the Kernel calls it.
pub(crate) fn on<T: 'static>(callback: yew::Callback<T>) -> services_kernel::Callback<T> {
    (move |value| callback.emit(value)).into()
}

pub fn start() -> Result<(), JsValue> {
    crate::link();
    let restored = services_kernel::modules::loader::restore();
    crate::language::install();
    mount::install();
    let root = window()
        .document()
        .ok_or("No document")?
        .get_element_by_id("app")
        .ok_or("Missing #app mount element")?;
    yew::Renderer::<sessions::Host>::with_root_and_props(root, sessions::HostProps { restored })
        .render();
    Ok(())
}
