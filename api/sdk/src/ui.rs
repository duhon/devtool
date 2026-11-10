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
    /// Space-separated looks: `primary`, `danger`, `muted`, … or empty.
    #[prost(string, tag = "2")]
    pub variant: String,
    #[prost(
        oneof = "node::Kind",
        tags = "3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 21, 22, 23"
    )]
    pub kind: Option<node::Kind>,
    /// The tooltip (`title`).
    #[prost(string, tag = "19")]
    pub title: String,
    /// The accessible name (`aria-label`) of a field, button, editor or list.
    #[prost(string, tag = "20")]
    pub label: String,
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
        #[prost(message, tag = "21")]
        Combobox(Combobox),
        #[prost(message, tag = "22")]
        Anchor(Anchor),
        #[prost(message, tag = "23")]
        Field(Field),
    }
}

#[derive(Clone, PartialEq, Message)]
pub struct Children {
    #[prost(message, repeated, tag = "1")]
    pub children: Vec<Node>,
}

/// A side panel: its header row is either a title or the panel's own tabs, level with the
/// Console's service tabs; `actions` follow them in that row.
#[derive(Clone, PartialEq, Message)]
pub struct PanelFrame {
    #[prost(oneof = "panel_frame::Header", tags = "1, 4")]
    pub header: Option<panel_frame::Header>,
    #[prost(message, repeated, tag = "2")]
    pub body: Vec<Node>,
    #[prost(message, repeated, tag = "3")]
    pub footer: Vec<Node>,
    /// A light on the title: `idle`, `busy`, `ok`, `warning`, `error` or empty for none.
    #[prost(string, tag = "5")]
    pub indicator: String,
    /// In the header row after the title or the tabs: where the content comes from, Reset, …
    #[prost(message, repeated, tag = "6")]
    pub actions: Vec<Node>,
}

pub mod panel_frame {
    use super::*;

    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Header {
        #[prost(string, tag = "1")]
        Title(String),
        /// The panel's own tabs (a [`tabs`](super::tabs) node): the active tab's body opens
        /// the panel body, before `body`.
        #[prost(message, tag = "4", boxed)]
        Tabs(Box<Node>),
    }
}

#[derive(Clone, PartialEq, Message)]
pub struct Tab {
    #[prost(string, tag = "1")]
    pub key: String,
    #[prost(string, tag = "2")]
    pub label: String,
    #[prost(message, repeated, tag = "3")]
    pub body: Vec<Node>,
    /// Double-clicking the label edits it in place: Enter or leaving the field sends the new
    /// label as the value, Escape cancels.
    #[prost(bytes = "vec", optional, tag = "4")]
    pub on_rename: Option<Vec<u8>>,
    /// A light on the tab: `idle`, `busy`, `ok`, `warning`, `error` or empty for none.
    #[prost(string, tag = "5")]
    pub indicator: String,
    /// A `×` after the label sends this with the tab key as the value.
    #[prost(bytes = "vec", optional, tag = "6")]
    pub on_close: Option<Vec<u8>>,
    /// Asked with `window.confirm` before `on_close` is sent; empty asks nothing.
    #[prost(string, tag = "7")]
    pub confirm: String,
    /// The test id of the `×`; `<strip>-close-<tab key>` when empty.
    #[prost(string, tag = "8")]
    pub close_key: String,
    /// The tooltip of the tab.
    #[prost(string, tag = "9")]
    pub title: String,
    /// A button of the strip that is not a tab (no `role=tab`, no `aria-selected`): it
    /// still sends `on_select` and looks active when its key is the active one.
    #[prost(bool, tag = "10")]
    pub plain: bool,
}

