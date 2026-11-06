//! Syntax highlighting: a line-state tokenizer with a per-line cache.
//! A console line's language follows its section (`#headers`, a template `@section`) or the
//! response region it is in; JSON and SQL have their own tokenizers, the rest use `syntax`.
use super::text::{Utf16Cursor, indentation, utf16_len};
use super::{json, sql};
use crate::document::ResponseRegion;
use crate::syntax::{self, Language};
use services_contracts::manifest::View;
use std::cell::RefCell;
use std::rc::Rc;

pub type Style = Option<&'static str>;

/// Lines past this many UTF-16 units stay unstyled.
const MAX_HIGHLIGHT: usize = 10_000;
/// How far back a line looks for a cached state before restarting from a fresh one.
const RESTART: usize = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    /// The UTF-16 column where the span ends.
    pub end: usize,
    pub style: Style,
}

/// What a mode highlights. A console mode reads its service's manifest once.
#[derive(Clone, Debug, Default)]
pub struct Config {
    /// The console service; empty for a single-language mode.
    service: String,
    /// The language of a single-language mode.
    base: String,
    /// `(section, language)` of the console sections.
    sections: Vec<(String, String)>,
    payload: String,
    response: View,
    /// The service's module highlights it (through the Kernel).
    kernel: bool,
}

impl Config {
    pub fn new(mode: &str) -> Self {
        let Some(id) = mode.strip_prefix("console-") else {
            return Self {
                base: mode.to_owned(),
                ..Self::default()
            };
        };
        if crate::kernel::is_service(id) {
            return Self {
                service: id.to_owned(),
                kernel: true,
                ..Self::default()
            };
        }
        let service = crate::services::get(id);
        Self {
            service: id.to_owned(),
            base: String::new(),
            sections: service
                .iter()
                .flat_map(|service| &service.console.sections)
                .map(|section| (section.name.clone(), section.language.clone()))
                .collect(),
            payload: service
                .as_ref()
                .map(|service| service.language("").to_owned())
                .unwrap_or_default(),
            response: service
                .map(|service| service.console.response)
                .unwrap_or_default(),
            kernel: false,
        }
    }

    #[cfg(test)]
    pub fn console_for_tests(sections: &[(&str, &str)], payload: &str, response: View) -> Self {
        Self {
            service: "test".into(),
            base: String::new(),
            sections: sections
                .iter()
                .map(|(name, language)| ((*name).to_owned(), (*language).to_owned()))
                .collect(),
            payload: payload.into(),
            response,
            kernel: false,
        }
    }

    pub fn console(&self) -> bool {
        !self.service.is_empty()
    }

    /// The console service whose module highlights it.
    pub fn kernel(&self) -> Option<&str> {
        self.kernel.then_some(self.service.as_str())
    }

    /// Responses are tables.
    pub fn grid(&self) -> bool {
        self.console() && self.response == View::Grid
    }

    pub fn section(&self, name: &str) -> bool {
        self.sections.iter().any(|(section, _)| section == name)
    }

    /// The language of `area` (a section; empty or unknown means the payload) or of a response.
    pub fn language(&self, area: &str, response: bool) -> &str {
        if !self.console() {
            &self.base
        } else if response {
            self.response.language()
        } else {
            self.sections
                .iter()
                .find(|(name, _)| name == area)
                .map_or(&self.payload, |(_, language)| language)
        }
    }
}

/// The tokenizer state between lines.
#[derive(Clone, Debug, Default)]
pub struct LineState {
    area: String,
    response: bool,
    json: Option<json::State>,
    sql: Option<sql::State>,
    lex: syntax::State,
    /// The region whose table metadata the table tokenizer uses.
    table: Option<usize>,
    /// A Kernel module's state; empty at the start of a check.
    module: Rc<[u8]>,
}

