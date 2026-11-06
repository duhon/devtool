//! Command lines a module imports checks from: shell words and the `curl` / `grpcurl`
//! options the built-in services understand.

use serde_json::{Map, Value, json};

/// A `curl` command line as `{"method","url","headers","body"?}`; data implies `POST`.
pub fn parse_curl(command: &str) -> Result<Value, String> {
    parse(command, false)
}

/// A `grpcurl` command line as `{"endpoint":"grpc[s]://target/Service/Method","headers","body"?}`.
pub fn parse_grpcurl(command: &str) -> Result<Value, String> {
    parse(command, true)
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
                        && chars.peek().is_some_and(|n| *n == '"' || *n == '\\')
                    {
                        word.push(chars.next().unwrap());
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
            if chars.peek().is_some_and(|c| c.is_whitespace()) {
                chars.next();
                break;
            }
        }
        words.push(word);
    }
    Ok(words)
}

fn parse(command: &str, grpc: bool) -> Result<Value, String> {
    let tokens = shell_words(command)?;
    let executable = tokens
        .first()
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    if !(if grpc {
        executable == "grpcurl"
    } else {
        executable == "curl" || executable == "curl.exe"
    }) {
        return Err(format!(
            "Command must start with \"{}\"",
            if grpc { "grpcurl" } else { "curl" }
        ));
    }
    let mut headers = Map::new();
    let mut method = None;
    let mut url = None;
    let mut data: Option<String> = None;
    let mut positional = Vec::new();
    let mut plaintext = false;
    let mut i = 1;
    while i < tokens.len() {
        let t = &tokens[i];
        let lower = t.to_ascii_lowercase();
        let val = tokens.get(i + 1).cloned().unwrap_or_default();
        match lower.as_str() {
            "-h" | "--header" | "-header" | "-rpc-header" | "-metadata" => {
                if let Some((k, v)) = val.split_once(':') {
                    headers.insert(k.trim().into(), json!(v.trim()));
                }
                i += 1;
            }
            "-x" | "--request" if !grpc => {
                method = Some(val.to_ascii_uppercase());
                i += 1;
            }
            "--url" if !grpc => {
                url = Some(val);
                i += 1;
            }
            "-d" | "--data" | "--data-raw" | "--data-binary" | "--data-ascii"
            | "--data-urlencode" | "-data" => {
                data = Some(if grpc {
                    val
                } else {
                    data.map(|d| format!("{d}&{val}")).unwrap_or(val)
                });
                i += 1;
            }
            "-a" | "--user-agent" | "-e" | "--referer" | "-b" | "--cookie" if !grpc => {
                headers.insert(
                    match lower.as_str() {
                        "-a" | "--user-agent" => "User-Agent",
                        "-e" | "--referer" => "Referer",
                        _ => "Cookie",
                    }
                    .into(),
                    json!(val),
                );
                i += 1;
            }
            "-g" | "--get" if !grpc => {
                if method.is_none() {
                    method = Some("GET".into())
                }
            }
            "-plaintext" if grpc => plaintext = true,
            "-k"
            | "--insecure"
            | "-s"
            | "--silent"
            | "-i"
            | "--include"
            | "-v"
            | "--verbose"
            | "-l"
            | "--location"
            | "--compressed"
            | "--globoff"
            | "-n"
            | "--netrc"
            | "-f"
            | "--fail"
            | "-#"
            | "--progress-bar"
            | "-ss"
            | "-insecure"
            | "-verbose"
            | "-format-error"
            | "-emit-defaults"
            | "-unix"
            | "-keepalive-time"
            | "-allow-unknown-fields"
            | "-vv" => {}
            _ if t.starts_with('-') => {
                if !val.is_empty() && !val.starts_with('-') {
                    i += 1;
                }
            }
            _ if grpc => positional.push(t.clone()),
            _ => url = Some(t.clone()),
        }
        i += 1;
    }
    if grpc {
        if positional.len() < 2 {
            return Err(
                "Could not find both a target and a Service/Method in the grpcurl command".into(),
            );
        }
        let mut symbol = positional.pop().unwrap();
        if !symbol.contains('/')
            && let Some(dot) = symbol.rfind('.')
        {
            symbol.replace_range(dot..=dot, "/");
        }
        let mut result = json!({"endpoint":format!("{}://{}/{symbol}",if plaintext{"grpc"}else{"grpcs"},positional.pop().unwrap()),"headers":headers});
        if let Some(d) = data.filter(|d| d != "@") {
            result["body"] = serde_json::from_str(&d).unwrap_or(json!(d));
        }
        Ok(result)
    } else {
        let url = url.ok_or("Could not find a URL in the curl command")?;
        let mut result = json!({"method":method.unwrap_or(if data.is_some(){"POST"}else{"GET"}.into()),"url":url,"headers":headers});
        if let Some(d) = data {
            result["body"] = json!(d);
        }
        Ok(result)
    }
}
