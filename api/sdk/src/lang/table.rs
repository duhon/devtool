//! Result tables as the MySQL client prints them: `+---+` bordered rows, or `\G` vertical
//! records; column kinds that decide how cells print, the `Columns-Type` / `Columns-Size`
//! overrides, and a tokenizer that styles such text by the kinds `Columns-Type` lines declare.

use super::json;
use crate::Token;
use serde_json::Value;
use std::collections::BTreeMap;

/// How a column's cells print and highlight.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kind {
    #[default]
    Text,
    Number,
    Boolean,
    Datetime,
    /// JSON documents, kept as text by the source: printed compact in rows, indented in
    /// vertical records.
    Json,
}

impl Kind {
    /// A kind by name or alias, ignoring case.
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value.to_ascii_lowercase().as_str() {
            "text" | "string" => Self::Text,
            "number" | "numeric" | "integer" | "int" | "float" | "decimal" => Self::Number,
            "boolean" | "bool" => Self::Boolean,
            "datetime" | "date" | "time" | "timestamp" => Self::Datetime,
            "json" => Self::Json,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    pub kind: Kind,
    /// The most characters a cell line takes; longer cells wrap.
    pub size: Option<usize>,
}

impl Column {
    pub fn new(name: impl Into<String>, kind: Kind) -> Self {
        Self {
            name: name.into(),
            kind,
            size: None,
        }
    }
}

/// `Columns-Type` and `Columns-Size` overrides: whitespace-separated `column=value`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    pub types: BTreeMap<String, Kind>,
    pub sizes: BTreeMap<String, usize>,
}

fn assignments(source: &str, key: &str) -> Result<BTreeMap<String, String>, String> {
    let mut result = BTreeMap::new();
    for pair in source.split_whitespace() {
        let (name, value) = pair
            .split_once('=')
            .filter(|(k, v)| !k.is_empty() && !v.is_empty())
            .ok_or_else(|| format!("Invalid {key} assignment {pair:?}; expected column=value"))?;
        if result.insert(name.to_owned(), value.to_owned()).is_some() {
            return Err(format!("Duplicate {key} assignment for {name}"));
        }
    }
    Ok(result)
}

impl Options {
    pub fn parse(types: Option<&str>, sizes: Option<&str>) -> Result<Self, String> {
        let mut options = Self::default();
        for (name, value) in assignments(types.unwrap_or(""), "Columns-Type")? {
            let kind = Kind::parse(&value)
                .ok_or_else(|| format!("Unsupported column type {value:?} for {name}"))?;
            options.types.insert(name, kind);
        }
        for (name, value) in assignments(sizes.unwrap_or(""), "Columns-Size")? {
            let size = value
                .parse::<usize>()
                .ok()
                .filter(|n| *n > 0)
                .ok_or_else(|| {
                    format!("Column size for {name} must be a positive number of characters")
                })?;
            options.sizes.insert(name, size);
        }
        Ok(options)
    }

    pub fn is_empty(&self) -> bool {
        self.types.is_empty() && self.sizes.is_empty()
    }

    /// Overrides the columns' kinds and sizes; every named column must exist.
    pub fn apply(&self, columns: &mut [Column]) -> Result<(), String> {
        for name in self.types.keys().chain(self.sizes.keys()) {
            if !columns.iter().any(|column| &column.name == name) {
                return Err(format!(
                    "Column options refer to an unknown result column: {name}"
                ));
            }
        }
        for column in columns {
            if let Some(kind) = self.types.get(&column.name) {
                column.kind = *kind;
            }
            if let Some(size) = self.sizes.get(&column.name) {
                column.size = Some(*size);
            }
        }
        Ok(())
    }
}

/// A value as a cell shows it: `NULL`, a string as is, anything else as JSON.
pub fn display(value: &Value) -> String {
    match value {
        Value::Null => "NULL".into(),
        Value::String(text) => text.clone(),
        value => value.to_string(),
    }
}

/// A cell of `kind`; a JSON column's text is parsed and printed compact or `pretty`.
pub fn cell(value: &Value, kind: Kind, pretty: bool) -> String {
    if kind == Kind::Json && !value.is_null() {
        let parsed;
        let json = match value {
            Value::String(text) => {
                parsed = serde_json::from_str::<Value>(text).ok();
                parsed.as_ref()
            }
            value => Some(value),
        };
        if let Some(value) = json {
            return if pretty {
                serde_json::to_string_pretty(value)
            } else {
                serde_json::to_string(value)
            }
            .expect("JSON values serialize");
        }
    }
    display(value)
}

fn chunks(text: &str, width: usize) -> Vec<&str> {
    let mut result = vec![];
    let mut n = 0;
    let mut start = 0;
    for (byte, _) in text.char_indices() {
        if n == width {
            result.push(&text[start..byte]);
            start = byte;
            n = 0;
        }
        n += 1;
    }
    result.push(&text[start..]);
    result
}