impl LineState {
    pub fn response(&self) -> bool {
        self.response
    }
    pub fn lex(&self) -> &syntax::State {
        &self.lex
    }
    fn reset_lex(&mut self) {
        self.lex = syntax::State::default();
        self.table = None;
    }
}

struct Region {
    header: usize,
    end: usize,
    table: Option<RefCell<crate::tables::Metadata>>,
}

enum Step {
    /// A token's byte end and style; `sql` when the SQL tokenizer produced it.
    Token(usize, Style, bool),
    /// A table line's spans (UTF-16 ends).
    Table(Vec<(usize, Style)>),
}

pub struct Highlighter {
    config: Config,
    template_section: Option<String>,
    source: Vec<ResponseRegion>,
    regions: Vec<Region>,
    /// The state before each line, when known.
    states: Vec<Option<Rc<LineState>>>,
    spans: Vec<Option<Rc<[Span]>>>,
    /// States up to here were computed from the first line, not from a restart.
    frontier: usize,
    /// Bumps when cached spans are repainted without an edit.
    revision: u64,
    pub(super) json_folds: super::fold::JsonFolds,
    /// A Kernel module's folds of the check at a line: `(start, end)` document lines.
    pub(super) module_folds: Option<(usize, ModuleFolds)>,
}

/// A module's folds of one check: `(start, end)` document lines.
pub(super) type ModuleFolds = Rc<[(usize, usize)]>;

impl Highlighter {
    pub fn new(mode: &str) -> Self {
        Self::with_config(Config::new(mode))
    }