impl Tab {
    /// A `×` that closes the tab: `on_close` is sent with the tab key as the value.
    pub fn closable(mut self, on_close: &impl Serialize) -> Self {
        self.on_close = msg(on_close);
        self
    }
    /// The question asked before the tab is closed (see [`Tab::closable`]).
    pub fn confirm(mut self, question: impl Into<String>) -> Self {
        self.confirm = question.into();
        self
    }
    /// The test id of the `×` (see [`Tab::closable`]).
    pub fn close_key(mut self, key: impl Into<String>) -> Self {
        self.close_key = key.into();
        self
    }
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }
    /// A button of the strip that is not a tab, such as a toggle for a list.
    pub fn plain(mut self) -> Self {
        self.plain = true;
        self
    }
    /// A tab renamed in place: see [`Tab::on_rename`].
    pub fn renamed(mut self, on_rename: &impl Serialize) -> Self {
        self.on_rename = msg(on_rename);
        self
    }
    pub fn indicator(mut self, state: impl Into<String>) -> Self {
        self.indicator = state.into();
        self
    }
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
    /// Sent when a tab is dragged onto another, with `["<dragged key>","<target key>"]` as
    /// the value: the dragged tab goes before the target.
    #[prost(bytes = "vec", optional, tag = "4")]
    pub on_reorder: Option<Vec<u8>>,
    /// A `+` after the last tab sends this; with `add_prompt` the value is what the user
    /// typed into the question (nothing is sent when it is cancelled or empty).
    #[prost(bytes = "vec", optional, tag = "5")]
    pub on_add: Option<Vec<u8>>,
    #[prost(string, tag = "6")]
    pub add_prompt: String,
    /// The test id of the `+`; `<strip>-add` when empty.
    #[prost(string, tag = "7")]
    pub add_key: String,
    #[prost(string, tag = "8")]
    pub add_title: String,
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
    /// `active` and `current` highlight the row; `muted` dims its text.
    #[prost(string, tag = "3")]
    pub variant: String,
    /// Shown across the row's width right under it.
    #[prost(message, repeated, tag = "4")]
    pub detail: Vec<Node>,
    /// Sent when the row is clicked outside its buttons, fields and links.
    #[prost(bytes = "vec", optional, tag = "5")]
    pub on_click: Option<Vec<u8>>,
}

impl TableRow {
    pub fn new(key: impl Into<String>, cells: Vec<Node>) -> Self {
        Self {
            key: key.into(),
            cells,
            variant: String::new(),
            detail: Vec::new(),
            on_click: None,
        }
    }
    /// A row that sends `on_click` when it is clicked outside its buttons and fields.
    pub fn clickable(mut self, on_click: &impl Serialize) -> Self {
        self.on_click = msg(on_click);
        self
    }
    /// Content across the row's width right under it, such as an error or a description.
    pub fn detail(mut self, detail: Vec<Node>) -> Self {
        self.detail = detail;
        self
    }
    pub fn variant(mut self, variant: impl Into<String>) -> Self {
        self.variant = variant.into();
        self
    }
}

#[derive(Clone, PartialEq, Message)]
pub struct Table {
    #[prost(string, repeated, tag = "1")]
    pub columns: Vec<String>,
    #[prost(message, repeated, tag = "2")]
    pub rows: Vec<TableRow>,
    /// Column widths in percent, 0 for the rest: the table then has fixed columns and clips
    /// the text of a cell with an ellipsis.
    #[prost(uint32, repeated, tag = "3")]
    pub widths: Vec<u32>,
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
    /// The width in characters; the field keeps it instead of filling its row. 0 fills.
    #[prost(uint32, tag = "5")]
    pub size: u32,
}

#[derive(Clone, PartialEq, Message)]
pub struct Choice {
    #[prost(string, tag = "1")]
    pub value: String,
    #[prost(string, tag = "2")]
    pub label: String,
    /// A second, dim text: the category of a model, …
    #[prost(string, tag = "3")]
    pub detail: String,
}

impl Choice {
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
            detail: String::new(),
        }
    }
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }
}

/// A searchable select: typing filters the options by label, value and detail.
#[derive(Clone, PartialEq, Message)]
pub struct Combobox {
    #[prost(message, repeated, tag = "1")]
    pub options: Vec<Choice>,
    #[prost(string, tag = "2")]
    pub value: String,
    /// Sent with the chosen option's value.
    #[prost(bytes = "vec", optional, tag = "3")]
    pub on_change: Option<Vec<u8>>,
    /// Shown when nothing matches what was typed.
    #[prost(string, tag = "4")]
    pub empty: String,
    #[prost(bool, tag = "5")]
    pub disabled: bool,
}

