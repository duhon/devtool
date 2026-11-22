//! The jsonpath-like paths of `Columns` (`metadata.name`, `.spec.containers[*].image`,
//! `{.status.phase}`, `metadata.labels['app']`) over a JSON value.

use serde_json::Value;

enum Step {
    Key(String),
    Index(usize),
    All,
}

fn steps(path: &str) -> Result<Vec<Step>, String> {
    let path = path
        .trim()
        .trim_start_matches('{')
        .trim_end_matches('}')
        .trim()
        .trim_start_matches('$');
    let chars: Vec<char> = path.chars().collect();
    let (mut steps, mut at) = (Vec::new(), 0);
    while at < chars.len() {
        match chars[at] {
            '.' => at += 1,
            '[' => {
                let Some(end) = chars[at..].iter().position(|c| *c == ']') else {
                    return Err(format!("Unclosed [ in the column path {path:?}"));
                };
                let inner: String = chars[at + 1..at + end].iter().collect();
                let inner = inner.trim();
                steps.push(if inner == "*" {
                    Step::All
                } else if let Ok(index) = inner.parse() {
                    Step::Index(index)
                } else {
                    Step::Key(inner.trim_matches(['\'', '"']).to_owned())
                });
                at += end + 1;
            }
            _ => {
                let mut key = String::new();
                while at < chars.len() && chars[at] != '.' && chars[at] != '[' {
                    if chars[at] == '\\' && at + 1 < chars.len() {
                        at += 1;
                    }
                    key.push(chars[at]);
                    at += 1;
                }
                steps.push(Step::Key(key));
            }
        }
    }
    Ok(steps)
}

/// The values `path` selects in `value`; none when it selects nothing.
pub fn eval(value: &Value, path: &str) -> Result<Vec<Value>, String> {
    let mut found = vec![value.clone()];
    for step in steps(path)? {
        let mut next = Vec::new();
        for item in found {
            match (&step, &item) {
                (Step::Key(key), Value::Object(map)) => next.extend(map.get(key).cloned()),
                (Step::Index(index), Value::Array(list)) => next.extend(list.get(*index).cloned()),
                (Step::All, Value::Array(list)) => next.extend(list.iter().cloned()),
                (Step::All, Value::Object(map)) => next.extend(map.values().cloned()),
                _ => {}
            }
        }
        found = next;
    }
    Ok(found.into_iter().filter(|value| !value.is_null()).collect())
}

/// A value as a cell prints: strings as they are, anything else as compact JSON.
pub fn cell(value: &Value) -> String {
    match value {
        Value::Null => "<none>".into(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// The selected values as one cell, comma-separated; `<none>` for nothing.
pub fn joined(values: &[Value]) -> String {
    if values.is_empty() {
        "<none>".into()
    } else {
        values.iter().map(cell).collect::<Vec<_>>().join(",")
    }
}