    pub fn with_config(config: Config) -> Self {
        Self {
            config,
            template_section: None,
            source: Vec::new(),
            regions: Vec::new(),
            states: Vec::new(),
            spans: Vec::new(),
            frontier: 0,
            revision: 0,
            json_folds: Default::default(),
            module_folds: None,
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn regions(&self) -> &[ResponseRegion] {
        &self.source
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn set_regions(&mut self, regions: &[ResponseRegion]) {
        if self.source == regions {
            return;
        }
        self.source = regions.to_vec();
        let mut sorted: Vec<_> = regions.iter().collect();
        sorted.sort_by_key(|region| region.header);
        self.regions = sorted
            .into_iter()
            .map(|region| Region {
                header: region.header,
                end: region.end,
                table: region.table.clone().map(RefCell::new),
            })
            .collect();
        self.reset();
    }

    pub fn set_template_section(&mut self, section: Option<&str>) {
        let section = section
            .filter(|section| !section.is_empty())
            .map(str::to_owned);
        if self.template_section != section {
            self.template_section = section;
            self.reset();
        }
    }

    pub fn reset(&mut self) {
        self.states.clear();
        self.spans.clear();
        self.frontier = 0;
        self.revision += 1;
        self.json_folds.clear();
        self.module_folds = None;
    }

    /// Forgets everything from `line` on; call after an edit starting there.
    pub fn edited(&mut self, line: usize) {
        self.spans.truncate(line);
        self.states.truncate(line + 1);
        self.frontier = self.frontier.min(line);
        self.json_folds.clear();
        self.module_folds = None;
    }

    fn start(&self) -> LineState {
        LineState {
            area: self.template_section.clone().unwrap_or_default(),
            module: self.section_state(),
            ..LineState::default()
        }
    }

    /// A Kernel module's state inside the template section: after its `#section` line.
    fn section_state(&self) -> Rc<[u8]> {
        let (Some(service), Some(section)) = (self.config.kernel(), &self.template_section) else {
            return Rc::from([]);
        };
        crate::kernel::with(|modules| {
            modules.highlight(service, Vec::new(), vec![format!("#{section}")])
        })
        .ok()
        .and_then(|mut lines| lines.pop())
        .map_or_else(|| Rc::from([]), |line| line.state.into())
    }

    fn region(&self, line: usize) -> Option<(usize, &Region)> {
        let index = self.regions.partition_point(|region| region.header <= line);
        let index = index.checked_sub(1)?;
        let region = &self.regions[index];
        (line < region.end).then_some((index, region))
    }

    /// The spans of `line`, from the nearest cached state.
    pub fn spans(&mut self, lines: &[String], line: usize) -> Rc<[Span]> {
        if let Some(Some(spans)) = self.spans.get(line) {
            return spans.clone();
        }
        let (from, state) = self.context(lines, line);
        self.run(lines, from, line + 1, state);
        self.spans[line].clone().unwrap_or_else(|| Rc::from([]))
    }

    /// The style of the character at UTF-16 column `ch`.
    pub fn style_at(&mut self, lines: &[String], line: usize, ch: usize) -> Style {
        self.spans(lines, line)
            .iter()
            .find(|span| span.end > ch)
            .and_then(|span| span.style)
    }

    /// The exact state before `line`, tokenizing from the first line as needed.
    pub fn state_before(&mut self, lines: &[String], line: usize) -> Rc<LineState> {
        let line = line.min(lines.len());
        if line > self.frontier {
            let state = self.precise(self.frontier);
            self.run(lines, self.frontier, line, state);
        }
        self.states[line].clone().unwrap_or_default()
    }

    /// Advances exact highlighting towards `until` while `more()` allows; true while work remains.
    pub fn work(&mut self, lines: &[String], until: usize, mut more: impl FnMut() -> bool) -> bool {
        let until = until.min(lines.len());
        while self.frontier < until {
            if !more() {
                return true;
            }
            let to = (self.frontier + 20).min(until);
            let state = self.precise(self.frontier);
            self.run(lines, self.frontier, to, state);
        }
        false
    }

    fn precise(&self, line: usize) -> LineState {
        match self.states.get(line) {
            Some(Some(state)) if line <= self.frontier => (**state).clone(),
            _ => self.start(),
        }
    }

    /// Where tokenizing `line` starts: a cached state within [`RESTART`] lines, else a fresh
    /// state at the least indented line in that window.
    fn context(&self, lines: &[String], line: usize) -> (usize, LineState) {
        let mut least: Option<(usize, usize)> = None;
        for search in (line.saturating_sub(RESTART - 1)..=line).rev() {
            if search == 0 {
                return (0, self.start());
            }
            if let Some(Some(state)) = self.states.get(search) {
                return (search, (**state).clone());
            }
            let indent = indentation(&lines[search - 1]);
            if least.is_none_or(|(_, least)| indent < least) {
                least = Some((search - 1, indent));
            }
        }
        (least.map_or(line, |(line, _)| line), self.start())
    }

    /// Tokenizes `from..to` from `state`, caching spans and the states after each line.
    fn run(&mut self, lines: &[String], from: usize, to: usize, mut state: LineState) {
        let exact = from <= self.frontier && self.states.get(from).is_some_and(Option::is_some)
            || from == 0;
        let to = to.min(lines.len());
        if self.spans.len() < to {
            self.spans.resize(to, None);
        }
        if self.states.len() < to + 1 {
            self.states.resize(to + 1, None);
        }
        for line in from..to {
            let spans = self.line(lines, line, &mut state);
            match &self.spans[line] {
                Some(old) if **old == *spans => {}
                old => {
                    if old.is_some() {
                        self.revision += 1;
                    }
                    self.spans[line] = Some(spans.into());
                }
            }
            self.states[line + 1] = Some(Rc::new(state.clone()));
        }
        if self.states[from].is_none() && from == 0 {
            self.states[0] = Some(Rc::new(self.start()));
        }
        if exact && to > self.frontier {
            self.frontier = to;
        }
    }

    fn line(&self, lines: &[String], index: usize, state: &mut LineState) -> Vec<Span> {
        let text = lines[index].as_str();
        if let Some(service) = self.config.kernel() {
            let tokens = self.module_line(service, index, text, state);
            return spans(text, merge(&tokens, &overlay(text, false)));
        }
        if text.is_empty() {
            return Vec::new();
        }
        let mut tokens: Vec<(usize, Style)> = Vec::new();
        let (mut pos, mut chars, mut utf16) = (0, 0, 0);
        let mut sql = false;
        while pos < text.len() {
            if utf16 > MAX_HIGHLIGHT {
                tokens.push((text.len(), None));
                break;
            }
            match self.token(lines, index, text, (pos, chars), state) {
                Step::Token(end, style, child) => {
                    let end = if end > pos {
                        end.min(text.len())
                    } else {
                        pos + text[pos..].chars().next().map_or(1, char::len_utf8)
                    };
                    sql |= pos == 0 && child;
                    chars += text[pos..end].chars().count();
                    utf16 += utf16_len(&text[pos..end]);
                    tokens.push((end, style));
                    pos = end;
                }
                Step::Table(spans) => {
                    let mut start = 0;
                    let mut bytes = Utf16Cursor::new(text);
                    for (end, style) in spans {
                        if start > MAX_HIGHLIGHT {
                            tokens.push((text.len(), None));
                            break;
                        }
                        tokens.push((bytes.byte_at(end), style));
                        start = end;
                    }
                    break;
                }
            }
        }
        spans(text, merge(&tokens, &overlay(text, sql)))
    }

    /// A line of a Kernel service: `#check` and template `@` lines are the App's, the rest
    /// the module's, which turns byte tokens into byte ends.
    fn module_line(
        &self,
        service: &str,
        index: usize,
        text: &str,
        state: &mut LineState,
    ) -> Vec<(usize, Style)> {
        state.response = self.region(index).is_some();
        let whole = |style| vec![(text.len(), style)];
        if self.template_section.is_some() && text.starts_with('@') && !text.starts_with("@@") {
            state.module = self.section_state();
            return whole(Some("comment"));
        }
        if crate::console::is_check(text) {
            state.module = Rc::from([]);
            return whole(Some("comment"));
        }
        if utf16_len(text) > MAX_HIGHLIGHT {
            return whole(None);
        }
        let highlighted = crate::kernel::with(|modules| {
            modules.highlight(service, state.module.to_vec(), vec![text.to_owned()])
        });
        let Some(line) = highlighted.ok().and_then(|mut lines| lines.pop()) else {
            let marker = text.trim_start().starts_with('#');
            return whole(marker.then_some("comment"));
        };
        state.module = line.state.into();
        let mut tokens = Vec::with_capacity(line.tokens.len() * 2);
        let mut at = 0;
        for token in line.tokens {
            let start = (token.start as usize).max(at);
            let end = (token.start as usize + token.len as usize).min(text.len());
            if start >= end || !text.is_char_boundary(start) || !text.is_char_boundary(end) {
                continue;
            }
            if start > at {
                tokens.push((start, None));
            }
            let style = (!token.kind.is_empty()).then(|| crate::kernel::style(&token.kind));
            tokens.push((end, style));
            at = end;
        }
        if at < text.len() {
            tokens.push((text.len(), None));
        }
        tokens
    }

    /// The token at byte `pos`, which is character `chars`.
    fn token(
        &self,
        lines: &[String],
        line: usize,
        text: &str,
        (pos, chars): (usize, usize),
        state: &mut LineState,
    ) -> Step {
        let console = self.config.console();
        if pos > 0 && console {
            match self.config.language(&state.area, state.response) {
                "json" if state.json.is_some() => {
                    let (end, style) = json::token(text, pos, state.json.as_mut().unwrap());
                    return Step::Token(end, style, false);
                }
                "sql" if state.sql.is_some() => {
                    let (end, style) = sql::token(text, pos, state.sql.as_mut().unwrap());
                    return Step::Token(end, style, true);
                }
                _ => {}
            }
        }
        if pos == 0
            && let Some(section) = &self.template_section
            && text.starts_with('@')
            && !text.starts_with("@@")
        {
            state.area = section.clone();
            state.response = false;
            state.json = None;
            state.sql = None;
            state.reset_lex();
            return Step::Token(text.len(), Some("comment"), false);
        }
        let output = self.region(line);
        if let Some((_, region)) = output {
            state.response = true;
            if line == region.header {
                state.reset_lex();
                return Step::Token(text.len(), Some("comment"), false);
            }
        }
        if pos == 0 && output.is_none() && console && text.trim_start().starts_with('#') {
            let marker = text
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_start_matches('#')
                .to_ascii_lowercase();
            let check = crate::console::is_check(text);
            let response = matches!(marker.as_str(), "response" | "responce");
            if !check && !response && !self.config.section(&marker) {
                let (n, style) = syntax::token(Language::Graphql, text, 0, &mut state.lex);
                return Step::Token(char_end(text, 0, n), style, false);
            }
            if response {
                state.response = true;
                state.area = "body".into();
            } else if check {
                state.response = false;
                state.area.clear();
            } else {
                state.area = marker;
            }
            state.json = None;
            state.sql = None;
            state.reset_lex();
            return Step::Token(text.len(), Some("comment"), false);
        }
        let language = self.config.language(&state.area, state.response);
        match language {
            "json" => {
                let (end, style) = json::token(text, pos, state.json.get_or_insert_default());
                return Step::Token(end, style, false);
            }
            "sql" => {
                let (end, style) = sql::token(text, pos, state.sql.get_or_insert_default());
                return Step::Token(end, style, true);
            }
            _ => {}
        }
        let Some(language) = Language::named(language) else {
            let (end, style) = fallback(text, pos);
            return Step::Token(end, style, false);
        };
        if language == Language::Table {
            return Step::Table(self.table(lines, line, text, output, state));
        }
        let (n, style) = syntax::token(language, text, chars, &mut state.lex);
        Step::Token(char_end(text, pos, n), style, false)
    }

    fn table(
        &self,
        lines: &[String],
        line: usize,
        text: &str,
        output: Option<(usize, &Region)>,
        state: &mut LineState,
    ) -> Vec<(usize, Style)> {
        if let Some((index, region)) = output
            && region.table.is_some()
        {
            state.table = Some(index);
        }
        let metadata = state
            .table
            .and_then(|index| self.regions.get(index)?.table.as_ref());
        if let Some(metadata) = metadata {
            std::mem::swap(&mut state.lex.table, &mut *metadata.borrow_mut());
        }
        if state.lex.table_cuts.is_empty()
            && text.starts_with('|')
            && let Some((_, region)) = output
        {
            for border_line in region.header + 1..(region.header + 8).min(region.end) {
                let border = lines.get(border_line).map_or("", String::as_str);
                if border.starts_with('+') && border.chars().all(|c| c == '+' || c == '-') {
                    let lex = &mut state.lex;
                    lex.table_cuts = border
                        .chars()
                        .enumerate()
                        .filter_map(|(i, c)| (c == '+').then_some(i))
                        .collect();
                    lex.table_borders = if line <= border_line + lex.table.header_lines {
                        1
                    } else {
                        2
                    };
                    lex.table_json = vec![Default::default(); lex.table.columns.len().max(1)];
                    break;
                }
            }
        }
        let spans = syntax::table_line(text, &mut state.lex);
        if let Some(metadata) = metadata {
            std::mem::swap(&mut state.lex.table, &mut *metadata.borrow_mut());
        }
        spans
    }
}

/// Byte-end tokens as spans ending at UTF-16 columns.
fn spans(text: &str, tokens: Vec<(usize, Style)>) -> Vec<Span> {
    let mut spans = Vec::with_capacity(tokens.len());
    let (mut start, mut end16) = (0, 0);
    for (end, style) in tokens {
        end16 += utf16_len(&text[start..end]);
        spans.push(Span { end: end16, style });
        start = end;
    }
    spans
}

/// The byte end of `n` characters from byte `pos`.
fn char_end(text: &str, pos: usize, n: usize) -> usize {
    text[pos..]
        .char_indices()
        .nth(n)
        .map_or(text.len(), |(index, _)| pos + index)
}

/// `{{variable}}` everywhere, plus `name(` function calls in SQL. The overlay's style
/// replaces the base style where it has one.
fn overlay(text: &str, sql: bool) -> Vec<(usize, usize, &'static str)> {
    const NOT_FUNCTIONS: [&str; 14] = [
        "in", "not", "and", "or", "on", "as", "values", "is", "all", "any", "some", "when", "then",
        "else",
    ];
    let mut out = Vec::new();
    let (mut pos, mut open) = (0, false);
    while pos < text.len() {
        let rest = &text[pos..];
        if rest.starts_with("{{") {
            open = true;
            out.push((pos, pos + 2, "gw-placeholder"));
            pos += 2;
            continue;
        }
        if open {
            if rest.starts_with("}}") {
                open = false;
                out.push((pos, pos + 2, "gw-placeholder"));
                pos += 2;
                continue;
            }
            if rest.starts_with(':') {
                out.push((pos, pos + 1, "gw-placeholder-colon"));
                pos += 1;
                continue;
            }
            let name = rest
                .bytes()
                .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-')
                .count();
            if name > 0 {
                out.push((pos, pos + name, "gw-placeholder"));
                pos += name;
                continue;
            }
            open = false;
        }
        let ordinary = if sql {
            let identifier = if rest.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
                rest.bytes()
                    .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'$')
                    .count()
            } else {
                0
            };
            if identifier > 0 {
                if rest[identifier..].starts_with('(')
                    && !NOT_FUNCTIONS.contains(&rest[..identifier].to_ascii_lowercase().as_str())
                {
                    out.push((pos, pos + identifier, "sql-function"));
                }
                pos += identifier;
                continue;
            }
            rest.find(|c: char| c.is_ascii_alphabetic() || c == '_' || c == '{')
                .unwrap_or(rest.len())
        } else {
            rest.find("{{").unwrap_or(rest.len())
        };
        pos += if ordinary > 0 {
            ordinary
        } else {
            rest.chars().next().map_or(1, char::len_utf8)
        };
    }
    out
}

/// Splits base tokens where overlay ranges begin and end; joins equal neighbours.
fn merge(base: &[(usize, Style)], overlay: &[(usize, usize, &'static str)]) -> Vec<(usize, Style)> {
    let mut out: Vec<(usize, Style)> = Vec::with_capacity(base.len());
    let mut push = |end: usize, style: Style| match out.last_mut() {
        Some(last) if last.1 == style => last.0 = end,
        _ => out.push((end, style)),
    };
    let (mut start, mut next) = (0, 0);
    for &(end, style) in base {
        let mut at = start;
        while at < end {
            while next < overlay.len() && overlay[next].1 <= at {
                next += 1;
            }
            match overlay.get(next) {
                Some(&(from, to, overlay)) if from <= at => {
                    let stop = to.min(end);
                    push(stop, Some(overlay));
                    at = stop;
                }
                Some(&(from, _, _)) => {
                    let stop = from.min(end);
                    push(stop, style);
                    at = stop;
                }
                None => {
                    push(end, style);
                    at = end;
                }
            }
        }
        start = end;
    }
    out
}

/// Languages without a tokenizer of their own.
fn fallback(text: &str, pos: usize) -> (usize, Style) {
    const KEYWORDS: [&str; 57] = [
        "SELECT",
        "FROM",
        "WHERE",
        "LIMIT",
        "ORDER",
        "BY",
        "GROUP",
        "AS",
        "JOIN",
        "LEFT",
        "RIGHT",
        "INNER",
        "ON",
        "INSERT",
        "UPDATE",
        "DELETE",
        "SET",
        "VALUES",
        "AND",
        "OR",
        "NOT",
        "IN",
        "IS",
        "NULL",
        "TRUE",
        "FALSE",
        "CREATE",
        "TABLE",
        "DROP",
        "SHOW",
        "DESCRIBE",
        "EXPLAIN",
        "QUERY",
        "MUTATION",
        "SUBSCRIPTION",
        "FRAGMENT",
        "SEARCH",
        "STATS",
        "EVAL",
        "SORT",
        "HEAD",
        "GET",
        "POST",
        "PUT",
        "PATCH",
        "PROTO3",
        "SYNTAX",
        "MESSAGE",
        "SERVICE",
        "RPC",
        "RETURNS",
        "IF",
        "THEN",
        "FI",
        "DO",
        "DONE",
        "EXPORT",
    ];
    let rest = &text[pos..];
    let end = |n: usize| pos + n;
    if rest.starts_with("{{") {
        return (
            rest.find("}}").map_or(text.len(), |n| end(n + 2)),
            Some("gw-placeholder"),
        );
    }
    let Some(c) = rest.chars().next() else {
        return (pos, None);
    };
    let take = |f: &dyn Fn(char) -> bool| rest.find(|c: char| !f(c)).unwrap_or(rest.len());
    if c.is_whitespace() {
        return (end(take(&|c| c.is_whitespace())), None);
    }
    if c == '"' || c == '\'' || c == '`' {
        let mut escaped = false;
        let mut stop = rest.len();
        for (index, ch) in rest.char_indices().skip(1) {
            if ch == c && !escaped {
                stop = index + ch.len_utf8();
                break;
            }
            escaped = ch == '\\' && !escaped;
        }
        return (end(stop), Some("string"));
    }
    if rest.starts_with("//") || rest.starts_with("--") || c == '#' {
        return (text.len(), Some("comment"));
    }
    if c.is_ascii_digit() || (c == '-' && rest[1..].starts_with(|c: char| c.is_ascii_digit())) {
        return (
            end(take(&|c| c.is_ascii_digit() || ".-+eE".contains(c))),
            Some("number"),
        );
    }
    if c.is_alphabetic() || c == '_' || c == '$' {
        let word = take(&|c| c.is_alphanumeric() || "_$.-".contains(c));
        let style = if KEYWORDS.contains(&rest[..word].to_ascii_uppercase().as_str()) {
            "keyword"
        } else if rest[word..].trim_start().starts_with(':') {
            "property"
        } else {
            "variable"
        };
        return (end(word), Some(style));
    }
    (end(c.len_utf8()), "{}[]()".contains(c).then_some("bracket"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn console(sections: &[(&str, &str)], payload: &str, response: View) -> Highlighter {
        Highlighter::with_config(Config::console_for_tests(sections, payload, response))
    }

    fn lines(text: &str) -> Vec<String> {
        text.split('\n').map(str::to_owned).collect()
    }

    /// `(text, style)` pairs of a line's styled spans.
    fn styled(
        highlighter: &mut Highlighter,
        lines: &[String],
        line: usize,
    ) -> Vec<(String, &'static str)> {
        let text = &lines[line];
        let mut start = 0;
        let mut out = Vec::new();
        for span in highlighter.spans(lines, line).iter() {
            let piece: String =
                String::from_utf16_lossy(&text.encode_utf16().collect::<Vec<_>>()[start..span.end]);
            if let Some(style) = span.style {
                out.push((piece, style));
            }
            start = span.end;
        }
        out
    }

    #[test]
    fn sections_switch_languages_and_overlays_apply() {
        let mut highlighter = console(
            &[("headers", "headers"), ("sql", "sql")],
            "json",
            View::BodyHeaders,
        );
        let doc = lines(
            "#check one\n{\"id\": \"{{user:id}}\"}\n#sql\nSELECT count(id) FROM t WHERE x IN (1)",
        );
        assert_eq!(
            styled(&mut highlighter, &doc, 0),
            [("#check one".to_owned(), "comment")]
        );
        assert_eq!(
            styled(&mut highlighter, &doc, 1),
            [
                ("\"id\"".to_owned(), "string property"),
                ("\"".to_owned(), "string"),
                ("{{user".to_owned(), "gw-placeholder"),
                (":".to_owned(), "gw-placeholder-colon"),
                ("id}}".to_owned(), "gw-placeholder"),
                ("\"".to_owned(), "string"),
            ]
        );
        assert_eq!(
            styled(&mut highlighter, &doc, 3),
            [
                ("SELECT".to_owned(), "keyword"),
                ("count".to_owned(), "sql-function"),
                ("(".to_owned(), "bracket"),
                (")".to_owned(), "bracket"),
                ("FROM".to_owned(), "keyword"),
                ("WHERE".to_owned(), "keyword"),
                ("IN".to_owned(), "keyword"),
                ("(".to_owned(), "bracket"),
                ("1".to_owned(), "number"),
                (")".to_owned(), "bracket"),
            ]
        );
    }

    #[test]
    fn response_regions_use_the_response_language() {
        let mut highlighter = console(&[], "graphql", View::BodyHeaders);
        let doc = lines("#check\nquery { a }\n#response 200\n{\"a\": 1}\n#check\n{ b }");
        highlighter.set_regions(&[ResponseRegion {
            request: 0,
            header: 2,
            end: 4,
            table: None,
        }]);
        assert_eq!(
            styled(&mut highlighter, &doc, 2),
            [("#response 200".to_owned(), "comment")]
        );
        assert_eq!(
            styled(&mut highlighter, &doc, 3),
            [
                ("\"a\"".to_owned(), "string property"),
                ("1".to_owned(), "number")
            ]
        );
        // After the next check the payload language returns.
        let state = highlighter.state_before(&doc, 5);
        assert!(!state.response());
    }

    #[test]
    fn a_cold_line_restarts_from_a_fresh_state_and_work_makes_it_exact() {
        let mut highlighter = console(&[("sql", "sql")], "json", View::BodyHeaders);
        let mut text = String::from("#sql\n");
        for _ in 0..300 {
            text.push_str("SELECT 1\n");
        }
        let doc = lines(&text);
        // 200 lines past the section header, the restart reads the line as the payload.
        assert_eq!(
            styled(&mut highlighter, &doc, 250)[0],
            ("SELECT".to_owned(), "variable")
        );
        let revision = highlighter.revision();
        assert!(!highlighter.work(&doc, doc.len(), || true));
        assert!(highlighter.revision() > revision);
        assert_eq!(
            styled(&mut highlighter, &doc, 250),
            [("SELECT".to_owned(), "keyword"), ("1".to_owned(), "number")]
        );
    }

    #[test]
    fn edits_invalidate_the_following_lines() {
        let mut highlighter = console(&[("sql", "sql")], "json", View::BodyHeaders);
        let mut doc = lines("#check\nSELECT 1");
        assert_eq!(
            styled(&mut highlighter, &doc, 1)[0],
            ("SELECT".to_owned(), "variable")
        );
        doc[0] = "#sql".into();
        highlighter.edited(0);
        assert_eq!(
            styled(&mut highlighter, &doc, 1)[0],
            ("SELECT".to_owned(), "keyword")
        );
    }

    #[test]
    fn template_sections_start_in_their_section() {
        let mut highlighter = console(&[("sql", "sql")], "json", View::BodyHeaders);
        highlighter.set_template_section(Some("sql"));
        let doc = lines("SELECT 1\n@other\nSELECT 1");
        assert_eq!(
            styled(&mut highlighter, &doc, 0)[0],
            ("SELECT".to_owned(), "keyword")
        );
        assert_eq!(
            styled(&mut highlighter, &doc, 1),
            [("@other".to_owned(), "comment")]
        );
    }
}
