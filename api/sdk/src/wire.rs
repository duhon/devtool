//! The bytes between the Kernel and a module: protobuf messages. Module authors use these
//! types through the SDK; they never encode them. JSON values travel as UTF-8 JSON bytes.

use prost::Message;
use serde::Serialize;
use serde_json::Value;

pub(crate) fn json(value: &impl Serialize) -> Vec<u8> {
    serde_json::to_vec(value).unwrap_or_else(|_| b"null".to_vec())
}

pub(crate) fn value(bytes: &[u8]) -> Value {
    if bytes.is_empty() {
        return Value::Null;
    }
    serde_json::from_slice(bytes).unwrap_or(Value::Null)
}

#[derive(Clone, PartialEq, Message)]
pub struct Empty {}

/// One `#check` of a service document, split into its `#section` blocks by the Kernel.
#[derive(Clone, PartialEq, Message)]
pub struct Check {
    /// The id of the module whose language the check is in.
    #[prost(string, tag = "1")]
    pub service: String,
    /// The whole check, from its `#endpoint` line on.
    #[prost(string, tag = "2")]
    pub text: String,
    #[prost(message, repeated, tag = "3")]
    pub sections: Vec<Section>,
    /// The check's Options as JSON.
    #[prost(bytes = "vec", tag = "4")]
    pub options: Vec<u8>,
}

#[derive(Clone, PartialEq, Message)]
pub struct Section {
    #[prost(string, tag = "1")]
    pub name: String,
    /// The text after the `#name` marker line; for `endpoint`, the rest of the marker line too.
    #[prost(string, tag = "2")]
    pub text: String,
    /// The 0-based line of the marker within the check.
    #[prost(uint32, tag = "3")]
    pub line: u32,
}

impl Check {
    pub fn section(&self, name: &str) -> Option<&str> {
        self.sections
            .iter()
            .find(|section| section.name == name)
            .map(|section| section.text.as_str())
    }
    /// The section's text, empty when it is absent.
    pub fn text_of(&self, name: &str) -> &str {
        self.section(name).unwrap_or("")
    }
    pub fn options(&self) -> Value {
        value(&self.options)
    }
}

/// A call to a Host driver (`http`, `grpc`, `mysql`, `process`, `tcp`).
#[derive(Clone, PartialEq, Message)]
pub struct DriverCall {
    #[prost(string, tag = "1")]
    pub driver: String,
    /// The driver request as JSON.
    #[prost(bytes = "vec", tag = "2")]
    pub request: Vec<u8>,
    /// Answers arrive as several replies and an end.
    #[prost(bool, tag = "3")]
    pub stream: bool,
}

impl DriverCall {
    pub fn new(driver: &str, request: &impl Serialize) -> Self {
        Self {
            driver: driver.into(),
            request: json(request),
            stream: false,
        }
    }
    pub fn streaming(mut self) -> Self {
        self.stream = true;
        self
    }
    pub fn request(&self) -> Value {
        value(&self.request)
    }
    pub fn set_request(&mut self, request: &impl Serialize) {
        self.request = json(request);
    }
}

/// A driver answer, or one piece of a stream, to render.
#[derive(Clone, PartialEq, Message)]
pub struct Chunk {
    #[prost(message, optional, tag = "1")]
    pub call: Option<DriverCall>,
    #[prost(bytes = "vec", tag = "2")]
    pub response: Vec<u8>,
    /// Position in the stream, 0 for a single answer.
    #[prost(uint32, tag = "3")]
    pub index: u32,
    #[prost(bool, tag = "4")]
    pub last: bool,
}

impl Chunk {
    pub fn response(&self) -> Value {
        value(&self.response)
    }
}

#[derive(Clone, PartialEq, Message)]
pub struct Rendered {
    #[prost(string, tag = "1")]
    pub text: String,
    /// The id of the module whose language highlights the text, or `json` / `text`.
    #[prost(string, tag = "2")]
    pub language: String,
    /// The module's verdict for the status line: `OK`, `WARNING` or `ERROR`.
    #[prost(string, tag = "3")]
    pub outcome: String,
}

#[derive(Clone, PartialEq, Message)]
pub struct Token {
    /// UTF-8 byte offset in the line.
    #[prost(uint32, tag = "1")]
    pub start: u32,
    #[prost(uint32, tag = "2")]
    pub len: u32,
    #[prost(string, tag = "3")]
    pub kind: String,
}

#[derive(Clone, PartialEq, Message)]
pub struct Line {
    #[prost(message, repeated, tag = "1")]
    pub tokens: Vec<Token>,
    /// The highlighter state at the end of the line.
    #[prost(bytes = "vec", tag = "2")]
    pub state: Vec<u8>,
}

#[derive(Clone, PartialEq, Message)]
pub struct HighlightRequest {
    /// The state at the start of the first line.
    #[prost(bytes = "vec", tag = "1")]
    pub state: Vec<u8>,
    #[prost(string, repeated, tag = "2")]
    pub lines: Vec<String>,
}

