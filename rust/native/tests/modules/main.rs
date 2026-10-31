//! BEHAVIOR: what each module does for the user, exercised through the host
//! protocol. One file per module; run one with
//! `cargo test -p services-native --test modules -- rest::`.

mod graphql;
mod mysql;
mod rest;
mod splunk;
mod stomp;
mod templates;
mod terminal;
mod variables;
