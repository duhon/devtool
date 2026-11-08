//! The UI tree a module returns from `view`: containers and elements, no HTML or CSS. A
//! handler holds the module's own message; the SDK encodes it and the Kernel sends it back
//! as `Event::Ui`.

use prost::Message;
use serde::Serialize;

#[derive(Clone, PartialEq, Message)]
pub struct Node {
    /// Required for the rows of lists and tables.
    #[prost(string, tag = "1")]
    pub key: String,
    /// `primary`, `danger`, `muted`, … or empty.
    #[prost(string, tag = "2")]
    pub variant: String,
    #[prost(
        oneof = "node::Kind",
        tags = "3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18"
    )]
    pub kind: Option<node::Kind>,
}

pub mod node {
    use super::*;

    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Kind {
        #[prost(message, tag = "3")]
        PanelFrame(PanelFrame),
        #[prost(message, tag = "4")]
        Tabs(Tabs),
        #[prost(message, tag = "5")]
        List(List),
        #[prost(message, tag = "6")]
        Table(Table),
        #[prost(message, tag = "7")]
        Row(Children),
        #[prost(message, tag = "8")]
        Column(Children),
        #[prost(string, tag = "9")]
        Text(String),
        #[prost(string, tag = "10")]
        Markdown(String),
        /// A JSON value shown as a tree.
        #[prost(bytes, tag = "11")]
        Tree(Vec<u8>),
        #[prost(message, tag = "12")]
        Input(Input),
        #[prost(message, tag = "13")]
        Select(Select),
        #[prost(message, tag = "14")]
        Checkbox(Checkbox),
        #[prost(message, tag = "15")]
        Slider(Slider),
        #[prost(message, tag = "16")]
        Button(Button),
        #[prost(message, tag = "17")]
        Indicator(Indicator),
        #[prost(message, tag = "18")]
        Editor(Editor),
    }
}

#[derive(Clone, PartialEq, Message)]
pub struct Children {
    #[prost(message, repeated, tag = "1")]
    pub children: Vec<Node>,
}

#[derive(Clone, PartialEq, Message)]
pub struct PanelFrame {
    #[prost(string, tag = "1")]
    pub title: String,
    #[prost(message, repeated, tag = "2")]
    pub body: Vec<Node>,
    #[prost(message, repeated, tag = "3")]
    pub footer: Vec<Node>,
}

#[derive(Clone, PartialEq, Message)]
pub struct Tab {
    #[prost(string, tag = "1")]
    pub key: String,
    #[prost(string, tag = "2")]
    pub label: String,
    #[prost(message, repeated, tag = "3")]
    pub body: Vec<Node>,
}

#[derive(Clone, PartialEq, Message)]
pub struct Tabs {
    #[prost(message, repeated, tag = "1")]
    pub tabs: Vec<Tab>,
    #[prost(string, tag = "2")]
    pub active: String,
    /// Sent with the chosen tab key as the value.
    #[prost(bytes = "vec", optional, tag = "3")]
    pub on_select: Option<Vec<u8>>,
}

/// A virtual list: `rows` is the visible window starting at `start` of `total`.
#[derive(Clone, PartialEq, Message)]
pub struct List {
    #[prost(uint32, tag = "1")]
    pub total: u32,
    #[prost(uint32, tag = "2")]
    pub start: u32,
    #[prost(message, repeated, tag = "3")]
    pub rows: Vec<Node>,
    /// Sent on scroll with `start:end` of the wanted window as the value.
    #[prost(bytes = "vec", optional, tag = "4")]
    pub on_range: Option<Vec<u8>>,
}

#[derive(Clone, PartialEq, Message)]
pub struct TableRow {
    #[prost(string, tag = "1")]
    pub key: String,
    #[prost(message, repeated, tag = "2")]
    pub cells: Vec<Node>,
}

