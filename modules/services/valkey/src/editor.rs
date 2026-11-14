use crate::{config, discovery::Scope};
use serde_json::Value;
use services_sdk::lang::{check::At, endpoint, finish, item, json, key_value, marker};
use services_sdk::{Check, Completions, Item, Line, Token};

const SECTIONS: &[&str] = &["target", "metadata", "options", "action"];
const COMMANDS: &str = "APPEND AUTH BITCOUNT BITOP BLPOP BRPOP CLIENT COMMAND COPY DBSIZE DECR DECRBY DEL DUMP ECHO EVAL EVALSHA EXISTS EXPIRE EXPIREAT EXPIRETIME FLUSHDB GET GETDEL GETEX GETRANGE GETSET HDEL HEXISTS HGET HGETALL HINCRBY HINCRBYFLOAT HKEYS HLEN HMGET HMSET HRANDFIELD HSCAN HSET HSETNX HSTRLEN HVALS INCR INCRBY INCRBYFLOAT INFO KEYS LINDEX LINSERT LLEN LMOVE LPOP LPOS LPUSH LPUSHX LRANGE LREM LSET LTRIM MGET MSET MSETNX OBJECT PERSIST PEXPIRE PEXPIREAT PING PSETEX PTTL PUBLISH PUBSUB RANDOMKEY RENAME RENAMENX RESTORE RPOP RPUSH RPUSHX SADD SCAN SCARD SDIFF SDIFFSTORE SELECT SET SETEX SETNX SETRANGE SINTER SINTERSTORE SISMEMBER SMEMBERS SMISMEMBER SMOVE SPOP SRANDMEMBER SREM SSCAN STRLEN SUBSCRIBE SUNION SUNIONSTORE TIME TOUCH TTL TYPE UNLINK XADD XDEL XLEN XRANGE XREAD XREVRANGE ZADD ZCARD ZCOUNT ZINCRBY ZRANGE ZRANGEBYSCORE ZRANK ZREM ZREVRANGE ZREVRANK ZSCAN ZSCORE ZMSCORE";

#[derive(Clone, Debug)]
pub struct Word {
    pub start: usize,
    pub end: usize,
    pub value: String,
}

// Unlike the execution parser this tolerates the unfinished word at the cursor.
pub fn words(text: &str) -> Vec<Word> {
    let mut chars = text.char_indices().peekable();
    let mut out = Vec::new();
    while let Some((start, ch)) = chars.next() {
        if ch.is_whitespace() {
            continue;
        }
        let mut value = String::new();
        let mut quote = None;
        let mut end = start;
        let mut current = Some((start, ch));
        while let Some((pos, ch)) = current.take().or_else(|| chars.next()) {
            if quote.is_none() && ch.is_whitespace() {
                break;
            }
            end = pos + ch.len_utf8();
            match ch {
                '\\' if quote != Some('\'') => {
                    if let Some((pos, ch)) = chars.next() {
                        end = pos + ch.len_utf8();
                        value.push(ch);
                    }
                }
                '\'' | '"' if quote == Some(ch) => quote = None,
                '\'' | '"' if quote.is_none() => quote = Some(ch),
                _ => value.push(ch),
            }
        }
        out.push(Word { start, end, value });
    }
    out
}

pub struct Context {
    pub words: Vec<Word>,
    pub index: usize,
    pub from: usize,
    pub to: usize,
    pub prefix: String,
}

pub fn context(at: &At<'_>) -> Option<Context> {
    if !matches!(at.name(), Some("action")) || at.body().trim_start().starts_with('[') {
        return None;
    }
    let base = at.line_start(at.body_start());
    let relative = at.pos - base;
    let all = words(at.whole());
    let before = words(at.body());
    let active = before.last().filter(|word| word.end == relative);
    let (index, from, prefix) = match active {
        Some(word) => (before.len() - 1, word.start, word.value.clone()),
        None => (before.len(), relative, String::new()),
    };
    let to = all
        .get(index)
        .filter(|word| word.start == from)
        .map_or(relative, |word| word.end);
    Some(Context {
        words: before,
        index,
        from: base + from,
        to: base + to,
        prefix,
    })
}

