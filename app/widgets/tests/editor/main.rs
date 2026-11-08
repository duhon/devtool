//! BEHAVIOR of the generic widgets: the editor's document, check identity, syntax,
//! variable hover, the find bar and the state indicator.
//! Run one area with `cargo test -p services-widgets --test editor -- document::`.

#[path = "../support/mod.rs"]
mod support;

mod document;
mod find_bar;
mod hover;
mod identity;
mod indicator;
mod status_bar;
mod syntax;
