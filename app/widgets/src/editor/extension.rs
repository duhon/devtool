//! What an owner adds to an editor beyond its mode: tokens, folds, completions and hints,
//! and the bookkeeping of a document whose text it keeps (output ranges, line classes,
//! edits it records). The editor knows none of the owner's terms; an [`Extension`] answers
//! in lines and columns, and the payloads it hands back travel to the owner untouched.
use super::model::Context;
use super::state::{Fold, Transaction};
use super::text::{Position, TextChange};
use std::any::Any;
use std::collections::BTreeSet;
use std::rc::Rc;

/// Something an extension hands its owner through an editor event.
#[derive(Clone)]
pub struct Payload(pub Rc<dyn Any>);

impl Payload {
    pub fn new(value: impl Any) -> Self {
        Self(Rc::new(value))
    }

    pub fn get<T: Any>(&self) -> Option<&T> {
        self.0.downcast_ref()
    }
}

impl std::fmt::Debug for Payload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Payload")
    }
}

/// What the editor redraws after the extension's state changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Refresh {
    /// Tokens and folds changed: highlight again from scratch.
    pub relight: bool,
    /// Line classes changed.
    pub redraw: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub text: String,
    pub label: Option<String>,
    pub detail: String,
    /// A letter in front of the item that tells where it comes from.
    pub badge: Option<char>,
    /// Completing it opens the hints again.
    pub continue_hint: bool,
    /// It inserts exactly its text.
    pub literal: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Completion {
    /// The UTF-16 columns of the cursor line the candidate replaces.
    pub from: usize,
    pub to: usize,
    pub candidates: Vec<Candidate>,
    /// Where a comma goes before the inserted text, as `(line, ch)`.
    pub comma: Option<(usize, usize)>,
    /// Typing may open it, not only asking.
    pub automatic: bool,
}

/// The tokens of one line, as `(byte end, kind)`, and the state after it.
pub type Tokens = (Vec<(usize, Option<String>)>, Vec<u8>);

/// What an owner adds to one editor. Every method has a plain default; the editor calls
/// them with the text as it is, never with the owner's own model.
pub trait Extension {
    fn as_any(&self) -> &dyn Any;

    /// The owner's text replaced the editor's (a new editor, `sync`, an option set):
    /// `generation` names the text in the undo history.
    fn synchronize(&self, _text: &str, _generation: u64) -> Refresh {
        Refresh::default()
    }

    /// The user edited the text: `None` reports it as plain text, `Some` as the owner's
    /// record of the edit, or an error when it could not be recorded. `generations` are
    /// the ones the undo history still holds.
    fn edited(
        &self,
        _text: &str,
        _changes: &[TextChange],
        _generation: u64,
        _generations: &BTreeSet<u64>,
    ) -> Option<Result<(Payload, Refresh), String>> {
        None
    }

    /// The text changed or is highlighted again: drop what was derived from it.
    fn invalidate(&self) {}

    /// `from..to` was copied.
    fn copy(&self, _from: Position, _to: Position) {}

    /// An insertion starting on `line` of `lines` is about to happen.
    fn inserting(&self, _lines: &[String], _line: usize) {}

    /// More changes an insertion of `inserted` at `from` brings, joining its undo step;
    /// `lines` is the text after it.
    fn inserted(
        &self,
        _lines: &[String],
        _from: Position,
        _inserted: &[String],
        _origin: &str,
    ) -> Option<Transaction> {
        None
    }

    /// The line a folded `line` removes up to (exclusive) when deleted as a whole.
    fn removal(&self, _source: &str, _line: usize) -> Option<usize> {
        None
    }

    /// Whether `line` is output: no hints there, and find never replaces in it.
    fn output(&self, _line: usize) -> bool {
        false
    }

    /// The output ranges as `(first line, end line)`.
    fn outputs(&self) -> Vec<(usize, usize)> {
        Vec::new()
    }

