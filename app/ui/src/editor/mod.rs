//! A text editor model: the parent owns a [`State`] and the
//! browser view renders it and turns input into commands.
pub mod brackets;
pub mod checks;
mod commands;
pub mod fold;
pub mod highlight;
mod history;
mod json;
pub mod layout;
pub mod session;
mod sql;
mod state;
pub mod text;

pub use commands::{Indent, Unit};
pub use session::Session;
pub use state::{Anchor, Change, Fold, Selection, State, Transaction};
