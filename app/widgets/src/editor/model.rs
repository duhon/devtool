//! An editor session: the model an owner keeps for one editor (text, highlighting,
//! hints, its extension) and its update function. The browser view renders a
//! session and turns DOM input into [`Msg`]; [`Session::update`] returns the [`Event`]s
//! the owner handles. Pure, so native tests cover the editor's behaviour.
use super::extension::{self, Extension, Payload, Refresh, Viewport};
use super::highlight::Highlighter;
use super::layout;
use super::text::{Position, TextChange, utf16_len};
use super::{Anchor, Change, Indent, Selection, State, Transaction, Unit};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

/// How long an Agent edit stays highlighted; the CSS fade (`editor-agent-edit`) lasts as long.
pub const AGENT_EDIT_FLASH_MS: f64 = 1200.0;
/// Typing in longer lines does not open hints.
const HINT_LINE_LIMIT: usize = 1000;

thread_local! {
    /// What the last copy put on the clipboard and whether it was a whole line.
    static LAST_COPIED: RefCell<Option<(String, bool)>> = const { RefCell::new(None) };
}

/// An Agent edit of one editor, applied only while the text is still what the Agent saw.
#[derive(Clone, Debug, PartialEq)]
pub struct EditCommand {
    pub serial: u64,
    pub expected: String,
    pub from: Position,
    pub to: Position,
    pub text: String,
}

/// What an editor edits; fixed for its lifetime except `readonly`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Setup {
    /// The highlighting mode: a language its extension may refine.
    pub mode: String,
    /// What the owner's extension edits; a kept editor of another scope is replaced.
    pub scope: String,
    /// A document: a fold gutter, room below the last line and viewport events.
    pub document: bool,
    pub readonly: bool,
    /// Lines that are only `\u{200b}` stand for no line: the gutter skips them.
    pub ghosts: bool,
}

/// What completion reads; the owner updates it as data arrives.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Context {
    pub hints: Vec<(String, String)>,
    pub templates: BTreeMap<String, String>,
    pub discovery: serde_json::Value,
}

/// The scroller's geometry the view measured, in CSS pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Metrics {
    pub scroll_top: f64,
    pub scroll_left: f64,
    pub client_height: f64,
    pub client_width: f64,
    pub line_height: f64,
    pub char_width: f64,
    pub gutter_width: f64,
}

/// A key press, as the view saw it.
#[derive(Clone, Copy, Debug, Default)]
pub struct Key<'a> {
    /// `KeyboardEvent.key`.
    pub key: &'a str,
    /// `KeyboardEvent.code`, which names letters independently of Alt and the layout.
    pub code: &'a str,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
    pub mac: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Run,
    Complete,
    Newline,
    ToggleFold,
    Backspace,
    Delete,
    Discover,
    Tab,
    Indent(Indent),
    Undo,
    Redo,
    SelectAll,
    DeleteLine,
    DeleteBy(i8, Unit),
    MoveH(i8, Unit, bool),
    MoveV(i8, bool),
    Page(i8, bool),
    Home(bool),
    End(bool),
    LineStart(bool),
    DocStart(bool),
    DocEnd(bool),
    HintMove(isize, bool),
    HintFirst,
    HintLast,
    HintPick,
    HintClose,
}

/// Work deferred to after the current task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    RefreshHints,
    /// Reopens hints after a completion that continues.
    Reshow,
}

impl Signal {
    const ALL: usize = 2;
    fn index(self) -> usize {
        self as usize
    }
}

pub enum Msg {
    Command(Command),
    /// Typed or composed text.
    Input(String),
    Paste(String),
    Cut,
    /// Mouse down in the text: `clicks` 1 places the cursor, 2 selects a word, 3 a line.
    Press {
        at: Position,
        extend: bool,
        clicks: u8,
    },
    Drag(Position),
    Release,
    Focus(bool),
    Metrics(Metrics),
    ToggleFold(usize),
    Unfold(usize),
    SelectHint(usize),
    PickHint(usize),
    Signal(Signal, u64),
    Expire(u64),
    /// Returns the events of changes made outside `update` (the facade, `sync`).
    Flush,
}

#[derive(Clone, Debug)]
pub enum Event {
    /// The text the editor now has.
    Changed(String),
    /// The extension's record of an edit.
    Edited(Payload),
    EditError(String),
    Run {
        source: String,
        line: usize,
    },
    Discover,
    /// The user asked for hints; what the extension tells the owner.
    Hints(Payload),
    Cursor(usize),
    Viewport(Viewport),
}

/// What the view does after rendering.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Effect {
    /// Send `Msg::Signal(signal, token)` after the current task.
    Later(Signal, u64),
    /// Send `Msg::Expire(id)` after `ms`.
    Expire(u64, f64),
    ScrollToCursor,
    /// Scroll so `line` is at the top.
    ScrollToLine(usize),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub text: String,
    pub display: String,
    pub badge: Option<char>,
    pub continue_hint: bool,
    pub literal: bool,
    pub comma: Option<Position>,
}

/// The open hints list.
#[derive(Clone, Debug, PartialEq)]
pub struct Popup {
    pub from: Position,
    pub to: Position,
    pub items: Vec<Item>,
    pub selected: usize,
}

#[derive(Clone, Copy, Debug, Default)]
struct Hints {
    enabled: bool,
    automatic: bool,
    applying: bool,
}

/// A highlighted range: `tok-<class>` style classes on its text, or a class on its line.
#[derive(Clone, Debug, PartialEq)]
pub enum Mark {
    Range {
        from: Position,
        to: Position,
        class: String,
    },
    Line {
        anchor: Anchor,
        class: String,
    },
}

#[derive(Clone, Copy, Debug)]
struct Press {
    origin: Selection,
    clicks: u8,
}

/// Collects what one operation did, so events go out in a fixed order:
/// changes, viewport, cursor, then hints.
#[derive(Default)]
struct Operation {
    changes: Vec<TextChange>,
    cursor: bool,
    /// A typed `/` asks the extension for hints.
    hints: bool,
    scroll: bool,
    /// Hints were already refreshed, so cursor activity need not.
    hinted: bool,
}

pub struct Session {
    state: State,
    highlighter: Highlighter,
    setup: Setup,
    context: Context,
    hints: Hints,
    popup: Option<Popup>,
    focused: bool,
    metrics: Option<Metrics>,
    viewport: Option<Viewport>,
    brackets: Vec<(Position, bool)>,
    marks: BTreeMap<u64, Mark>,
    next_mark: u64,
    signals: [Option<(u64, Option<u64>)>; Signal::ALL],
    next_token: u64,
    agent_serial: u64,
    press: Option<Press>,
    syncing: bool,
    source: RefCell<Option<Rc<str>>>,
    op: Option<Operation>,
    /// Open `start_operation` calls, and whether the outermost one opened `op`.
    batch: (usize, bool),
    events: Vec<Event>,
    effects: Vec<Effect>,
    changes: Vec<TextChange>,
    revision: u64,
    edits: u64,
}

