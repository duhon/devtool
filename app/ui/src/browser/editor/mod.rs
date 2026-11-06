//! The editor component: [`View`] renders the model a parent keeps in a [`Handle`].
mod facade;
mod handle;
// TEMPORARY: the table-performance scenario's standalone editor.
pub mod mount;
mod render;
mod view;

pub use handle::Handle;
pub use view::View;

use super::{get, js, set, window};
use crate::editor::session::Setup;
use std::{cell::RefCell, collections::BTreeMap};

thread_local! {
    static KEPT: RefCell<BTreeMap<String, Handle>> = const { RefCell::new(BTreeMap::new()) };
}

/// Highlights the kept editors of `service` again: its module has just started.
pub fn relight(service: &str) {
    let handles: Vec<Handle> = KEPT.with(|kept| kept.borrow().values().cloned().collect());
    for handle in handles {
        if handle.read(|session| session.setup().service == service) {
            handle.with(crate::editor::session::Session::relight);
        }
    }
}

/// The editor kept under `key` beyond its owner, so undo and scroll survive switching
/// sessions; a new one showing `text` when there is none for this `setup`.
pub fn kept(key: &str, setup: Setup, text: &str) -> Handle {
    KEPT.with(|kept| {
        let mut kept = kept.borrow_mut();
        match kept.get(key) {
            Some(handle) if handle.read(|session| *session.setup() == setup) => handle.clone(),
            _ => {
                let handle = Handle::new(setup, text);
                kept.insert(key.to_owned(), handle.clone());
                handle
            }
        }
    })
}
