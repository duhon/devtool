use crate::action::COMMANDS;
use services_sdk::{
    Check, Completions, Line, LineColumns, Token,
    lang::{check::At, endpoint, finish, item, json, key_value, marker, table},
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

/// What a resource slot takes.
#[derive(Clone, Copy)]
enum Slot {
    Topic,
    Group,
    /// A name that does not exist yet.
    New,
}

/// The resources a command takes after its words `head` (`DELETE GROUP`, `ALTER CONFIG`).
fn slots(command: &str, sub: Option<&str>) -> &'static [Slot] {
    use Slot::*;
    match (command, sub) {
        ("BROKERS" | "TOPICS" | "GROUPS", _) => &[],
        ("GROUP" | "LAG", _) | ("DELETE", Some("GROUP")) => &[Group],
        ("RESET" | "COMMIT", _) => &[Group, Topic],
        ("CREATE", _) => &[New],
        _ => &[Topic],
    }
}

/// The clauses a command takes once its resources are named.
fn clauses(command: &str, sub: Option<&str>) -> &'static [&'static str] {
    const PARTITIONS: [&str; 2] = ["PARTITION ", "PARTITIONS "];
    match (command, sub) {
        ("READ" | "TAIL", _) => &[
            PARTITIONS[0],
            PARTITIONS[1],
            "FROM BEGINNING",
            "FROM END",
            "FROM LAST ",
            "FROM OFFSET ",
            "FROM TIMESTAMP ",
            "WHERE KEY = ",
            "WHERE VALUE CONTAINS ",
            "LIMIT ",
        ],
        ("OFFSETS", _) => &[
            PARTITIONS[0],
            PARTITIONS[1],
            "AT BEGINNING",
            "AT END",
            "AT TIMESTAMP ",
            "LIMIT ",
        ],
        ("PRODUCE", _) => &[PARTITIONS[0]],
        ("CREATE", _) => &[PARTITIONS[1], "REPLICAS "],
        ("ALTER", None) => &[PARTITIONS[1]],
        ("PURGE", _) => &[PARTITIONS[0], PARTITIONS[1], "BEFORE OFFSET "],
        ("RESET", _) => &[
            "TO BEGINNING",
            "TO END",
            "TO OFFSET ",
            "TO TIMESTAMP ",
            PARTITIONS[0],
            PARTITIONS[1],
        ],
        ("COMMIT", _) => &["OFFSET ", PARTITIONS[0], PARTITIONS[1]],
        ("DELETE" | "ALTER", _) => &[],
        _ => &["LIMIT "],
    }
}

