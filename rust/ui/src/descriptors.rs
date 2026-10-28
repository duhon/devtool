use crate::console::pairs;
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug)]
pub struct Field {
    pub key: &'static str,
    pub label: &'static str,
    pub control: &'static str,
    pub mode: &'static str,
    pub slot: &'static str,
    pub default: &'static str,
}
const fn editor(
    key: &'static str,
    label: &'static str,
    mode: &'static str,
    slot: &'static str,
) -> Field {
    Field {
        key,
        label,
        control: "editor",
        mode,
        slot,
        default: "",
    }
}
const fn option(
    key: &'static str,
    label: &'static str,
    control: &'static str,
    default: &'static str,
) -> Field {
    Field {
        key,
        label,
        control,
        mode: "",
        slot: "options",
        default,
    }
}
const INTERVAL: Field = option("intervalSeconds", "Auto-refresh, sec:", "number", "0");
const MYSQL: &[Field] = &[
    editor("dsn", "Endpoint", "endpoint", "endpoint"),
    editor("sql", "Query", "sql", "section"),
    INTERVAL,
];
const REST: &[Field] = &[
    option("method", "Method", "select", "GET"),
    editor("url", "Endpoint", "endpoint", "endpoint"),
    editor("paramsText", "Params", "headers", "request"),
    editor("headersText", "Headers", "headers", "request"),
    editor("body", "Body", "json", "section"),
    INTERVAL,
];
const GRAPHQL: &[Field] = &[
    editor("url", "Endpoint", "endpoint", "endpoint"),
    editor("headersText", "Headers", "headers", "request"),
    INTERVAL,
    editor("query", "Query", "graphql", "operation"),
    editor("variables", "Variables", "json", "operation"),
];
const GRPC: &[Field] = &[
    editor("endpointText", "Endpoint", "endpoint", "endpoint"),
    editor("proto", "Proto", "proto", "request"),
    editor("headersText", "Headers", "headers", "request"),
    INTERVAL,
    editor("requestMessage", "Message", "json", "section"),
];
const SPLUNK: &[Field] = &[
    editor("baseUrl", "Endpoint", "endpoint", "endpoint"),
    editor("headersText", "Headers", "headers", "request"),
    editor("search", "Search", "spl", "section"),
    option("app", "App:", "text", ""),
    option("earliestTime", "Earliest:", "text", "-24h"),
    option("latestTime", "Latest:", "text", "now"),
    option("count", "Max results:", "number", "30"),
    option("offset", "Offset:", "number", "0"),
    option("preview", "Preview results", "checkbox", "false"),
    INTERVAL,
    option(
        "parseLogfmt",
        "Parse log message (key=value / Java toString)",
        "checkbox",
        "false",
    ),
];
const TERMINAL: &[Field] = &[
    editor("target", "Endpoint", "shell", "endpoint"),
    editor("envText", "Env", "headers", "request"),
    editor("command", "Command", "shell", "section"),
];

