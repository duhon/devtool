use serde_json::{Map, Value, json};
pub fn service(name: &str) -> &'static str {
    if name == "var" || name == "env" {
        "vars"
    } else {
        "dictionary"
    }
}
pub fn parse_checked(text: &str) -> Result<Value, String> {
    let mut names = std::collections::BTreeSet::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, _)) = line
            .split_once(':')
            .filter(|(name, _)| !name.trim().is_empty())
        else {
            return Err(format!("Line {}: expected NAME: value.", index + 1));
        };
        if !names.insert(name.trim()) {
            return Err(format!(
                "Line {}: duplicate variable {}.",
                index + 1,
                name.trim()
            ));
        }
    }
    Ok(parse(text))
}
pub fn parse(text: &str) -> Value {
    let mut vars = Map::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if line.starts_with('#') {
            vars.insert(line.into(), json!(""));
            continue;
        }
        if let Some((k, v)) = line.split_once(':')
            && !k.trim().is_empty()
        {
            vars.insert(k.trim().into(), json!(v.trim()));
        }
    }
    Value::Object(vars)
}
pub fn format(vars: &Value) -> String {
    vars.as_object()
        .map(|m| {
            m.iter()
                .map(|(k, v)| {
                    if k.starts_with('#') {
                        k.clone()
                    } else {
                        format!(
                            "{k}: {}",
                            v.as_str()
                                .map(str::to_owned)
                                .unwrap_or_else(|| v.to_string())
                        )
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}
pub fn hints(state: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let order = state["order"].as_array().cloned().unwrap_or_default();
    for name in order
        .iter()
        .filter_map(Value::as_str)
        .chain(std::iter::once("env"))
    {
        let vars = if name == "env" {
            state.get("env").unwrap_or(&state["profiles"]["env"])
        } else {
            state["resolvedProfiles"]
                .get(name)
                .unwrap_or(&state["profiles"][name])
        };
        if let Some(vars) = vars.as_object() {
            for (k, v) in vars.iter().filter(|(k, _)| !k.starts_with('#')) {
                let val = v
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| v.to_string());
                out.push((format!("{{{{{name}:{k}}}}}"), val.clone()));
                let bare = format!("{{{{:{k}}}}}");
                if !out.iter().any(|(k, _)| k == &bare) {
                    out.push((bare, val));
                }
            }
        }
    }
    out
}
pub fn resolve_text(text: &str, hints: &[(String, String)]) -> Result<String, String> {
    let mut output = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        output.push_str(&rest[..start]);
        let end = rest[start + 2..]
            .find("}}")
            .map(|end| start + 2 + end + 2)
            .ok_or("Incomplete variable reference")?;
        let reference = &rest[start..end];
        let replacement = hints
            .iter()
            .find(|(name, _)| name == reference)
            .map(|(_, value)| value)
            .filter(|value| !value.contains("{{"))
            .ok_or_else(|| format!("Unresolved variable reference: {reference}"))?;
        output.push_str(replacement);
        rest = &rest[end..];
    }
    output.push_str(rest);
    Ok(output)
}
pub struct VariableHover<'a> {
    pub start: usize,
    pub end: usize,
    pub key: &'a str,
    pub value: &'a str,
}
pub fn hover_at<'a>(
    line: &str,
    ch: usize,
    hints: &'a [(String, String)],
) -> Option<VariableHover<'a>> {
    line.match_indices("{{").find_map(|(start, _)| {
        let end = start + 2 + line[start + 2..].find("}}")? + 2;
        let from = line[..start].encode_utf16().count();
        let to = line[..end].encode_utf16().count();
        if !(from..to).contains(&ch) {
            return None;
        }
        hints
            .iter()
            .find(|(key, _)| key == &line[start..end])
            .map(|(key, value)| VariableHover {
                start: from,
                end: to,
                key,
                value,
            })
    })
}
