//! Events the Kernel sends to modules: the current check, discovery, hints, a restored
//! workspace.

use crate::Effect;
use crate::modules::registry::with;
use services_sdk::Check;
use services_sdk::wire::{self, event};

/// A check as the module sees it: the text after the `#check` line.
pub fn document(check: Check) -> wire::Event {
    wire::Event {
        kind: Some(event::Kind::Document(check)),
        now: 0.0,
    }
}

pub fn discover(check: Check) -> wire::Event {
    wire::Event {
        kind: Some(event::Kind::Discover(check)),
        now: 0.0,
    }
}

/// The user asked for hints at `pos`, a UTF-8 offset in the check text.
pub fn hints(check: Check, pos: u32) -> wire::Event {
    wire::Event {
        kind: Some(event::Kind::Hints(wire::CompleteRequest {
            check: Some(check),
            pos,
        })),
        now: 0.0,
    }
}

/// A session was restored.
pub fn workspace() -> wire::Event {
    wire::Event {
        kind: Some(event::Kind::Workspace(wire::Empty {})),
        now: 0.0,
    }
}

/// Sends an event to the running modules subscribed to it.
pub fn broadcast(event: wire::Event) -> Vec<Effect> {
    with(|modules| modules.broadcast(event))
}
