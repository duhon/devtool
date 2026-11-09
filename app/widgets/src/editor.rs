//! The editor: the text model ([`Session`]) and the component — [`View`] renders the model a
//! parent keeps in a [`Handle`]. Everything an owner adds (highlighting, folds, completion,
//! its document's bookkeeping) comes through the editor's [`extension::Extension`].
pub mod brackets;
mod commands;
pub mod extension;
pub mod fold;
pub mod highlight;
mod history;
pub mod hover;
mod json;
pub mod layout;
pub mod model;
mod state;
pub mod syntax;
pub mod text;

pub use commands::{Indent, Unit};
pub use model::Session;
pub use state::{Anchor, Change, Fold, Selection, State, Transaction};
pub use text::{Position, TextChange};

#[cfg(target_arch = "wasm32")]
mod facade;
#[cfg(target_arch = "wasm32")]
mod handle;
#[cfg(target_arch = "wasm32")]
mod render;
#[cfg(target_arch = "wasm32")]
mod view;

#[cfg(target_arch = "wasm32")]
pub use handle::Handle;
#[cfg(target_arch = "wasm32")]
pub use kept::{kept, kept_handles};
#[cfg(target_arch = "wasm32")]
pub use view::View;

pub const CSS: &str = include_str!("editor/editor.css");

#[cfg(target_arch = "wasm32")]
mod kept {
    use super::{Handle, extension::Extension, model::Setup};
    use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

    thread_local! {
        static KEPT: RefCell<BTreeMap<String, Handle>> = const { RefCell::new(BTreeMap::new()) };
    }

    /// The kept editors.
    pub fn kept_handles() -> Vec<Handle> {
        KEPT.with(|kept| kept.borrow().values().cloned().collect())
    }

    /// The editor kept under `key` beyond its owner, so undo and scroll survive switching
    /// sessions; a new one showing `text` with `extension` when there is none for `setup`.
    pub fn kept(
        key: &str,
        setup: Setup,
        extension: impl FnOnce() -> Rc<dyn Extension>,
        text: &str,
    ) -> Handle {
        KEPT.with(|kept| {
            let mut kept = kept.borrow_mut();
            match kept.get(key) {
                Some(handle) if handle.read(|session| *session.setup() == setup) => handle.clone(),
                _ => {
                    let handle = Handle::with_extension(setup, extension(), text);
                    kept.insert(key.to_owned(), handle.clone());
                    handle
                }
            }
        })
    }
}