pub fn scope(check: &Check, pos: u32) -> Option<Scope> {
    let at = At::new(&check.text, pos as usize);
    if at.in_response() || at.in_placeholder() {
        return None;
    }
    let context = context(&at)?;
    let command = context
        .words
        .first()
        .map(|word| word.value.to_ascii_uppercase())
        .unwrap_or_default();
    if field_position(&command, context.index) {
        return Some(Scope::Hash(context.words.get(1)?.value.clone()));
    }
    if context.index == 0 || context.index == 1 && key_command(&command) {
        return Some(Scope::Keys(if context.index == 1 {
            context.prefix
        } else {
            String::new()
        }));
    }
    None
}

fn options(command: &str) -> &'static [&'static str] {
    match command {
        "SET" => &["EX", "PX", "EXAT", "PXAT", "NX", "XX", "KEEPTTL", "GET"],
        "GETEX" => &["EX", "PX", "EXAT", "PXAT", "PERSIST"],
        "SCAN" | "SSCAN" | "HSCAN" => &["MATCH", "COUNT"],
        "ZSCAN" => &["MATCH", "COUNT"],
        "ZADD" => &["NX", "XX", "GT", "LT", "CH", "INCR"],
        "ZRANGE" => &["BYSCORE", "BYLEX", "REV", "LIMIT", "WITHSCORES"],
        "ZREVRANGE" | "ZRANGEBYSCORE" => &["WITHSCORES", "LIMIT"],
        "EXPIRE" | "PEXPIRE" => &["NX", "XX", "GT", "LT"],
        _ => &[],
    }
}

fn key_kind(command: &str) -> Option<&'static str> {
    match command {
        "GET" | "GETDEL" | "GETEX" | "APPEND" | "INCR" | "INCRBY" | "DECR" | "STRLEN" => {
            Some("string")
        }
        "HGET" | "HGETALL" | "HSET" | "HDEL" | "HSCAN" | "HEXISTS" | "HMGET" | "HINCRBY"
        | "HKEYS" => Some("hash"),
        "LPUSH" | "RPUSH" | "LRANGE" | "LPOP" | "RPOP" | "LLEN" | "LINDEX" | "LREM" => Some("list"),
        "SADD" | "SMEMBERS" | "SREM" | "SCARD" | "SISMEMBER" | "SSCAN" => Some("set"),
        "ZADD" | "ZRANGE" | "ZREVRANGE" | "ZSCORE" | "ZREM" | "ZSCAN" => Some("zset"),
        _ => None,
    }
}

fn field_position(command: &str, index: usize) -> bool {
    match command {
        "HGET" | "HEXISTS" | "HINCRBY" | "HINCRBYFLOAT" => index == 2,
        "HDEL" | "HMGET" => index >= 2,
        "HSET" | "HMSET" => index >= 2 && index.is_multiple_of(2),
        _ => false,
    }
}

fn quoted(value: &str) -> String {
    if value.is_empty()
        || value
            .chars()
            .any(|ch| ch.is_whitespace() || matches!(ch, '\'' | '"' | '\\' | '#' | '$'))
    {
        format!("'{}'", value.replace('\'', "'\\''"))
    } else {
        value.to_owned()
    }
}

