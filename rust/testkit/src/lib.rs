//! Shared harness for Services DevTools tests.
//!
//! Suites talk to modules only through [`Host`], which speaks the same
//! `{service, op, params}` protocol the extension sends. Today `Host` wraps the
//! in-process `Runtime`; when modules become loadable, only `Host` and the
//! [`catalog`] change, and the behavior and conformance suites stay as they are.
//!
//! Test types (see `DEVELOPMENT.md`, "Test"):
//! - behavior: one module's user-visible behavior through the protocol;
//! - contract: rules every module or transport must keep (conformance, host);
//! - perf: budgets, ignored by default and run in release;
//! - ui / install: UI logic and installer lifecycle, outside this crate.

pub mod catalog;
pub mod host;
pub mod http;
pub mod mysql;
pub mod native;
pub mod packages;

pub use catalog::{Kind, MODULES, ModuleSpec, module, panels, services};
pub use host::{DataDir, Host, Reply, Streamed};
