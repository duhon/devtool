//! The Splunk check language: SPL, endpoint, headers and options highlighting, and
//! completion of markers, endpoints, options, headers and common SPL.

use services_sdk::lang::check::At;
use services_sdk::lang::table;
use services_sdk::lang::{endpoint, finish, item, key_value, marker};
use services_sdk::{Check, Completions, Item, Line, Token};

pub const SECTIONS: [&str; 4] = ["target", "headers", "options", "payload"];

const COMMANDS: &str = "search stats eval where table fields sort dedup rex rename lookup join transaction head tail top rare chart timechart bin bucket stream streamstats eventstats fillnull convert foreach makemv mvexpand mvcombine spath xmlkv regex format addinfo appendcols append appendpipe multisearch map tstats inputlookup outputlookup collect delete outputcsv inputcsv iplocation geostats cluster anomalydetection predict trendline gauge highlight loadjob savedsearch sendemail sistats sireduce strcat transpose untable union";

const OPTIONS: [(&str, &str); 11] = [
    ("From: -24h", "Option"),
    ("To: now", "Option"),
    ("App: search", "Option"),
    ("Count: 30", "Option"),
    ("Preview: false", "Option"),
    ("Offset: 0", "Option"),
    (
        "Columns-Type: ",
        "column=json column=datetime column=number column=boolean column=text",
    ),
    ("Columns-Size: ", "column=100 (characters)"),
    ("Format: table", "Option"),
    ("Format: vertical", "Option"),
    ("Format: json", "Option"),
];

const HEADERS: [&str; 5] = [
    "Accept: */*",
    "Authorization: Bearer ",
    "Cookie: ",
    "User-Agent: ",
    "X-Api-Key: ",
];

const SEARCH: [&str; 8] = [
    "search ",
    "index=",
    "sourcetype=",
    "| stats ",
    "| table ",
    "| sort ",
    "| head ",
    "| eval ",
];

/// SPL tokenizer state across lines.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Spl {
    /// Inside a ```` ``` ```` comment.
    pub fenced: bool,
    /// The next word is a command.
    pub after_pipe: bool,
}

impl Spl {
    fn encode(self, out: &mut Vec<u8>) {
        out.push(u8::from(self.fenced) | u8::from(self.after_pipe) << 1);
    }
    fn decode(bytes: &[u8]) -> Self {
        let byte = bytes.first().copied().unwrap_or(0);
        Self {
            fenced: byte & 1 != 0,
            after_pipe: byte & 2 != 0,
        }
    }
}

fn token(start: usize, len: usize, kind: &str) -> Token {
    Token {
        start: start as u32,
        len: len as u32,
        kind: kind.into(),
    }
}

fn word(text: &str) -> usize {
    text.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'))
        .unwrap_or(text.len())
}

fn quoted(text: &str, quote: char) -> usize {
    let mut escaped = false;
    for (i, c) in text.char_indices().skip(1) {
        if c == quote && !escaped {
            return i + 1;
        }
        escaped = c == '\\' && !escaped;
    }
    text.len()
}

