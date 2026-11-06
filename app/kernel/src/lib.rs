//! The App Kernel: no UI. Loads wasm modules, runs their MVU loop and commands, splits
//! documents into checks and drives the request pipeline (`before_request` → driver →
//! `after_response`).

pub mod document;
pub mod modules;
#[cfg(target_arch = "wasm32")]
pub mod wasm;

pub use modules::{Effect, Instance, Loaded, Modules};
