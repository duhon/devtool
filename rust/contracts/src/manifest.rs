//! What a module declares about itself (`Module/Manifest`): the host reads the params,
//! the console reads everything else. Built-in modules keep it in `manifest.json`.

use crate::ParamConfig;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub execute: bool,
    #[serde(default)]
    pub discover: bool,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub params: Vec<ParamConfig>,
    #[serde(default)]
    pub console: Console,
    /// SHA-256 of the package's console part (`ui.wasm`); the host fills it and serves the
    /// file by `{"service":"modules","op":"ui","module"}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui: Option<String>,
}

/// How the console edits, highlights and shows one service.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Console {
    /// Default tab position; modules without one follow the ordered ones in load order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<u32>,
    /// `#marker` sections of a check in their canonical order (`#check` is implied).
    #[serde(default)]
    pub sections: Vec<Section>,
    /// The main section: what a check sends (query, body, command, ...).
    #[serde(default)]
    pub payload: String,
    #[serde(default)]
    pub response: View,
    /// The program whose command lines (`curl`, `grpcurl`, ...) the console part imports
    /// checks from and exports them as.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// What schema the service discovers and how the console asks for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery: Option<Discovery>,
    /// Completion words offered anywhere in a check.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    /// The service's completion hints document before the user saves one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hints: String,
    /// Seconds the console waits for a check (default 120).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
    /// Text before the first marker is the payload instead of an error.
    #[serde(
        default,
        rename = "implicitPayload",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub implicit_payload: bool,
    /// An error answer shows its response body like a success instead of the error text.
    #[serde(
        default,
        rename = "errorBody",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub error_body: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Discovery {
    pub kind: DiscoveryKind,
    /// Address schemes discovery accepts.
    #[serde(default)]
    pub schemes: Vec<String>,
}

/// The schema a discovery returns; the console completes checks from it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiscoveryKind {
    /// Paths, methods and body schemas of an HTTP API.
    Openapi,
    Graphql,
    /// Services, methods and message fields of a gRPC server.
    GrpcReflection,
    /// Tables and columns of a database.
    Sql,
}

impl DiscoveryKind {
    /// Discovered for the path a check edits, not when its connection changes; schemas
    /// stay cached by base address.
    pub fn lazy(self) -> bool {
        self == Self::Openapi
    }

    /// The path of the address is completed from the schema; an address that ends with
    /// `/` discovers.
    pub fn paths(self) -> bool {
        matches!(self, Self::Openapi | Self::GrpcReflection)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Section {
    pub name: String,
    #[serde(default)]
    pub shape: Shape,
    /// Highlighting and completion language: `endpoint` (an address), `http` (`METHOD URL`),
    /// `grpc` (`target/Service/Method`), `headers`, `json`, `sql`, `graphql`, `spl`, `proto`,
    /// `shell`.
    #[serde(default)]
    pub language: String,
    /// The request param the section fills.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub param: Option<String>,
    /// Part of the connection: discovery asks with it and runs again when it changes.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub connection: bool,
    /// Completion candidates of the section.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<Hint>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Hint {
    pub text: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Shape {
    /// Every line up to the next marker.
    #[default]
    Block,
    /// The first non-empty line; the rest goes back to the payload.
    Line,
    /// `Name: value` lines; the first other line goes back to the payload.
    Pairs,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum View {
    /// A JSON body with status and headers (REST, GraphQL, gRPC).
    #[default]
    BodyHeaders,
    /// Rows and columns (MySQL, Splunk).
    Grid,
    /// Streamed output (Terminal).
    Stream,
}

impl View {
    /// Highlighting language of a response.
    pub fn language(self) -> &'static str {
        match self {
            Self::BodyHeaders => "json",
            Self::Grid => "table",
            Self::Stream => "shell",
        }
    }
}

impl Manifest {
    pub fn section(&self, name: &str) -> Option<&Section> {
        self.console
            .sections
            .iter()
            .find(|section| section.name == name)
    }

    /// Template library sections: `check` plus every console section.
    pub fn template_sections(&self) -> impl Iterator<Item = &str> {
        std::iter::once("check").chain(self.console.sections.iter().map(|s| s.name.as_str()))
    }

    /// The highlighting language of `area` (a section name; empty or unknown means the payload).
    pub fn language(&self, area: &str) -> &str {
        self.section(area)
            .or_else(|| self.section(&self.console.payload))
            .map(|section| section.language.as_str())
            .unwrap_or("")
    }
}