#[derive(Clone, PartialEq, Message)]
pub struct Table {
    #[prost(string, repeated, tag = "1")]
    pub columns: Vec<String>,
    #[prost(message, repeated, tag = "2")]
    pub rows: Vec<TableRow>,
}

#[derive(Clone, PartialEq, Message)]
pub struct Input {
    /// Set only when the module changes the text itself; the field keeps what is typed.
    #[prost(string, optional, tag = "1")]
    pub value: Option<String>,
    #[prost(string, tag = "2")]
    pub placeholder: String,
    /// Sent while typing, debounced.
    #[prost(bytes = "vec", optional, tag = "3")]
    pub on_input: Option<Vec<u8>>,
    /// Sent on Enter or blur.
    #[prost(bytes = "vec", optional, tag = "4")]
    pub on_change: Option<Vec<u8>>,
}

#[derive(Clone, PartialEq, Message)]
pub struct Choice {
    #[prost(string, tag = "1")]
    pub value: String,
    #[prost(string, tag = "2")]
    pub label: String,
}

#[derive(Clone, PartialEq, Message)]
pub struct Select {
    #[prost(message, repeated, tag = "1")]
    pub options: Vec<Choice>,
    #[prost(string, tag = "2")]
    pub value: String,
    #[prost(bytes = "vec", optional, tag = "3")]
    pub on_change: Option<Vec<u8>>,
}

#[derive(Clone, PartialEq, Message)]
pub struct Checkbox {
    #[prost(string, tag = "1")]
    pub label: String,
    #[prost(bool, tag = "2")]
    pub checked: bool,
    /// Sent with `true` or `false` as the value.
    #[prost(bytes = "vec", optional, tag = "3")]
    pub on_change: Option<Vec<u8>>,
}

#[derive(Clone, PartialEq, Message)]
pub struct Slider {
    #[prost(double, tag = "1")]
    pub min: f64,
    #[prost(double, tag = "2")]
    pub max: f64,
    #[prost(double, tag = "3")]
    pub step: f64,
    #[prost(double, tag = "4")]
    pub value: f64,
    #[prost(bytes = "vec", optional, tag = "5")]
    pub on_change: Option<Vec<u8>>,
}

#[derive(Clone, PartialEq, Message)]
pub struct Button {
    #[prost(string, tag = "1")]
    pub label: String,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub on_click: Option<Vec<u8>>,
    #[prost(bool, tag = "3")]
    pub disabled: bool,
}

#[derive(Clone, PartialEq, Message)]
pub struct Indicator {
    /// `idle`, `busy`, `ok`, `warning`, `error`.
    #[prost(string, tag = "1")]
    pub state: String,
    #[prost(string, tag = "2")]
    pub label: String,
}

#[derive(Clone, PartialEq, Message)]
pub struct Editor {
    /// The id of the module that highlights the text.
    #[prost(string, tag = "1")]
    pub language: String,
    #[prost(string, tag = "2")]
    pub text: String,
    #[prost(bytes = "vec", optional, tag = "3")]
    pub on_change: Option<Vec<u8>>,
}

fn msg(msg: &impl Serialize) -> Option<Vec<u8>> {
    Some(crate::wire::json(msg))
}

fn node(kind: node::Kind) -> Node {
    Node {
        key: String::new(),
        variant: String::new(),
        kind: Some(kind),
    }
}

impl Node {
    pub fn key(mut self, key: impl Into<String>) -> Self {
        self.key = key.into();
        self
    }
    pub fn variant(mut self, variant: impl Into<String>) -> Self {
        self.variant = variant.into();
        self
    }
    /// A button that cannot be clicked now.
    pub fn disabled(mut self, disabled: bool) -> Self {
        if let Some(node::Kind::Button(button)) = &mut self.kind {
            button.disabled = disabled;
        }
        self
    }
    /// An input that also reports its text while typing, debounced.
    pub fn on_input(mut self, on_input: &impl Serialize) -> Self {
        if let Some(node::Kind::Input(input)) = &mut self.kind {
            input.on_input = msg(on_input);
        }
        self
    }
}

