//! A run's answer as the console shows it: raw (kubectl-like text), table, json or yaml.
//! Owned by the language and run part.
//!
//! The run builds a [`Content`] from the driver answers; [`render`] prints it by the `#options`:
//! rows (a server Table, `top`, `api-resources`, logs as a table) honour `Columns`, `Grep`,
//! `Table-Sort`, `Table-Count` and `Table-Offset` (the last three for `Format: table` only);
//! logs and text lines honour `Grep`; `Format: json|yaml` print the answer's JSON.

pub mod path;
pub mod time;

use crate::config::{Format, Options};
use regex::Regex;
use serde_json::{Map, Value, json};
use services_sdk::Rendered;
use services_sdk::lang::table::{self, Column, Kind, Options as ColumnOptions};
use std::cmp::Ordering;

/// How a run's answer prints: the `#options`.
#[derive(Clone, Debug, Default)]
pub struct Display {
    pub options: Options,
    grep: Option<Regex>,
}

impl Display {
    pub fn new(options: &Options) -> Result<Self, String> {
        let grep = options
            .grep
            .as_deref()
            .map(Regex::new)
            .transpose()
            .map_err(|error| format!("Grep is not a valid regular expression: {error}"))?;
        Ok(Self {
            options: options.clone(),
            grep,
        })
    }

    /// Whether the text passes `Grep`.
    pub fn keeps(&self, text: &str) -> bool {
        self.grep.as_ref().is_none_or(|grep| grep.is_match(text))
    }

    fn structured(&self) -> bool {
        matches!(self.options.format, Format::Json | Format::Yaml)
    }
}

/// One row of a table: its cells, and the object behind it for `Columns` paths.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Row {
    pub cells: Vec<Value>,
    pub object: Value,
}

/// A table: a server Table, `top`, `api-resources`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rows {
    pub columns: Vec<Column>,
    pub rows: Vec<Row>,
    /// What `Format: json|yaml` print.
    pub data: Value,
    /// Raw text of an answer without rows.
    pub empty: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LogLine {
    pub pod: String,
    pub container: String,
    pub text: String,
}

/// Log lines of one or several pod containers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Logs {
    pub lines: Vec<LogLine>,
    /// Several pod containers: lines carry `[pod/container]`, a table has the columns.
    pub multi: bool,
    /// What went wrong for single pod containers (shown after the lines).
    pub notes: Vec<String>,
}

/// Text lines of `raw` and `table`, the JSON of `json` and `yaml`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Text {
    pub lines: Vec<String>,
    pub data: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Content {
    Rows(Rows),
    Logs(Logs),
    Text(Text),
}

impl Content {
    /// Text lines whose JSON is `data`.
    pub fn text(lines: Vec<String>, data: Value) -> Self {
        Self::Text(Text { lines, data })
    }
}

pub fn yaml(value: &Value) -> String {
    serde_yaml_ng::to_string(value)
        .unwrap_or_else(|_| value.to_string())
        .trim_end()
        .to_owned()
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).expect("JSON values serialize")
}

fn kind_of(kind: &str) -> Kind {
    match kind {
        "integer" | "number" => Kind::Number,
        "boolean" => Kind::Boolean,
        _ => Kind::Text,
    }
}

