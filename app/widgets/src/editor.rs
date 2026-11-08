//! The editor: the text model ([`Session`]) and the component — [`View`] renders the model a
//! parent keeps in a [`Handle`]. Everything a service adds (highlighting, folds, completion)
//! comes through the installed [`language::Language`].
pub mod brackets;
pub mod checks;
mod commands;
pub mod document;
pub mod fold;
pub mod highlight;
mod history;
pub mod hover;
pub mod identity;
mod json;
pub mod language;
pub mod layout;
pub mod model;
mod state;
pub mod syntax;
pub mod text;

pub use commands::{Indent, Unit};
pub use model::Session;
pub use state::{Anchor, Change, Fold, Selection, State, Transaction};

#[cfg(target_arch = "wasm32")]
mod facade;
#[cfg(target_arch = "wasm32")]
mod handle;
// TEMPORARY: the table-performance scenario's standalone editor.
#[cfg(target_arch = "wasm32")]
pub mod mount;
#[cfg(target_arch = "wasm32")]
mod render;
#[cfg(target_arch = "wasm32")]
mod view;

#[cfg(target_arch = "wasm32")]
pub use handle::Handle;
#[cfg(target_arch = "wasm32")]
pub use kept::{kept, relight};
#[cfg(target_arch = "wasm32")]
pub use view::View;

pub const CSS: &str = include_str!("editor/editor.css");

#[cfg(target_arch = "wasm32")]
mod kept {
    use super::{Handle, Session, model::Setup};
    use std::{cell::RefCell, collections::BTreeMap};

    thread_local! {
        static KEPT: RefCell<BTreeMap<String, Handle>> = const { RefCell::new(BTreeMap::new()) };
    }

    /// Highlights the kept editors of `service` again: its module has just started.
    pub fn relight(service: &str) {
        let handles: Vec<Handle> = KEPT.with(|kept| kept.borrow().values().cloned().collect());
        for handle in handles {
            if handle.read(|session| session.setup().service == service) {
                handle.with(Session::relight);
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
}