/// The rows as a pretty JSON array of objects in column order; a JSON column holds its
/// parsed value.
pub fn json(columns: &[Column], rows: &[Vec<Value>]) -> String {
    let rows: Vec<Value> = rows
        .iter()
        .map(|row| {
            Value::Object(
                columns
                    .iter()
                    .zip(row)
                    .map(|(column, value)| {
                        let parsed = (column.kind == Kind::Json)
                            .then(|| value.as_str())
                            .flatten()
                            .and_then(|text| serde_json::from_str(text).ok());
                        (column.name.clone(), parsed.unwrap_or_else(|| value.clone()))
                    })
                    .collect(),
            )
        })
        .collect();
    serde_json::to_string_pretty(&rows).expect("JSON values serialize")
}

/// How a table response shows (`Format:` in `#options`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Table,
    Vertical,
    Json,
}

impl Format {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "table" => Ok(Self::Table),
            "vertical" => Ok(Self::Vertical),
            "json" => Ok(Self::Json),
            _ => Err(format!(
                "Format takes table, vertical or json, not {:?}",
                value.trim()
            )),
        }
    }
}

/// The rows in `format`; without one, a table, vertical when `vertical`.
pub fn show(
    columns: &[Column],
    rows: &[Vec<Value>],
    format: Option<Format>,
    vertical: bool,
) -> String {
    match format {
        Some(Format::Json) => json(columns, rows),
        Some(Format::Table) => render(columns, rows, false),
        Some(Format::Vertical) => render(columns, rows, true),
        None => render(columns, rows, vertical),
    }
}

