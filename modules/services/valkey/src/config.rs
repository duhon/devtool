use percent_encoding::percent_decode_str;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use services_sdk::{Check, lang::headers, tcp};
use url::Url;

pub const RESPONSE_LIMIT: usize = 16 * 1024 * 1024;
pub const HISTORY_LIMIT: usize = 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Plan {
    pub endpoint: String,
    pub args: Vec<String>,
    pub options: Vec<(String, String)>,
}

impl Plan {
    pub fn from_check(check: &Check, command: bool) -> Result<Self, String> {
        let endpoint = check.text_of("target").trim().to_owned();
        if endpoint.is_empty() {
            return Err("#target is required (redis://host:6379/0)".into());
        }
        let args = if command {
            parse_command(check.section("action").unwrap_or(""))?
        } else {
            Vec::new()
        };
        Ok(Self {
            endpoint,
            args,
            options: headers::pairs(check.text_of("options")),
        })
    }

    pub fn identity(&self) -> String {
        serde_json::to_string(&(&self.endpoint, &self.options)).unwrap()
    }
}

pub fn parse_command(text: &str) -> Result<Vec<String>, String> {
    let args: Vec<String> = if text.trim_start().starts_with('[') {
        serde_json::from_str(text).map_err(|_| "Command JSON must be an array of strings")?
    } else {
        shlex::split(text).ok_or("Unclosed quote or escape in command")?
    };
    validate_command(&args)?;
    Ok(args)
}

pub fn validate_command(args: &[String]) -> Result<(), String> {
    let name = args
        .first()
        .filter(|name| !name.is_empty() && !name.contains(char::is_whitespace))
        .ok_or("A command name is required")?;
    if matches!(
        name.to_ascii_uppercase().as_str(),
        "PSUBSCRIBE"
            | "SSUBSCRIBE"
            | "UNSUBSCRIBE"
            | "PUNSUBSCRIBE"
            | "SUNSUBSCRIBE"
            | "MONITOR"
            | "SYNC"
            | "PSYNC"
            | "HELLO"
    ) {
        return Err(
            "This streaming/protocol-switching command is not supported; use SUBSCRIBE for Pub/Sub"
                .into(),
        );
    }
    if name.eq_ignore_ascii_case("SUBSCRIBE") && args.len() < 2 {
        return Err("SUBSCRIBE requires at least one channel".into());
    }
    Ok(())
}

pub struct Connection {
    pub request: tcp::Request,
    pub auth: Option<Vec<String>>,
    pub database: u32,
    pub timeout_ms: u32,
}

impl Connection {
    pub fn parse(plan: &Plan) -> Result<Self, String> {
        let url = Url::parse(&plan.endpoint).map_err(|_| "Invalid Valkey endpoint URL")?;
        if !matches!(url.scheme(), "redis" | "rediss")
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err("Use redis:// or rediss:// without query parameters or fragments".into());
        }
        let host = url.host_str().ok_or("Endpoint requires a host")?;
        let host = host.trim_start_matches('[').trim_end_matches(']');
        let mut request = tcp::Request::session(host, url.port().unwrap_or(6379));
        request.tls = url.scheme() == "rediss";
        let database = match url.path().trim_start_matches('/') {
            "" => 0,
            path => path.parse::<u32>().map_err(|_| "Invalid database number")?,
        };
        let decode = |value: &str| {
            percent_decode_str(value)
                .decode_utf8()
                .map(|value| value.into_owned())
                .map_err(|_| "Credentials must be UTF-8")
        };
        let auth = match url.password() {
            Some(password) if !url.username().is_empty() => Some(vec![
                "AUTH".into(),
                decode(url.username())?,
                decode(password)?,
            ]),
            Some(password) => Some(vec!["AUTH".into(), decode(password)?]),
            None if !url.username().is_empty() => {
                return Err("A username requires a password in the endpoint".into());
            }
            None => None,
        };
        let mut timeout_ms = 30_000;
        let mut seen = std::collections::BTreeSet::new();
        for (name, value) in &plan.options {
            let name = name.to_ascii_lowercase();
            if !seen.insert(name.clone()) {
                return Err("Duplicate option".into());
            }
            match name.as_str() {
                "timeout" => {
                    timeout_ms = value
                        .trim()
                        .parse::<u32>()
                        .ok()
                        .filter(|value| (1..=110).contains(value))
                        .ok_or("Timeout must be 1-110 seconds")?
                        * 1000;
                }
                "ca-pem" if request.tls && !value.trim().is_empty() => {
                    request.ca_pem = Some(value.clone());
                }
                "ca-pem" => return Err("CA-PEM requires rediss:// and a PEM certificate".into()),
                "ca-cert" => {
                    return Err(
                        "CA-Cert paths are no longer supported; use CA-PEM: {{VALKEY_CA}}".into(),
                    );
                }
                _ => return Err(format!("Unknown Valkey option: {name}")),
            }
        }
        request.connect_ms = Some(timeout_ms.into());
        Ok(Self {
            request,
            auth,
            database,
            timeout_ms,
        })
    }
}

pub fn resolved(value: Value) -> Result<Plan, String> {
    serde_json::from_value(value).map_err(|_| "Invalid resolved Valkey request".into())
}
