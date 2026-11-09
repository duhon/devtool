//! BEHAVIOR of the Shell's core every module plugs into: the Console, completion, the
//! editors' language, formatting and persisted state.
//! Run one area with `cargo test -p services-shell --test core -- console::`.

mod completion;
mod console;
mod language;
mod services;
mod storage;