/// The rows as the MySQL client prints them; `vertical` as after `\G`. No columns print as
/// `Empty set`.
pub fn render(columns: &[Column], rows: &[Vec<Value>], vertical: bool) -> String {
    if columns.is_empty() {
        return "Empty set".into();
    }
    let value = |row: &[Value], i: usize| row.get(i).cloned().unwrap_or(Value::Null);
    if vertical {
        let width = columns
            .iter()
            .map(|c| c.name.chars().count())
            .max()
            .unwrap_or(0);
        return rows
            .iter()
            .enumerate()
            .map(|(n, row)| {
                let values = columns
                    .iter()
                    .enumerate()
                    .map(|(i, column)| {
                        let text = cell(&value(row, i), column.kind, true);
                        let text = if column.kind == Kind::Json {
                            text.replace('\n', &format!("\n{}", " ".repeat(width + 2)))
                        } else {
                            text
                        };
                        format!("{:>width$}: {text}", column.name)
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                format!(
                    "*************************** {}. row ***************************\n{values}",
                    n + 1
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    let values: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            columns
                .iter()
                .enumerate()
                .map(|(i, column)| {
                    cell(&value(row, i), column.kind, false)
                        .replace('\r', "\\r")
                        .replace('\n', "\\n")
                        .replace('\t', "\\t")
                })
                .collect()
        })
        .collect();
    let widths: Vec<usize> = columns
        .iter()
        .enumerate()
        .map(|(i, column)| {
            let natural = values
                .iter()
                .map(|row| row[i].chars().count())
                .chain(std::iter::once(column.name.chars().count()))
                .max()
                .unwrap_or(1)
                .max(1);
            column.size.map_or(natural, |limit| natural.min(limit))
        })
        .collect();
    let border = format!(
        "+{}+",
        widths
            .iter()
            .map(|n| "-".repeat(n + 2))
            .collect::<Vec<_>>()
            .join("+")
    );
    let lines = |row: &[String]| {
        let cells: Vec<_> = row
            .iter()
            .enumerate()
            .map(|(i, value)| chunks(value, widths[i]))
            .collect();
        (0..cells.iter().map(Vec::len).max().unwrap_or(1))
            .map(|line| {
                format!(
                    "| {} |",
                    cells
                        .iter()
                        .enumerate()
                        .map(|(i, cell)| {
                            let text = cell.get(line).copied().unwrap_or("");
                            format!("{text}{}", " ".repeat(widths[i] - text.chars().count()))
                        })
                        .collect::<Vec<_>>()
                        .join(" | ")
                )
            })
            .collect::<Vec<_>>()
    };
    let names: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
    let mut output = vec![border.clone()];
    output.extend(lines(&names));
    output.push(border.clone());
    for (i, row) in values.iter().enumerate() {
        let rendered = lines(row);
        let wrapped = rendered.len() > 1;
        output.extend(rendered);
        if wrapped && i + 1 < values.len() {
            output.push(border.clone());
        }
    }
    output.push(border);
    output.join("\n")
}

/// The style of the borders and `*** N. row ***` separators.
pub const BORDER: &str = "mysql-table-border";

/// The tokenizer state between lines of a rendered table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct State {
    /// The char columns of the `+` in the last border.
    pub cuts: Vec<u32>,
    /// Borders since the table started: rows after the second are data.
    pub borders: u8,
    /// In vertical records.
    pub vertical: bool,
    /// Inside a multi-line JSON value of a vertical record.
    pub json: Option<json::State>,
    /// Column kinds named by `Columns-Type` lines ([`State::declare`]); they outlive tables.
    pub declared: BTreeMap<String, Kind>,
    /// Column kinds the server reported for the current response (`render` metadata); a
    /// declared kind wins. They end with the response.
    pub reported: BTreeMap<String, Kind>,
    /// The declared kinds of the current table's columns, from its header row.
    pub columns: Vec<Option<Kind>>,
    /// Undeclared cells that look like JSON are styled as JSON.
    pub guess_json: bool,
    /// The JSON values of a row's cells that continue on its next line (a wrapped cell).
    pub cells: Vec<Option<Fragment>>,
}

/// Where a JSON cell value wrapped onto several lines stopped on the last of them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fragment {
    quoted: bool,
    escaped: bool,
    number: bool,
    depth: u32,
    /// The rest of a `true`, `false` or `null` cut by the wrap.
    atom: String,
}

impl Fragment {
    fn open(&self) -> bool {
        self.quoted || self.depth > 0 || !self.atom.is_empty()
    }

    /// The tokens of `text` (at byte `offset` of its line), a piece of a JSON value.
    fn tokens(&mut self, text: &str, offset: usize, out: &mut Vec<Token>) {
        let mut at = 0;
        while let Some(first) = text[at..].chars().next() {
            let rest = &text[at..];
            let (len, kind) = self.token(rest, first);
            let len = len.max(first.len_utf8()).min(rest.len());
            if let Some(kind) = kind {
                out.push(super::token(offset + at, offset + at + len, kind));
            }
            at += len;
        }
    }

    fn token(&mut self, text: &str, first: char) -> (usize, Option<&'static str>) {
        if !self.quoted && first.is_whitespace() {
            return (text.len() - text.trim_start().len(), None);
        }
        if !self.atom.is_empty() {
            let n = text
                .bytes()
                .zip(self.atom.bytes())
                .take_while(|(a, b)| a == b)
                .count();
            if n > 0 {
                self.atom.drain(..n);
                return (n, Some("atom"));
            }
            self.atom.clear();
        }
        if first == '"' || self.quoted {
            let start = usize::from(!self.quoted);
            self.quoted = true;
            for (byte, c) in text.char_indices().skip(start) {
                if c == '"' && !self.escaped {
                    self.quoted = false;
                    let property = text[byte + 1..].trim_start().starts_with(':');
                    return (byte + 1, Some(if property { "property" } else { "string" }));
                }
                self.escaped = c == '\\' && !self.escaped;
            }
            return (text.len(), Some("string"));
        }
        if "{}[],:".contains(first) {
            self.number = false;
            match first {
                '{' | '[' => self.depth += 1,
                '}' | ']' => self.depth = self.depth.saturating_sub(1),
                _ => {}
            }
            return (1, Some("bracket"));
        }
        if first.is_ascii_digit() || first == '-' || self.number && ".eE+".contains(first) {
            self.number = true;
            let n = text
                .bytes()
                .take_while(|b| b.is_ascii_digit() || b".eE+-".contains(b))
                .count();
            return (n, Some("number"));
        }
        for atom in ["true", "false", "null"] {
            if text.starts_with(atom) {
                return (atom.len(), Some("atom"));
            }
            let fragment = text.trim_end();
            if let Some(rest) = atom.strip_prefix(fragment) {
                self.atom = rest.to_owned();
                return (fragment.len(), Some("atom"));
            }
        }
        (first.len_utf8(), Some("error"))
    }

    fn encode(&self, out: &mut Vec<u8>) {
        out.push(
            1 | u8::from(self.quoted) << 1
                | u8::from(self.escaped) << 2
                | u8::from(self.number) << 3,
        );
        out.extend(self.depth.to_le_bytes());
        out.push(self.atom.len() as u8);
        out.extend(self.atom.as_bytes());
    }
}

fn kind_byte(kind: Option<Kind>) -> u8 {
    match kind {
        Some(Kind::Text) => 0,
        Some(Kind::Number) => 1,
        Some(Kind::Boolean) => 2,
        Some(Kind::Datetime) => 3,
        Some(Kind::Json) => 4,
        None => 255,
    }
}

fn byte_kind(byte: u8) -> Option<Kind> {
    [
        Kind::Text,
        Kind::Number,
        Kind::Boolean,
        Kind::Datetime,
        Kind::Json,
    ]
    .get(byte as usize)
    .copied()
}

impl State {
    /// A state that keeps the declared kinds and the JSON guess of this one.
    pub fn restart(&self) -> Self {
        Self {
            declared: self.declared.clone(),
            guess_json: self.guess_json,
            ..Self::default()
        }
    }

    /// Takes the column types the server reported for the response that starts here.
    pub fn report(&mut self, columns: &[crate::ColumnType]) {
        self.reported = reported(columns);
    }

    /// The kind of column `name`: declared by `Columns-Type`, else reported by the server.
    pub fn kind_of(&self, name: &str) -> Option<Kind> {
        self.declared
            .get(name)
            .or_else(|| self.reported.get(name))
            .copied()
    }

    /// Takes the kinds of a `Columns-Type: column=kind…` line; other lines change nothing.
    pub fn declare(&mut self, line: &str) {
        let Some((name, value)) = line.split_once(':') else {
            return;
        };
        if !name.trim().eq_ignore_ascii_case("Columns-Type") {
            return;
        }
        for (column, kind) in value
            .split_whitespace()
            .filter_map(|pair| pair.split_once('='))
        {
            if let Some(kind) = Kind::parse(kind) {
                self.declared.insert(column.to_owned(), kind);
            }
        }
    }

    pub fn encode(&self, out: &mut Vec<u8>) {
        let len = |out: &mut Vec<u8>, n: usize| out.extend((n as u32).to_le_bytes());
        out.push(self.borders);
        out.push(
            u8::from(self.vertical)
                | u8::from(self.json.is_some()) << 1
                | u8::from(self.guess_json) << 2,
        );
        len(out, self.cuts.len());
        for cut in &self.cuts {
            out.extend(cut.to_le_bytes());
        }
        len(out, self.columns.len());
        out.extend(self.columns.iter().map(|kind| kind_byte(*kind)));
        len(out, self.declared.len());
        for (name, kind) in &self.declared {
            len(out, name.len());
            out.extend(name.as_bytes());
            out.push(kind_byte(Some(*kind)));
        }
        let mut json = Vec::new();
        if let Some(state) = &self.json {
            state.encode(&mut json);
        }
        len(out, json.len());
        out.extend(json);
        len(out, self.cells.len());
        for cell in &self.cells {
            match cell {
                Some(cell) => cell.encode(out),
                None => out.push(0),
            }
        }
        len(out, self.reported.len());
        for (name, kind) in &self.reported {
            len(out, name.len());
            out.extend(name.as_bytes());
            out.push(kind_byte(Some(*kind)));
        }
    }

    /// The state [`State::encode`] wrote and the bytes after it; the default for anything else.
    pub fn read(bytes: &[u8]) -> (Self, &[u8]) {
        fn take<'a>(rest: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
            let (head, tail) = (rest.get(..n)?, rest.get(n..)?);
            *rest = tail;
            Some(head)
        }
        fn count(rest: &mut &[u8]) -> Option<usize> {
            Some(u32::from_le_bytes(take(rest, 4)?.try_into().ok()?) as usize)
        }
        fn read(mut rest: &[u8]) -> Option<(State, &[u8])> {
            let [borders, flags] = *take(&mut rest, 2)? else {
                return None;
            };
            let n = count(&mut rest)?;
            let cuts = take(&mut rest, n.checked_mul(4)?)?
                .chunks(4)
                .map(|cut| u32::from_le_bytes(cut.try_into().expect("4 bytes")))
                .collect();
            let n = count(&mut rest)?;
            let columns = take(&mut rest, n)?
                .iter()
                .map(|byte| byte_kind(*byte))
                .collect();
            let mut declared = BTreeMap::new();
            for _ in 0..count(&mut rest)? {
                let n = count(&mut rest)?;
                let name = String::from_utf8(take(&mut rest, n)?.to_vec()).ok()?;
                declared.insert(name, byte_kind(take(&mut rest, 1)?[0])?);
            }
            let n = count(&mut rest)?;
            let json = take(&mut rest, n)?;
            let mut cells = Vec::new();
            for _ in 0..count(&mut rest)? {
                let flags = take(&mut rest, 1)?[0];
                if flags == 0 {
                    cells.push(None);
                    continue;
                }
                let depth = u32::from_le_bytes(take(&mut rest, 4)?.try_into().ok()?);
                let n = take(&mut rest, 1)?[0] as usize;
                cells.push(Some(Fragment {
                    quoted: flags & 2 != 0,
                    escaped: flags & 4 != 0,
                    number: flags & 8 != 0,
                    depth,
                    atom: String::from_utf8(take(&mut rest, n)?.to_vec()).ok()?,
                }));
            }
            let mut reported = BTreeMap::new();
            for _ in 0..count(&mut rest)? {
                let n = count(&mut rest)?;
                let name = String::from_utf8(take(&mut rest, n)?.to_vec()).ok()?;
                reported.insert(name, byte_kind(take(&mut rest, 1)?[0])?);
            }
            let state = State {
                reported,
                cuts,
                borders,
                vertical: flags & 1 != 0,
                json: (flags & 2 != 0).then(|| json::State::decode(json)),
                declared,
                columns,
                guess_json: flags & 4 != 0,
                cells,
            };
            Some((state, rest))
        }
        read(bytes).unwrap_or_default()
    }

    /// The state [`State::encode`] wrote; the default for anything else.
    pub fn decode(bytes: &[u8]) -> Self {
        Self::read(bytes).0
    }
}

fn whole(line: &str, kind: &str) -> Vec<Token> {
    if line.is_empty() {
        return vec![];
    }
    vec![super::token(0, line.len(), kind)]
}

fn number(text: &str) -> bool {
    let digits = text.strip_prefix('-').unwrap_or(text);
    digits.starts_with(|c: char| c.is_ascii_digit())
        && digits
            .chars()
            .all(|c| c.is_ascii_digit() || ".eE+-".contains(c))
        && text.parse::<f64>().is_ok()
}

fn datetime(text: &str) -> bool {
    let b = text.as_bytes();
    let digits = |range: std::ops::Range<usize>| {
        b.get(range)
            .is_some_and(|part| part.iter().all(u8::is_ascii_digit))
    };
    let date = digits(0..4) && b.get(4) == Some(&b'-') && digits(5..7) && b.get(7) == Some(&b'-');
    let time = b.len() >= 8 && digits(0..2) && b.get(2) == Some(&b':') && digits(3..5);
    date || time
}

/// The tokens of one cell's text at byte `start`, by its declared kind; an undeclared cell
/// styles `NULL` and booleans as atoms, numbers, dates and times as strings, and JSON documents
/// as JSON when `guess_json`; other text unstyled.
fn cell_tokens(
    text: &str,
    start: usize,
    kind: Option<Kind>,
    guess_json: bool,
    out: &mut Vec<Token>,
) -> Option<json::State> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let at = start + (text.len() - text.trim_start().len());
    let end = at + trimmed.len();
    let json = |out: &mut Vec<Token>| {
        let mut state = json::State::default();
        out.extend(cell_json(text, &mut state).into_iter().map(|token| Token {
            start: token.start + start as u32,
            ..token
        }));
        Some(state)
    };
    if trimmed == "NULL" {
        out.push(super::token(at, end, "atom"));
        return None;
    }
    match kind {
        Some(Kind::Text) => {}
        Some(Kind::Number) => out.push(super::token(at, end, "number")),
        Some(Kind::Boolean) => out.push(super::token(at, end, "atom")),
        Some(Kind::Datetime) => out.push(super::token(at, end, "string")),
        Some(Kind::Json) => return json(out),
        None if trimmed == "true" || trimmed == "false" => {
            out.push(super::token(at, end, "atom"));
        }
        None if number(trimmed) => out.push(super::token(at, end, "number")),
        None if guess_json && trimmed.starts_with(['{', '[']) => return json(out),
        None if datetime(trimmed) => out.push(super::token(at, end, "string")),
        None => {}
    }
    None
}