/// The rows of a server `Table` (`Accept: …;as=Table`), its wide columns (`priority` > 0)
/// only when `wide` (`Format: table`, like `kubectl -o wide`); a plain object or list
/// becomes `NAME` and `AGE`.
pub fn server_table(body: &Value, now: f64, empty: &str, wide: bool) -> Rows {
    if body["kind"] != "Table" {
        return plain_table(body, now, empty);
    }
    let definitions = body["columnDefinitions"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let shown: Vec<usize> = (0..definitions.len())
        .filter(|at| wide || definitions[*at]["priority"].as_i64().unwrap_or(0) == 0)
        .collect();
    let columns = shown
        .iter()
        .map(|at| {
            Column::new(
                definitions[*at]["name"]
                    .as_str()
                    .unwrap_or_default()
                    .to_uppercase(),
                kind_of(definitions[*at]["type"].as_str().unwrap_or("string")),
            )
        })
        .collect();
    let rows = body["rows"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .map(|row| Row {
            cells: shown.iter().map(|at| row["cells"][*at].clone()).collect(),
            object: row["object"].clone(),
        })
        .collect();
    Rows {
        columns,
        rows,
        data: Value::Null,
        empty: empty.into(),
    }
}

fn plain_table(body: &Value, now: f64, empty: &str) -> Rows {
    let items: Vec<&Value> = match body["items"].as_array() {
        Some(items) => items.iter().collect(),
        None if body.is_object() => vec![body],
        None => vec![],
    };
    Rows {
        columns: vec![
            Column::new("NAME", Kind::Text),
            Column::new("AGE", Kind::Text),
        ],
        rows: items
            .into_iter()
            .map(|item| Row {
                cells: vec![
                    json!(item["metadata"]["name"].as_str().unwrap_or_default()),
                    json!(
                        item["metadata"]["creationTimestamp"]
                            .as_str()
                            .map(|at| time::age(now, at))
                            .unwrap_or_default()
                    ),
                ],
                object: item.clone(),
            })
            .collect(),
        data: Value::Null,
        empty: empty.into(),
    }
}

fn cell_text(value: &Value) -> String {
    match value {
        Value::Null => "<none>".into(),
        Value::String(text) => text.replace(['\n', '\r'], " "),
        other => other.to_string(),
    }
}

/// The columns `Columns` names: a column of the table by name (any case), or a path into the row's
/// object, optionally as `HEADER:path`.
fn project(rows: Rows, specs: &[String]) -> Result<Rows, String> {
    let mut columns = Vec::new();
    let mut sources: Vec<Result<usize, String>> = Vec::new();
    for spec in specs {
        let (alias, path) = match spec.split_once(':') {
            Some((alias, path)) if !alias.contains(['.', '[', '{', '$', '*']) => {
                (Some(alias.trim()), path.trim())
            }
            _ => (None, spec.as_str()),
        };
        let named = rows
            .columns
            .iter()
            .position(|column| column.name.eq_ignore_ascii_case(path));
        match named {
            Some(at) => {
                let mut column = rows.columns[at].clone();
                if let Some(alias) = alias {
                    column.name = alias.to_uppercase();
                }
                columns.push(column);
                sources.push(Ok(at));
            }
            None => {
                path::eval(&Value::Null, path)?;
                let header = alias.map(str::to_uppercase).unwrap_or_else(|| {
                    path.trim_matches(['{', '}', '$', '.'])
                        .rsplit(['.', '['])
                        .next()
                        .unwrap_or(path)
                        .trim_matches([']', '\'', '"', '*'])
                        .to_uppercase()
                });
                columns.push(Column::new(header, Kind::Text));
                sources.push(Err(path.to_owned()));
            }
        }
    }
    let projected = rows
        .rows
        .iter()
        .map(|row| {
            let cells = sources
                .iter()
                .map(|source| match source {
                    Ok(at) => row.cells.get(*at).cloned().unwrap_or(Value::Null),
                    Err(path) => json!(path::joined(
                        &path::eval(&row.object, path).unwrap_or_default()
                    )),
                })
                .collect();
            Row {
                cells,
                object: row.object.clone(),
            }
        })
        .collect();
    Ok(Rows {
        columns,
        rows: projected,
        data: rows.data,
        empty: rows.empty,
    })
}

fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::String(text) => {
            let digits = text.trim_end_matches(|c: char| c.is_ascii_alphabetic() || c == '%');
            digits.parse().ok()
        }
        _ => None,
    }
}