    /// A class for `line` itself.
    fn line_class(&self, _line: usize) -> Option<&'static str> {
        None
    }

    /// The line a run starts from, given the viewport; `None` runs from its top line.
    fn run_line(&self, _source: &str, _viewport: Viewport) -> Option<usize> {
        None
    }

    /// Whether `line` resets the tokenizer state to [`Extension::start`], so highlighting
    /// can restart there exactly.
    fn restarts(&self, _line: &str) -> bool {
        false
    }

    /// The tokenizer state the first line starts in.
    fn start(&self) -> Vec<u8> {
        Vec::new()
    }

    /// The tokens of `lines[line]` from `state`; `None` leaves it to the editor's mode.
    fn highlight(&self, _state: &[u8], _lines: &[String], _line: usize) -> Option<Tokens> {
        None
    }

    /// The fold starting at `line`: `None` leaves it to brackets.
    fn fold(&self, _lines: &[String], _line: usize) -> Option<Option<Fold>> {
        None
    }

    /// Completion at `line`/`ch` of `source`.
    fn suggest(&self, _source: &str, _line: usize, _ch: usize, _context: &Context) -> Completion {
        Completion::default()
    }

    /// The text a non-literal candidate inserts on `line`; `None` inserts the candidate.
    fn insertion(&self, _candidate: &str, _line: &str) -> Option<String> {
        None
    }

    /// The user asked for hints at `line`/`ch`: what the owner hears.
    fn hints(&self, _source: &str, _line: usize, _ch: usize) -> Option<Payload> {
        None
    }

    /// An option of the owner's, read through the editor's facade.
    fn option(&self, _name: &str) -> Option<serde_json::Value> {
        None
    }

    /// Sets an option of the owner's; true when it took it, so the editor synchronizes.
    fn set_option(&self, _name: &str, _value: serde_json::Value) -> bool {
        false
    }
}

/// No additions: the editor's mode alone.
pub struct Plain;

impl Extension for Plain {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub fn plain() -> Rc<dyn Extension> {
    Rc::new(Plain)
}

/// The viewport of a document editor: its top line and the cursor line when visible.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Viewport {
    pub top: usize,
    pub visible_cursor: Option<usize>,
}

/// The editors of a module's UI tree: what an owner gives each `language`.
pub trait Languages {
    /// The mode and extension of an editor of `language`.
    fn editor(&self, language: &str) -> (String, Rc<dyn Extension>);

    /// The hints an editor of `language` completes; `None` when it has none.
    fn hints(&self, _language: &str) -> Option<Vec<(String, String)>> {
        None
    }
}

struct Modes;

impl Languages for Modes {
    fn editor(&self, language: &str) -> (String, Rc<dyn Extension>) {
        (language.to_owned(), plain())
    }
}

thread_local! {
    static LANGUAGES: std::cell::Cell<&'static dyn Languages> = const { std::cell::Cell::new(&Modes) };
}

/// Installs what the editors of UI trees get; until then each `language` is a mode.
pub fn install(languages: &'static dyn Languages) {
    LANGUAGES.with(|cell| cell.set(languages));
}

pub fn languages() -> &'static dyn Languages {
    LANGUAGES.with(std::cell::Cell::get)
}

/// The text a candidate inserts and where the cursor lands in it: at `$0`, inside an empty
/// line, quotes or brackets.
pub fn prepare_insertion(text: &str) -> (String, Option<usize>) {
    if let Some(cursor) = text.find("$0") {
        return (text.replacen("$0", "", 1), Some(cursor));
    }
    let cursor = text
        .split_once('\n')
        .and_then(|(first, rest)| {
            let (inner, _) = rest.split_once('\n')?;
            inner
                .chars()
                .all(|c| c == ' ' || c == '\t')
                .then_some(first.len() + 1 + inner.len())
        })
        .or_else(|| (text.ends_with("\"\"") || text.ends_with("[]")).then(|| text.len() - 1));
    (text.to_owned(), cursor)
}