/// JSON tokens of a cell, whose keys are plain properties as in the rest of a table.
fn cell_json(text: &str, state: &mut json::State) -> Vec<Token> {
    let mut tokens = json::tokens(text, state);
    for token in &mut tokens {
        if token.kind == "string property" {
            token.kind = "property".into();
        }
    }
    tokens
}

/// The byte offset of each char column of `line`, and its end.
fn offsets(line: &str) -> Vec<usize> {
    line.char_indices()
        .map(|(byte, _)| byte)
        .chain(std::iter::once(line.len()))
        .collect()
}

/// The char columns of the `|` that separate the padded cells of a data row.
fn row_cuts(line: &str) -> Vec<u32> {
    let chars: Vec<char> = line.trim_end().chars().collect();
    (0..chars.len())
        .filter(|&i| {
            chars[i] == '|'
                && (i == 0 || chars[i - 1] == ' ')
                && (i + 1 == chars.len() || chars[i + 1] == ' ')
        })
        .map(|i| i as u32)
        .collect()
}

/// Reported kinds other than text: a text column may still hold JSON the tokenizer guesses.
fn reported(columns: &[crate::ColumnType]) -> BTreeMap<String, Kind> {
    columns
        .iter()
        .filter_map(|column| Some((column.name.clone(), Kind::parse(&column.kind)?)))
        .filter(|(_, kind)| *kind != Kind::Text)
        .collect()
}

