//! `curl` command lines: import into a request and export from one.

use serde_json::Value;

/// A request parsed from a `curl` command line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Command {
    pub method: String,
    pub url: String,
    /// In order; a repeated name keeps its first place and its last value.
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

/// Shell words of a command line: quotes, backslash escapes and line continuations.
pub fn shell_words(source: &str) -> Result<Vec<String>, String> {
    let normalized = source.replace("\\\r\n", " ").replace("\\\n", " ");
    let mut chars = normalized.chars().peekable();
    let mut words = Vec::new();
    while chars.peek().is_some() {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        if chars.peek().is_none() {
            break;
        }
        let mut word = String::new();
        while let Some(c) = chars.next() {
            if c.is_whitespace() {
                break;
            }
            if c == '\'' || c == '"' {
                let quote = c;
                let mut closed = false;
                while let Some(q) = chars.next() {
                    if q == quote {
                        closed = true;
                        break;
                    }
                    if quote == '"'
                        && q == '\\'
                        && let Some(n) = chars.next_if(|n| *n == '"' || *n == '\\')
                    {
                        word.push(n);
                    } else {
                        word.push(q);
                    }
                }
                if !closed {
                    return Err(format!(
                        "Unterminated {} quote",
                        if quote == '\'' { "single" } else { "double" }
                    ));
                }
            } else if c == '\\' {
                if let Some(n) = chars.next() {
                    word.push(n);
                }
            } else {
                word.push(c);
            }
            if chars.next_if(|c| c.is_whitespace()).is_some() {
                break;
            }
        }
        words.push(word);
    }
    Ok(words)
}

/// A `curl` command line; data implies `POST`, `-G` implies `GET`.
pub fn parse(command: &str) -> Result<Command, String> {
    let tokens = shell_words(command)?;
    let executable = tokens
        .first()
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    if executable != "curl" && executable != "curl.exe" {
        return Err("Command must start with \"curl\"".into());
    }
    let mut headers: Vec<(String, String)> = Vec::new();
    let mut header = |name: &str, value: &str| match headers.iter_mut().find(|(k, _)| k == name) {
        Some(pair) => pair.1 = value.into(),
        None => headers.push((name.into(), value.into())),
    };
    let mut method = None;
    let mut url = None;
    let mut data: Option<String> = None;
    let mut i = 1;
    while i < tokens.len() {
        let t = &tokens[i];
        let lower = t.to_ascii_lowercase();
        let val = tokens.get(i + 1).cloned().unwrap_or_default();
        match lower.as_str() {
            "-h" | "--header" => {
                if let Some((k, v)) = val.split_once(':') {
                    header(k.trim(), v.trim());
                }
                i += 1;
            }
            "-x" | "--request" => {
                method = Some(val.to_ascii_uppercase());
                i += 1;
            }
            "--url" => {
                url = Some(val);
                i += 1;
            }
            "-d" | "--data" | "--data-raw" | "--data-binary" | "--data-ascii"
            | "--data-urlencode" => {
                data = Some(data.map(|d| format!("{d}&{val}")).unwrap_or(val));
                i += 1;
            }
            "-a" | "--user-agent" | "-e" | "--referer" | "-b" | "--cookie" => {
                let name = match lower.as_str() {
                    "-a" | "--user-agent" => "User-Agent",
                    "-e" | "--referer" => "Referer",
                    _ => "Cookie",
                };
                header(name, &val);
                i += 1;
            }
            "-g" | "--get" => {
                method.get_or_insert_with(|| "GET".into());
            }
            "-k" | "--insecure" | "-s" | "--silent" | "-i" | "--include" | "-v" | "--verbose"
            | "-l" | "--location" | "--compressed" | "--globoff" | "-n" | "--netrc" | "-f"
            | "--fail" | "-#" | "--progress-bar" | "-ss" => {}
            _ if t.starts_with('-') => {
                if !val.is_empty() && !val.starts_with('-') {
                    i += 1;
                }
            }
            _ => url = Some(t.clone()),
        }
        i += 1;
    }
    let url = url.ok_or("Could not find a URL in the curl command")?;
    Ok(Command {
        method: method.unwrap_or_else(|| if data.is_some() { "POST" } else { "GET" }.into()),
        url,
        headers,
        body: data,
    })
}

