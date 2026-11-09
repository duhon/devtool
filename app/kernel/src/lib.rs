//! The App Kernel: no UI. Loads wasm modules, runs their MVU loop and commands, keeps the
//! console document model (checks, responses, edit history), gives editors the language of
//! a service (highlighting, folds, completion) and drives the request pipeline
//! (`before_request` → driver → `after_response`).

pub mod document;
pub mod editor;
pub mod modules;
#[cfg(target_arch = "wasm32")]
pub mod wasm;

pub use modules::{Effect, Instance, Loaded, Modules, Status};