fn sorted(rows: &mut Rows, sort: &str) -> Result<(), String> {
    let (descending, name) = match sort.strip_prefix('-') {
        Some(name) => (true, name.trim()),
        None => (false, sort.trim()),
    };
    let Some(at) = rows
        .columns
        .iter()
        .position(|column| column.name.eq_ignore_ascii_case(name))
    else {
        let known: Vec<_> = rows.columns.iter().map(|c| c.name.as_str()).collect();
        return Err(format!(
            "Table-Sort refers to an unknown column {name:?}; the columns are {}",
            known.join(", ")
        ));
    };
    rows.rows.sort_by(|a, b| {
        let (left, right) = (&a.cells[at], &b.cells[at]);
        let order = match (number(left), number(right)) {
            (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(Ordering::Equal),
            _ => cell_text(left).cmp(&cell_text(right)),
        };
        if descending { order.reverse() } else { order }
    });
    Ok(())
}

/// Aligned columns like kubectl: three spaces between them, no trailing space.
pub fn aligned(header: &[String], rows: &[Vec<String>]) -> Vec<String> {
    let mut widths: Vec<usize> = header.iter().map(|name| name.chars().count()).collect();
    for row in rows {
        for (at, cell) in row.iter().enumerate() {
            if let Some(width) = widths.get_mut(at) {
                *width = (*width).max(cell.chars().count());
            }
        }
    }
    std::iter::once(header)
        .chain(rows.iter().map(Vec::as_slice))
        .map(|row| {
            let last = row.len().saturating_sub(1);
            let line: String = row
                .iter()
                .enumerate()
                .map(|(at, cell)| {
                    if at == last {
                        cell.clone()
                    } else {
                        format!("{cell:<width$}   ", width = widths[at])
                    }
                })
                .collect();
            line.trim_end().to_owned()
        })
        .collect()
}

/// Keeps the items of a list (or the elements of an array) whose JSON passes `Grep`.
fn grepped(display: &Display, data: &Value) -> Value {
    let keeps = |item: &Value| display.keeps(&item.to_string());
    match data {
        Value::Array(items) => Value::Array(items.iter().filter(|i| keeps(i)).cloned().collect()),
        Value::Object(map) if map.get("items").is_some_and(Value::is_array) => {
            let mut map = map.clone();
            let items = map["items"].as_array().cloned().unwrap_or_default();
            map.insert(
                "items".into(),
                Value::Array(items.into_iter().filter(|item| keeps(item)).collect()),
            );
            Value::Object(map)
        }
        other => other.clone(),
    }
}

fn structured(display: &Display, data: &Value) -> String {
    let data = grepped(display, data);
    match display.options.format {
        Format::Yaml => yaml(&data),
        _ => pretty(&data),
    }
}

fn rows_text(display: &Display, rows: Rows, grep: bool) -> Result<(Rendered, String), String> {
    let options = &display.options;
    let mut rows = if options.columns.is_empty() {
        rows
    } else {
        project(rows, &options.columns)?
    };
    if grep {
        rows.rows.retain(|row| {
            display.keeps(
                &row.cells
                    .iter()
                    .map(cell_text)
                    .collect::<Vec<_>>()
                    .join(" "),
            )
        });
    }
    if let Some(sort) = &options.sort {
        sorted(&mut rows, sort)?;
    }
    let total = rows.rows.len();
    if options.format == Format::Table {
        let end = if options.count == 0 {
            total
        } else {
            (options.offset + options.count).min(total)
        };
        let shown: Vec<Row> = rows
            .rows
            .iter()
            .skip(options.offset)
            .take(end.saturating_sub(options.offset))
            .cloned()
            .collect();
        let mut columns = rows.columns.clone();
        let mut overrides = ColumnOptions::parse(
            options.columns_type.as_deref(),
            options.columns_size.as_deref(),
        )?;
        let canonical = |name: &String| {
            columns
                .iter()
                .find(|column| column.name.eq_ignore_ascii_case(name))
                .map_or_else(|| name.clone(), |column| column.name.clone())
        };
        overrides.types = overrides
            .types
            .iter()
            .map(|(name, kind)| (canonical(name), *kind))
            .collect();
        overrides.sizes = overrides
            .sizes
            .iter()
            .map(|(name, size)| (canonical(name), *size))
            .collect();
        overrides.apply(&mut columns)?;
        let cells: Vec<Vec<Value>> = shown.into_iter().map(|row| row.cells).collect();
        let mut text = table::render(&columns, &cells, false);
        if total > end || options.offset > 0 {
            text.push_str(&format!(
                "\n{} of {total} rows shown; Table-Count and Table-Offset page through them (Table-Count: 0 shows all)",
                cells.len()
            ));
        }
        let status = if total == cells.len() {
            format!("{total} rows")
        } else {
            format!("{} of {total} rows", cells.len())
        };
        return Ok((
            Rendered {
                text,
                language: "kubernetes".into(),
                outcome: "OK".into(),
                columns: table::types(&columns),
            },
            status,
        ));
    }
    let text = if rows.rows.is_empty() && options.columns.is_empty() {
        rows.empty.clone()
    } else {
        let header: Vec<String> = rows.columns.iter().map(|c| c.name.clone()).collect();
        let cells: Vec<Vec<String>> = rows
            .rows
            .iter()
            .map(|row| row.cells.iter().map(cell_text).collect())
            .collect();
        aligned(&header, &cells).join("\n")
    };
    Ok((plain(text, "OK"), format!("{total} rows")))
}

fn plain(text: String, outcome: &str) -> Rendered {
    Rendered {
        text,
        language: "text".into(),
        outcome: outcome.into(),
        columns: Vec::new(),
    }
}

/// A log line as an object: its own JSON object, else `{"message": line}`; `pod` and
/// `container` first when the lines come from several containers.
fn log_object(line: &LogLine, json_lines: bool, multi: bool) -> Value {
    let mut object = Map::new();
    if multi {
        object.insert("pod".into(), json!(line.pod));
        object.insert("container".into(), json!(line.container));
    }
    let parsed = json_lines
        .then(|| serde_json::from_str::<Value>(line.text.trim()).ok())
        .flatten();
    match parsed {
        Some(Value::Object(fields)) => {
            for (key, value) in fields {
                object.entry(key).or_insert(value);
            }
        }
        _ => {
            object.insert("message".into(), json!(line.text));
        }
    }
    Value::Object(object)
}

const MAX_LOG_COLUMNS: usize = 30;

fn logs_rows(logs: &Logs, display: &Display) -> Rows {
    let json_lines = display.options.logs_json;
    let objects: Vec<Value> = logs
        .lines
        .iter()
        .map(|line| log_object(line, json_lines, logs.multi))
        .collect();
    let mut names: Vec<String> = Vec::new();
    if json_lines {
        for object in &objects {
            for key in object.as_object().into_iter().flat_map(Map::keys) {
                if !names.contains(key) && names.len() < MAX_LOG_COLUMNS {
                    names.push(key.clone());
                }
            }
        }
    } else {
        if logs.multi {
            names.extend(["pod".into(), "container".into()]);
        }
        names.push("message".into());
    }
    let columns = names
        .iter()
        .map(|name| Column::new(name.to_uppercase(), Kind::Text))
        .collect();
    let rows = objects
        .into_iter()
        .map(|object| Row {
            cells: names
                .iter()
                .map(|name| object.get(name).cloned().unwrap_or(Value::Null))
                .collect(),
            object,
        })
        .collect();
    Rows {
        columns,
        rows,
        data: Value::Null,
        empty: String::new(),
    }
}

fn logs_text(display: &Display, mut logs: Logs) -> Result<(Rendered, String), String> {
    let options = &display.options;
    logs.lines.retain(|line| display.keeps(&line.text));
    let count = logs.lines.len();
    let status = format!("{count} {}", if count == 1 { "line" } else { "lines" });
    let outcome = if logs.notes.is_empty() {
        "OK"
    } else {
        "WARNING"
    };
    if display.structured() {
        let data = Value::Array(
            logs.lines
                .iter()
                .map(|line| log_object(line, options.logs_json, logs.multi))
                .collect(),
        );
        let text = match options.format {
            Format::Yaml => yaml(&data),
            _ => pretty(&data),
        };
        let language = if options.format == Format::Json {
            "json"
        } else {
            "text"
        };
        return Ok((
            Rendered {
                text,
                language: language.into(),
                outcome: outcome.into(),
                columns: Vec::new(),
            },
            status,
        ));
    }
    let notes = logs.notes.join("\n");
    if options.format == Format::Table || !options.columns.is_empty() {
        let rows = logs_rows(&logs, display);
        let (mut rendered, _) = rows_text(display, rows, false)?;
        if !notes.is_empty() {
            rendered.text.push_str(&format!("\n{notes}"));
        }
        rendered.outcome = outcome.into();
        return Ok((rendered, status));
    }
    let mut lines: Vec<String> = logs
        .lines
        .iter()
        .map(|line| {
            if options.logs_json {
                log_object(line, true, logs.multi).to_string()
            } else if logs.multi {
                format!("[{}/{}] {}", line.pod, line.container, line.text)
            } else {
                line.text.clone()
            }
        })
        .collect();
    lines.extend(logs.notes);
    Ok((plain(lines.join("\n"), outcome), status))
}

/// What a `Transform` gets of `content` (`.`), and the notes shown after what it gives back.
/// `Format: json|yaml` give the answer's JSON; otherwise rows are objects keyed by column
/// name, with the whole API object behind the row under `object`, logs are their lines (objects as a table, `Logs-JSON` or `Columns` show them), and
/// text is its lines.
pub fn data(display: &Display, content: &Content) -> (Value, Vec<String>) {
    let options = &display.options;
    match content {
        Content::Rows(rows) if display.structured() => (rows.data.clone(), Vec::new()),
        Content::Rows(rows) => {
            let objects = rows
                .rows
                .iter()
                .map(|row| {
                    let mut object: serde_json::Map<String, Value> = rows
                        .columns
                        .iter()
                        .zip(&row.cells)
                        .map(|(column, cell)| (column.name.clone(), cell.clone()))
                        .collect();
                    if !row.object.is_null() {
                        object.insert("object".into(), row.object.clone());
                    }
                    Value::Object(object)
                })
                .collect();
            (Value::Array(objects), Vec::new())
        }
        Content::Logs(logs) => {
            let objects = display.structured()
                || options.logs_json
                || options.format == Format::Table
                || !options.columns.is_empty();
            let lines = logs
                .lines
                .iter()
                .map(|line| {
                    if objects {
                        log_object(line, options.logs_json, logs.multi)
                    } else if logs.multi {
                        json!(format!("[{}/{}] {}", line.pod, line.container, line.text))
                    } else {
                        json!(line.text)
                    }
                })
                .collect();
            (Value::Array(lines), logs.notes.clone())
        }
        Content::Text(text) if display.structured() => (text.data.clone(), Vec::new()),
        Content::Text(text) => (json!(text.lines), Vec::new()),
    }
}

/// What a `Transform` gave back for [`data`], shown by the `#options`: `Format: json|yaml`
/// print it; an array of objects (or `{"columns", "rows"}`) is a table whose `Columns`
/// paths read the objects; lines and a string are text; anything else is JSON.
pub fn shaped(
    display: &Display,
    data: Value,
    outcome: &str,
    notes: &[String],
) -> Result<(Rendered, String), String> {
    let count = |data: &Value| data.as_array().map_or(0, Vec::len);
    let (mut rendered, status) = if display.structured() {
        let mut rendered = plain(structured(display, &data), "OK");
        if display.options.format == Format::Json {
            rendered.language = "json".into();
        }
        (rendered, format!("{} rows", count(&data)))
    } else if let Some((names, cells)) = table::grid(&data) {
        let objects = match &data {
            Value::Array(objects) => objects.clone(),
            _ => data["rows"].as_array().cloned().unwrap_or_default(),
        };
        let columns = names
            .into_iter()
            .enumerate()
            .map(|(i, name)| {
                let kind = cells
                    .iter()
                    .filter_map(|row| row.get(i))
                    .find(|cell| !cell.is_null())
                    .map_or(Kind::Text, |cell| match cell {
                        Value::Number(_) => Kind::Number,
                        Value::Bool(_) => Kind::Boolean,
                        _ => Kind::Text,
                    });
                Column::new(name, kind)
            })
            .collect();
        let rows = cells
            .into_iter()
            .zip(objects)
            .map(|(cells, object)| Row { cells, object })
            .collect();
        let rows = Rows {
            columns,
            rows,
            data: Value::Null,
            empty: String::new(),
        };
        rows_text(display, rows, true)?
    } else {
        match data {
            Value::String(text) => (plain(text, "OK"), String::new()),
            Value::Array(items) if items.iter().all(Value::is_string) => {
                let lines: Vec<&str> = items
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|line| display.keeps(line))
                    .collect();
                let count = lines.len();
                let status = format!("{count} {}", if count == 1 { "line" } else { "lines" });
                (plain(lines.join("\n"), "OK"), status)
            }
            other => {
                let mut rendered = plain(pretty(&other), "OK");
                rendered.language = "json".into();
                (rendered, String::new())
            }
        }
    };
    if !notes.is_empty() {
        rendered.text.push_str(&format!("\n{}", notes.join("\n")));
    }
    if outcome != "OK" {
        rendered.outcome = outcome.into();
    }
    Ok((rendered, status))
}

