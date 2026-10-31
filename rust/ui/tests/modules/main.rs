//! BEHAVIOR: what each UI module (service or side panel) does for the user.
//! One file per module; run one with
//! `cargo test -p services-ui --test modules -- rest::`.

#[path = "../support/mod.rs"]
mod support;

mod agent;
mod graphql;
mod grpc;
mod history;
mod mysql;
mod rest;
mod sessions;
mod splunk;
mod stomp;
mod templates;
mod terminal;
mod variables;