/// A link that opens an `http(s)` address in a new tab.
#[derive(Clone, PartialEq, Message)]
pub struct Anchor {
    #[prost(string, tag = "1")]
    pub label: String,
    #[prost(string, tag = "2")]
    pub url: String,
}

/// A label and its control: clicking the label focuses the control.
#[derive(Clone, PartialEq, Message)]
pub struct Field {
    #[prost(string, tag = "1")]
    pub label: String,
    #[prost(message, repeated, tag = "2")]
    pub control: Vec<Node>,
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
    /// Sent with the value on every move.
    #[prost(bytes = "vec", optional, tag = "5")]
    pub on_change: Option<Vec<u8>>,
    /// One label per step under the track; choosing one sends its value. With labels the
    /// track shows a tick for each step.
    #[prost(string, repeated, tag = "6")]
    pub labels: Vec<String>,
    /// The step a default sits at, marked on the track; `marker_title` explains it.
    #[prost(uint32, optional, tag = "7")]
    pub marker: Option<u32>,
    #[prost(string, tag = "8")]
    pub marker_title: String,
    #[prost(bool, tag = "9")]
    pub disabled: bool,
}

#[derive(Clone, PartialEq, Message)]
pub struct Button {
    #[prost(string, tag = "1")]
    pub label: String,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub on_click: Option<Vec<u8>>,
    #[prost(bool, tag = "3")]
    pub disabled: bool,
    /// Asked with `window.confirm` before `on_click` is sent; empty asks nothing.
    #[prost(string, tag = "4")]
    pub confirm: String,
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
    /// Dim text shown while the editor is empty.
    #[prost(string, tag = "4")]
    pub placeholder: String,
    /// `on_change` waits this long (milliseconds) for more typing; the pending text is sent
    /// first when the panel closes. 0 sends every change at once.
    #[prost(uint32, tag = "5")]
    pub debounce_ms: u32,
}

fn msg(msg: &impl Serialize) -> Option<Vec<u8>> {
    Some(crate::wire::json(msg))
}

