//! Log lines as objects: logfmt `key=value` pairs, Java-style `{a=1, b=[2]}` maps and
//! embedded JSON, so a raw event can be read as a tree.

use serde_json::{Map, Value, json};

/// The value after a `"$java":` wrapper key (any case, quotes optional).
fn java_wrapper(inner: &str) -> Option<&str> {
    let rest = inner.strip_prefix('"').unwrap_or(inner);
    let head = rest.get(..5)?;
    if !head.eq_ignore_ascii_case("$java") {
        return None;
    }
    let rest = rest[5..].strip_prefix('"').unwrap_or(&rest[5..]);
    Some(rest.trim_start().strip_prefix(':')?.trim_start())
}

/// The `key=value` pairs of a text, left to right: their byte range, key and value (up to
/// whitespace, maybe empty). A key starts with a letter or `_`, then letters, digits, `_`,
/// `.` and `-`.
fn assignments(text: &str) -> Vec<(usize, usize, &str, &str)> {
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(c) = text[at..].chars().next() {
        if c.is_ascii_alphabetic() || c == '_' {
            let name_end = text[at..]
                .find(|c: char| !(c.is_alphanumeric() || "_.-".contains(c)))
                .map_or(text.len(), |n| at + n);
            if text[name_end..].starts_with('=') {
                let value_start = name_end + 1;
                let end = text[value_start..]
                    .find(char::is_whitespace)
                    .map_or(text.len(), |n| value_start + n);
                found.push((at, end, &text[at..name_end], &text[value_start..end]));
                at = end;
                continue;
            }
        }
        at += c.len_utf8();
    }
    found
}

fn separators(text: &str, wanted: char) -> Vec<usize> {
    let (mut depth, mut quoted, mut escaped) = (0i32, false, false);
    let mut positions = Vec::new();
    for (i, c) in text.char_indices() {
        if quoted {
            if escaped {
                escaped = false
            } else if c == '\\' {
                escaped = true
            } else if c == '"' {
                quoted = false
            }
            continue;
        }
        match c {
            '"' => quoted = true,
            '{' | '[' => depth += 1,
            '}' | ']' => depth -= 1,
            _ if c == wanted && depth == 0 => positions.push(i),
            _ => {}
        }
    }
    positions
}
fn split(text: &str) -> Vec<&str> {
    let mut start = 0;
    let mut parts = Vec::new();
    for i in separators(text, ',') {
        parts.push(&text[start..i]);
        start = i + 1;
    }
    parts.push(&text[start..]);
    parts
}
fn scalar(text: &str) -> Value {
    let text = text.trim();
    if let Ok(value) = serde_json::from_str(text) {
        return value;
    }
    if text.starts_with('{') && text.ends_with('}') {
        let inner = text[1..text.len() - 1].trim();
        if let Some(value) = java_wrapper(inner) {
            return json!(value.trim());
        }
        let mut result = Map::new();
        for part in split(inner) {
            let sep = separators(part, '=')
                .into_iter()
                .chain(separators(part, ':'))
                .min();
            if let Some(i) = sep {
                let key = part[..i].trim().trim_matches('"');
                if !key.is_empty() {
                    result.insert(key.into(), scalar(&part[i + 1..]));
                }
            }
        }
        return Value::Object(result);
    }
    if text.starts_with('[') && text.ends_with(']') {
        return Value::Array(
            split(&text[1..text.len() - 1])
                .into_iter()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(scalar)
                .collect(),
        );
    }
    json!(text)
}
fn matching(text: &str, start: usize) -> Option<usize> {
    let open = text[start..].chars().next()?;
    let close = if open == '{' { '}' } else { ']' };
    let (mut depth, mut quoted, mut escaped) = (0i32, false, false);
    for (i, c) in text[start..].char_indices() {
        if quoted {
            if escaped {
                escaped = false
            } else if c == '\\' {
                escaped = true
            } else if c == '"' {
                quoted = false
            }
            continue;
        }
        if c == '"' {
            quoted = true
        } else if c == open {
            depth += 1
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                return Some(start + i + c.len_utf8());
            }
        }
    }
    None
}
/// A log line as an object; `None` for plain text (at most one pair and no map).
pub fn parse(text: &str) -> Option<Value> {
    let mut spans = Vec::new();
    let mut scan = 0;
    while scan < text.len() {
        if let Some((relative, _)) = text[scan..]
            .char_indices()
            .find(|(_, c)| *c == '{' || *c == '[')
        {
            let start = scan + relative;
            let end = matching(text, start)?;
            spans.push((start, end));
            scan = end;
        } else {
            break;
        }
    }
    let (mut cursor, mut pairs, mut contexts) = (0, 0, 0);
    let mut first = None;
    let mut result = Map::new();
    for span in spans.iter().copied().map(Some).chain(std::iter::once(None)) {
        let end = span.map(|s| s.0).unwrap_or(text.len());
        let gap = &text[cursor..end];
        let mut last_key = None;
        let mut last_end = 0;
        for (start, end, name, value) in assignments(gap) {
            first.get_or_insert(cursor + start);
            pairs += 1;
            last_end = end;
            if value.is_empty() {
                last_key = Some(name.to_owned());
            } else {
                result.insert(name.into(), scalar(value));
                last_key = None;
            }
        }
        if let Some((start, end)) = span {
            let value = scalar(&text[start..end]);
            if let Some(key) = last_key
                && gap[last_end..].trim().is_empty()
            {
                result.insert(key, value);
            } else {
                contexts += 1;
                result.insert(
                    if contexts == 1 {
                        "context".into()
                    } else {
                        format!("context{contexts}")
                    },
                    value,
                );
            }
            cursor = end;
        }
    }
    if pairs == 0 || (pairs == 1 && spans.is_empty()) {
        return None;
    }
    let prefix = text[..first.unwrap_or(0)].trim();
    if !prefix.is_empty() {
        let mut prefixed = Map::new();
        prefixed.insert("text".into(), json!(prefix));
        prefixed.extend(result);
        return Some(Value::Object(prefixed));
    }
    Some(Value::Object(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_left_alone_and_key_values_are_typed() {
        assert!(parse("ordinary text a=b").is_none());
        assert_eq!(
            parse("INFO id=3 ok=true").unwrap(),
            json!({"text":"INFO","id":3,"ok":true})
        );
    }

    #[test]
    fn java_style_maps_and_json_values_become_nested_objects() {
        assert_eq!(
            parse("context={storeViewCode=default,categoryId=[2,3],nested={a: 1}}").unwrap(),
            json!({"context":{"storeViewCode":"default","categoryId":[2,3],"nested":{"a":1}}})
        );
        assert_eq!(
            parse("a=1 metadata={\"x\":\"y\"}").unwrap()["metadata"],
            json!({"x":"y"})
        );
    }

    #[test]
    fn anonymous_maps_become_context_and_unbalanced_maps_are_rejected() {
        let parsed = parse("id=1 type=event {time={\"$java\":2026-09-26T04:12:36Z}}").unwrap();
        assert_eq!(parsed["context"]["time"], "2026-09-26T04:12:36Z");
        assert!(parse("a={broken").is_none());
    }
}