pub fn complete(check: &Check, pos: u32, snapshot: Option<&Value>) -> Completions {
    let at = At::new(&check.text, pos as usize);
    if at.in_placeholder() || at.in_response() {
        return at.none();
    }
    if let Some(markers) = at.markers(SECTIONS) {
        return markers;
    }
    match at.name() {
        Some("target") => {
            if let Some(next) = at.next_section(SECTIONS) {
                return next;
            }
            finish(
                at.line_start(at.line),
                at.pos,
                at.before.trim(),
                vec![
                    item("redis://localhost:6380/0", "Valkey endpoint", "url"),
                    item("rediss://", "TLS endpoint", "url"),
                ],
            )
        }
        Some("options") => at.none(),
        Some("metadata") => {
            let used = key_value::used(at.other_lines());
            finish(
                at.line_start(at.line),
                at.pos,
                at.before.trim(),
                key_value::items(&["CA-Certificate: {{VALKEY_CA}}"], &[], &used),
            )
        }
        Some("action") => {
            let Some(context) = context(&at) else {
                return at.none();
            };
            let mut candidates = Vec::new();
            let command = context
                .words
                .first()
                .map(|word| word.value.to_ascii_uppercase())
                .unwrap_or_default();
            if context.index == 0 {
                if let Some(commands) = snapshot
                    .and_then(|snapshot| snapshot["commands"].as_array())
                    .filter(|commands| !commands.is_empty())
                {
                    candidates.extend(commands.iter().filter_map(|command| {
                        let name = command["name"].as_str()?;
                        (!name.contains(' '))
                            .then(|| item(format!("{name} "), "Server command", "keyword"))
                    }));
                } else {
                    candidates.extend(
                        COMMANDS
                            .split_whitespace()
                            .map(|name| item(format!("{name} "), "Valkey command", "keyword")),
                    );
                }
                candidates.retain(|candidate| {
                    config::validate_command(&[candidate.label.trim().into(), "channel".into()])
                        .is_ok()
                });
            } else if context.index == 1 && key_command(&command) {
                if let Some(keys) = snapshot.and_then(|snapshot| snapshot["keys"].as_array()) {
                    candidates.extend(keys.iter().filter_map(|key| {
                        let name = key["name"].as_str()?;
                        let kind = key["type"].as_str().unwrap_or("unknown type");
                        if let Some(expected) = key_kind(&command)
                            && kind != expected
                            && kind != "unknown type"
                            && kind != "none"
                        {
                            return None;
                        }
                        Some(Item {
                            label: name.into(),
                            insert: quoted(name),
                            detail: kind.into(),
                            kind: "property".into(),
                            next: true,
                        })
                    }));
                }
            } else if field_position(&command, context.index) {
                if let Some(fields) = snapshot.and_then(|snapshot| snapshot["fields"].as_array()) {
                    candidates.extend(fields.iter().filter_map(|field| {
                        let name = field.as_str()?;
                        Some(Item {
                            label: name.into(),
                            insert: quoted(name),
                            detail: "Hash field".into(),
                            kind: "property".into(),
                            next: false,
                        })
                    }));
                }
            } else {
                let minimum = match command.as_str() {
                    "SET" => 3,
                    "ZADD" => 2,
                    "ZRANGE" | "ZREVRANGE" | "ZRANGEBYSCORE" => 4,
                    "SCAN" => 2,
                    "HSCAN" | "SSCAN" | "ZSCAN" | "EXPIRE" | "PEXPIRE" => 3,
                    _ => 2,
                };
                let mut used = std::collections::BTreeSet::new();
                let mut values = 0;
                let mut positional = false;
                for word in context.words.iter().take(context.index).skip(minimum) {
                    if values > 0 {
                        values -= 1;
                        continue;
                    }
                    let name = word.value.to_ascii_uppercase();
                    if !options(&command).contains(&name.as_str()) {
                        if command == "ZADD" {
                            positional = true;
                        }
                        continue;
                    }
                    values = match name.as_str() {
                        "LIMIT" => 2,
                        "EX" | "PX" | "EXAT" | "PXAT" | "MATCH" | "COUNT" => 1,
                        _ => 0,
                    };
                    used.insert(name);
                }
                if context.index >= minimum && values == 0 && !positional {
                    candidates.extend(
                        options(&command)
                            .iter()
                            .filter(|option| {
                                let condition_conflict = command == "SET"
                                    && matches!(**option, "NX" | "XX")
                                    && used.iter().any(|name| matches!(name.as_str(), "NX" | "XX"));
                                let expiry_conflict = matches!(command.as_str(), "SET" | "GETEX")
                                    && matches!(
                                        **option,
                                        "EX" | "PX" | "EXAT" | "PXAT" | "KEEPTTL" | "PERSIST"
                                    )
                                    && used.iter().any(|name| {
                                        matches!(
                                            name.as_str(),
                                            "EX" | "PX" | "EXAT" | "PXAT" | "KEEPTTL" | "PERSIST"
                                        )
                                    });
                                !(used.contains(**option) || condition_conflict || expiry_conflict)
                            })
                            .map(|option| item(format!("{option} "), "Command option", "keyword")),
                    );
                }
            }
            finish(context.from, context.to, &context.prefix, candidates)
        }
        _ => at.none(),
    }
}