#[derive(Clone, PartialEq, Message)]
pub struct Highlighted {
    #[prost(message, repeated, tag = "1")]
    pub lines: Vec<Line>,
}

#[derive(Clone, PartialEq, Message)]
pub struct CompleteRequest {
    #[prost(message, optional, tag = "1")]
    pub check: Option<Check>,
    /// UTF-8 byte offset of the cursor in the check text.
    #[prost(uint32, tag = "2")]
    pub pos: u32,
}

#[derive(Clone, PartialEq, Message)]
pub struct Item {
    #[prost(string, tag = "1")]
    pub label: String,
    /// Inserted text; the label when empty.
    #[prost(string, tag = "2")]
    pub insert: String,
    #[prost(string, tag = "3")]
    pub detail: String,
    #[prost(string, tag = "4")]
    pub kind: String,
    /// The App asks for hints again once it is inserted, e.g. for the value of a field
    /// that takes one of a list.
    #[prost(bool, tag = "5")]
    pub next: bool,
}

#[derive(Clone, PartialEq, Message)]
pub struct Completions {
    /// The replaced range of the check text, in UTF-8 bytes.
    #[prost(uint32, tag = "1")]
    pub from: u32,
    #[prost(uint32, tag = "2")]
    pub to: u32,
    #[prost(message, repeated, tag = "3")]
    pub items: Vec<Item>,
}

#[derive(Clone, PartialEq, Message)]
pub struct Fold {
    /// 0-based lines of the check; `start` stays visible.
    #[prost(uint32, tag = "1")]
    pub start: u32,
    #[prost(uint32, tag = "2")]
    pub end: u32,
}

#[derive(Clone, PartialEq, Message)]
pub struct Folds {
    #[prost(message, repeated, tag = "1")]
    pub folds: Vec<Fold>,
}

/// A request to the Kernel; its answer arrives as `Reply` with the same tag.
#[derive(Clone, PartialEq, Message)]
pub struct Cmd {
    #[prost(string, tag = "1")]
    pub tag: String,
    /// `group.op`, e.g. `driver.call`, `kv.put`, `timer.after`.
    #[prost(string, tag = "2")]
    pub name: String,
    #[prost(bytes = "vec", tag = "3")]
    pub args: Vec<u8>,
}

impl Cmd {
    pub fn args(&self) -> Value {
        value(&self.args)
    }
}

#[derive(Clone, PartialEq, Message)]
pub struct Cmds {
    #[prost(message, repeated, tag = "1")]
    pub cmds: Vec<Cmd>,
}

#[derive(Clone, PartialEq, Message)]
pub struct Failure {
    #[prost(string, tag = "1")]
    pub message: String,
    /// Structured details as JSON, e.g. a driver's `stage` and `causes`.
    #[prost(bytes = "vec", tag = "2")]
    pub details: Vec<u8>,
}

impl Failure {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            details: Vec::new(),
        }
    }
    pub fn with_details(message: impl Into<String>, details: &impl Serialize) -> Self {
        Self {
            message: message.into(),
            details: json(details),
        }
    }
    pub fn details(&self) -> Value {
        value(&self.details)
    }
}

/// The events a module subscribes to besides the ones it always gets.
#[derive(Clone, Copy, PartialEq, Message)]
pub struct Subscriptions {
    #[prost(bool, tag = "1")]
    pub document: bool,
    #[prost(bool, tag = "2")]
    pub discover: bool,
    #[prost(bool, tag = "3")]
    pub requests: bool,
    #[prost(bool, tag = "4")]
    pub workspace: bool,
    #[prost(bool, tag = "5")]
    pub modules: bool,
    #[prost(bool, tag = "6")]
    pub tools: bool,
    #[prost(bool, tag = "7")]
    pub hints: bool,
    /// The module runs its checks itself: `Run` and `Cancel` instead of `build`.
    #[prost(bool, tag = "8")]
    pub run: bool,
}

/// An event from the Kernel (the `Msg` table of the design).
#[derive(Clone, PartialEq, Message)]
pub struct Event {
    #[prost(
        oneof = "event::Kind",
        tags = "1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16"
    )]
    pub kind: Option<event::Kind>,
    /// When the Kernel sent it, in milliseconds since 1970: modules have no clock.
    #[prost(double, tag = "20")]
    pub now: f64,
}

pub mod event {
    use super::*;

    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Kind {
        /// Settings as JSON.
        #[prost(bytes, tag = "1")]
        Init(Vec<u8>),
        #[prost(message, tag = "2")]
        Ui(UiEvent),
        #[prost(message, tag = "3")]
        Reply(Reply),
        #[prost(string, tag = "4")]
        End(String),
        #[prost(message, tag = "5")]
        Document(Check),
        /// Discover again for this check, cached or not.
        #[prost(message, tag = "6")]
        Discover(Check),
        #[prost(message, tag = "7")]
        Request(RequestEvent),
        #[prost(message, tag = "8")]
        Workspace(Empty),
        #[prost(string, tag = "9")]
        Storage(String),
        #[prost(message, tag = "10")]
        Shown(Empty),
        #[prost(message, tag = "11")]
        Hidden(Empty),
        #[prost(message, tag = "12")]
        Modules(Empty),
        #[prost(message, tag = "13")]
        ToolCall(ToolCall),
        #[prost(message, tag = "14")]
        Hints(CompleteRequest),
        /// Run a check: the module calls drivers and finishes with `run.result`.
        #[prost(message, tag = "15")]
        Run(RunEvent),
        /// Stop the run with this tag; its driver calls, timers and locks are already gone.
        #[prost(string, tag = "16")]
        Cancel(String),
    }
}