impl Session {
    pub fn new(setup: Setup, extension: Rc<dyn Extension>, text: &str) -> Self {
        let state = State::new(text);
        extension.synchronize(text, state.generation());
        Self {
            state,
            highlighter: Highlighter::with_extension(&setup.mode, extension),
            setup,
            context: Context::default(),
            hints: Hints::default(),
            popup: None,
            focused: false,
            metrics: None,
            viewport: None,
            brackets: Vec::new(),
            marks: BTreeMap::new(),
            next_mark: 0,
            signals: [None; Signal::ALL],
            next_token: 0,
            agent_serial: 0,
            press: None,
            syncing: false,
            source: RefCell::new(None),
            op: None,
            batch: (0, false),
            events: Vec::new(),
            effects: Vec::new(),
            changes: Vec::new(),
            revision: 0,
            edits: 0,
        }
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    /// The model and its highlighter, for rendering.
    pub fn parts(&mut self) -> (&State, &mut Highlighter) {
        (&self.state, &mut self.highlighter)
    }

    pub fn setup(&self) -> &Setup {
        &self.setup
    }

    pub fn context(&self) -> &Context {
        &self.context
    }

    pub fn extension(&self) -> &Rc<dyn Extension> {
        self.highlighter.extension()
    }

    pub fn popup(&self) -> Option<&Popup> {
        self.popup.as_ref()
    }

    pub fn focused(&self) -> bool {
        self.focused
    }

    pub fn metrics(&self) -> Option<Metrics> {
        self.metrics
    }

    pub fn brackets(&self) -> &[(Position, bool)] {
        &self.brackets
    }

    pub fn marks(&self) -> impl Iterator<Item = (u64, &Mark)> {
        self.marks.iter().map(|(id, mark)| (*id, mark))
    }

    /// Bumps whenever something the view shows changes.
    pub fn revision(&self) -> u64 {
        self.revision.wrapping_add(self.state.version())
    }

    /// The whole text, cached until it changes.
    pub fn source(&self) -> Rc<str> {
        self.source
            .borrow_mut()
            .get_or_insert_with(|| Rc::from(self.state.text()))
            .clone()
    }

    pub fn anchor(&self, line: usize) -> Option<Anchor> {
        self.state.anchor(line)
    }

    pub fn anchor_line(&self, anchor: Anchor) -> Option<usize> {
        self.state.anchor_line(anchor)
    }

    /// Bumps whenever the text changes.
    pub fn edits(&self) -> u64 {
        self.edits
    }

    /// Whether the view has work to do after rendering.
    pub fn has_effects(&self) -> bool {
        !self.effects.is_empty()
    }

    /// Whether events wait for the owner, queued by a change made outside `update`.
    pub fn pending(&self) -> bool {
        !self.events.is_empty()
    }

    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    /// Drops the scrolls still asked for: a scroll the owner makes now wins over them.
    pub fn cancel_scrolls(&mut self) {
        self.effects
            .retain(|effect| !matches!(effect, Effect::ScrollToCursor | Effect::ScrollToLine(_)));
    }

    pub fn take_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.effects)
    }

    /// The changes since the last call, for `changes` listeners of the facade.
    pub fn take_changes(&mut self) -> Vec<TextChange> {
        std::mem::take(&mut self.changes)
    }

    pub fn update(&mut self, msg: Msg, now: f64) -> Vec<Event> {
        self.dispatch(msg, now);
        self.take_events()
    }

    /// Handles `msg` but keeps its events queued, like the other external changes.
    pub fn dispatch(&mut self, msg: Msg, now: f64) {
        match msg {
            Msg::Command(command) => self.command(command, now),
            Msg::Input(text) => self.input(&text, "+input", now),
            Msg::Paste(text) => self.paste(&text, now),
            Msg::Cut => self.cut(now),
            Msg::Press { at, extend, clicks } => self.press(at, extend, clicks),
            Msg::Drag(at) => self.drag(at),
            Msg::Release => self.press = None,
            Msg::Focus(focused) => self.focus(focused),
            Msg::Metrics(metrics) => self.set_metrics(metrics),
            Msg::ToggleFold(line) => self.toggle_fold(line),
            Msg::Unfold(line) => {
                self.unfold(line);
            }
            Msg::SelectHint(index) => {
                if let Some(popup) = &mut self.popup
                    && index < popup.items.len()
                {
                    popup.selected = index;
                    self.revision += 1;
                }
            }
            Msg::PickHint(index) => self.pick(index, now),
            Msg::Signal(signal, token) => self.fire(signal, token),
            Msg::Expire(id) => {
                if self.marks.remove(&id).is_some() {
                    self.revision += 1;
                }
            }
            Msg::Flush => {}
        }
    }

    /// The command a key runs; `None` leaves the key to the browser.
    pub fn resolve(&self, key: Key) -> Option<Command> {
        let name = key_name(key);
        let primary = if key.mac { key.meta } else { key.ctrl };
        let other = if key.mac { key.ctrl } else { key.meta };
        if self.popup.is_some() && !key.ctrl && !key.alt && !key.meta {
            let popup = match (name.as_str(), key.shift) {
                ("Up", false) => Some(Command::HintMove(-1, true)),
                ("Down", false) => Some(Command::HintMove(1, true)),
                ("PageUp", false) => {
                    Some(Command::HintMove(-(self.menu_size() as isize - 1), false))
                }
                ("PageDown", false) => {
                    Some(Command::HintMove(self.menu_size() as isize - 1, false))
                }
                ("Home", false) => Some(Command::HintFirst),
                ("End", false) => Some(Command::HintLast),
                ("Enter" | "Tab", false) => Some(Command::HintPick),
                ("Esc", _) => Some(Command::HintClose),
                _ => None,
            };
            if popup.is_some() {
                return popup;
            }
        }
        let extend = key.shift;
        let command = match name.as_str() {
            "Enter" if key.shift && !key.ctrl && !key.alt && !key.meta => Command::Run,
            "Enter" if !key.ctrl && !key.alt && !key.meta => Command::Newline,
            "Space" if key.ctrl && !key.alt && !key.meta && !key.shift => Command::Complete,
            "Space" if key.alt && !key.ctrl && !key.meta && !key.shift => Command::Discover,
            "Q" if key.ctrl && !key.alt && !key.meta && !key.shift => Command::ToggleFold,
            "Tab" if !key.ctrl && !key.alt && !key.meta => {
                if key.shift {
                    Command::Indent(Indent::Prev)
                } else {
                    Command::Tab
                }
            }
            "Backspace" if key.mac && key.meta && !key.alt => Command::DeleteBy(-1, Unit::Line),
            "Delete" if key.mac && key.meta && !key.alt => Command::DeleteBy(1, Unit::Line),
            "Backspace" if key.alt || (!key.mac && key.ctrl) => Command::DeleteBy(-1, Unit::Group),
            "Delete" if key.alt || (!key.mac && key.ctrl) => Command::DeleteBy(1, Unit::Group),
            "Backspace" if !key.ctrl && !key.meta => Command::Backspace,
            "Delete" if !key.ctrl && !key.meta && !key.shift => Command::Delete,
            "A" if primary && !key.alt && !key.shift => Command::SelectAll,
            "A" if key.mac && key.ctrl && !key.meta && !key.alt => Command::LineStart(extend),
            "E" if key.mac && key.ctrl && !key.meta && !key.alt => Command::End(extend),
            "D" if primary && !key.alt && !key.shift => Command::DeleteLine,
            "Z" if primary && !key.alt => {
                if key.shift {
                    Command::Redo
                } else {
                    Command::Undo
                }
            }
            "Y" if primary && !key.alt && !key.shift => Command::Redo,
            "[" if primary && !key.alt && !key.shift => Command::Indent(Indent::Subtract),
            "]" if primary && !key.alt && !key.shift => Command::Indent(Indent::Add),
            _ if other || (key.ctrl && key.meta) => return None,
            "Left" | "Right" => {
                let dir = if name == "Left" { -1 } else { 1 };
                if (key.mac && key.meta) || (key.alt && !key.mac) {
                    if dir < 0 {
                        Command::LineStart(extend)
                    } else {
                        Command::End(extend)
                    }
                } else if key.alt || key.ctrl {
                    Command::MoveH(dir, Unit::Group, extend)
                } else {
                    Command::MoveH(dir, Unit::Char, extend)
                }
            }
            "Up" | "Down" if key.alt => return None,
            "Up" if primary || (key.mac && key.ctrl) => Command::DocStart(extend),
            "Down" if primary || (key.mac && key.ctrl) => Command::DocEnd(extend),
            "Up" => Command::MoveV(-1, extend),
            "Down" => Command::MoveV(1, extend),
            "Home" if primary => Command::DocStart(extend),
            "End" if primary => Command::DocEnd(extend),
            "Home" if !key.alt => Command::Home(extend),
            "End" if !key.alt => Command::End(extend),
            "PageUp" if !key.alt && !primary => Command::Page(-1, extend),
            "PageDown" if !key.alt && !primary => Command::Page(1, extend),
            _ => return None,
        };
        Some(command)
    }

    fn menu_size(&self) -> usize {
        self.popup
            .as_ref()
            .map_or(1, |popup| popup.items.len().clamp(1, 10))
    }

    /// The text a copy puts on the clipboard: the selection, or the whole line when nothing
    /// is selected. The extension hears what was copied.
    pub fn copy(&self) -> String {
        let selection = self.state.selection();
        let (text, line_wise) = if selection.empty() {
            let (from, to) = self.line_range(selection.head.line);
            (self.state.range(from, to), true)
        } else {
            (self.state.selected_text(), false)
        };
        self.extension().copy(selection.from(), selection.to());
        LAST_COPIED.with(|copied| *copied.borrow_mut() = Some((text.clone(), line_wise)));
        text
    }

    fn line_range(&self, line: usize) -> (Position, Position) {
        let from = Position { line, ch: 0 };
        let to = if line < self.state.last_line() {
            Position {
                line: line + 1,
                ch: 0,
            }
        } else {
            Position {
                line,
                ch: utf16_len(self.state.line(line)),
            }
        };
        (from, to)
    }

    /// Replaces the text; the owner calls this when its document changed. `preserve_after`
    /// caps the common prefix so a response inserted there keeps the next line's anchor.
    /// The extension synchronizes with it too.
    pub fn sync(&mut self, text: &str, preserve_after: Option<usize>, now: f64) {
        self.syncing = true;
        let selection = self.state.selection();
        let changes =
            self.change(|state| state.set_text(text, preserve_after, "+services-response", now));
        self.changes.extend(changes);
        self.synchronize();
        if self.state.selection() != selection {
            self.cursor_activity();
            self.events.push(Event::Cursor(self.state.cursor().line));
        }
        self.emit_viewport();
        self.syncing = false;
    }

    /// The range a fold starting at `line` would cover.
    pub fn fold_range(&mut self, line: usize) -> Option<super::state::Fold> {
        super::fold::range(&mut self.highlighter, self.state.lines(), line)
    }

    /// Highlights everything again from scratch: what the extension highlights changed.
    pub fn relight(&mut self) {
        self.highlighter.reset();
        self.revision += 1;
    }

    /// The extension synchronizes with the text as it is, e.g. after one of its options
    /// changed.
    pub fn synchronize(&mut self) {
        let text = self.source();
        let refresh = self.extension().synchronize(&text, self.state.generation());
        self.refresh(refresh);
    }

    /// Sets an option of the extension's; true when it took it.
    pub fn set_option(&mut self, name: &str, value: serde_json::Value) -> bool {
        let taken = self.extension().set_option(name, value);
        if taken {
            self.synchronize();
        }
        taken
    }

    fn refresh(&mut self, refresh: Refresh) {
        if refresh.relight {
            self.highlighter.reset();
        }
        if refresh.relight || refresh.redraw {
            self.revision += 1;
        }
    }

    /// Updates what completion reads; open hints follow it. Returns whether it changed.
    pub fn set_context(&mut self, context: Context) -> bool {
        if context == self.context {
            return false;
        }
        self.context = context;
        if self.hints.enabled && self.focused {
            self.show_hints(self.hints.automatic);
        }
        self.revision += 1;
        true
    }

    pub fn set_readonly(&mut self, readonly: bool) {
        if readonly != self.setup.readonly {
            self.setup.readonly = readonly;
            self.revision += 1;
        }
    }

    /// Puts the cursor on `line` and scrolls it to the top.
    pub fn scroll_to_line(&mut self, line: usize) {
        self.select(Selection::cursor(Position { line, ch: 0 }), false);
        self.effects.push(Effect::ScrollToLine(line));
    }

    /// Changes the text from outside the keyboard (the facade, tests): no input handling.
    pub fn apply(&mut self, transaction: Transaction, now: f64) -> Vec<TextChange> {
        let mut applied = Vec::new();
        self.operation(|session| {
            applied = session.change(|state| state.apply(transaction, now));
            applied.clone()
        });
        applied
    }

    pub fn select(&mut self, selection: Selection, scroll: bool) {
        self.operation(|session| {
            session.state.set_selection(selection);
            if let Some(op) = &mut session.op {
                op.scroll = scroll;
            }
            Vec::new()
        });
    }

    pub fn undo(&mut self) {
        self.command(Command::Undo, 0.0);
    }

    pub fn redo(&mut self) {
        self.command(Command::Redo, 0.0);
    }

    pub fn clear_history(&mut self) {
        self.state.clear_history();
    }

    pub fn close_history(&mut self) -> u64 {
        self.state.close_history()
    }

    pub fn mark(&mut self, mark: Mark) -> u64 {
        self.next_mark += 1;
        self.marks.insert(self.next_mark, mark);
        self.revision += 1;
        self.next_mark
    }

    pub fn clear_mark(&mut self, id: u64) {
        if self.marks.remove(&id).is_some() {
            self.revision += 1;
        }
    }

    pub fn find_mark(&self, id: u64) -> Option<(Position, Position)> {
        match self.marks.get(&id)? {
            Mark::Range { from, to, .. } => Some((*from, *to)),
            Mark::Line { anchor, .. } => {
                let line = self.state.anchor_line(*anchor)?;
                Some((
                    Position { line, ch: 0 },
                    Position {
                        line,
                        ch: utf16_len(self.state.line(line)),
                    },
                ))
            }
        }
    }

    /// Applies an Agent edit when the document is still what the Agent saw (`current`: and
    /// whatever else the owner expects still holds), and flashes it. `None` when this
    /// command was already applied.
    pub fn agent_edit(
        &mut self,
        command: &EditCommand,
        current: bool,
        now: f64,
    ) -> Option<Result<(), String>> {
        if command.serial == self.agent_serial {
            return None;
        }
        self.agent_serial = command.serial;
        if *self.source() != *command.expected || !current {
            return Some(Err(
                "CONFLICT: editor changed before the Agent edit was applied".into(),
            ));
        }
        self.state.close_history();
        let replaced = self.state.range(command.from, command.to);
        let length = utf16_len(&self.source());
        self.apply(
            Transaction::replace(command.from, command.to, &command.text, "+services-agent"),
            now,
        );
        self.state.close_history();
        let inserted = (utf16_len(&replaced) + utf16_len(&self.source())).saturating_sub(length);
        self.flash(command.from, &replaced, inserted);
        Some(Ok(()))
    }

    fn flash(&mut self, from: Position, replaced: &str, inserted: usize) {
        let start = self.state.index(from);
        let new: Vec<u16> = self
            .state
            .range(from, self.state.position(start + inserted))
            .encode_utf16()
            .collect();
        let old: Vec<u16> = replaced.encode_utf16().collect();
        if old == new {
            return;
        }
        let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
        let suffix = old
            .iter()
            .rev()
            .zip(new.iter().rev())
            .take(old.len().min(new.len()) - prefix)
            .take_while(|(a, b)| a == b)
            .count();
        let (mut first, mut last) = (prefix, new.len() - suffix);
        let word = |unit: u16| !matches!(unit, 0x09 | 0x0a | 0x0d | 0x20);
        let mark = if first < last {
            while first > 0 && word(new[first - 1]) {
                first -= 1;
            }
            while last < new.len() && word(new[last]) {
                last += 1;
            }
            Mark::Range {
                from: self.state.position(start + first),
                to: self.state.position(start + last),
                class: "editor-agent-edit".into(),
            }
        } else {
            let line = self.state.position(start + first).line;
            let Some(anchor) = self.state.anchor(line) else {
                return;
            };
            Mark::Line {
                anchor,
                class: "editor-agent-edit-line".into(),
            }
        };
        let id = self.mark(mark);
        self.effects.push(Effect::Expire(id, AGENT_EDIT_FLASH_MS));
    }

    /// The fold marker of `line` in a document's gutter: `Some(true)` when folded.
    pub fn fold_marker(&mut self, line: usize) -> Option<bool> {
        if self.state.fold_starting(line).is_some() {
            return Some(true);
        }
        super::fold::range(&mut self.highlighter, self.state.lines(), line).map(|_| false)
    }

    pub fn set_metrics(&mut self, metrics: Metrics) {
        self.metrics = Some(metrics);
        self.emit_viewport();
    }

    /// The viewport a document reports: the top line and the cursor line when visible.
    pub fn viewport(&self) -> Option<Viewport> {
        let metrics = self.metrics?;
        if metrics.line_height <= 0.0 {
            return None;
        }
        let rows = self.state.row_count();
        let row =
            ((metrics.scroll_top - layout::PADDING_TOP).max(0.0) / metrics.line_height) as usize;
        let top = self.state.line_of_row(row.min(rows - 1));
        let cursor = self.state.cursor();
        let visible = metrics.client_height > 0.0
            && metrics.client_width > 0.0
            && layout::caret(&self.state, &metrics, cursor).is_some_and(|caret| {
                caret.bottom > metrics.scroll_top
                    && caret.top < metrics.scroll_top + metrics.client_height
                    && caret.left >= metrics.scroll_left
                    && metrics.gutter_width + caret.left - metrics.scroll_left
                        < metrics.client_width
            });
        Some(Viewport {
            top,
            visible_cursor: visible.then_some(cursor.line),
        })
    }

    fn emit_viewport(&mut self) {
        if !self.setup.document {
            return;
        }
        let viewport = self.viewport();
        if viewport.is_some() && viewport != self.viewport {
            self.viewport = viewport;
            if let Some(viewport) = viewport {
                self.events.push(Event::Viewport(viewport));
            }
        }
    }

    fn command(&mut self, command: Command, now: f64) {
        let editing = matches!(
            command,
            Command::Newline
                | Command::Backspace
                | Command::Delete
                | Command::Tab
                | Command::Indent(_)
                | Command::Undo
                | Command::Redo
                | Command::DeleteLine
                | Command::DeleteBy(..)
                | Command::HintPick
        );
        if editing && self.setup.readonly {
            return;
        }
        match command {
            Command::Run => self.run(),
            Command::Complete => {
                self.show_hints(false);
                self.ask_hints();
            }
            Command::Newline => {
                self.operation(|session| session.change(|state| state.newline_and_indent(now)));
                self.show_hints(true);
            }
            Command::ToggleFold => self.toggle_fold(self.state.cursor().line),
            Command::Backspace | Command::Delete => {
                let dir = if command == Command::Backspace { -1 } else { 1 };
                self.operation(|session| match session.remove_folded(now) {
                    Some(changes) => changes,
                    None => session.change(|state| state.delete(dir, Unit::Char, now)),
                });
            }
            Command::DeleteBy(dir, unit) => {
                self.operation(|session| session.change(|state| state.delete(dir, unit, now)));
            }
            Command::Discover => self.events.push(Event::Discover),
            Command::Tab => self.operation(|session| session.change(|state| state.tab(now))),
            Command::Indent(how) => {
                self.operation(|session| session.change(|state| state.indent_selection(how, now)));
            }
            Command::Undo => self.operation(|session| session.change(State::undo)),
            Command::Redo => self.operation(|session| session.change(State::redo)),
            Command::SelectAll => self.motion(|state| state.select_all()),
            Command::DeleteLine => {
                self.operation(|session| session.change(|state| state.delete_lines(now)));
            }
            Command::MoveH(dir, unit, extend) => {
                self.motion(|state| state.move_h(dir, unit, extend))
            }
            Command::MoveV(dir, extend) => self.motion(|state| state.move_v(dir, 1, extend)),
            Command::Page(dir, extend) => {
                let rows = self.page_rows();
                self.motion(|state| state.move_v(dir, rows, extend));
            }
            Command::Home(extend) => self.motion(|state| state.home(extend)),
            Command::End(extend) => self.motion(|state| state.end(extend)),
            Command::LineStart(extend) => {
                self.motion(|state| state.move_h(-1, Unit::Line, extend));
            }
            Command::DocStart(extend) => self.motion(|state| state.doc_start(extend)),
            Command::DocEnd(extend) => self.motion(|state| state.doc_end(extend)),
            Command::HintMove(delta, wrap) => {
                if let Some(popup) = &mut self.popup {
                    let count = popup.items.len() as isize;
                    let next = popup.selected as isize + delta;
                    popup.selected = if wrap {
                        next.rem_euclid(count)
                    } else {
                        next.clamp(0, count - 1)
                    } as usize;
                    self.revision += 1;
                }
            }
            Command::HintFirst | Command::HintLast => {
                if let Some(popup) = &mut self.popup {
                    popup.selected = if command == Command::HintFirst {
                        0
                    } else {
                        popup.items.len() - 1
                    };
                    self.revision += 1;
                }
            }
            Command::HintPick => {
                if let Some(selected) = self.popup.as_ref().map(|popup| popup.selected) {
                    self.pick(selected, now);
                }
            }
            Command::HintClose => self.close_hints(),
        }
    }

    fn page_rows(&self) -> usize {
        self.metrics
            .filter(|metrics| metrics.line_height > 0.0)
            .map_or(10, |metrics| {
                ((metrics.client_height / metrics.line_height) - 0.5).max(3.0) as usize
            })
    }

    fn run(&mut self) {
        let source = self.source().to_string();
        let line = if self.setup.document {
            let Some(viewport) = self.viewport() else {
                return;
            };
            if Some(viewport) != self.viewport {
                self.viewport = Some(viewport);
                self.events.push(Event::Viewport(viewport));
            }
            self.extension()
                .run_line(&source, viewport)
                .unwrap_or(viewport.top)
        } else {
            self.state.cursor().line
        };
        self.events.push(Event::Run { source, line });
    }

    fn motion(&mut self, step: impl FnOnce(&mut State) -> bool) {
        self.operation(|session| {
            step(&mut session.state);
            Vec::new()
        });
    }

    /// Makes everything until the matching `end_operation` one operation: many edits then
    /// cost one update.
    pub fn start_operation(&mut self) {
        if self.batch.0 == 0 {
            self.state.hold(true);
            self.batch.1 = self.op.is_none();
            if self.batch.1 {
                self.op = Some(Operation {
                    scroll: true,
                    ..Operation::default()
                });
            }
        }
        self.batch.0 += 1;
    }

    pub fn end_operation(&mut self) {
        if self.batch.0 == 0 {
            return;
        }
        self.batch.0 -= 1;
        if self.batch.0 > 0 {
            return;
        }
        self.state.hold(false);
        if std::mem::take(&mut self.batch.1) {
            self.finish();
        }
    }

    fn operation(&mut self, edit: impl FnOnce(&mut Self) -> Vec<TextChange>) {
        let outer = self.op.is_none();
        if outer {
            self.op = Some(Operation {
                scroll: true,
                ..Operation::default()
            });
        }
        let selection = self.state.selection();
        let folds = self.state.folds().len();
        let changes = edit(self);
        let moved = self.state.selection() != selection || folds != self.state.folds().len();
        if let Some(op) = &mut self.op {
            op.cursor |= moved;
            op.changes.extend(changes);
        }
        if moved {
            self.cursor_activity();
        }
        if outer {
            self.finish();
        }
    }

    fn finish(&mut self) {
        let Some(op) = self.op.take() else {
            return;
        };
        let mut tracked = true;
        if !op.changes.is_empty() {
            self.update_brackets();
            tracked = self.track(&op.changes);
            self.changes.extend(op.changes.iter().cloned());
        }
        if op.scroll && (op.cursor || !op.changes.is_empty()) {
            self.effects.push(Effect::ScrollToCursor);
        }
        if !op.changes.is_empty() || op.cursor {
            self.emit_viewport();
        }
        if op.cursor {
            self.events.push(Event::Cursor(self.state.cursor().line));
        }
        if tracked && op.hints {
            self.ask_hints();
        }
    }

    /// Reports an edit: the extension's record of it, or the text.
    fn track(&mut self, changes: &[TextChange]) -> bool {
        let text = self.source();
        let generation = self.state.generation();
        let generations = self.state.generations();
        let extension = self.extension().clone();
        match extension.edited(&text, changes, generation, &generations) {
            None => {
                self.events.push(Event::Changed(text.to_string()));
                true
            }
            Some(Ok((edit, refresh))) => {
                self.refresh(refresh);
                self.events.push(Event::Edited(edit));
                true
            }
            Some(Err(error)) => {
                self.events.push(Event::EditError(error));
                false
            }
        }
    }

    /// Edits the text and updates what depends on it.
    fn change(&mut self, edit: impl FnOnce(&mut State) -> Vec<TextChange>) -> Vec<TextChange> {
        let changes = edit(&mut self.state);
        if !changes.is_empty() {
            self.text_changed(&changes);
        }
        changes
    }

    fn text_changed(&mut self, changes: &[TextChange]) {
        *self.source.borrow_mut() = None;
        if let Some(line) = changes.iter().map(|change| change.from.line).min() {
            self.highlighter.edited(line);
        }
        for change in changes {
            self.marks.retain(|_, mark| match mark {
                Mark::Range { from, to, .. } => {
                    *from = map_start(*from, change);
                    *to = map_end(*to, change);
                    super::text::before(*from, *to)
                }
                Mark::Line { .. } => true,
            });
        }
        self.marks
            .retain(|_, mark| !matches!(mark, Mark::Line { anchor, .. } if self.state.anchor_line(*anchor).is_none()));
        self.revision += 1;
        self.edits += 1;
    }

    fn input(&mut self, text: &str, origin: &str, now: f64) {
        if self.setup.readonly || text.is_empty() {
            return;
        }
        let selection = self.state.selection();
        let mut from = selection.from();
        let mut to = selection.to();
        if origin == "paste"
            && selection.empty()
            && LAST_COPIED.with(|copied| {
                copied
                    .borrow()
                    .as_ref()
                    .is_some_and(|(copied, line_wise)| *line_wise && copied == text)
            })
        {
            from = Position {
                line: from.line,
                ch: 0,
            };
            to = from;
        }
        self.extension().inserting(self.state.lines(), from.line);
        self.operation(|session| {
            let mut changes = session
                .change(|state| state.apply(Transaction::replace(from, to, text, origin), now));
            let inserted = super::text::split_lines(text);
            session
                .state
                .set_selection(Selection::cursor(end_of(from, &inserted)));
            changes.extend(session.inserted(from, &inserted, origin, now));
            session.input_read(origin);
            changes
        });
    }

    /// What the extension adds to an insertion, in the same undo step.
    fn inserted(
        &mut self,
        from: Position,
        inserted: &[String],
        origin: &str,
        now: f64,
    ) -> Vec<TextChange> {
        self.extension()
            .inserted(self.state.lines(), from, inserted, origin)
            .map(|transaction| self.change(|state| state.apply(transaction, now)))
            .unwrap_or_default()
    }

    /// Hints after typing or pasting.
    fn input_read(&mut self, origin: &str) {
        let cursor = self.state.cursor();
        if origin != "paste"
            && cursor.ch > 0
            && self
                .state
                .line(cursor.line)
                .encode_utf16()
                .nth(cursor.ch - 1)
                == Some(u16::from(b'/'))
            && let Some(op) = &mut self.op
        {
            op.hints = true;
        }
        if self.state.line(cursor.line).len() < HINT_LINE_LIMIT {
            self.show_hints(true);
        }
    }

    fn paste(&mut self, text: &str, now: f64) {
        if self.setup.readonly {
            return;
        }
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        self.input(&text, "paste", now);
    }

    fn cut(&mut self, now: f64) {
        if self.setup.readonly {
            return;
        }
        let selection = self.state.selection();
        let (from, to) = if selection.empty() {
            self.line_range(selection.head.line)
        } else {
            (selection.from(), selection.to())
        };
        self.operation(|session| {
            session.change(|state| {
                state.apply(
                    Transaction {
                        changes: vec![Change {
                            from,
                            to,
                            text: String::new(),
                        }],
                        origin: "cut".into(),
                        selection: Some(Selection::cursor(from)),
                        join: false,
                    },
                    now,
                )
            })
        });
    }

    fn press(&mut self, at: Position, extend: bool, clicks: u8) {
        self.hints.enabled = false;
        self.close_hints();
        let at = self.state.clip(at);
        let current = self.state.selection();
        let selection = match clicks {
            2 => self.state.word_at(at),
            3 => {
                let (from, to) = self.line_range(at.line);
                Selection {
                    anchor: from,
                    head: to,
                }
            }
            _ if extend => Selection {
                anchor: current.anchor,
                head: at,
            },
            _ => Selection::cursor(at),
        };
        self.press = Some(Press {
            origin: if extend && clicks <= 1 {
                Selection::cursor(current.anchor)
            } else {
                selection
            },
            clicks,
        });
        self.select(selection, false);
    }

    fn drag(&mut self, at: Position) {
        let Some(press) = self.press else {
            return;
        };
        let at = self.state.clip(at);
        let unit = match press.clicks {
            2 => self.state.word_at(at),
            3 => {
                let (from, to) = self.line_range(at.line);
                Selection {
                    anchor: from,
                    head: to,
                }
            }
            _ => Selection::cursor(at),
        };
        let origin = press.origin;
        let selection = if super::text::before(unit.from(), origin.from()) {
            Selection {
                anchor: origin.to(),
                head: unit.from(),
            }
        } else {
            Selection {
                anchor: origin.from(),
                head: super::text::max(unit.to(), origin.to()),
            }
        };
        self.select(selection, false);
    }

    fn focus(&mut self, focused: bool) {
        if focused == self.focused {
            return;
        }
        self.focused = focused;
        self.revision += 1;
        self.update_brackets();
        if !focused {
            self.close_hints();
        }
    }

    /// Folds `from..to`, like `markText` with `collapsed`.
    pub fn fold(&mut self, from: Position, to: Position) -> bool {
        let folded = self.state.fold(from, to);
        if folded {
            self.revision += 1;
            self.emit_viewport();
        }
        folded
    }

    pub fn unfold(&mut self, line: usize) -> bool {
        let unfolded = self.state.unfold(line);
        if unfolded {
            self.revision += 1;
            self.emit_viewport();
        }
        unfolded
    }

    /// Folds the range starting on `line`, or opens it when folded.
    pub fn toggle_fold(&mut self, line: usize) {
        let Some(range) = super::fold::range(&mut self.highlighter, self.state.lines(), line)
        else {
            return;
        };
        if self
            .state
            .fold_starting(range.from.line)
            .is_some_and(|fold| fold.from == range.from)
        {
            self.state.unfold(range.from.line);
        } else {
            self.state.fold(range.from, range.to);
        }
        self.revision += 1;
        self.emit_viewport();
    }

    /// Backspace or Delete on a folded line the extension removes as a whole.
    fn remove_folded(&mut self, now: f64) -> Option<Vec<TextChange>> {
        let line = self.state.cursor().line;
        let end = Position {
            line,
            ch: utf16_len(self.state.line(line)),
        };
        self.state.fold_at(end)?;
        let response_end = self.extension().removal(&self.source(), line)?;
        let from = if line > 0 {
            Position {
                line: line - 1,
                ch: utf16_len(self.state.line(line - 1)),
            }
        } else {
            Position::default()
        };
        let to = if response_end < self.state.line_count() {
            Position {
                line: response_end,
                ch: 0,
            }
        } else {
            let last = self.state.last_line();
            Position {
                line: last,
                ch: utf16_len(self.state.line(last)),
            }
        };
        Some(self.change(|state| state.apply(Transaction::replace(from, to, "\n", "+delete"), now)))
    }

    /// After the cursor moves: brackets, and the open hints follow the cursor.
    fn cursor_activity(&mut self) {
        self.update_brackets();
        if self.syncing {
            return;
        }
        if self.popup.is_some() && !self.op.as_ref().is_some_and(|op| op.hinted) {
            if self.hints.enabled && self.focused {
                self.show_hints(self.hints.automatic);
            } else {
                self.close_hints();
            }
        }
    }

    fn update_brackets(&mut self) {
        let brackets = if self.focused {
            super::brackets::matching(
                &mut self.highlighter,
                self.state.lines(),
                self.state.selection(),
            )
        } else {
            Vec::new()
        };
        if brackets != self.brackets {
            self.brackets = brackets;
            self.revision += 1;
        }
    }

    /// The owner hears that hints were asked for, so it may fetch what they need.
    fn ask_hints(&mut self) {
        let cursor = self.state.cursor();
        if let Some(payload) = self
            .extension()
            .hints(&self.source(), cursor.line, cursor.ch)
        {
            self.events.push(Event::Hints(payload));
        }
    }

    /// Runs `signal` after the current task unless the text changes first; a newer
    /// signal of the same kind replaces it.
    fn signal(&mut self, signal: Signal) {
        self.next_token += 1;
        let generation = (signal != Signal::Reshow).then(|| self.state.generation());
        self.signals[signal.index()] = Some((self.next_token, generation));
        self.effects.push(Effect::Later(signal, self.next_token));
    }

    fn fire(&mut self, signal: Signal, token: u64) {
        let slot = &mut self.signals[signal.index()];
        match *slot {
            Some((pending, generation)) if pending == token => {
                *slot = None;
                if generation.is_some_and(|generation| generation != self.state.generation()) {
                    return;
                }
            }
            _ => return,
        }
        if signal == Signal::Reshow {
            if self.hints.enabled {
                self.show_hints(true);
            }
            return;
        }
        if self.hints.enabled && self.focused {
            self.show_hints(true);
        }
    }

    fn close_hints(&mut self) {
        if self.popup.take().is_some() {
            self.revision += 1;
        }
    }

    /// Opens or refreshes the hints at the cursor; `automatic` shows only what typing offers.
    pub fn show_hints(&mut self, automatic: bool) {
        if self.hints.applying {
            return;
        }
        self.hints.enabled = true;
        self.hints.automatic = automatic;
        if let Some(op) = &mut self.op {
            op.hinted = true;
        }
        let cursor = self.state.cursor();
        if self.extension().output(cursor.line) {
            self.close_hints();
            return;
        }
        let completion =
            self.extension()
                .suggest(&self.source(), cursor.line, cursor.ch, &self.context);
        if completion.candidates.is_empty() || (automatic && !completion.automatic) {
            self.close_hints();
            return;
        }
        let comma = completion.comma.map(|(line, ch)| Position { line, ch });
        let items: Vec<Item> = completion
            .candidates
            .iter()
            .map(|candidate| {
                let label = candidate
                    .label
                    .clone()
                    .unwrap_or_else(|| candidate.text.clone());
                Item {
                    text: candidate.text.clone(),
                    display: if candidate.detail.is_empty() {
                        label
                    } else {
                        format!("{label} — {}", candidate.detail)
                    },
                    badge: candidate.badge,
                    continue_hint: candidate.continue_hint,
                    literal: candidate.literal,
                    comma,
                }
            })
            .collect();
        let from = Position {
            line: cursor.line,
            ch: completion.from,
        };
        let to = Position {
            line: cursor.line,
            ch: completion.to,
        };
        let selected = self
            .popup
            .as_ref()
            .filter(|popup| popup.from == from && popup.to.ch == to.ch)
            .and_then(|popup| popup.items.get(popup.selected))
            .and_then(|item| items.iter().position(|next| next.text == item.text))
            .unwrap_or(0);
        let popup = Popup {
            from,
            to,
            items,
            selected,
        };
        if self.popup.as_ref() != Some(&popup) {
            self.popup = Some(popup);
            self.revision += 1;
        }
    }

    fn pick(&mut self, index: usize, now: f64) {
        let Some(popup) = self.popup.take() else {
            return;
        };
        self.revision += 1;
        let Some(item) = popup.items.get(index).cloned() else {
            return;
        };
        if self.setup.readonly {
            return;
        }
        self.hints.applying = true;
        self.apply_hint(&popup, &item, now);
        self.hints.applying = false;
    }

    fn apply_hint(&mut self, popup: &Popup, item: &Item, now: f64) {
        self.hints.enabled = false;
        let from = popup.from;
        let (text, target) = if item.literal {
            (item.text.clone(), None)
        } else {
            let text = self
                .extension()
                .insertion(&item.text, self.state.line(from.line))
                .unwrap_or_else(|| item.text.clone());
            extension::prepare_insertion(&text)
        };
        let to = popup.to;
        self.extension().inserting(self.state.lines(), from.line);
        self.operation(|session| {
            let mut changes = Vec::new();
            if let Some(comma) = item.comma {
                changes.extend(session.change(|state| {
                    state.apply(Transaction::replace(comma, comma, ",", "complete"), now)
                }));
            }
            let mut transaction = Transaction::replace(from, to, &text, "complete");
            transaction.join = item.comma.is_some();
            changes.extend(session.change(|state| state.apply(transaction, now)));
            let inserted = super::text::split_lines(&text);
            session
                .state
                .set_selection(Selection::cursor(end_of(from, &inserted)));
            changes.extend(session.inserted(from, &inserted, "complete", now));
            if let Some(target) = target {
                let lines = super::text::split_lines(&text[..target]);
                session
                    .state
                    .set_selection(Selection::cursor(end_of(from, &lines)));
            }
            changes
        });
        let continuation = item.continue_hint
            || text.ends_with([':', '/', ' '])
            || (text.contains('\n') && target.is_some());
        if continuation {
            self.hints.enabled = true;
            self.hints.automatic = true;
            self.signal(Signal::Reshow);
        }
    }
}