fn key_command(name: &str) -> bool {
    !matches!(
        name,
        "PING"
            | "ECHO"
            | "AUTH"
            | "SELECT"
            | "INFO"
            | "COMMAND"
            | "DBSIZE"
            | "FLUSHDB"
            | "TIME"
            | "CLIENT"
            | "PUBSUB"
            | "PUBLISH"
            | "SUBSCRIBE"
            | "SCAN"
            | "KEYS"
            | "EVAL"
            | "EVALSHA"
    )
}

pub fn highlight(state: &[u8], lines: &[String]) -> Vec<Line> {
    let mut area = state.first().copied().unwrap_or(0);
    let mut argument = state.get(1).copied().unwrap_or(0);
    let mut quote = state.get(2).copied().filter(|quote| *quote != 0);
    let mut json_mode = state.get(3) == Some(&1);
    let mut json_state = json::State::decode(state.get(4..).unwrap_or_default());
    lines
        .iter()
        .map(|line| {
            let tokens = if let Some((name, _)) = marker(line)
                && (area != 4 || matches!(name.as_str(), "response" | "responce"))
            {
                area = match name.as_str() {
                    "target" => 1,
                    "metadata" | "options" => 2,
                    "action" => 3,
                    "responce" | "response" => 4,
                    _ => 0,
                };
                argument = 0;
                quote = None;
                json_mode = false;
                json_state = json::State::default();
                vec![Token {
                    start: 0,
                    len: line.len() as u32,
                    kind: "comment".into(),
                }]
            } else {
                match area {
                    1 => endpoint::tokens(line, false),
                    2 => key_value::tokens(line),
                    3 => {
                        if argument == 0 && line.trim_start().starts_with('[') {
                            json_mode = true;
                        }
                        if json_mode {
                            json::tokens(line, &mut json_state)
                        } else {
                            command_tokens(line, &mut argument, &mut quote)
                        }
                    }
                    _ => vec![],
                }
            };
            let mut state = vec![area, argument, quote.unwrap_or(0), u8::from(json_mode)];
            json_state.encode(&mut state);
            Line { tokens, state }
        })
        .collect()
}

fn command_tokens(line: &str, argument: &mut u8, quote: &mut Option<u8>) -> Vec<Token> {
    let mut chars = line.char_indices().peekable();
    let mut tokens = Vec::new();
    while let Some(&(start, ch)) = chars.peek() {
        if quote.is_none() && ch.is_whitespace() {
            chars.next();
            continue;
        }
        let continuation = quote.is_some();
        let mut end = start;
        while let Some(&(pos, ch)) = chars.peek() {
            if quote.is_none() && ch.is_whitespace() {
                break;
            }
            chars.next();
            end = pos + ch.len_utf8();
            match ch {
                '\\' if *quote != Some(b'\'') => {
                    if let Some((pos, ch)) = chars.next() {
                        end = pos + ch.len_utf8();
                    }
                }
                '\'' | '"' if *quote == Some(ch as u8) => *quote = None,
                '\'' | '"' if quote.is_none() => *quote = Some(ch as u8),
                _ => {}
            }
        }
        let source = &line[start..end];
        let kind = if continuation || source.starts_with(['\'', '"']) {
            "string"
        } else if *argument == 0 {
            "keyword"
        } else if source.parse::<f64>().is_ok() {
            "number"
        } else if matches!(
            source,
            "EX" | "PX"
                | "EXAT"
                | "PXAT"
                | "NX"
                | "XX"
                | "GET"
                | "MATCH"
                | "COUNT"
                | "WITHSCORES"
                | "KEEPTTL"
        ) {
            "keyword"
        } else if *argument == 1 {
            "property"
        } else {
            "atom"
        };
        if !continuation {
            *argument = argument.saturating_add(1);
        }
        tokens.push(Token {
            start: start as u32,
            len: (end - start) as u32,
            kind: kind.into(),
        });
    }
    tokens
}