#[derive(Clone, PartialEq, Message)]
pub struct RunEvent {
    #[prost(string, tag = "1")]
    pub tag: String,
    #[prost(message, optional, tag = "2")]
    pub check: Option<Check>,
}

#[derive(Clone, PartialEq, Message)]
pub struct UiEvent {
    /// The module's own message, as the SDK encoded it in the UI tree.
    #[prost(bytes = "vec", tag = "1")]
    pub msg: Vec<u8>,
    #[prost(string, optional, tag = "2")]
    pub value: Option<String>,
}

#[derive(Clone, PartialEq, Message)]
pub struct Reply {
    #[prost(string, tag = "1")]
    pub tag: String,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub ok: Option<Vec<u8>>,
    #[prost(message, optional, tag = "3")]
    pub error: Option<Failure>,
}

#[derive(Clone, PartialEq, Message)]
pub struct RequestEvent {
    #[prost(string, tag = "1")]
    pub id: String,
    #[prost(string, tag = "2")]
    pub service: String,
    #[prost(bool, tag = "3")]
    pub finished: bool,
}

#[derive(Clone, PartialEq, Message)]
pub struct ToolCall {
    #[prost(string, tag = "1")]
    pub tag: String,
    #[prost(string, tag = "2")]
    pub name: String,
    #[prost(bytes = "vec", tag = "3")]
    pub args: Vec<u8>,
}

#[derive(Clone, PartialEq, Message)]
pub struct UpdateRequest {
    #[prost(message, optional, tag = "1")]
    pub event: Option<Event>,
    /// The panel is shown: answer with its UI tree.
    #[prost(bool, tag = "2")]
    pub view: bool,
}

#[derive(Clone, PartialEq, Message)]
pub struct Updated {
    #[prost(message, repeated, tag = "1")]
    pub cmds: Vec<Cmd>,
    #[prost(message, optional, tag = "2")]
    pub view: Option<crate::ui::Node>,
    #[prost(message, optional, tag = "3")]
    pub subscriptions: Option<Subscriptions>,
}

#[derive(Clone, PartialEq, Message)]
pub struct AfterResponse {
    #[prost(message, optional, tag = "1")]
    pub call: Option<DriverCall>,
    #[prost(bytes = "vec", tag = "2")]
    pub response: Vec<u8>,
}

/// A call from the Kernel into a module.
#[derive(Clone, PartialEq, Message)]
pub struct Request {
    #[prost(oneof = "request::Kind", tags = "1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11")]
    pub kind: Option<request::Kind>,
}

pub mod request {
    use super::*;

    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Kind {
        #[prost(message, tag = "1")]
        Update(UpdateRequest),
        #[prost(message, tag = "2")]
        View(Empty),
        #[prost(message, tag = "3")]
        Highlight(HighlightRequest),
        #[prost(message, tag = "4")]
        Complete(CompleteRequest),
        #[prost(message, tag = "5")]
        Fold(Check),
        #[prost(message, tag = "6")]
        Build(Check),
        #[prost(message, tag = "7")]
        Render(Chunk),
        #[prost(message, tag = "8")]
        Export(Check),
        #[prost(string, tag = "9")]
        Import(String),
        #[prost(message, tag = "10")]
        BeforeRequest(DriverCall),
        #[prost(message, tag = "11")]
        AfterResponse(AfterResponse),
    }
}

#[derive(Clone, PartialEq, Message)]
pub struct OptionalNode {
    #[prost(message, optional, tag = "1")]
    pub node: Option<crate::ui::Node>,
}

/// A module's answer to one [`Request`].
#[derive(Clone, PartialEq, Message)]
pub struct Answer {
    #[prost(oneof = "answer::Kind", tags = "1, 2, 3, 4, 5, 6, 7, 8, 9, 10")]
    pub kind: Option<answer::Kind>,
}

pub mod answer {
    use super::*;

    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Kind {
        #[prost(string, tag = "1")]
        Error(String),
        #[prost(message, tag = "2")]
        Updated(Updated),
        #[prost(message, tag = "3")]
        View(OptionalNode),
        #[prost(message, tag = "4")]
        Highlighted(Highlighted),
        #[prost(message, tag = "5")]
        Completions(Completions),
        #[prost(message, tag = "6")]
        Folds(Folds),
        #[prost(message, tag = "7")]
        Call(DriverCall),
        #[prost(message, tag = "8")]
        Rendered(Rendered),
        #[prost(string, tag = "9")]
        Text(String),
        #[prost(message, tag = "10")]
        Cmds(Cmds),
    }
}
