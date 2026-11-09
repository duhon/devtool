//! The App Kernel: no UI. Loads wasm modules, runs their MVU loop and commands, keeps the
//! console document model (checks, responses, edit history) and the workspace, gives editors
//! the language of a service (highlighting, folds, completion), talks to the Host and drives
//! the request pipeline (`before_request` → driver → `after_response`).

pub mod activity;
pub mod agent;
pub mod document;
pub mod editor;
pub mod events;
pub mod host;
#[cfg(target_arch = "wasm32")]
mod js;
#[cfg(target_arch = "wasm32")]
pub mod locks;
pub mod modules;
#[cfg(target_arch = "wasm32")]
pub mod page;
pub mod requests;
pub mod runtime;
pub mod startup;
pub mod storage;
pub mod updates;
#[cfg(target_arch = "wasm32")]
pub mod wasm;
pub mod workspace;

pub use modules::{Effect, Instance, Loaded, Modules, Status, Stop};

use std::rc::Rc;

/// What the Kernel calls back with: answers of the Host, saved workspaces, granted locks.
/// The Shell turns its UI callbacks into these.
pub struct Callback<T>(Rc<dyn Fn(T)>);

impl<T> Callback<T> {
    pub fn emit(&self, value: T) {
        (self.0)(value);
    }
}

impl<T> Clone for Callback<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T, F: Fn(T) + 'static> From<F> for Callback<T> {
    fn from(callback: F) -> Self {
        Self(Rc::new(callback))
    }
}