fn node(kind: node::Kind) -> Node {
    Node {
        key: String::new(),
        variant: String::new(),
        kind: Some(kind),
        title: String::new(),
        label: String::new(),
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
    /// The tooltip of any node.
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }
    /// The accessible name (`aria-label`) of a field, button or editor.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }
    /// A button, slider or combobox that cannot be used now.
    pub fn disabled(mut self, disabled: bool) -> Self {
        match &mut self.kind {
            Some(node::Kind::Button(button)) => button.disabled = disabled,
            Some(node::Kind::Slider(slider)) => slider.disabled = disabled,
            Some(node::Kind::Combobox(combobox)) => combobox.disabled = disabled,
            _ => {}
        }
        self
    }
    /// Button: the question asked with `window.confirm` before `on_click` is sent.
    pub fn confirm(mut self, question: impl Into<String>) -> Self {
        if let Some(node::Kind::Button(button)) = &mut self.kind {
            button.confirm = question.into();
        }
        self
    }
    /// Table: column widths in percent; the columns are then fixed and a cell's text is clipped
    /// with an ellipsis. With some widths 0, those columns fit their content instead and the
    /// others share the rest.
    pub fn widths(mut self, widths: Vec<u32>) -> Self {
        if let Some(node::Kind::Table(table)) = &mut self.kind {
            table.widths = widths;
        }
        self
    }
    /// Tabs: a `+` after the last tab sends `on_add`.
    pub fn on_add(mut self, on_add: &impl Serialize) -> Self {
        if let Some(node::Kind::Tabs(tabs)) = &mut self.kind {
            tabs.on_add = msg(on_add);
        }
        self
    }
    /// Tabs: the question asked before `on_add` is sent; the answer is the value.
    pub fn add_prompt(mut self, question: impl Into<String>) -> Self {
        if let Some(node::Kind::Tabs(tabs)) = &mut self.kind {
            tabs.add_prompt = question.into();
        }
        self
    }
    /// Tabs: the test id and the tooltip of the `+`.
    pub fn add_button(mut self, key: impl Into<String>, title: impl Into<String>) -> Self {
        if let Some(node::Kind::Tabs(tabs)) = &mut self.kind {
            tabs.add_key = key.into();
            tabs.add_title = title.into();
        }
        self
    }
    /// Input: the width in characters.
    pub fn size(mut self, chars: u32) -> Self {
        if let Some(node::Kind::Input(input)) = &mut self.kind {
            input.size = chars;
        }
        self
    }
    /// Editor or input: the dim text shown while it is empty.
    pub fn placeholder(mut self, text: impl Into<String>) -> Self {
        match &mut self.kind {
            Some(node::Kind::Editor(editor)) => editor.placeholder = text.into(),
            Some(node::Kind::Input(input)) => input.placeholder = text.into(),
            _ => {}
        }
        self
    }
    /// Editor: its changes are sent after this many milliseconds without typing.
    pub fn debounce(mut self, ms: u32) -> Self {
        if let Some(node::Kind::Editor(editor)) = &mut self.kind {
            editor.debounce_ms = ms;
        }
        self
    }
    /// Combobox: the text shown when nothing matches.
    pub fn empty_text(mut self, text: impl Into<String>) -> Self {
        if let Some(node::Kind::Combobox(combobox)) = &mut self.kind {
            combobox.empty = text.into();
        }
        self
    }
    /// Slider: one label per step under the track; choosing one sends its value.
    pub fn labels(mut self, labels: Vec<String>) -> Self {
        if let Some(node::Kind::Slider(slider)) = &mut self.kind {
            slider.labels = labels;
        }
        self
    }
    /// Slider: marks the step of a default on the track.
    pub fn marker(mut self, step: u32, title: impl Into<String>) -> Self {
        if let Some(node::Kind::Slider(slider)) = &mut self.kind {
            slider.marker = Some(step);
            slider.marker_title = title.into();
        }
        self
    }
    /// Tabs that can be reordered by dragging.
    pub fn on_reorder(mut self, on_reorder: &impl Serialize) -> Self {
        if let Some(node::Kind::Tabs(tabs)) = &mut self.kind {
            tabs.on_reorder = msg(on_reorder);
        }
        self
    }
    /// A light on a panel's title: `idle`, `busy`, `ok`, `warning`, `error` or empty.
    pub fn indicator(mut self, state: impl Into<String>) -> Self {
        if let Some(node::Kind::PanelFrame(frame)) = &mut self.kind {
            frame.indicator = state.into();
        }
        self
    }
    /// What a panel shows in its header row after the title or the tabs.
    pub fn actions(mut self, actions: Vec<Node>) -> Self {
        if let Some(node::Kind::PanelFrame(frame)) = &mut self.kind {
            frame.actions = actions;
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

/// A bold section title.
pub fn heading(text: impl Into<String>) -> Node {
    text_with(text, "heading")
}

/// A small bordered label, such as where a panel's content comes from.
pub fn badge(text: impl Into<String>) -> Node {
    text_with(text, "badge")
}

fn text_with(text: impl Into<String>, variant: &str) -> Node {
    self::text(text).variant(variant)
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

/// An empty stretch that takes the free space of a [`row`], pushing the items after it to
/// the row's end.
pub fn spacer() -> Node {
    row(Vec::new()).variant("spacer")
}

/// A side panel headed by its title.
pub fn panel(title: impl Into<String>, body: Vec<Node>, footer: Vec<Node>) -> Node {
    node(node::Kind::PanelFrame(PanelFrame {
        header: Some(panel_frame::Header::Title(title.into())),
        body,
        footer,
        indicator: String::new(),
        actions: Vec::new(),
    }))
}

/// A side panel headed by its own tabs (a [`tabs`] node) instead of a title; `body` follows
/// the active tab's body.
pub fn tabbed_panel(tabs: Node, body: Vec<Node>, footer: Vec<Node>) -> Node {
    node(node::Kind::PanelFrame(PanelFrame {
        header: Some(panel_frame::Header::Tabs(Box::new(tabs))),
        body,
        footer,
        indicator: String::new(),
        actions: Vec::new(),
    }))
}

pub fn tabs(tabs: Vec<Tab>, active: impl Into<String>, on_select: &impl Serialize) -> Node {
    node(node::Kind::Tabs(Tabs {
        tabs,
        active: active.into(),
        on_select: msg(on_select),
        on_reorder: None,
        on_add: None,
        add_prompt: String::new(),
        add_key: String::new(),
        add_title: String::new(),
    }))
}

pub fn tab(key: impl Into<String>, label: impl Into<String>, body: Vec<Node>) -> Tab {
    Tab {
        key: key.into(),
        label: label.into(),
        body,
        on_rename: None,
        indicator: String::new(),
        on_close: None,
        confirm: String::new(),
        close_key: String::new(),
        title: String::new(),
        plain: false,
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
    node(node::Kind::Table(Table {
        columns,
        rows,
        widths: Vec::new(),
    }))
}

pub fn button(label: impl Into<String>, on_click: &impl Serialize) -> Node {
    node(node::Kind::Button(Button {
        label: label.into(),
        on_click: msg(on_click),
        disabled: false,
        confirm: String::new(),
    }))
}

/// Clickable inline text (dotted underline) that opens more about what it names, in place
/// of a boxed [`button`].
pub fn link(label: impl Into<String>, on_click: &impl Serialize) -> Node {
    button(label, on_click).variant("link")
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
        size: 0,
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
        labels: Vec::new(),
        marker: None,
        marker_title: String::new(),
        disabled: false,
    }))
}

/// A select whose options are searched by typing.
pub fn combobox(
    options: Vec<Choice>,
    value: impl Into<String>,
    on_change: &impl Serialize,
) -> Node {
    node(node::Kind::Combobox(Combobox {
        options,
        value: value.into(),
        on_change: msg(on_change),
        empty: String::new(),
        disabled: false,
    }))
}

/// A link that opens `url` (`http` or `https`) in a new tab.
pub fn anchor(label: impl Into<String>, url: impl Into<String>) -> Node {
    node(node::Kind::Anchor(Anchor {
        label: label.into(),
        url: url.into(),
    }))
}

/// A label and its control side by side; variant `grid` aligns the controls of several
/// fields in a column.
pub fn field(label: impl Into<String>, control: Node) -> Node {
    node(node::Kind::Field(Field {
        label: label.into(),
        control: vec![control],
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
        placeholder: String::new(),
        debounce_ms: 0,
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
        placeholder: String::new(),
        debounce_ms: 0,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_fields_survive_the_wire() {
        let tree = tabs(
            vec![
                tab("a", "A", vec![])
                    .closable(&"close")
                    .confirm("Sure?")
                    .title("tip"),
                tab("b", "B", vec![]).plain(),
            ],
            "a",
            &"select",
        )
        .on_add(&"add")
        .add_prompt("Name")
        .add_button("add-x", "New");
        let table = table(
            vec!["x".into()],
            vec![TableRow::new("r", vec![text("1")]).detail(vec![text("more")])],
        )
        .widths(vec![50, 50]);
        let combo = combobox(vec![Choice::new("v", "V").detail("d")], "v", &"pick")
            .label("Model")
            .title("tip");
        for node in [
            tree,
            table,
            combo,
            anchor("docs", "https://x"),
            field("L", text("c")),
        ] {
            let bytes = node.encode_to_vec();
            assert_eq!(Node::decode(bytes.as_slice()).unwrap(), node);
        }
    }

    #[test]
    fn builders_set_what_the_widgets_read() {
        let Some(node::Kind::Tabs(strip)) = tabs(vec![tab("a", "A", vec![]).closable(&1)], "a", &0)
            .on_add(&2)
            .add_prompt("p")
            .kind
        else {
            panic!("not tabs");
        };
        assert!(strip.tabs[0].on_close.is_some() && strip.on_add.is_some());
        assert_eq!(strip.add_prompt, "p");
        let slider = slider(0.0, 2.0, 1.0, 1.0, &0)
            .labels(vec!["a".into(), "b".into(), "c".into()])
            .marker(2, "default");
        let Some(node::Kind::Slider(slider)) = slider.kind else {
            panic!("not a slider");
        };
        assert_eq!((slider.labels.len(), slider.marker), (3, Some(2)));
    }
}