/// The key's name without modifiers, as key bindings spell it.
fn key_name(key: Key) -> String {
    if let Some(letter) = key.code.strip_prefix("Key") {
        return letter.to_owned();
    }
    match key.code {
        "Space" => return "Space".into(),
        "BracketLeft" => return "[".into(),
        "BracketRight" => return "]".into(),
        _ => {}
    }
    match key.key {
        "ArrowLeft" => "Left".into(),
        "ArrowRight" => "Right".into(),
        "ArrowUp" => "Up".into(),
        "ArrowDown" => "Down".into(),
        "Escape" => "Esc".into(),
        " " => "Space".into(),
        other if other.chars().count() == 1 => other.to_uppercase(),
        other => other.into(),
    }
}

/// Where text inserted at `from` ends.
fn end_of(from: Position, inserted: &[String]) -> Position {
    match inserted {
        [] => from,
        [only] => Position {
            line: from.line,
            ch: from.ch + utf16_len(only),
        },
        [.., last] => Position {
            line: from.line + inserted.len() - 1,
            ch: utf16_len(last),
        },
    }
}

/// Where `at` is after `change`; positions inside the replaced text move after it.
fn map_start(at: Position, change: &TextChange) -> Position {
    if super::text::before(at, change.from) {
        at
    } else if super::text::before(at, change.to) {
        end_of(change.from, &change.text)
    } else {
        shift(at, change)
    }
}

