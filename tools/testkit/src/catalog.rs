//! Every module the host serves today. This list is the seed of the future
//! module manifest: conformance runs each rule against every entry, so a new
//! module only has to be listed here to be checked.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A console service tab: a wasm module the App runs, which the host only serves.
    Service,
    /// A side panel or settings feature with its own operations.
    Panel,
}

#[derive(Clone, Copy, Debug)]
pub struct ModuleSpec {
    /// Protocol `service` name, or the module id.
    pub name: &'static str,
    pub title: &'static str,
    pub kind: Kind,
    /// Needs Copilot, GitHub or another external system, so offline
    /// conformance cannot exercise it.
    pub external: bool,
    /// Operations that read state, with the fields they need as JSON; they
    /// must succeed on an empty data directory.
    pub reads: &'static [(&'static str, &'static str)],
}

const fn service(name: &'static str, title: &'static str) -> ModuleSpec {
    ModuleSpec {
        name,
        title,
        kind: Kind::Service,
        external: false,
        reads: &[],
    }
}

const fn panel(
    name: &'static str,
    title: &'static str,
    reads: &'static [(&'static str, &'static str)],
) -> ModuleSpec {
    ModuleSpec {
        name,
        title,
        kind: Kind::Panel,
        external: false,
        reads,
    }
}

/// Services in the modules' `order`, then the host's own panels.
pub const MODULES: &[ModuleSpec] = &[
    service("rest", "REST"),
    service("mysql", "MySQL"),
    service("graphql", "GraphQL"),
    service("grpc", "gRPC"),
    service("splunk", "Splunk"),
    service("terminal", "Terminal"),
    service("artemis", "Artemis"),
    service("opensearch", "OpenSearch"),
    service("valkey", "Valkey"),
    service("mongodb", "MongoDB"),
    panel(
        "history",
        "History",
        &[(
            "list",
            r#"{"checkId":"00000000-0000-4000-8000-000000000001"}"#,
        )],
    ),
    ModuleSpec {
        external: true,
        ..panel("agent", "Agent", &[])
    },
    ModuleSpec {
        external: true,
        ..panel("agent_models", "Agent models", &[])
    },
    panel("agent_sessions", "Agent sessions", &[("list", "{}")]),
    panel("sources", "Sources", &[("get", "{}")]),
    ModuleSpec {
        external: true,
        ..panel("updates", "Updates", &[])
    },
];

#[track_caller]
pub fn module(name: &str) -> &'static ModuleSpec {
    MODULES
        .iter()
        .find(|module| module.name == name)
        .unwrap_or_else(|| panic!("Module {name:?} is not in the test catalog"))
}

pub fn services() -> impl Iterator<Item = &'static ModuleSpec> {
    MODULES.iter().filter(|module| module.kind == Kind::Service)
}

pub fn panels() -> impl Iterator<Item = &'static ModuleSpec> {
    MODULES.iter().filter(|module| module.kind == Kind::Panel)
}