/// The answer as text with its column types, and the run's status. `outcome` is `OK`, `WARNING`
/// or `ERROR`.
pub fn render(
    display: &Display,
    content: Content,
    outcome: &str,
) -> Result<(Rendered, String), String> {
    let (mut rendered, status) = match content {
        Content::Rows(rows) if display.structured() => {
            let text = structured(display, &rows.data);
            let language = if display.options.format == Format::Json {
                "json"
            } else {
                "text"
            };
            let mut rendered = plain(text, "OK");
            rendered.language = language.into();
            let count = rows
                .data
                .get("items")
                .and_then(Value::as_array)
                .map_or(rows.rows.len(), Vec::len);
            (rendered, format!("{count} rows"))
        }
        Content::Rows(rows) => rows_text(display, rows, true)?,
        Content::Logs(logs) => logs_text(display, logs)?,
        Content::Text(text) if display.structured() => {
            let mut rendered = plain(structured(display, &text.data), "OK");
            if display.options.format == Format::Json {
                rendered.language = "json".into();
            }
            (rendered, String::new())
        }
        Content::Text(text) => {
            let lines: Vec<String> = text
                .lines
                .into_iter()
                .filter(|line| display.keeps(line))
                .collect();
            (plain(lines.join("\n"), "OK"), String::new())
        }
    };
    if outcome != "OK" {
        rendered.outcome = outcome.into();
    }
    Ok((rendered, status))
}
