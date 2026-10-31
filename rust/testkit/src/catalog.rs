//! Every module the host serves today. This list is the seed of the future
//! module manifest: conformance runs each rule against every entry, so a new
//! module only has to be listed here to be checked.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A console service tab that executes `#check` requests.
    Service,
    /// A side panel or settings feature with its own operations.
    Panel,
}

#[derive(Clone, Copy, Debug)]
pub struct ModuleSpec {
    /// Protocol `service` name.
    pub name: &'static str,
    pub title: &'static str,
    pub kind: Kind,
    /// Answers `{"type":"discover"}` with a schema for completion.
    pub discovery: bool,
    /// Executes as a stream of events instead of a single reply.
    pub streaming: bool,
    /// Needs Copilot, GitHub or another external system, so offline
    /// conformance cannot exercise it.
    pub external: bool,
    /// The parameter naming what a service talks to (endpoint, DSN, URL).
    pub target: &'static str,
    /// Operations that read state, with the fields they need as JSON; they
    /// must succeed on an empty data directory.
    pub reads: &'static [(&'static str, &'static str)],
}

const fn service(
    name: &'static str,
    title: &'static str,
    target: &'static str,
    discovery: bool,
) -> ModuleSpec {
    ModuleSpec {
        name,
        title,
        kind: Kind::Service,
        discovery,
        streaming: false,
        external: false,
        target,
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
        discovery: false,
        streaming: false,
        external: false,
        target: "",
        reads,
    }
}

/// Services in the host's order (`console.order`), then the host's own panels.
pub const MODULES: &[ModuleSpec] = &[
    service("rest", "REST", "url", true),
    service("mysql", "MySQL", "connectionString", true),
    service("graphql", "GraphQL", "url", true),
    service("grpc", "gRPC", "endpoint", true),
    service("splunk", "Splunk", "baseUrl", false),
    ModuleSpec {
        streaming: true,
        ..service("terminal", "Terminal", "endpoint", false)
    },
    service("artemis", "Artemis", "url", false),
    panel("vars", "Variables", &[("list", "{}")]),
    panel("dictionary", "Dictionary", &[("list", "{}")]),
    panel("templates", "Templates", &[("get", "{}")]),
    panel("hints", "Hints", &[("get", r#"{"serviceName":"rest"}"#)]),
    panel(
        "history",
        "History",
        &[(
            "list",
            r#"{"checkId":"00000000-0000-4000-8000-000000000001"}"#,
        )],
    ),
    panel("sessions", "Sessions", &[("bootstrap", "{}")]),
    ModuleSpec {
        external: true,
        ..panel("agent", "Agent", &[])
    },
    ModuleSpec {
        external: true,
        ..panel("agent_models", "Agent models", &[])
    },
    panel("agent_sessions", "Agent sessions", &[("list", "{}")]),
    panel("agent_skills", "Agent skills", &[("get", "{}")]),
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