pub fn text(text: impl Into<String>) -> Node {
    node(node::Kind::Text(text.into()))
}

pub fn markdown(text: impl Into<String>) -> Node {
    node(node::Kind::Markdown(text.into()))
}

pub fn tree(value: &serde_json::Value) -> Node {
    node(node::Kind::Tree(crate::wire::json(value)))
}

pub fn row(children: Vec<Node>) -> Node {
    node(node::Kind::Row(Children { children }))
}

pub fn column(children: Vec<Node>) -> Node {
    node(node::Kind::Column(Children { children }))
}

pub fn panel(title: impl Into<String>, body: Vec<Node>, footer: Vec<Node>) -> Node {
    node(node::Kind::PanelFrame(PanelFrame {
        title: title.into(),
        body,
        footer,
    }))
}

pub fn tabs(tabs: Vec<Tab>, active: impl Into<String>, on_select: &impl Serialize) -> Node {
    node(node::Kind::Tabs(Tabs {
        tabs,
        active: active.into(),
        on_select: msg(on_select),
    }))
}

pub fn tab(key: impl Into<String>, label: impl Into<String>, body: Vec<Node>) -> Tab {
    Tab {
        key: key.into(),
        label: label.into(),
        body,
    }
}

pub fn list(total: u32, start: u32, rows: Vec<Node>, on_range: &impl Serialize) -> Node {
    node(node::Kind::List(List {
        total,
        start,
        rows,
        on_range: msg(on_range),
    }))
}

pub fn table(columns: Vec<String>, rows: Vec<TableRow>) -> Node {
    node(node::Kind::Table(Table { columns, rows }))
}

pub fn button(label: impl Into<String>, on_click: &impl Serialize) -> Node {
    node(node::Kind::Button(Button {
        label: label.into(),
        on_click: msg(on_click),
        disabled: false,
    }))
}

/// An input that reports Enter/blur with `on_change`; set `value` only to replace the text.
/// Variant `secret` hides what is typed; `multiline` is a text area that reports only Enter
/// (Shift+Enter breaks the line).
pub fn input(
    value: Option<String>,
    placeholder: impl Into<String>,
    on_change: &impl Serialize,
) -> Node {
    node(node::Kind::Input(Input {
        value,
        placeholder: placeholder.into(),
        on_input: None,
        on_change: msg(on_change),
    }))
}

pub fn select(options: Vec<Choice>, value: impl Into<String>, on_change: &impl Serialize) -> Node {
    node(node::Kind::Select(Select {
        options,
        value: value.into(),
        on_change: msg(on_change),
    }))
}

pub fn checkbox(label: impl Into<String>, checked: bool, on_change: &impl Serialize) -> Node {
    node(node::Kind::Checkbox(Checkbox {
        label: label.into(),
        checked,
        on_change: msg(on_change),
    }))
}

pub fn slider(min: f64, max: f64, step: f64, value: f64, on_change: &impl Serialize) -> Node {
    node(node::Kind::Slider(Slider {
        min,
        max,
        step,
        value,
        on_change: msg(on_change),
    }))
}

pub fn indicator(state: impl Into<String>, label: impl Into<String>) -> Node {
    node(node::Kind::Indicator(Indicator {
        state: state.into(),
        label: label.into(),
    }))
}

/// A read-only [`editor`].
pub fn viewer(language: impl Into<String>, text: impl Into<String>) -> Node {
    node(node::Kind::Editor(Editor {
        language: language.into(),
        text: text.into(),
        on_change: None,
    }))
}

pub fn editor(
    language: impl Into<String>,
    text: impl Into<String>,
    on_change: &impl Serialize,
) -> Node {
    node(node::Kind::Editor(Editor {
        language: language.into(),
        text: text.into(),
        on_change: msg(on_change),
    }))
}