/// The tokens of one SPL line.
pub fn spl(line: &str, state: &mut Spl) -> Vec<Token> {
    let mut out = vec![];
    let mut pos = 0;
    while pos < line.len() {
        let rest = &line[pos..];
        let first = rest.chars().next().unwrap_or(' ');
        if state.fenced {
            let end = rest.find("```").map_or(rest.len(), |end| {
                state.fenced = false;
                end + 3
            });
            out.push(token(pos, end, "comment"));
            pos += end;
            continue;
        }
        if first.is_whitespace() {
            pos += first.len_utf8();
            continue;
        }
        let (len, kind) = if let Some(content) = rest.strip_prefix("```") {
            state.after_pipe = false;
            match content.find("```") {
                Some(end) => (end + 6, Some("comment")),
                None => {
                    state.fenced = true;
                    (rest.len(), Some("comment"))
                }
            }
        } else if first == '|' {
            state.after_pipe = true;
            (1, Some("operator"))
        } else if first == '"' || first == '\'' {
            state.after_pipe = false;
            (quoted(rest, first), Some("string"))
        } else if "=!<>".contains(first) {
            state.after_pipe = false;
            (
                if rest[1..].starts_with('=') { 2 } else { 1 },
                Some("operator"),
            )
        } else if first.is_ascii_digit()
            || (first == '-' && rest[1..].starts_with(|c: char| c.is_ascii_digit()))
        {
            state.after_pipe = false;
            let len = rest
                .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
                .unwrap_or(rest.len());
            (len, Some("number"))
        } else {
            let len = word(rest);
            if len == 0 {
                state.after_pipe = false;
                (first.len_utf8(), None)
            } else {
                let value = rest[..len].to_ascii_lowercase();
                let after = std::mem::take(&mut state.after_pipe);
                let keyword = ["and", "or", "not", "xor"].contains(&value.as_str())
                    || (after && COMMANDS.split_whitespace().any(|c| c == value));
                let tail = rest[len..].trim_start();
                let kind = if keyword {
                    Some("keyword")
                } else if tail.starts_with('=') && !tail.starts_with("==") {
                    Some("attribute")
                } else {
                    None
                };
                (len, kind)
            }
        };
        if let Some(kind) = kind {
            out.push(token(pos, len, kind));
        }
        pos += len;
    }
    out
}

/// The part of a check a line is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum Area {
    /// Before the first marker, or a rendered response highlighted alone: table text.
    Prelude,
    Endpoint,
    Headers,
    Search,
    Other,
    Response,
}

impl Area {
    const ALL: [Area; 6] = [
        Area::Prelude,
        Area::Endpoint,
        Area::Headers,
        Area::Search,
        Area::Other,
        Area::Response,
    ];
}

/// Highlighting of a Splunk check, and of its result tables.
pub fn highlight(
    state: &[u8],
    lines: &[String],
    columns: &[services_sdk::LineColumns],
) -> Vec<Line> {
    let (mut area, rest) = match state.split_first() {
        Some((area, rest)) => (
            Area::ALL
                .get(*area as usize)
                .copied()
                .unwrap_or(Area::Other),
            rest,
        ),
        None => (Area::Prelude, &[][..]),
    };
    let (mut response, rest) = table::State::read(rest);
    let mut search = Spl::decode(rest);
    lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let named = marker(line).map(|(name, _)| name);
            let is_marker = named.as_deref().is_some_and(|name| {
                (area != Area::Response && !(area == Area::Search && search.fenced))
                    || matches!(name, "responce" | "response")
            });
            let tokens = if is_marker {
                area = match named.as_deref() {
                    Some("target") => Area::Endpoint,
                    Some("headers" | "options") => Area::Headers,
                    Some("payload") => Area::Search,
                    Some("responce" | "response") => Area::Response,
                    _ => Area::Other,
                };
                search = Spl::default();
                response = response.restart();
                if let Some(reported) = columns.iter().find(|at| at.line == index as u32) {
                    response.report(&reported.columns);
                }
                if line.is_empty() {
                    vec![]
                } else {
                    vec![token(0, line.len(), "comment")]
                }
            } else {
                match area {
                    Area::Endpoint => endpoint::tokens(line, false),
                    Area::Headers => {
                        response.declare(line);
                        key_value::tokens(line)
                    }
                    Area::Search => spl(line, &mut search),
                    Area::Prelude | Area::Response => table::tokens(line, &mut response),
                    Area::Other => vec![],
                }
            };
            let mut state = vec![area as u8];
            response.encode(&mut state);
            search.encode(&mut state);
            Line { tokens, state }
        })
        .collect()
}

fn pairs(at: &At, hints: Vec<Item>) -> Completions {
    let used = key_value::used(at.other_lines());
    let items: Vec<Item> = hints
        .into_iter()
        .filter(|item| !used.contains(&key_value::name(&item.insert)))
        .collect();
    let line_start = at.line_start(at.line);
    let mut result = finish(line_start, at.pos, at.before.trim(), items);
    if at.before.trim().is_empty() && !used.is_empty() {
        let mut items = finish(line_start, at.pos, "", at.unused(&SECTIONS)).items;
        items.append(&mut result.items);
        result.items = items;
    }
    result
}