pub fn fields(service: &str) -> &'static [Field] {
    match service {
        "mysql" => MYSQL,
        "rest" => REST,
        "graphql" => GRAPHQL,
        "grpc" => GRPC,
        "splunk" => SPLUNK,
        "terminal" => TERMINAL,
        _ => &[],
    }
}
pub fn label(service: &str) -> &str {
    match service {
        "mysql" => "MySQL",
        "rest" => "REST",
        "graphql" => "GraphQL",
        "grpc" => "gRPC",
        "splunk" => "Splunk",
        "terminal" => "Terminal",
        "vars" => "Variables",
        "dictionary" => "Dictionary",
        "console" => "Console",
        _ => service,
    }
}
pub fn title(service: &str) -> &str {
    match service {
        "mysql" => "MySQL Monitor",
        "rest" => "REST Client",
        "graphql" => "GraphQL Client",
        "grpc" => "gRPC Client",
        "splunk" => "Splunk Web Client",
        "terminal" => "Terminal Client",
        _ => label(service),
    }
}
pub fn placeholder(service: &str, field: &str) -> &'static str {
    match (service, field) {
        ("mysql", "dsn") => "mysql://user:password@127.0.0.1:3306/database_name",
        ("mysql", "sql") => "select * from table1 limit 10;",
        ("rest", "url") => "https://api.example.com/v1/resource",
        ("rest", "paramsText") => {
            "page: 1\nlimit: 20\n\nAppended to the URL's query string when sent, alongside anything already typed into it directly."
        }
        ("rest", "headersText") => "Content-Type: application/json\nAuthorization: Bearer ...",
        ("graphql", "url") => "https://api.example.com/graphql",
        ("graphql", "query") => "query {\n  viewer {\n    id\n  }\n}",
        ("graphql", "variables") => "{\n  \"id\": \"1\"\n}",
        ("grpc", "endpointText") => {
            "grpcs://host:443/package.Service/Method, e.g. grpc://localhost:50051/helloworld.Greeter/SayHello"
        }
        ("grpc", "requestMessage") => {
            "For client- or bidi-streaming methods, use a JSON array of messages instead of one object."
        }
        ("splunk", "baseUrl") => "https://splunk.example.com:8089",
        ("splunk", "headersText") => "Authorization: Splunk {{:TOKEN_ENV_VAR}}",
        ("splunk", "search") => {
            "index=main error\n\n\"search \" is prepended automatically unless it already starts with \"search\" or \"|\"."
        }
        ("splunk", "app") => "search",
        ("splunk", "count") => "0 = unlimited",
        ("terminal", "target") => "bash -c, zsh -c, or ssh admin@host -i key.pem",
        ("terminal", "envText") => {
            "NAME: value\n\nWoven into the command as export NAME=value; ..., locally or over ssh."
        }
        ("terminal", "command") => {
            "echo hello\n\nOr something that never exits on its own, like: tail -f /var/log/system.log"
        }
        (_, "headersText") => "Authorization: Bearer ...",
        _ => "",
    }
}
pub fn store_key(service: &str) -> &'static str {
    match service {
        "mysql" => "devtoolsMysqlMonitor.state",
        "rest" => "restWebClient.state",
        "graphql" => "graphqlWebClient.state",
        "grpc" => "grpcWebClient.state",
        "splunk" => "splunkWebClient.state",
        "terminal" => "terminalWebClient.state",
        _ => "servicesConsole.state",
    }
}
pub fn endpoint(service: &str) -> &str {
    match service {
        "mysql" => "query",
        "splunk" => "search",
        "terminal" => "stream",
        _ => "request",
    }
}
pub fn defaults(service: &str) -> Value {
    let mut out = json!({});
    for f in fields(service) {
        out[f.key] = match f.control {
            "number" => json!(f.default.parse::<f64>().unwrap_or(0.0)),
            "checkbox" => json!(f.default == "true"),
            _ => json!(f.default),
        };
    }
    out
}
pub fn string(values: &Value, key: &str) -> String {
    match &values[key] {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        v => v.to_string(),
    }
}
pub fn preview(service: &str, v: &Value) -> String {
    let text = match service {
        "mysql" => string(v, "sql"),
        "rest" => format!("{} {}", string(v, "method"), string(v, "url")),
        "graphql" => {
            let query = string(v, "query");
            if query.trim().is_empty() {
                "Query".into()
            } else {
                query
            }
        }
        "grpc" => string(v, "endpointText"),
        "splunk" => string(v, "search"),
        "terminal" => string(v, "target"),
        _ => String::new(),
    };
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.is_empty() {
        "Request".into()
    } else {
        line
    }
}
pub fn params(service: &str, v: &Value) -> Result<Value, String> {
    let get = |k| string(v, k);
    let nonempty = |k, label: &str| {
        if get(k).trim().is_empty() {
            Err(format!("{label} is required"))
        } else {
            Ok(())
        }
    };
    Ok(match service {
        "mysql" => {
            nonempty("dsn", "Connection string")?;
            nonempty("sql", "SQL query")?;
            json!({"connectionString":get("dsn"),"sql":get("sql")})
        }
        "rest" => {
            nonempty("url", "URL")?;
            let mut url = get("url");
            let p = pairs(&get("paramsText"));
            if !p.as_object().unwrap().is_empty() {
                let mut parsed = url::Url::parse(&url).map_err(|e| format!("Invalid URL: {e}"))?;
                for (k, v) in p.as_object().unwrap() {
                    parsed
                        .query_pairs_mut()
                        .append_pair(k, v.as_str().unwrap_or(""));
                }
                url = parsed.into();
            }
            json!({"method":get("method"),"url":url,"headers":pairs(&get("headersText")),"body":get("body")})
        }
        "graphql" => {
            nonempty("url", "URL")?;
            nonempty("query", "Query")?;
            json!({"url":get("url"),"headers":pairs(&get("headersText")),"query":get("query"),"variables":get("variables")})
        }
        "grpc" => {
            let endpoint = get("endpointText");
            let clean = endpoint
                .strip_prefix("grpc://")
                .or_else(|| endpoint.strip_prefix("grpcs://"))
                .unwrap_or(&endpoint);
            if clean.split('/').filter(|s| !s.is_empty()).count() < 3 {
                return Err("Endpoint must be target/service/method".into());
            }
            json!({"endpoint":endpoint,"headers":pairs(&get("headersText")),"proto":get("proto"),"requestMessage":get("requestMessage")})
        }
        "splunk" => {
            nonempty("baseUrl", "Endpoint")?;
            nonempty("search", "Search")?;
            let mut p = json!({"baseUrl":get("baseUrl"),"headers":pairs(&get("headersText")),"search":get("search"),"count":v["count"],"offset":v["offset"],"preview":v["preview"]});
            for k in ["earliestTime", "latestTime", "app"] {
                if !get(k).is_empty() {
                    p[k] = json!(get(k));
                }
            }
            p
        }
        "terminal" => {
            nonempty("target", "Command target")?;
            nonempty("command", "Command")?;
            json!({"endpoint":get("target"),"env":pairs(&get("envText")),"command":get("command")})
        }
        _ => return Err(format!("Unsupported service {service}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_core_services_have_browser_descriptors() {
        for config in services_contracts::service_configs() {
            assert!(!fields(&config.service).is_empty(), "{}", config.service);
        }
    }
    #[test]
    fn old_keys_preserved() {
        assert_eq!(store_key("mysql"), "devtoolsMysqlMonitor.state");
        assert_eq!(store_key("rest"), "restWebClient.state");
    }
    #[test]
    fn rest_query_parameters_append() {
        let mut v = defaults("rest");
        v["url"] = json!("https://a.test/x?a=1");
        v["paramsText"] = json!("#comment\nb: two words");
        let p = params("rest", &v).unwrap();
        assert_eq!(p["url"], "https://a.test/x?a=1&b=two+words");
    }
    #[test]
    fn splunk_defaults_and_terminal_validation() {
        assert_eq!(defaults("splunk")["count"], 30.0);
        assert_eq!(defaults("splunk")["offset"], 0.0);
        assert_eq!(defaults("splunk")["preview"], false);
        let mut values = defaults("splunk");
        values["baseUrl"] = json!("https://fixture.invalid");
        values["search"] = json!("index=fixture");
        values["preview"] = json!(true);
        values["offset"] = json!(30.0);
        let request = params("splunk", &values).unwrap();
        assert_eq!(request["preview"], true);
        assert_eq!(request["offset"], 30.0);
        assert!(params("terminal", &defaults("terminal")).is_err());
    }
}