/// The [`crate::ColumnType`]s of `columns`, for `render` metadata.
pub fn types(columns: &[Column]) -> Vec<crate::ColumnType> {
    columns
        .iter()
        .map(|column| crate::ColumnType::new(&column.name, kind_name(column.kind)))
        .collect()
}

/// The name [`Kind::parse`] reads back.
pub fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Text => "text",
        Kind::Number => "number",
        Kind::Boolean => "boolean",
        Kind::Datetime => "datetime",
        Kind::Json => "json",
    }
}

/// The folds of the JSON values of vertical records (`name: {` to its closing bracket) in a
/// check's text, except of columns `Columns-Type` declares, or the server reported
/// (`columns`), as another kind; table rows never fold.
pub fn folds(text: &str, columns: &[crate::ColumnType]) -> Vec<crate::Fold> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut declared = State {
        reported: reported(columns),
        ..State::default()
    };
    for line in lines
        .iter()
        .take_while(|line| !line.trim_start().starts_with("#respon"))
    {
        declared.declare(line);
    }
    let separator = |line: &str| line.starts_with("*****") && line.trim_end().ends_with("*****");
    let mut out = Vec::new();
    let mut at = 0;
    while at < lines.len() {
        if !separator(lines[at]) {
            at += 1;
            continue;
        }
        at += 1;
        let mut colon = None;
        while at < lines.len() && !separator(lines[at]) && !lines[at].starts_with(['+', '#']) {
            let line = lines[at];
            let colon = *colon.get_or_insert_with(|| line.find(": ").unwrap_or(0));
            let name = line.get(..colon).map(str::trim_start).unwrap_or_default();
            let start = at;
            at += 1;
            // Continuation lines are indented past the aligned `name: `.
            while at < lines.len()
                && !separator(lines[at])
                && !lines[at].starts_with(['+', '#'])
                && lines[at].get(colon..colon + 1) != Some(":")
            {
                at += 1;
            }
            if declared
                .kind_of(name)
                .is_some_and(|kind| kind != Kind::Json)
            {
                continue;
            }
            out.extend(
                json::folds(lines[start..at].iter().copied())
                    .into_iter()
                    .map(|fold| crate::Fold {
                        start: fold.start + start as u32,
                        end: fold.end + start as u32,
                    }),
            );
        }
    }
    out
}

