use crate::format::display;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use services_contracts::columns::{Kind, kind};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metadata {
    pub columns: Vec<String>,
    pub types: BTreeMap<String, Kind>,
    #[serde(default = "one")]
    pub header_lines: usize,
}
fn one() -> usize {
    1
}

impl Metadata {
    pub fn from_result(result: &Value) -> Self {
        let columns: Vec<String> = result["columns"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        let header_lines = columns
            .iter()
            .map(|name| {
                let width = result["columnSizes"][name]
                    .as_u64()
                    .and_then(|n| usize::try_from(n).ok())
                    .filter(|n| *n > 0)
                    .unwrap_or(usize::MAX);
                name.chars().count().div_ceil(width).max(1)
            })
            .max()
            .unwrap_or(1);
        Self {
            columns,
            types: serde_json::from_value(result["columnTypes"].clone()).unwrap_or_default(),
            header_lines,
        }
    }
}

pub fn cell(value: &Value, kind: Kind, pretty: bool) -> String {
    if value.is_null() {
        return display(value);
    }
    if kind == Kind::Json {
        let parsed = value
            .as_str()
            .and_then(|text| serde_json::from_str::<Value>(text).ok());
        let json = if value.is_string() {
            parsed.as_ref()
        } else {
            Some(value)
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

pub fn render(result: &Value, vertical: bool) -> String {
    let columns = Metadata::from_result(result).columns;
    let rows = result["rows"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    if columns.is_empty() {
        return if rows.is_empty() {
            "Empty set".into()
        } else {
            rows.iter().map(display).collect::<Vec<_>>().join("\n")
        };
    }
    if vertical {
        let width = columns.iter().map(|c| c.chars().count()).max().unwrap_or(0);
        return rows
            .iter()
            .enumerate()
            .map(|(i, row)| {
                let values = columns
                    .iter()
                    .map(|name| {
                        let kind = kind(result, name);
                        let value = cell(&row[name], kind, true);
                        format!(
                            "{name:>width$}: {}",
                            if kind == Kind::Json {
                                value.replace('\n', &format!("\n{}", " ".repeat(width + 2)))
                            } else {
                                value
                            }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                format!(
                    "*************************** {}. row ***************************\n{values}",
                    i + 1
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
                .map(|name| {
                    cell(&row[name], kind(result, name), false)
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
        .map(|(i, name)| {
            let natural = values
                .iter()
                .map(|row| row[i].chars().count())
                .chain(std::iter::once(name.chars().count()))
                .max()
                .unwrap_or(1)
                .max(1);
            result["columnSizes"][name]
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .filter(|n| *n > 0)
                .map(|limit| natural.min(limit))
                .unwrap_or(natural)
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
    let mut output = vec![border.clone()];
    output.extend(lines(&columns));
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

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct JsonState {
    pub quoted: bool,
    pub escaped: bool,
    #[serde(default)]
    pub atom: String,
    #[serde(default)]
    pub number: bool,
}

pub fn json_token(text: &str, state: &mut JsonState) -> (usize, Option<&'static str>) {
    let Some(first) = text.chars().next() else {
        return (0, None);
    };
    if !state.quoted && first.is_whitespace() {
        return (text.chars().take_while(|c| c.is_whitespace()).count(), None);
    }
    if !state.atom.is_empty() {
        let n = text
            .chars()
            .zip(state.atom.chars())
            .take_while(|(a, b)| a == b)
            .count();
        if n > 0 {
            state.atom = state.atom.chars().skip(n).collect();
            return (n, Some("atom"));
        }
        state.atom.clear();
    }
    if first == '"' || state.quoted {
        let start = usize::from(!state.quoted);
        state.quoted = true;
        let mut length = start;
        for (byte, c) in text.char_indices().skip(start) {
            length += 1;
            if c == '"' && !state.escaped {
                state.quoted = false;
                let property = text[byte + 1..].chars().find(|c| !c.is_whitespace()) == Some(':');
                return (length, Some(if property { "property" } else { "string" }));
            }
            state.escaped = c == '\\' && !state.escaped;
        }
        return (length, Some("string"));
    }
    if "{}[],:".contains(first) {
        state.number = false;
        return (1, Some("bracket"));
    }
    if first.is_ascii_digit() || first == '-' || state.number && ".eE+".contains(first) {
        state.number = true;
        return (
            text.chars()
                .take_while(|c| c.is_ascii_digit() || ".eE+-".contains(*c))
                .count(),
            Some("number"),
        );
    }
    for atom in ["true", "false", "null"] {
        if text.starts_with(atom) {
            return (atom.len(), Some("atom"));
        }
        let fragment = text.trim_end();
        if !fragment.is_empty() && atom.starts_with(fragment) {
            state.atom = atom[fragment.len()..].to_owned();
            return (fragment.len(), Some("atom"));
        }
    }
    (1, Some("error"))
}
