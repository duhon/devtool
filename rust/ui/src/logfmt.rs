use serde_json::{Map, Value, json};

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
        let wrapper = regex::Regex::new(r#"(?i)^"?\$java"?\s*:\s*"#).unwrap();
        if let Some(m) = wrapper.find(inner) {
            return json!(inner[m.end()..].trim());
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
    let key = regex::Regex::new(r"([A-Za-z_][\w.-]*)=(\S*)").unwrap();
    let (mut cursor, mut pairs, mut contexts) = (0, 0, 0);
    let mut first = None;
    let mut result = Map::new();
    for span in spans.iter().copied().map(Some).chain(std::iter::once(None)) {
        let end = span.map(|s| s.0).unwrap_or(text.len());
        let gap = &text[cursor..end];
        let mut last_key = None;
        let mut last_end = 0;
        for capture in key.captures_iter(gap) {
            let m = capture.get(0).unwrap();
            first.get_or_insert(cursor + m.start());
            pairs += 1;
            last_end = m.end();
            if capture[2].is_empty() {
                last_key = Some(capture[1].to_owned());
            } else {
                result.insert(capture[1].into(), scalar(&capture[2]));
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