/// The tokens of one line of a rendered table, or of other text (left unstyled).
pub fn tokens(line: &str, state: &mut State) -> Vec<Token> {
    let trimmed = line.trim();
    if let Some(json_state) = &mut state.json {
        let tokens = cell_json(line, json_state);
        if json_state.open.is_empty() {
            state.json = None;
        }
        return tokens;
    }
    if line.starts_with('+') && trimmed.len() > 1 && trimmed.chars().all(|c| c == '+' || c == '-') {
        state.cuts = line
            .chars()
            .enumerate()
            .filter_map(|(i, c)| (c == '+').then_some(i as u32))
            .collect();
        state.borders = state.borders.saturating_add(1);
        state.vertical = false;
        state.cells.clear();
        return whole(line, BORDER);
    }
    if line.starts_with("*****") && trimmed.ends_with("*****") {
        state.vertical = true;
        state.cuts.clear();
        return whole(line, BORDER);
    }
    if line.starts_with('|') && state.cuts.len() < 2 && !state.vertical {
        // A row reached without its table (highlighting restarted mid-table): the cells are
        // padded, so its ` | ` separators are the borders, and it is a data row.
        state.cuts = row_cuts(line);
        state.borders = 2;
    }
    if line.starts_with('|') && state.cuts.len() > 1 {
        let offsets = offsets(line);
        let at = |column: u32| offsets.get(column as usize).copied();
        let mut out = Vec::new();
        let header = state.borders == 1;
        if header {
            state.columns.clear();
        }
        for (index, bounds) in state.cuts.windows(2).enumerate() {
            if let Some(start) = at(bounds[0]) {
                out.push(super::token(start, start + 1, BORDER));
            }
            let (Some(start), Some(end)) = (at(bounds[0] + 1), at(bounds[1])) else {
                continue;
            };
            if header {
                let name = line[start..end].trim();
                state.columns.push(state.kind_of(name));
            } else if state.borders >= 2 {
                let kind = state.columns.get(index).copied().flatten();
                let cell = &line[start..end];
                let trimmed = cell.trim();
                if state.cells.len() <= index {
                    state.cells.resize(index + 1, None);
                }
                let json = trimmed != "NULL"
                    && (kind == Some(Kind::Json)
                        || kind.is_none()
                            && (state.cells[index].is_some()
                                || state.guess_json && trimmed.starts_with(['{', '['])));
                if json {
                    // Without its one space of padding on each side.
                    let inner = cell.strip_prefix(' ').unwrap_or(cell);
                    let inner = inner.strip_suffix(' ').unwrap_or(inner);
                    let mut fragment = state.cells[index].take().unwrap_or_default();
                    fragment.tokens(inner, start + usize::from(cell.starts_with(' ')), &mut out);
                    state.cells[index] = fragment.open().then_some(fragment);
                } else {
                    cell_tokens(cell, start, kind, state.guess_json, &mut out);
                }
            }
        }
        if let Some(start) = state.cuts.last().and_then(|cut| at(*cut))
            && line[start..].starts_with('|')
        {
            out.push(super::token(start, start + 1, BORDER));
        }
        return out;
    }
    if !line.starts_with('|') {
        state.cuts.clear();
        state.borders = 0;
    }
    if state.vertical
        && let Some(colon) = line.find(": ")
        && !line[..colon].trim().is_empty()
    {
        let mut out = vec![super::token(0, colon + 2, "property")];
        let kind = state.kind_of(line[..colon].trim());
        if let Some(json) = cell_tokens(
            &line[colon + 2..],
            colon + 2,
            kind,
            state.guess_json,
            &mut out,
        ) && !json.open.is_empty()
        {
            state.json = Some(json);
        }
        return out;
    }
    vec![]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::styled;
    use serde_json::json;

    fn columns(names: &[(&str, Kind)]) -> Vec<Column> {
        names
            .iter()
            .map(|(name, kind)| Column::new(*name, *kind))
            .collect()
    }

    fn styles(text: &str) -> Vec<(String, String)> {
        styled_from(
            State {
                guess_json: true,
                ..State::default()
            },
            text,
        )
    }

    fn styled_from(mut state: State, text: &str) -> Vec<(String, String)> {
        text.split('\n')
            .flat_map(|line| {
                let mut bytes = Vec::new();
                state.encode(&mut bytes);
                assert_eq!(State::decode(&bytes), state);
                styled(line, &tokens(line, &mut state))
            })
            .filter(|(_, kind)| kind != BORDER)
            .collect()
    }

    #[test]
    fn rows_render_as_mysql_tables_and_vertical_records() {
        let columns = columns(&[("id", Kind::Number), ("name", Kind::Text)]);
        let rows = vec![vec![json!(1), Value::Null]];
        assert_eq!(
            render(&columns, &rows, false),
            "+----+------+\n| id | name |\n+----+------+\n| 1  | NULL |\n+----+------+"
        );
        assert_eq!(
            render(&columns, &rows, true),
            "*************************** 1. row ***************************\n  id: 1\nname: NULL"
        );
        assert_eq!(render(&[], &[], false), "Empty set");
    }

    #[test]
    fn json_cells_are_compact_in_rows_and_indented_in_records() {
        let columns = columns(&[("doc", Kind::Json), ("text", Kind::Text)]);
        let rows = vec![vec![json!("{\"a\": [1]}"), json!("a\nb\tc")]];
        assert!(render(&columns, &rows, false).contains("| {\"a\":[1]} | a\\nb\\tc |"));
        assert!(render(&columns, &rows, true).contains("\n doc: {\n        \"a\": [\n"));
        assert_eq!(
            json(&columns, &rows),
            "[\n  {\n    \"doc\": {\n      \"a\": [\n        1\n      ]\n    },\n    \"text\": \"a\\nb\\tc\"\n  }\n]"
        );
    }

    #[test]
    fn sizes_wrap_cells_and_separate_wrapped_rows() {
        let mut columns = columns(&[("message", Kind::Text)]);
        let options = Options::parse(None, Some("message=4")).unwrap();
        options.apply(&mut columns).unwrap();
        let rows = vec![vec![json!("abcdefg")], vec![json!("x")]];
        assert_eq!(
            render(&columns, &rows, false),
            "+------+\n| mess |\n| age  |\n+------+\n| abcd |\n| efg  |\n+------+\n| x    |\n+------+"
        );
    }

    #[test]
    fn options_parse_aliases_and_reject_bad_assignments() {
        let options = Options::parse(Some("a=JSON b=decimal c=bool"), None).unwrap();
        assert_eq!(options.types["a"], Kind::Json);
        assert_eq!(options.types["b"], Kind::Number);
        assert_eq!(options.types["c"], Kind::Boolean);
        for (types, sizes) in [
            (Some("missing-equals"), None),
            (Some("=json"), None),
            (Some("a=xml"), None),
            (Some("a=json a=text"), None),
            (None, Some("a=0")),
            (None, Some("a=1.5")),
        ] {
            assert!(Options::parse(types, sizes).is_err(), "{types:?} {sizes:?}");
        }
        let error = options
            .apply(&mut columns(&[("a", Kind::Text)]))
            .unwrap_err();
        assert!(error.contains("unknown result column: b"), "{error}");
    }

    #[test]
    fn tables_highlight_cells_by_their_text() {
        let columns = columns(&[
            ("id", Kind::Number),
            ("doc", Kind::Json),
            ("at", Kind::Datetime),
            ("name", Kind::Text),
        ]);
        let rows = vec![vec![
            json!(7),
            json!("{\"a\":true}"),
            json!("2024-01-02 03:04:05"),
            Value::Null,
        ]];
        let pairs = |items: &[(&str, &str)]| {
            items
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            styles(&render(&columns, &rows, false)),
            pairs(&[
                ("7", "number"),
                ("{", "bracket"),
                ("\"a\"", "property"),
                (":", "bracket"),
                ("true", "atom"),
                ("}", "bracket"),
                ("2024-01-02 03:04:05", "string"),
                ("NULL", "atom"),
            ])
        );
        let vertical = styles(&render(&columns, &rows, true));
        assert_eq!(vertical[0], ("  id: ".into(), "property".into()));
        assert!(vertical.contains(&("\"a\"".into(), "property".into())));
        assert!(vertical.contains(&("2024-01-02 03:04:05".into(), "string".into())));
        assert!(styles("Empty set\nERROR 1064 (42000): x").is_empty());
    }

    #[test]
    fn declared_kinds_style_their_columns_and_outlive_tables() {
        let mut state = State::default();
        state.declare("Columns-Type: n=number doc=json plain=text bad=xml");
        state.declare("Columns-Size: n=4");
        let restarted = state.restart();
        assert_eq!(restarted.declared.len(), 3);
        let names = ["n", "doc", "plain", "other"].map(|name| Column::new(name, Kind::Text));
        let rows = vec![
            vec![
                json!("12"),
                json!("{\"k\":1}"),
                json!("{\"not\":1}"),
                json!("3"),
            ],
            vec![
                json!("x"),
                json!("{\"k\":2}"),
                json!("5"),
                json!("{\"j\":1}"),
            ],
        ];
        let table = render(&names, &rows, false);
        let styles = styled_from(restarted, &table);
        assert!(styles.contains(&("12".into(), "number".into())));
        assert!(styles.contains(&("x".into(), "number".into())));
        assert!(styles.contains(&("\"k\"".into(), "property".into())));
        assert!(styles.contains(&("3".into(), "number".into())));
        assert!(
            !styles
                .iter()
                .any(|(text, _)| text.contains("not") || text == "5" || text.contains('j'))
        );
        let vertical = "*************************** 1. row ***************************\n  doc: {\"k\":1}\nplain: {\"j\":1}";
        let styles = styled_from(state, vertical);
        assert!(styles.contains(&("\"k\"".into(), "property".into())));
        assert!(!styles.iter().any(|(text, _)| text.contains('j')));
    }

    #[test]
    fn rows_reached_cold_take_their_separators_and_records_fold_their_json() {
        let mut state = State {
            guess_json: true,
            ..State::default()
        };
        let styles = styled(
            "| 7  | {\"a\":1} |",
            &tokens("| 7  | {\"a\":1} |", &mut state),
        );
        assert!(styles.contains(&("7".into(), "number".into())));
        assert!(styles.contains(&("\"a\"".into(), "property".into())));
        let text = "#payload\nx\\G\n#responce OK\n*************************** 1. row ***************************\n doc: {\n        \"a\": [\n          1\n        ]\n      }\n  id: 1\n+---+\n| { |\n+---+\n| } |";
        let ranges = |text| -> Vec<(u32, u32)> {
            folds(text, &[])
                .into_iter()
                .map(|f| (f.start, f.end))
                .collect()
        };
        assert_eq!(ranges(text), [(4, 8), (5, 7)]);
        let typed = "#options\nColumns-Type: doc=text\n#responce OK\n***** 1. row *****\n doc: {\n        \"a\": 1\n      }\nmore: {\n      }";
        assert_eq!(ranges(typed), [(7, 8)]);
    }

    #[test]
    fn reported_kinds_style_and_fold_unless_columns_type_overrides_them() {
        let reported = [
            crate::ColumnType::new("doc", "json"),
            crate::ColumnType::new("n", "number"),
        ];
        let mut state = State::default();
        state.report(&reported);
        let mut styles = Vec::new();
        for line in [
            "+----+---------+",
            "| n  | doc     |",
            "+----+---------+",
            "| 7  | {\"a\":1} |",
        ] {
            styles.extend(styled(line, &tokens(line, &mut state)));
        }
        assert!(styles.contains(&("7".into(), "number".into())));
        assert!(styles.contains(&("\"a\"".into(), "property".into())));
        let mut declared = State::default();
        declared.report(&reported);
        declared.declare("Columns-Type: doc=text");
        assert_eq!(declared.kind_of("doc"), Some(Kind::Text));
        assert_eq!(declared.kind_of("n"), Some(Kind::Number));
        let text =
            "#payload\nx\\G\n#responce OK\n***** 1. row *****\n doc: {\n        \"a\": 1\n      }";
        let folded = |columns: &[crate::ColumnType]| folds(text, columns).len();
        assert_eq!(folded(&[]), 1);
        assert_eq!(folded(&[crate::ColumnType::new("doc", "text")]), 1);
        assert_eq!(folded(&[crate::ColumnType::new("doc", "number")]), 0);
    }

    #[test]
    fn wrapped_json_cells_continue_on_their_next_lines() {
        let mut state = State::default();
        state.declare("Columns-Type: doc=json");
        let lines = [
            "+----+------------+",
            "| id | doc        |",
            "+----+------------+",
            "| 1  | {\"k\":\"lon |",
            "|    | g\",\"n\":42} |",
            "+----+------------+",
        ];
        let mut styles = Vec::new();
        for line in lines {
            let encoded = {
                let mut out = Vec::new();
                state.encode(&mut out);
                out
            };
            state = State::decode(&encoded);
            styles.extend(styled(line, &tokens(line, &mut state)));
        }
        assert!(styles.contains(&("g\"".into(), "string".into())));
        assert!(styles.contains(&("\"n\"".into(), "property".into())));
        assert!(styles.contains(&("42".into(), "number".into())));
        assert!(state.cells.is_empty());
    }
}
