use regex::Regex;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

pub const SERVICES: [&str; 6] = ["mysql", "rest", "graphql", "grpc", "splunk", "terminal"];

pub fn validate_hints(text: &str) -> Result<(), String> {
    for (index, line) in text.lines().enumerate() {
        if line.trim().starts_with("##") {
            return Err(format!(
                "Line {}: ## service headers are managed automatically; use a single # for sections.",
                index + 1
            ));
        }
        if line.contains('\0') {
            return Err(format!(
                "Line {}: NUL characters are not supported.",
                index + 1
            ));
        }
        let mut rest = line;
        while let Some(start) = rest.find("{{") {
            let Some(end) = rest[start + 2..].find("}}") else {
                return Err(format!(
                    "Line {}: incomplete variable reference.",
                    index + 1
                ));
            };
            let reference = &rest[start + 2..start + 2 + end];
            if reference.contains(['{', '}']) || !reference.contains(':') {
                return Err(format!("Line {}: invalid variable reference.", index + 1));
            }
            rest = &rest[start + 2 + end + 2..];
        }
    }
    Ok(())
}

pub fn markers(service: &str) -> &'static [&'static str] {
    services_contracts::templates::sections(service)
        .get(1..)
        .unwrap_or_default()
}

fn header_shaped(line: &str) -> bool {
    let line = line.trim();
    !line.contains("://")
        && line.split_once(':').is_some_and(|(k, _)| {
            k.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
                && k.trim()
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
}

pub fn pairs(text: &str) -> Value {
    let mut out = Map::new();
    for line in text.lines().map(str::trim).filter(|l| !l.starts_with('#')) {
        if let Some((key, value)) = line.split_once(':')
            && !key.trim().is_empty()
        {
            out.insert(key.trim().into(), Value::String(value.trim().into()));
        }
    }
    Value::Object(out)
}

pub fn sections(service: &str, source: &str) -> BTreeMap<String, String> {
    let mut result: BTreeMap<String, String> = BTreeMap::new();
    let mut current = String::from("leftover");
    let mut single = false;
    let mut headers = false;
    for raw in source.lines() {
        let line = raw.trim();
        let marker = line.strip_prefix('#').unwrap_or("").to_ascii_lowercase();
        if markers(service).contains(&marker.as_str()) {
            current = marker;
            single = current == "endpoint" && service != "splunk";
            headers = matches!(current.as_str(), "headers" | "env") && service != "splunk";
            result.entry(current.clone()).or_default();
            continue;
        }
        if single && line.is_empty() {
            continue;
        }
        if headers && (line.is_empty() || !header_shaped(raw)) {
            current = "leftover".into();
            headers = false;
        }
        let entry = result.entry(current.clone()).or_default();
        if !entry.is_empty() {
            entry.push('\n');
        }
        entry.push_str(raw);
        if single {
            current = "leftover".into();
            single = false;
        }
    }
    result.values_mut().for_each(|s| *s = s.trim().to_owned());
    result
}

pub fn parse_sql(source: &str, connection: &str) -> Result<Value, String> {
    let mut sql = source.trim().to_string();
    if sql.is_empty() {
        return Err("SQL query is required".into());
    }

    let end = Regex::new(r"(?i);?\s*\\G\s*;?\s*$").unwrap();
    let vertical = end.is_match(&sql);
    if vertical {
        sql = end.replace(&sql, "").trim().to_owned();
    }
    Ok(json!({"sql":sql,"connectionString":connection,"vertical":vertical}))
}

#[cfg(test)]
mod column_options_tests {
    use super::*;
    #[test]
    fn mysql_and_splunk_options_use_literal_column_names() {
        let mysql=parse("mysql","#endpoint\nmysql://fixture/db\n#options\nColumns-Type: feed_data=json enabled=boolean\n#query\nselect * from fixture\\G").unwrap();
        assert_eq!(mysql["columnsType"], "feed_data=json enabled=boolean");
        assert_eq!(mysql["vertical"], true);
        assert_eq!(mysql["sql"], "select * from fixture");
        let splunk=parse("splunk","#endpoint\nhttps://fixture.invalid\n#options\nColumns-Type: message.timestamp=datetime message.message=json\nColumns-Size: message.message=100\n#search\nindex=fixture").unwrap();
        assert_eq!(
            splunk["columnsType"],
            "message.timestamp=datetime message.message=json"
        );
        assert_eq!(splunk["columnsSize"], "message.message=100");
        assert!(parse("splunk","#endpoint\nhttps://fixture.invalid\n#options\nColumns-Size: message=100px\n#search\nindex=fixture").is_err());
        assert!(
            parse(
                "mysql",
                "#endpoint\nmysql://fixture/db\n#options\nColumns-Type: x=xml\n#query\nselect 1"
            )
            .is_err()
        );
    }
}

pub fn parse(service: &str, source: &str) -> Result<Value, String> {
    if !SERVICES.contains(&service) {
        return Err(format!("Unsupported console service: {service}"));
    }
    if service == "splunk" {
        for line in source.lines() {
            if let Some(marker) = line.trim().strip_prefix('#') {
                let marker = marker
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if ["from", "to", "app", "count", "preview", "offset"].contains(&marker.as_str()) {
                    return Err(format!(
                        "Unsupported Splunk section #{marker}; use #options."
                    ));
                }
            }
        }
    }
    let s = sections(service, source);
    let text = |key: &str| s.get(key).map(String::as_str).unwrap_or("");
    let endpoint = text("endpoint");
    if endpoint.is_empty() {
        return Err(format!("{service} request needs a #endpoint block"));
    }
    let payload = match service {
        "mysql" | "graphql" => "query",
        "grpc" => "message",
        "splunk" => "search",
        "terminal" => "command",
        _ => "body",
    };
    if service != "terminal" && !s.contains_key(payload) && !text("leftover").is_empty() {
        return Err(format!(
            "{service} request needs an explicit #{payload} marker before its body"
        ));
    }
    let mut out = match service {
        "mysql" => {
            if !endpoint.to_ascii_lowercase().starts_with("mysql://") {
                return Err(
                    "MySQL request needs a #endpoint block (a mysql:// connection string)".into(),
                );
            }
            let mut value = parse_sql(text("query"), endpoint)?;
            for (name, option) in pairs(text("options")).as_object().unwrap() {
                if name.eq_ignore_ascii_case("Columns-Type") {
                    value["columnsType"] = option.clone();
                }
            }
            services_contracts::columns::Options::parse(&value)?;
            return Ok(value);
        }
        "rest" => {
            let pieces: Vec<_> = endpoint.split_whitespace().collect();
            if pieces.len() != 2
                || !["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"]
                    .contains(&pieces[0].to_ascii_uppercase().as_str())
            {
                return Err("#endpoint must be METHOD URL".into());
            }
            json!({"method":pieces[0].to_ascii_uppercase(),"url":pieces[1],"headers":pairs(text("headers"))})
        }
        "graphql" => {
            if endpoint.contains(char::is_whitespace) {
                return Err("#endpoint must be a GraphQL URL".into());
            }
            json!({"url":endpoint,"headers":pairs(text("headers"))})
        }
        "grpc" => {
            json!({"endpoint":endpoint,"headers":pairs(text("headers")),"proto":text("proto")})
        }
        "splunk" => {
            if text("search").is_empty() {
                return Err("Splunk request needs a #search block".into());
            }
            let options = pairs(text("options"));
            let options: Map<String, Value> = options
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
                .collect();
            let mut value = json!({"baseUrl":endpoint,"headers":pairs(text("headers")),"search":text("search")});
            for (opt, key) in [
                ("from", "earliestTime"),
                ("to", "latestTime"),
                ("app", "app"),
                ("count", "count"),
                ("preview", "preview"),
                ("offset", "offset"),
                ("columns-type", "columnsType"),
                ("columns-size", "columnsSize"),
            ] {
                if let Some(v) = options
                    .get(opt)
                    .and_then(Value::as_str)
                    .filter(|v| !v.is_empty())
                {
                    value[key] = json!(v);
                }
            }
            services_contracts::columns::Options::parse(&value)?;
            return Ok(value);
        }
        "terminal" => {
            return Ok(
                json!({"endpoint":endpoint,"env":pairs(text("env")),"command":s.get("command").map(String::as_str).unwrap_or(text("leftover"))}),
            );
        }
        _ => unreachable!(),
    };
    if service == "rest" && !text("body").is_empty() {
        out["body"] = json!(text("body"));
    }
    if service == "graphql" {
        if s.contains_key("query") {
            out["query"] = json!(text("query"));
        }
        if !text("variables").is_empty() {
            out["variables"] = serde_json::from_str(text("variables"))
                .map_err(|_| "Variables must be valid JSON")?;
        }
    }
    if service == "grpc" && !text("message").is_empty() {
        out["requestMessage"] = serde_json::from_str(text("message"))
            .map_err(|_| "Request message must be valid JSON")?;
    }
    Ok(out)
}

#[derive(Clone, Debug, PartialEq)]
pub struct RequestBlock {
    pub start: usize,
    pub end: usize,
    pub response_end: usize,
    pub text: String,
}

fn marker(line: &str, name: &str) -> bool {
    let line = line.trim_start().to_ascii_lowercase();
    line.strip_prefix(&format!("#{name}"))
        .is_some_and(|s| s.is_empty() || s.starts_with(char::is_whitespace))
}

pub fn is_check(line: &str) -> bool {
    marker(line, "check") || marker(line, "request")
}

pub fn request_at(source: &str, cursor_line: usize) -> Option<RequestBlock> {
    let lines: Vec<_> = source.lines().collect();
    if lines.is_empty() {
        return None;
    }
    let start = (0..=cursor_line.min(lines.len().saturating_sub(1)))
        .rev()
        .find(|i| is_check(lines[*i]))?;
    let next = ((start + 1)..lines.len())
        .find(|i| is_check(lines[*i]))
        .unwrap_or(lines.len());
    let end = ((start + 1)..next)
        .find(|i| marker(lines[*i], "responce") || marker(lines[*i], "response"))
        .unwrap_or(next);
    Some(RequestBlock {
        start,
        end,
        response_end: next,
        text: lines[start + 1..end].join("\n").trim().into(),
    })
}

pub fn replace_response(source: &str, request_line: usize, status: &str, body: &str) -> String {
    let Some(block) = request_at(source, request_line) else {
        return source.into();
    };
    let lines: Vec<_> = source.lines().collect();
    let before = lines[..block.end].join("\n");
    let after = lines[block.response_end..].join("\n");
    format!(
        "{}\n\n#responce {}\n{}\n{}{}",
        before.trim_end(),
        status,
        body.trim_end(),
        if after.is_empty() { "" } else { "\n" },
        after
    )
}

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

pub fn parse_command(command: &str, grpc: bool) -> Result<Value, String> {
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

fn pretty(s: &str) -> String {
    serde_json::from_str::<Value>(s)
        .ok()
        .and_then(|v| serde_json::to_string_pretty(&v).ok())
        .unwrap_or_else(|| s.into())
}

pub fn import(service: &str, command: &str) -> Result<String, String> {
    if !["rest", "graphql", "grpc"].contains(&service) {
        return Err(format!("Import isn't supported for {service}"));
    }
    let p = parse_command(command, service == "grpc")?;
    let mut lines = vec![
        "#endpoint".into(),
        if service == "rest" {
            format!(
                "{} {}",
                p["method"].as_str().unwrap(),
                p["url"].as_str().unwrap()
            )
        } else {
            p[if service == "grpc" { "endpoint" } else { "url" }]
                .as_str()
                .unwrap()
                .into()
        },
    ];
    if let Some(headers) = p["headers"].as_object().filter(|m| !m.is_empty()) {
        lines.push("#headers".into());
        for (k, v) in headers {
            lines.push(format!("{k}: {}", v.as_str().unwrap_or("")));
        }
    }
    if service == "graphql" {
        let body = p["body"].as_str().unwrap_or("");
        let parsed = serde_json::from_str::<Value>(body).unwrap_or(Value::Null);
        let query = parsed["query"].as_str().unwrap_or(body);
        if query.trim().is_empty() {
            return Err("Could not find a GraphQL query in the curl command".into());
        }
        if let Some(vars) = parsed
            .get("variables")
            .filter(|v| v.as_object().is_some_and(|m| !m.is_empty()))
        {
            lines.extend([
                "#variables".into(),
                serde_json::to_string_pretty(vars).unwrap(),
            ]);
        }
        lines.extend(["#query".into(), query.trim().into()]);
    } else if let Some(body) = p.get("body") {
        let b = body
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| serde_json::to_string_pretty(body).unwrap());
        if !b.is_empty() {
            lines.extend([
                if service == "grpc" {
                    "#message"
                } else {
                    "#body"
                }
                .into(),
                pretty(&b),
            ]);
        }
    }
    Ok(lines.join("\n"))
}

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
pub fn export(service: &str, source: &str) -> Result<String, String> {
    let p = parse(service, source)?;
    let mut lines = Vec::new();
    if service == "grpc" {
        let endpoint = p["endpoint"].as_str().unwrap_or("");
        let secure = endpoint.to_ascii_lowercase().starts_with("grpcs://");
        let clean = endpoint
            .strip_prefix("grpcs://")
            .or_else(|| endpoint.strip_prefix("grpc://"))
            .unwrap_or(endpoint);
        let (target, symbol) = clean
            .split_once('/')
            .ok_or("#endpoint must be target/Service/Method")?;
        lines.push("grpcurl".into());
        if !secure {
            lines.push("  -plaintext".into());
        }
        lines.push("  -emit-defaults".into());
        if let Some(headers) = p["headers"].as_object() {
            for (k, v) in headers {
                lines.push(format!(
                    "  -H {}",
                    quote(&format!("{k}: {}", v.as_str().unwrap_or("")))
                ));
            }
        }
        if let Some(body) = p.get("requestMessage") {
            lines.push(format!(
                "  -d {}",
                quote(&serde_json::to_string_pretty(body).unwrap())
            ));
        }
        lines.extend([format!("  {target}"), format!("  {symbol}")]);
    } else if service == "rest" || service == "graphql" {
        lines.push("curl".into());
        let method = if service == "graphql" {
            "POST"
        } else {
            p["method"].as_str().unwrap_or("GET")
        };
        if method != "GET" {
            lines.push(format!("  --request {method}"));
        }
        lines.push(format!(
            "  --url {}",
            quote(p["url"].as_str().unwrap_or(""))
        ));
        let mut headers = p["headers"].as_object().cloned().unwrap_or_default();
        if service == "graphql"
            && !headers
                .keys()
                .any(|k| k.eq_ignore_ascii_case("content-type"))
        {
            headers.insert("Content-Type".into(), json!("application/json"));
        }
        for (k, v) in headers {
            lines.push(format!(
                "  --header {}",
                quote(&format!("{k}: {}", v.as_str().unwrap_or("")))
            ));
        }
        let body = if service == "graphql" {
            let mut b = json!({});
            if let Some(q) = p.get("query") {
                b["query"] = q.clone();
            }
            if let Some(v) = p.get("variables") {
                b["variables"] = v.clone();
            }
            serde_json::to_string_pretty(&b).unwrap()
        } else {
            p["body"].as_str().unwrap_or("").into()
        };
        if !body.is_empty() {
            lines.push(format!("  --data {}", quote(&body)));
        }
    } else {
        return Err(format!("Export isn't supported for {service}"));
    }
    Ok(lines.join(" \\\n"))
}

pub fn fuzzy_match(candidate: &str, query: &str) -> bool {
    let mut chars = candidate.chars().flat_map(char::to_lowercase);
    query
        .chars()
        .flat_map(char::to_lowercase)
        .all(|q| chars.by_ref().any(|c| c == q))
}

#[cfg(test)]
mod tests {
    #[test]
    fn hints_allow_free_text_but_reject_storage_boundaries_and_broken_references() {
        for text in [
            "#endpoint\nhttps://{{var:host}}/api\n#headers\nX-Token: value",
            "#query\nquery { viewer { name } }",
            "#body\n{\"name\": \"\"}",
            "plain completion",
        ] {
            assert!(super::validate_hints(text).is_ok(), "{text}");
        }
        for text in [
            "## graphql\nleaked",
            "https://{{var:host",
            "{{broken}}",
            "a\0b",
        ] {
            assert!(super::validate_hints(text).is_err(), "{text}");
        }
    }
    use super::*;
    #[test]
    fn markers_are_explicit() {
        for (service, source, required) in [
            (
                "mysql",
                "#endpoint\nmysql://localhost/db\nSELECT 1",
                "#query",
            ),
            (
                "rest",
                "#endpoint\nPOST https://a\n#headers\nX: y\n{\"a\":1}",
                "#body",
            ),
            ("grpc", "#endpoint\ngrpc://a/b/c\n{}", "#message"),
            ("graphql", "#endpoint\nhttps://a\nquery { a }", "#query"),
        ] {
            assert!(parse(service, source).unwrap_err().contains(required));
        }
    }
    #[test]
    fn sql_vertical() {
        for sql in ["SELECT 1 \\G", "SELECT 1; \\G", "SELECT 1 \\g;"] {
            let p = parse_sql(sql, "mysql://db").unwrap();
            assert_eq!(p["sql"], "SELECT 1");
            assert_eq!(p["vertical"], true);
        }
    }
    #[test]
    fn all_service_shapes() {
        assert_eq!(
            parse(
                "rest",
                "#headers\nX: y\n#body\n{}\n#endpoint\nPOST https://a"
            )
            .unwrap(),
            json!({"method":"POST","url":"https://a","headers":{"X":"y"},"body":"{}"})
        );
        assert_eq!(
            parse("terminal", "#endpoint\nbash -c\necho hi").unwrap()["command"],
            "echo hi"
        );
        assert_eq!(
            parse(
                "splunk",
                "#endpoint\nhttps://a\n#options\nFrom: -1h\nCount: 4\n#search\nindex=main"
            )
            .unwrap()["count"],
            "4"
        );
        assert!(parse("graphql", "#endpoint\nhttps://a\n#variables\nbad").is_err());
    }
    #[test]
    fn splunk_rejects_removed_sections_instead_of_sending_them_as_spl() {
        for marker in ["from", "to", "app", "count", "preview", "offset"] {
            for spelling in [
                format!("#{marker}"),
                format!("  #{}  ", marker.to_uppercase()),
                format!("#{marker} @template"),
            ] {
                for options in [
                    "",
                    "#options\nFrom: -24h\nTo: now\nApp: search\nCount: 30\n",
                ] {
                    let source = format!(
                        "#endpoint\nhttps://splunk.fixture\n{options}#search\nindex=main\n{spelling}\nvalue"
                    );
                    assert_eq!(
                        parse("splunk", &source).unwrap_err(),
                        format!("Unsupported Splunk section #{marker}; use #options.")
                    );
                }
            }
        }
    }
    #[test]
    fn splunk_reads_parameters_only_from_options() {
        assert_eq!(
            parse(
                "splunk",
                "#endpoint\nhttps://splunk.fixture\n#options\nFrom: -24h\nTo: -5m\nApp: search\nCount: 30\nPreview: true\nOffset: 10\n#search\nindex=main"
            )
            .unwrap(),
            json!({
                "baseUrl": "https://splunk.fixture", "headers": {}, "search": "index=main",
                "earliestTime": "-24h", "latestTime": "-5m", "app": "search", "count": "30",
                "preview":"true","offset":"10"
            })
        );
        assert_eq!(
            parse(
                "splunk",
                "#endpoint\nhttps://splunk.fixture\n#search\nindex=main"
            )
            .unwrap(),
            json!({"baseUrl": "https://splunk.fixture", "headers": {}, "search": "index=main"})
        );
    }
    #[test]
    fn import_export_roundtrip() {
        for (service, command) in [
            (
                "rest",
                "curl -X POST 'https://a' -H 'X: y' -d '{\"a\":\"it is\"}'",
            ),
            (
                "graphql",
                "curl https://a -H 'Content-Type: application/json' -d '{\"query\":\"query { a }\",\"variables\":{\"x\":1}}'",
            ),
            (
                "grpc",
                "grpcurl -plaintext -H 'x: y' -d '{\"a\":1}' localhost:50051 demo.Service.Method",
            ),
        ] {
            let block = import(service, command).unwrap();
            let exported = export(service, &block).unwrap();
            let second = import(service, &exported).unwrap();
            assert_eq!(
                parse(service, &block).unwrap(),
                parse(service, &second).unwrap()
            );
        }
    }
    #[test]
    fn tls_and_quotes() {
        assert!(
            import("grpc", "grpcurl a:443 demo.S/M")
                .unwrap()
                .contains("grpcs://")
        );
        let s = "it's \"quoted\"";
        assert_eq!(shell_words(&quote(s)).unwrap(), vec![s]);
        assert!(shell_words("'bad").is_err());
    }
    #[test]
    fn response_replaced_not_stacked() {
        let s = "#check\n#endpoint\nGET https://a\n\n#responce old\nold\n\n#check\n#endpoint\nGET https://b";
        let n = replace_response(s, 2, "OK", "new");
        assert_eq!(n.matches("#responce").count(), 1);
        assert!(n.contains("new"));
        assert!(n.contains("https://b"));
        assert!(!n.contains("old"));
    }
    #[test]
    fn subsequence_completion() {
        assert!(fuzzy_match("Content-Type", "cty"));
        assert!(!fuzzy_match("Content-Type", "xyz"));
    }
}
