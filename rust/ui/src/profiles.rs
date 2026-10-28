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
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checked_editor_does_not_silently_drop_invalid_lines() {
        assert_eq!(
            parse_checked("# note\nURL: https://a:443\nEMPTY:").unwrap(),
            parse("# note\nURL: https://a:443\nEMPTY:")
        );
        for text in ["broken", ": value", "X: one\nX: two"] {
            assert!(parse_checked(text).is_err(), "{text}");
        }
    }
    #[test]
    fn comments_and_colons_preserved() {
        let s = "# note\nURL: https://a:443\nX: one";
        assert_eq!(format(&parse(s)), s);
    }
    #[test]
    fn hints_follow_profile_order_and_env() {
        let state = json!({"order":["b","a"],"profiles":{"a":{"X":"a"},"b":{"X":"b","#comment":""}},"env":{"HOME":"/home"}});
        let h = hints(&state);
        assert!(h.contains(&("{{:X}}".into(), "b".into())));
        assert!(!h.iter().any(|(k, _)| k.contains("#")));
        assert!(h.contains(&("{{env:HOME}}".into(), "/home".into())));
    }
    #[test]
    fn reference_resolution_uses_known_values_and_rejects_unresolved_templates() {
        let hints = hints(&json!({
            "order":["var"],
            "profiles":{"var":{"HOST":"fixture.invalid","NESTED":"{{other:HOST}}","EMPTY":""}},
            "env":{"ROOT":"/rest/default"}
        }));
        assert_eq!(
            resolve_text("http://{{var:HOST}}{{env:ROOT}}/{{:HOST}}", &hints).unwrap(),
            "http://fixture.invalid/rest/default/fixture.invalid"
        );
        assert_eq!(resolve_text("{{var:EMPTY}}", &hints).unwrap(), "");
        assert_eq!(resolve_text("literal", &hints).unwrap(), "literal");
        for text in ["{{var:HOST", "{{missing:HOST}}", "{{var:NESTED}}"] {
            assert!(resolve_text(text, &hints).is_err(), "{text}");
        }
    }
    #[test]
    fn hover_matches_the_whole_reference_at_utf16_positions() {
        let state = json!({
            "order": ["default", "second"],
            "profiles": {
                "default": {"TOKEN": "first", "REF": "{{second:TOKEN}}"},
                "second": {"TOKEN": "final"}
            },
            "resolvedProfiles": {
                "default": {"TOKEN": "first", "REF": "final"},
                "second": {"TOKEN": "final"}
            },
            "env": {"PROBE": "companion"}
        });
        let hints = hints(&state);
        let line = "VALUE: \u{1f642} {{second:TOKEN}}{{default:REF}} {{:TOKEN}} {{env:PROBE}}.";
        for (key, expected) in [
            ("{{second:TOKEN}}", "final"),
            ("{{default:REF}}", "final"),
            ("{{:TOKEN}}", "first"),
            ("{{env:PROBE}}", "companion"),
        ] {
            let start = line[..line.find(key).unwrap()].encode_utf16().count();
            let end = start + key.encode_utf16().count();
            for ch in start..end {
                let hover = hover_at(line, ch, &hints).unwrap();
                assert_eq!((hover.start, hover.end), (start, end));
                assert_eq!((hover.key, hover.value), (key, expected));
            }
        }
        assert!(hover_at(line, 0, &hints).is_none());
        assert!(hover_at(line, line.encode_utf16().count() - 1, &hints).is_none());
        assert!(hover_at("{{unknown:TOKEN}}", 5, &hints).is_none());
        assert!(hover_at("{{second:TOKEN", 5, &hints).is_none());
    }
}
