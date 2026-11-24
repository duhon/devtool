use crate::action::COMMANDS;
use services_sdk::{
    Check, Completions, Line, LineColumns, Token,
    lang::{check::At, finish, item, json, key_value, marker, table},
};

pub const SECTIONS: &[&str] = &["target", "action", "metadata", "options", "payload"];
pub const OPTIONS: &[&str] = &[
    "Format: table",
    "Format: vertical",
    "Format: json",
    "Isolation: read-committed",
    "Isolation: read-uncommitted",
    "Timeout: 30",
    "Max-Bytes: 1048576",
    "Scan-Limit: 1000",
    "Security-Protocol: plaintext",
    "Security-Protocol: sasl_ssl",
    "Sasl-Mechanism: SCRAM-SHA-512",
    "Sasl-Username: {{:KAFKA_USERNAME}}",
    "Sasl-Password: {{:KAFKA_PASSWORD}}",
    "Sasl-Token: {{:KAFKA_TOKEN}}",
    "Ca-Pem: {{:KAFKA_CA_PEM}}",
    "Certificate-Pem: {{:KAFKA_CERT_PEM}}",
    "Key-Pem: {{:KAFKA_KEY_PEM}}",
    "Payload-Encoding: json",
    "Tombstone: true",
];

pub fn complete(check: &Check, pos: u32, topics: &[String]) -> Completions {
    let at = At::new(&check.text, pos as usize);
    if at.in_response() || at.in_placeholder() {
        return at.none();
    }
    if let Some(markers) = at.markers(SECTIONS) {
        return markers;
    }
    let before = at.before;
    let start = before.rfind(char::is_whitespace).map_or(0, |i| i + 1);
    let from = at.line_start(at.line) + start;
    let prefix = &before[start..];
    let items = match at.name() {
        Some("action") => {
            let mut items: Vec<_> = COMMANDS
                .iter()
                .map(|c| item(format!("{c} "), "Kafka command", "command"))
                .collect();
            items.extend(
                [
                    "PARTITION ",
                    "PARTITIONS ",
                    "FROM BEGINNING",
                    "FROM END",
                    "FROM LAST ",
                    "FROM OFFSET ",
                    "FROM TIMESTAMP ",
                    "AT END",
                    "AT BEGINNING",
                    "AT TIMESTAMP ",
                    "LIMIT ",
                    "WHERE KEY = ",
                    "WHERE VALUE CONTAINS ",
                ]
                .iter()
                .map(|c| item(*c, "Kafka clause", "keyword")),
            );
            items.extend(
                topics
                    .iter()
                    .map(|topic| item(format!("/{topic}"), "Discovered topic", "topic")),
            );
            items
        }
        Some("options") => {
            return finish(
                at.line_start(at.line),
                at.pos,
                before.trim(),
                OPTIONS
                    .iter()
                    .map(|s| item(*s, "Kafka option", "option"))
                    .collect(),
            );
        }
        Some("metadata") => {
            return finish(
                at.line_start(at.line),
                at.pos,
                before.trim(),
                [
                    "Key: ",
                    "Key-Base64: ",
                    "Header: ",
                    "Header-Base64: ",
                    "Null-Header: ",
                ]
                .iter()
                .map(|s| item(*s, "Message metadata", "header"))
                .collect(),
            );
        }
        _ => Vec::new(),
    };
    finish(from, at.pos, prefix, items)
}

pub fn highlight(state: &[u8], lines: &[String], columns: &[LineColumns]) -> Vec<Line> {
    let mut area = state.first().copied().unwrap_or(0);
    let rest = state.get(1..).unwrap_or_default();
    let mut json_state = json::State::decode(rest);
    let (mut table_state, _) = table::State::read(rest);
    lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            let tokens = if let Some((name, _)) = marker(line)
                && (area != 5 || matches!(name.as_str(), "response" | "responce"))
            {
                area = match name.as_str() {
                    "action" => 1,
                    "metadata" | "options" => 2,
                    "payload" => 3,
                    "target" => 4,
                    "response" | "responce" => 5,
                    _ => 0,
                };
                json_state = json::State::default();
                table_state = table::State::default();
                vec![Token {
                    start: 0,
                    len: line.len() as u32,
                    kind: "comment".into(),
                }]
            } else {
                match area {
                    1 => {
                        let mut tokens = Vec::new();
                        let mut offset = 0;
                        for word in line.split_inclusive(char::is_whitespace) {
                            let token = word.trim_end();
                            let upper = token.to_ascii_uppercase();
                            let kind = if token.starts_with('/') {
                                "def"
                            } else if token.parse::<i64>().is_ok() {
                                "number"
                            } else if token.starts_with(['\'', '"']) {
                                "string"
                            } else if COMMANDS.contains(&upper.as_str())
                                || [
                                    "FROM",
                                    "AT",
                                    "LAST",
                                    "OFFSET",
                                    "TIMESTAMP",
                                    "LIMIT",
                                    "PARTITION",
                                    "PARTITIONS",
                                    "BEGINNING",
                                    "END",
                                    "WHERE",
                                    "KEY",
                                    "VALUE",
                                    "CONTAINS",
                                ]
                                .contains(&upper.as_str())
                            {
                                "keyword"
                            } else {
                                "text"
                            };
                            if !token.is_empty() {
                                tokens.push(Token {
                                    start: offset as u32,
                                    len: token.len() as u32,
                                    kind: kind.into(),
                                });
                            }
                            offset += word.len();
                        }
                        tokens
                    }
                    2 => key_value::tokens(line),
                    3 => json::tokens(line, &mut json_state),
                    0 | 5 => {
                        let metadata = columns
                            .iter()
                            .find(|c| c.line == i as u32)
                            .map(|c| c.columns.as_slice())
                            .unwrap_or_default();
                        if !metadata.is_empty() {
                            table_state.report(metadata);
                        }
                        table::tokens(line, &mut table_state)
                    }
                    _ => Vec::new(),
                }
            };
            let mut state = vec![area];
            if matches!(area, 0 | 5) {
                table_state.encode(&mut state);
            } else {
                json_state.encode(&mut state);
            }
            Line { tokens, state }
        })
        .collect()
}
