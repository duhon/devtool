pub mod agent_workspace;
pub mod console;
pub mod format;
pub mod kernel;
pub mod language;
pub mod services;
pub mod sessions;
pub mod storage;

#[cfg(target_arch = "wasm32")]
mod browser;

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() -> Result<(), wasm_bindgen::JsValue> {
    browser::start()
}
