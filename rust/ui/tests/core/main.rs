//! BEHAVIOR of the shared UI core every module plugs into: the Console
//! document, completion, coloring, tables, formatting and persisted state.
//! Run one area with `cargo test -p services-ui --test core -- document::`.

#[path = "../support/mod.rs"]
mod support;

mod activity;
mod checks;
mod completion;
mod console;
mod discovery;
mod document;
mod format;
mod logfmt;
mod services;
mod storage;
mod syntax;
mod tables;