/// What may follow the finished words of an action: the grammar in `action.rs`, by position.
fn action_items(done: &str, topics: &[String], groups: &[String]) -> Vec<services_sdk::Item> {
    let words: Vec<String> = done
        .split_whitespace()
        .map(str::to_ascii_uppercase)
        .collect();
    let Some(command) = words.first().map(String::as_str) else {
        return COMMANDS
            .iter()
            .map(|c| item(format!("{c} "), "Kafka command", "command"))
            .collect();
    };
    if !COMMANDS.contains(&command) {
        return Vec::new();
    }
    let names = |slot: Slot| -> Vec<_> {
        match slot {
            Slot::Topic => topics
                .iter()
                .map(|topic| item(format!("/{topic}"), "Discovered topic", "topic"))
                .collect(),
            Slot::Group => groups
                .iter()
                .map(|group| item(format!("/{group}"), "Discovered consumer group", "group"))
                .collect(),
            Slot::New => Vec::new(),
        }
    };
    let sub = match (command, words.get(1).map(String::as_str)) {
        ("DELETE", Some("GROUP")) => Some("GROUP"),
        ("ALTER", Some("CONFIG")) => Some("CONFIG"),
        _ => None,
    };
    let rest = &words[1 + usize::from(sub.is_some())..];
    let wanted = slots(command, sub);
    if rest.len() < wanted.len() {
        let mut items = names(wanted[rest.len()]);
        if rest.is_empty() && sub.is_none() {
            let keyword = match command {
                "DELETE" => Some("GROUP "),
                "ALTER" => Some("CONFIG "),
                _ => None,
            };
            items.extend(keyword.map(|k| item(k, "Kafka clause", "keyword")));
        }
        return items;
    }
    let keywords = |values: &[&str]| -> Vec<_> {
        values
            .iter()
            .map(|v| item(*v, "Kafka clause", "keyword"))
            .collect()
    };
    let last = rest.last().map_or("", String::as_str);
    match last {
        "FROM" => return keywords(&["BEGINNING", "END", "LAST ", "OFFSET ", "TIMESTAMP "]),
        "AT" => return keywords(&["BEGINNING", "END", "TIMESTAMP "]),
        "TO" => return keywords(&["BEGINNING", "END", "OFFSET ", "TIMESTAMP "]),
        "BEFORE" => return keywords(&["OFFSET "]),
        "WHERE" => return keywords(&["KEY = ", "VALUE CONTAINS "]),
        "LIMIT" | "PARTITION" | "PARTITIONS" | "LAST" | "OFFSET" | "TIMESTAMP" | "REPLICAS"
        | "KEY" | "=" | "CONTAINS" | "VALUE" | "," => return Vec::new(),
        word if word.ends_with(',') => return Vec::new(),
        _ => {}
    }
    let used = |clause: &str| {
        let head = clause.split_whitespace().next().unwrap_or_default();
        let partition = |w: &str| w == "PARTITION" || w == "PARTITIONS";
        rest.iter()
            .any(|w| w == head || partition(head) && partition(w))
    };
    clauses(command, sub)
        .iter()
        .filter(|clause| !used(clause))
        .map(|clause| item(*clause, "Kafka clause", "keyword"))
        .collect()
}

pub fn complete(check: &Check, pos: u32, topics: &[String], groups: &[String]) -> Completions {
    let at = At::new(&check.text, pos as usize);
    if at.in_response() || at.in_placeholder() {
        return at.none();
    }
    if let Some(markers) = at.markers(SECTIONS) {
        return markers;
    }
    // A blank line in #payload belongs to the message.
    if at.name() != Some("payload")
        && let Some(next) = at.next_section(SECTIONS)
    {
        return next;
    }
    let before = at.before;
    let start = before.rfind(char::is_whitespace).map_or(0, |i| i + 1);
    let from = at.line_start(at.line) + start;
    let prefix = &before[start..];
    let items = match at.name() {
        Some("action") => action_items(&before[..start], topics, groups),
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

/// The shape of a response, told by its first line: a table (or text) or `Format: json`.
const SHAPE_UNKNOWN: u8 = 0;
const SHAPE_ROWS: u8 = 1;
const SHAPE_JSON: u8 = 2;

pub fn highlight(state: &[u8], lines: &[String], columns: &[LineColumns]) -> Vec<Line> {
    let first = state.first().copied().unwrap_or(0);
    let mut area = first & 0x0f;
    let mut shape = first >> 4;
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
                shape = SHAPE_UNKNOWN;
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
                                    "REPLICAS",
                                    "TO",
                                    "BEFORE",
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
                    4 => endpoint::tokens(line, false),
                    0 | 5 => {
                        if shape == SHAPE_UNKNOWN && !line.trim().is_empty() {
                            shape = match line.trim_start().starts_with(['{', '[']) {
                                true => SHAPE_JSON,
                                false => SHAPE_ROWS,
                            };
                        }
                        if shape == SHAPE_JSON {
                            json::tokens(line, &mut json_state)
                        } else {
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
                    }
                    _ => Vec::new(),
                }
            };
            let mut state = vec![area | shape << 4];
            if matches!(area, 0 | 5) && shape != SHAPE_JSON {
                table_state.encode(&mut state);
            } else {
                json_state.encode(&mut state);
            }
            Line { tokens, state }
        })
        .collect()
}
