//! BEHAVIOR of the generic widgets: the editor's syntax, the find bar and
//! the state indicator.
//! Run one area with `cargo test -p services-widgets --test editor -- syntax::`.

#[path = "../support/mod.rs"]
mod support;

mod find_bar;
mod indicator;
mod syntax;