/// The headers as the text of an imported `#metadata` section; `None` without headers.
pub fn imported_headers(command: &Command) -> Option<String> {
    (!command.headers.is_empty()).then(|| {
        command
            .headers
            .iter()
            .map(|(k, v)| format!("{k}: {v}"))
            .collect::<Vec<_>>()
            .join("\n")
    })
}

/// The body as the text of an imported section, pretty-printed when JSON; `None` without one.
pub fn imported_body(command: &Command) -> Option<String> {
    command
        .body
        .as_deref()
        .filter(|body| !body.is_empty())
        .map(pretty)
}

/// JSON text pretty-printed with a 2-space indent; anything else unchanged.
pub fn pretty(text: &str) -> String {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|v| serde_json::to_string_pretty(&v).ok())
        .unwrap_or_else(|| text.into())
}

/// A single-quoted shell word.
pub fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// A `curl` command line, one option per line; `GET` is implied.
pub fn export(method: &str, url: &str, headers: &[(String, String)], body: &str) -> String {
    let mut lines = vec!["curl".to_owned()];
    if method != "GET" {
        lines.push(format!("  --request {method}"));
    }
    lines.push(format!("  --url {}", quote(url)));
    for (k, v) in headers {
        lines.push(format!("  --header {}", quote(&format!("{k}: {v}"))));
    }
    if !body.is_empty() {
        lines.push(format!("  --data {}", quote(body)));
    }
    lines.join(" \\\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(k: &str, v: &str) -> (String, String) {
        (k.into(), v.into())
    }

    #[test]
    fn export_quotes_every_word_and_implies_get() {
        let headers = [pair("Accept", "*/*"), pair("X", "it's")];
        assert_eq!(
            export("POST", "https://x/a b", &headers, "{}"),
            "curl \\\n  --request POST \\\n  --url 'https://x/a b' \\\n  --header 'Accept: */*' \\\n  --header 'X: it'\\''s' \\\n  --data '{}'"
        );
        assert_eq!(export("GET", "u", &[], ""), "curl \\\n  --url 'u'");
    }

    #[test]
    fn parse_reads_headers_data_and_method() {
        let command = parse(r#"curl https://x -H 'A: 1' -H 'B: 2' --data '{"a":1}'"#).unwrap();
        assert_eq!(
            command,
            Command {
                method: "POST".into(),
                url: "https://x".into(),
                headers: vec![pair("A", "1"), pair("B", "2")],
                body: Some(r#"{"a":1}"#.into()),
            }
        );
        assert_eq!(imported_headers(&command).unwrap(), "A: 1\nB: 2");
        assert_eq!(imported_body(&command).unwrap(), "{\n  \"a\": 1\n}");
        assert_eq!(
            parse("wget https://x"),
            Err("Command must start with \"curl\"".into())
        );
    }

    #[test]
    fn parse_understands_shell_and_curl_options() {
        let command = parse(
            "curl.exe -X put --url \"https://x/\\\"q\\\"\" \\\n -A agent -e ref -b c=1 -H 'A: 1' -H 'A: 2' -d a=1 --data-raw b=2 -k -o out.txt",
        )
        .unwrap();
        assert_eq!(command.method, "PUT");
        assert_eq!(command.url, "https://x/\"q\"");
        assert_eq!(
            command.headers,
            [
                pair("User-Agent", "agent"),
                pair("Referer", "ref"),
                pair("Cookie", "c=1"),
                pair("A", "2")
            ]
        );
        assert_eq!(command.body.as_deref(), Some("a=1&b=2"));
        assert_eq!(parse("curl -G https://x -d q=1").unwrap().method, "GET");
        assert_eq!(
            parse("curl -H"),
            Err("Could not find a URL in the curl command".into())
        );
        assert_eq!(
            parse("curl 'https://x"),
            Err("Unterminated single quote".into())
        );
        assert_eq!(shell_words("a\\ b 'c d'\"e\"").unwrap(), ["a b", "c de"]);
    }

    #[test]
    fn export_and_parse_round_trip() {
        let headers = [pair("Content-Type", "application/json"), pair("X", "it's")];
        let body = "{\n  \"name\": \"it's\"\n}";
        let command = parse(&export("PATCH", "https://x/a?b=1&c", &headers, body)).unwrap();
        assert_eq!(
            command,
            Command {
                method: "PATCH".into(),
                url: "https://x/a?b=1&c".into(),
                headers: headers.to_vec(),
                body: Some(body.into()),
            }
        );
    }
}
