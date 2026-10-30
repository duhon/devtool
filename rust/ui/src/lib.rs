pub mod activity;
pub mod agent_workspace;
pub mod checks;
pub mod completion;
pub mod console;
pub mod discovery;
pub mod document;
pub mod format;
mod graphql_completion;
/// GraphQL editor structure used by the browser editor: operation folding
/// and object-field ranges.
pub mod graphql {
    pub use crate::graphql_completion::{object_fields, operation_fold};
}
pub mod logfmt;
pub mod profiles;
pub mod services;
pub mod sessions;
mod sql_completion;
pub mod storage;
pub mod syntax;
pub mod tables;
pub mod template_completion;

#[cfg(target_arch = "wasm32")]
mod browser;

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() -> Result<(), wasm_bindgen::JsValue> {
    browser::start()
}
