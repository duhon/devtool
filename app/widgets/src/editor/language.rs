//! What a service adds to the editor: highlighting, folds, completion and hints. The Shell
//! installs its [`Language`] (the Kernel's modules) once; until then the editor is plain text.
use super::model::Context;
use services_sdk::{Check, ColumnType, Fold, Line, LineColumns};
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub text: String,
    pub label: Option<String>,
    pub detail: String,
    pub continue_hint: bool,
    pub literal: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Completion {
    pub from: usize,
    pub to: usize,
    pub candidates: Vec<Candidate>,
    pub comma: Option<(usize, usize)>,
    pub automatic: bool,
}

/// The services the editor edits checks of. Every method answers for one service.
pub trait Language {
    /// Whether `service` is a module service, so its checks highlight and hint through it.
    fn is_service(&self, _service: &str) -> bool {
        false
    }

    /// The `#marker` sections of a service's checks.
    fn markers(&self, _service: &str) -> Vec<String> {
        Vec::new()
    }

    /// The service's highlighting of `lines`, starting in `state`; `None` when it has none.
    fn highlight(
        &self,
        _service: &str,
        _state: Vec<u8>,
        _lines: Vec<String>,
        _columns: Vec<LineColumns>,
    ) -> Option<Vec<Line>> {
        None
    }

    /// The folds of one check body, its lines counted from the line after the check header.
    fn fold(&self, _service: &str, _text: &str, _columns: Vec<ColumnType>) -> Vec<Fold> {
        Vec::new()
    }

    /// Completion at `line`/`ch` of `source` (response regions masked out).
    fn suggest(
        &self,
        _service: &str,
        _field: &str,
        _source: &str,
        _line: usize,
        _ch: usize,
        _context: &Context,
    ) -> Completion {
        Completion::default()
    }

    /// The check and offset hints were asked for, so the service may fetch what they need.
    fn hints(
        &self,
        _service: &str,
        _source: &str,
        _line: usize,
        _ch: usize,
    ) -> Option<(Check, u32)> {
        None
    }

    /// Variable names and values the editor hovers and hints.
    fn variables(&self) -> Vec<(String, String)> {
        Vec::new()
    }
}

struct Plain;
impl Language for Plain {}

static LANGUAGE: OnceLock<&'static (dyn Language + Sync)> = OnceLock::new();

/// Installs the editor's language; the first one stays.
pub fn install(language: &'static (dyn Language + Sync)) {
    let _ = LANGUAGE.set(language);
}

pub fn language() -> &'static dyn Language {
    match LANGUAGE.get() {
        Some(language) => *language,
        None => &Plain,
    }
}

/// The text a candidate inserts and where the cursor lands in it: at `$0`, inside an empty
/// line, quotes or brackets; a console marker gets its line break.
pub fn prepare_insertion(candidate: &str, console: bool) -> (String, Option<usize>) {
    if let Some(cursor) = candidate.find("$0") {
        return (candidate.replacen("$0", "", 1), Some(cursor));
    }
    let text = if console && candidate.starts_with('#') {
        format!(
            "{candidate}{}",
            if candidate == "#check" { " " } else { "\n" }
        )
    } else {
        candidate.to_owned()
    };
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
    (text, cursor)
}
