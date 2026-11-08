//! `{{set:name}}` variable references: `set` is `[A-Za-z0-9_-]*` (empty for `{{:name}}`),
//! `name` is `[A-Za-z0-9_-]+`.

fn word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// The reference at the start of `tail` (just after `{{`): its set, name and length up to and
/// including `}}`.
fn at(tail: &str) -> Option<(&str, &str, usize)> {
    let end = tail.find("}}")?;
    let (set, name) = tail[..end].split_once(':')?;
    (set.chars().all(word) && !name.is_empty() && name.chars().all(word)).then_some((
        set,
        name,
        end + 2,
    ))
}

/// Replaces each reference of `text` for which `resolve` gives `Some`; the others stay.
pub fn replace(
    text: &str,
    mut resolve: impl FnMut(&str, &str) -> Result<Option<String>, String>,
) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let tail = &rest[start + 2..];
        match at(tail) {
            Some((set, name, length)) => {
                match resolve(set, name)? {
                    Some(value) => out.push_str(&value),
                    None => out.push_str(&rest[start..start + 2 + length]),
                }
                rest = &tail[length..];
            }
            None => {
                out.push_str("{{");
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    Ok(out)
}

/// Every reference of `text` as `(set, name)`, in order.
pub fn find(text: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let _ = replace(text, |set, name| {
        found.push((set.to_owned(), name.to_owned()));
        Ok(None)
    });
    found
}

/// `value` as the content of a JSON string.
pub fn escape_json(value: &str) -> String {
    let quoted = serde_json::Value::String(value.into()).to_string();
    quoted[1..quoted.len() - 1].to_owned()
}

/// Names whose values never show in History: they are masked wherever they appear.
pub fn sensitive(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [
        "password",
        "passwd",
        "pwd",
        "token",
        "secret",
        "credential",
        "api_key",
        "apikey",
        "api-key",
        "authorization",
        "cookie",
        "private_key",
    ]
    .iter()
    .any(|part| name.contains(part))
}

/// Variables written one per line as `NAME: value`; `#` lines are comments, kept in place.
pub fn parse_lines(text: &str) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let mut vars = serde_json::Map::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('#') {
            vars.insert(line.into(), "".into());
            continue;
        }
        let Some((name, value)) = line
            .split_once(':')
            .filter(|(name, _)| !name.trim().is_empty())
        else {
            return Err(format!("Line {}: expected NAME: value.", index + 1));
        };
        let name = name.trim();
        if vars.contains_key(name) {
            return Err(format!("Line {}: duplicate variable {name}.", index + 1));
        }
        vars.insert(name.into(), value.trim().into());
    }
    Ok(vars)
}

/// `vars` as [`parse_lines`] reads them.
pub fn format_lines(vars: &serde_json::Map<String, serde_json::Value>) -> String {
    vars.iter()
        .map(|(name, value)| {
            if name.starts_with('#') {
                name.clone()
            } else {
                format!(
                    "{name}: {}",
                    value
                        .as_str()
                        .map_or_else(|| value.to_string(), str::to_owned)
                )
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The value of a variable as text.
pub fn text(value: &serde_json::Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_only_what_resolves() {
        let text = "a {{:x}} {{dev:y}} {{bad}} {{env:Z}}";
        let out = replace(text, |set, name| {
            Ok((set == "env").then(|| format!("<{name}>")))
        })
        .unwrap();
        assert_eq!(out, "a {{:x}} {{dev:y}} {{bad}} <Z>");
        assert_eq!(find(text).len(), 3);
    }

    #[test]
    fn lines_round_trip() {
        let vars = parse_lines("# note\nA: 1\n\nB: x: y").unwrap();
        assert_eq!(format_lines(&vars), "# note\nA: 1\nB: x: y");
        assert_eq!(
            parse_lines("A: 1\nmissing delimiter").unwrap_err(),
            "Line 2: expected NAME: value."
        );
        assert!(parse_lines("A: 1\nA: 2").unwrap_err().contains("duplicate"));
    }
}
