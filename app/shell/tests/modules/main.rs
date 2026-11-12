//! BEHAVIOR: what each UI module (service or side panel) does for the user.
//! One file per module; run one with
//! `cargo test -p services-shell --test modules -- rest::`.

mod agent;
mod mongodb;
mod rest;
mod sessions;
mod templates;
