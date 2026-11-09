//! The App Shell: the layout, the toolbar, side panels and the console, wiring the Kernel
//! to the widgets and the Host.

pub mod language;
mod link;

pub use link::link;

#[cfg(target_arch = "wasm32")]
pub(crate) mod browser;

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() -> Result<(), wasm_bindgen::JsValue> {
    browser::start()
}