/// Completion in a Splunk check.
pub fn complete(check: &Check, pos: usize) -> Completions {
    let at = At::new(&check.text, pos);
    if at.in_placeholder() || at.in_response() {
        return at.none();
    }
    if let Some(markers) = at.markers(&SECTIONS) {
        return markers;
    }
    match at.name() {
        Some("target") => {
            if let Some(next) = at.next_section(&SECTIONS) {
                return next;
            }
            let value = at.before.trim_start();
            let from = at.line_start(at.line) + at.before.len() - value.len();
            let items = ["https://", "http://"]
                .map(|scheme| item(scheme, "Target", "url"))
                .into();
            finish(from, at.pos, value, items)
        }
        Some("options") => {
            let hints = OPTIONS
                .iter()
                .map(|(insert, detail)| item(*insert, detail, "header"))
                .collect();
            pairs(&at, hints)
        }
        Some("headers") => pairs(&at, key_value::items(&HEADERS, &[], &Default::default())),
        Some("payload") => {
            if let Some(next) = at.next_section(&SECTIONS) {
                return next;
            }
            let before = at.before;
            let start = before
                .rfind(|c: char| c.is_whitespace())
                .map_or(0, |i| i + 1);
            let fragment = &before[start..];
            let items = SEARCH
                .iter()
                .map(|insert| item(*insert, "SPL", "keyword"))
                .collect();
            finish(at.pos - fragment.len(), at.pos, fragment, items)
        }
        _ => at.none(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(line: &str, state: &mut Spl) -> Vec<(String, String)> {
        spl(line, state)
            .into_iter()
            .map(|t| {
                let at = t.start as usize;
                (line[at..at + t.len as usize].to_owned(), t.kind)
            })
            .collect()
    }

    #[test]
    fn spl_comments_and_command_boundaries() {
        let mut state = Spl::default();
        let tokens = kinds("index=main | stats count by host", &mut state);
        assert!(tokens.contains(&("index".into(), "attribute".into())));
        assert!(tokens.contains(&("stats".into(), "keyword".into())));
        assert!(!tokens.contains(&("count".into(), "keyword".into())));
        kinds("```note", &mut state);
        assert!(state.fenced);
        assert_eq!(kinds("other```", &mut state)[0].1, "comment");
        assert!(!state.fenced);
    }

    #[test]
    fn highlight_keeps_spl_state_across_lines() {
        let lines: Vec<String> = ["#payload", "```a", "#b```", "index=x"]
            .map(String::from)
            .into();
        let out = highlight(&[], &lines, &[]);
        assert_eq!(out[2].tokens[0].kind, "comment");
        assert_eq!(out[3].tokens[0].kind, "attribute");
        let more = highlight(&out[1].state, &lines[2..], &[]);
        assert_eq!(more[0].tokens, out[2].tokens);
    }

    fn labels(text: &str) -> Vec<String> {
        let check = Check {
            columns: Vec::new(),
            text: text.into(),
            ..Check::default()
        };
        complete(&check, text.len())
            .items
            .into_iter()
            .map(|item| item.insert)
            .collect()
    }

    #[test]
    fn completes_markers_endpoints_and_options() {
        assert_eq!(
            labels("#"),
            ["#target\n", "#headers\n", "#options\n", "#payload\n"]
        );
        assert_eq!(labels("#target\nhttp"), ["https://", "http://"]);
        assert_eq!(
            labels("#options\n"),
            [
                "From: -24h",
                "To: now",
                "App: search",
                "Count: 30",
                "Preview: false",
                "Offset: 0",
                "Columns-Type: ",
                "Columns-Size: ",
                "Format: table",
                "Format: vertical",
                "Format: json"
            ]
        );
        assert!(
            !labels("#options\nFormat: json\n")
                .iter()
                .any(|l| l.starts_with("Format"))
        );
        assert!(labels("#payload\nso").contains(&"sourcetype=".into()));
    }
}