/// Where `at` is after `change`; positions inside the replaced text move before it.
fn map_end(at: Position, change: &TextChange) -> Position {
    if !super::text::before(change.from, at) {
        at
    } else if super::text::before(at, change.to) || at == change.to && change.from == change.to {
        change.from
    } else {
        shift(at, change)
    }
}

fn shift(at: Position, change: &TextChange) -> Position {
    let end = end_of(change.from, &change.text);
    if at.line == change.to.line {
        Position {
            line: end.line,
            ch: end.ch + at.ch - change.to.ch,
        }
    } else {
        Position {
            line: at.line + end.line - change.to.line,
            ch: at.ch,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(line: usize, ch: usize) -> Position {
        Position { line, ch }
    }

    fn session(text: &str) -> Session {
        Session::new(
            Setup {
                mode: "json".into(),
                ..Setup::default()
            },
            extension::plain(),
            text,
        )
    }

    #[test]
    fn an_operation_reports_its_edits_once() {
        let mut session = session(&"a\n".repeat(3));
        session.start_operation();
        for line in (0..3).rev() {
            session.apply(
                Transaction::replace(at(line, 0), at(line, 1), "b", "*replace"),
                0.0,
            );
        }
        assert!(session.take_events().is_empty());
        session.end_operation();
        let events = session.take_events();
        let changed: Vec<_> = events
            .iter()
            .filter(|event| matches!(event, Event::Changed(_)))
            .collect();
        assert!(
            matches!(&changed[..], [Event::Changed(text)] if text == "b\nb\nb\n"),
            "{events:?}"
        );
        assert_eq!(session.take_changes().len(), 3);
    }

    #[test]
    fn an_operation_is_one_undo_step() {
        let mut session = session("a\n\n");
        session.start_operation();
        session.apply(Transaction::replace(at(1, 0), at(2, 0), "", "+input"), 0.0);
        session.apply(
            Transaction::replace(at(1, 0), at(1, 0), "b\nc", "paste"),
            0.0,
        );
        session.end_operation();
        session.apply(Transaction::replace(at(0, 0), at(0, 0), "d", "paste"), 0.0);
        session.command(Command::Undo, 0.0);
        assert_eq!(session.source().as_ref(), "a\nb\nc");
        session.command(Command::Undo, 0.0);
        assert_eq!(session.source().as_ref(), "a\n\n");
    }

    fn key(name: &str) -> Key<'_> {
        Key {
            key: name,
            ..Key::default()
        }
    }

    #[test]
    fn typing_reports_the_text_then_the_cursor() {
        let mut editor = session("ab");
        editor.select(Selection::cursor(at(0, 1)), false);
        editor.take_events();
        let events = editor.update(Msg::Input("x".into()), 0.0);
        assert!(matches!(&events[..], [Event::Changed(text), Event::Cursor(0)] if text == "axb"));
        assert_eq!(editor.state().cursor(), at(0, 2));
        assert!(editor.take_effects().contains(&Effect::ScrollToCursor));
    }

    #[test]
    fn readonly_ignores_edits_but_moves() {
        let mut editor = session("ab");
        editor.set_readonly(true);
        assert!(editor.update(Msg::Input("x".into()), 0.0).is_empty());
        editor.update(Msg::Command(Command::MoveH(1, Unit::Char, false)), 0.0);
        assert_eq!(editor.state().cursor(), at(0, 1));
        assert_eq!(&*editor.source(), "ab");
    }

    #[test]
    fn keys_map_by_name() {
        let editor = session("");
        let shift_enter = Key {
            shift: true,
            ..key("Enter")
        };
        assert_eq!(editor.resolve(shift_enter), Some(Command::Run));
        assert_eq!(editor.resolve(key("Enter")), Some(Command::Newline));
        let undo = Key {
            code: "KeyZ",
            meta: true,
            mac: true,
            ..key("z")
        };
        assert_eq!(editor.resolve(undo), Some(Command::Undo));
        let pc_undo = Key {
            code: "KeyZ",
            ctrl: true,
            ..key("z")
        };
        assert_eq!(editor.resolve(pc_undo), Some(Command::Undo));
        let mac_ctrl_z = Key {
            mac: true,
            ..pc_undo
        };
        assert_eq!(editor.resolve(mac_ctrl_z), None);
        let copy = Key {
            code: "KeyC",
            meta: true,
            mac: true,
            ..key("c")
        };
        assert_eq!(editor.resolve(copy), None);
        assert_eq!(editor.resolve(key("Escape")), None);
        assert_eq!(
            editor.resolve(Key {
                shift: true,
                ..key("ArrowLeft")
            }),
            Some(Command::MoveH(-1, Unit::Char, true))
        );
    }

    #[test]
    fn copying_nothing_copies_the_line_and_pastes_above() {
        let mut editor = session("one\ntwo");
        editor.select(Selection::cursor(at(0, 1)), false);
        assert_eq!(editor.copy(), "one\n");
        editor.select(Selection::cursor(at(1, 2)), false);
        editor.update(Msg::Paste("one\n".into()), 0.0);
        assert_eq!(&*editor.source(), "one\none\ntwo");
        editor.update(Msg::Cut, 0.0);
        assert_eq!(&*editor.source(), "one\none\n");
    }

    #[test]
    fn signals_drop_when_the_text_changed() {
        let mut editor = session("x");
        editor.signal(Signal::Reshow);
        editor.signal(Signal::RefreshHints);
        let effects = editor.take_effects();
        let Some(Effect::Later(_, token)) = effects.last().copied() else {
            panic!("no signal");
        };
        editor.update(Msg::Input("y".into()), 0.0);
        assert!(editor.signals[Signal::RefreshHints.index()].is_some());
        editor.update(Msg::Signal(Signal::RefreshHints, token), 0.0);
        assert!(editor.signals[Signal::RefreshHints.index()].is_none());
    }

    #[test]
    fn marks_follow_edits_and_vanish_when_deleted() {
        let mut editor = session("hello world");
        let id = editor.mark(Mark::Range {
            from: at(0, 6),
            to: at(0, 11),
            class: "find-match".into(),
        });
        editor.apply(Transaction::replace(at(0, 0), at(0, 0), ">> ", ""), 0.0);
        assert_eq!(editor.find_mark(id), Some((at(0, 9), at(0, 14))));
        editor.apply(Transaction::replace(at(0, 8), at(0, 14), "", ""), 0.0);
        assert_eq!(editor.find_mark(id), None);
    }

    #[test]
    fn sync_keeps_the_cursor_line_and_skips_input_handling() {
        let mut editor = session("a\nb");
        editor.select(Selection::cursor(at(1, 1)), false);
        editor.take_events();
        editor.sync("x\na\nb", None, 0.0);
        assert_eq!(editor.state().cursor(), at(2, 1));
        assert!(matches!(&editor.take_events()[..], [Event::Cursor(2)]));
        assert!(
            editor
                .take_changes()
                .iter()
                .any(|change| change.origin.as_deref() == Some("+services-response"))
        );
    }

    #[test]
    fn viewport_reports_the_top_line_and_a_visible_cursor() {
        let mut editor = session("a\nb\nc\nd\ne");
        editor.select(Selection::cursor(at(3, 0)), false);
        let metrics = Metrics {
            scroll_top: layout::PADDING_TOP + 20.0,
            client_height: 40.0,
            client_width: 100.0,
            line_height: 10.0,
            char_width: 7.0,
            gutter_width: 20.0,
            ..Metrics::default()
        };
        editor.update(Msg::Metrics(metrics), 0.0);
        assert_eq!(
            editor.viewport(),
            Some(Viewport {
                top: 2,
                visible_cursor: Some(3)
            })
        );
        editor.select(Selection::cursor(at(0, 0)), false);
        assert_eq!(editor.viewport().unwrap().visible_cursor, None);
    }

    #[test]
    fn agent_edits_check_the_text_and_flash_words() {
        let mut editor = session("alpha beta");
        let mut command = EditCommand {
            serial: 1,
            expected: "other".into(),
            from: at(0, 6),
            to: at(0, 10),
            text: "gamma".into(),
        };
        assert!(matches!(
            editor.agent_edit(&command, true, 0.0),
            Some(Err(_))
        ));
        assert!(editor.agent_edit(&command, true, 0.0).is_none());
        command.serial = 2;
        command.expected = "alpha beta".into();
        assert_eq!(editor.agent_edit(&command, true, 0.0), Some(Ok(())));
        assert_eq!(&*editor.source(), "alpha gamma");
        let (_, mark) = editor.marks().next().unwrap();
        assert_eq!(
            mark,
            &Mark::Range {
                from: at(0, 6),
                to: at(0, 11),
                class: "editor-agent-edit".into()
            }
        );
        editor.update(Msg::Command(Command::Undo), 0.0);
        assert_eq!(&*editor.source(), "alpha beta");
    }
}
