pub mod activity;
pub mod agent_workspace;
pub mod checks;
pub mod completion;
pub mod console;
pub mod document;
pub mod editor;
pub mod find;
pub mod format;
pub mod kernel;
pub mod profiles;
pub mod services;
pub mod sessions;
pub mod storage;
pub mod syntax;
pub mod template_completion;

#[cfg(target_arch = "wasm32")]
mod browser;

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() -> Result<(), wasm_bindgen::JsValue> {
    browser::start()
}
