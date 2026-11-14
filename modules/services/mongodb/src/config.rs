//! The `#target` connection string, the `#headers` (client certificate) and the `#options`
//! of a check.

use services_sdk::lang::key_value;
use services_sdk::lang::table::{Format, Options};

pub const DEFAULT_TIMEOUT_S: u64 = 30;
pub const DEFAULT_MAX_DOCUMENTS: u64 = 1000;
const MAX_DOCUMENTS: u64 = 10_000;
const MAX_TIMEOUT_S: u64 = 3600;
const MAX_CONNECT_S: u64 = 120;

pub const OPTIONS: [&str; 7] = [
    "Timeout",
    "Database",
    "Max-Documents",
    "Batch-Size",
    "Connect-Timeout",
    "Format",
    "Columns-Type",
];

pub const HEADERS: [&str; 3] = ["Auth", "Certificate", "CertificatePassword"];

#[derive(Clone, Debug, PartialEq)]
pub struct Connection {
    pub uri: String,
    pub database: String,
    pub timeout_ms: u64,
    pub connect_ms: Option<u64>,
    pub max_documents: u64,
    pub batch_size: Option<u32>,
    pub format: Option<Format>,
    pub columns_type: Option<String>,
}

struct Split<'a> {
    scheme: &'a str,
    userinfo: Option<&'a str>,
    hosts: &'a str,
    database: &'a str,
    query: &'a str,
}

fn split(uri: &str) -> Option<Split<'_>> {
    let (scheme, rest) = uri.split_once("://")?;
    let scheme_ok =
        scheme.eq_ignore_ascii_case("mongodb") || scheme.eq_ignore_ascii_case("mongodb+srv");
    if !scheme_ok || uri.contains(char::is_whitespace) {
        return None;
    }
    let (rest, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (authority, database) = rest.split_once('/').unwrap_or((rest, ""));
    let (userinfo, hosts) = match authority.rsplit_once('@') {
        Some((userinfo, hosts)) => (Some(userinfo), hosts),
        None => (None, authority),
    };
    (!hosts.is_empty() && !hosts.starts_with([',', ':'])).then_some(Split {
        scheme,
        userinfo,
        hosts,
        database,
        query,
    })
}

/// `text` percent-encoded for a connection string value.
fn encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|pair| std::str::from_utf8(pair).ok())
            .and_then(|pair| u8::from_str_radix(pair, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            (byte, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn secret_option(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    ["password", "token", "secret"]
        .iter()
        .any(|word| name.contains(word))
}

/// The passwords and secret option values of a connection string, raw and decoded.
pub fn secrets(uri: &str) -> Vec<String> {
    let Some(split) = split(uri) else {
        return vec![];
    };
    let mut found = Vec::new();
    if let Some((_, password)) = split.userinfo.and_then(|info| info.split_once(':')) {
        found.extend([password.to_owned(), decode(password)]);
    }
    for pair in split.query.split('&') {
        if let Some((name, value)) = pair.split_once('=')
            && secret_option(name)
        {
            found.extend([value.to_owned(), decode(value)]);
        }
    }
    found.retain(|secret| !secret.is_empty());
    found
}

/// The connection string without its passwords.
pub fn redact(uri: &str) -> String {
    let Some(split) = split(uri) else {
        return "mongodb://…".into();
    };
    let mut out = format!("{}://", split.scheme);
    if let Some(info) = split.userinfo {
        match info.split_once(':') {
            Some((user, _)) => out.push_str(&format!("{user}:***@")),
            None => out.push_str(&format!("{info}@")),
        }
    }
    out.push_str(split.hosts);
    if !split.database.is_empty() || !split.query.is_empty() {
        out.push('/');
        out.push_str(split.database);
    }
    if !split.query.is_empty() {
        let query: Vec<String> = split
            .query
            .split('&')
            .map(|pair| match pair.split_once('=') {
                Some((name, _)) if secret_option(name) => format!("{name}=***"),
                _ => pair.to_owned(),
            })
            .collect();
        out.push('?');
        out.push_str(&query.join("&"));
    }
    out
}

/// `text` without the passwords of `uri`.
pub fn scrub(text: &str, uri: &str) -> String {
    let mut text = text.replace(uri, &redact(uri));
    for secret in secrets(uri) {
        text = text.replace(&secret, "***");
    }
    text
}

fn number(name: &str, value: &str, min: u64, max: u64, unit: &str) -> Result<u64, String> {
    value
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|n| (min..=max).contains(n))
        .ok_or_else(|| format!("{name} must be {min}-{max}{unit}"))
}

/// The target with the client certificate of `#headers` put into its query.
fn with_certificate(target: &str, parts: &Split, headers: &str) -> Result<String, String> {
    let mut auth = None;
    let mut certificate = None;
    let mut password = None;
    let mut seen = Vec::new();
    for (name, value) in key_value::pairs(headers) {
        let key = name.to_ascii_lowercase();
        if seen.contains(&key) {
            return Err(format!("Duplicate header {name}"));
        }
        seen.push(key.clone());
        let value = value.trim().to_owned();
        match key.as_str() {
            "auth" => auth = Some(value),
            "certificate" => certificate = Some(value),
            "certificatepassword" => password = Some(value),
            _ => {
                return Err(format!(
                    "Unknown header {name:?}; MongoDB takes {}",
                    HEADERS.join(", ")
                ));
            }
        }
    }
    let x509 = match auth.as_deref().map(str::to_ascii_lowercase).as_deref() {
        Some("x509") => true,
        Some("scram") => false,
        Some(_) => return Err("Auth takes x509 or scram".into()),
        None => false,
    };
    if !x509 {
        return match (certificate, password) {
            (None, None) => Ok(target.to_owned()),
            _ => Err("Certificate needs Auth: x509".into()),
        };
    }
    let Some(certificate) = certificate.filter(|path| !path.is_empty()) else {
        return Err("Auth: x509 needs a Certificate: the path of a PEM file with the client certificate and key".into());
    };
    if parts.userinfo.is_some_and(|info| info.contains(':')) {
        return Err("Auth: x509 does not take a password in the #target".into());
    }
    for pair in parts.query.split('&') {
        let name = pair.split_once('=').map_or(pair, |(name, _)| name);
        if matches!(
            name.to_ascii_lowercase().as_str(),
            "authmechanism"
                | "authsource"
                | "tlscertificatekeyfile"
                | "tlscertificatekeyfilepassword"
        ) {
            return Err(format!(
                "Auth: x509 sets {name} itself; remove it from the #target"
            ));
        }
    }
    let mut query = format!(
        "authMechanism=MONGODB-X509&authSource=%24external&tlsCertificateKeyFile={}",
        encode(&certificate)
    );
    if let Some(password) = password.filter(|password| !password.is_empty()) {
        query.push_str(&format!(
            "&tlsCertificateKeyFilePassword={}",
            encode(&password)
        ));
    }
    let mut uri = target.to_owned();
    if parts.query.is_empty() {
        if !uri.ends_with('?') {
            let rest = uri.split_once("://").map_or("", |(_, rest)| rest);
            if !rest.contains('/') {
                uri.push('/');
            }
            uri.push('?');
        }
    } else {
        uri.push('&');
    }
    uri.push_str(&query);
    Ok(uri)
}

impl Connection {
    /// The target, headers and options of a check, `{{…}}` variables resolved.
    pub fn parse(target: &str, headers: &str, options: &str) -> Result<Self, String> {
        let target = target.trim();
        if target.is_empty() {
            return Err(
                "#target is required: a mongodb:// or mongodb+srv:// connection string".into(),
            );
        }
        let Some(parts) = split(target) else {
            return Err("#target must be a mongodb:// or mongodb+srv:// connection string".into());
        };
        let mut connection = Self {
            uri: with_certificate(target, &parts, headers)?,
            database: decode(parts.database),
            timeout_ms: DEFAULT_TIMEOUT_S * 1000,
            connect_ms: None,
            max_documents: DEFAULT_MAX_DOCUMENTS,
            batch_size: None,
            format: None,
            columns_type: None,
        };
        let mut database = None;
        let mut seen = Vec::new();
        for (name, value) in key_value::pairs(options) {
            let key = name.to_ascii_lowercase();
            if seen.contains(&key) {
                return Err(format!("Duplicate option {name}"));
            }
            seen.push(key.clone());
            match key.as_str() {
                "timeout" => {
                    connection.timeout_ms =
                        number("Timeout", &value, 1, MAX_TIMEOUT_S, " seconds")? * 1000;
                }
                "connect-timeout" => {
                    connection.connect_ms = Some(
                        number("Connect-Timeout", &value, 1, MAX_CONNECT_S, " seconds")? * 1000,
                    );
                }
                "database" if value.trim().is_empty() => {
                    return Err("Database needs a database name".into());
                }
                "database" => database = Some(value.trim().to_owned()),
                "max-documents" => {
                    connection.max_documents =
                        number("Max-Documents", &value, 1, MAX_DOCUMENTS, "")?;
                }
                "batch-size" => {
                    connection.batch_size =
                        Some(number("Batch-Size", &value, 1, 100_000, "")? as u32);
                }
                "format" => connection.format = Some(Format::parse(&value)?),
                "columns-type" => {
                    Options::parse(Some(&value), None)?;
                    connection.columns_type = Some(value);
                }
                _ => {
                    return Err(format!(
                        "Unknown option {name:?}; MongoDB takes {}",
                        OPTIONS.join(", ")
                    ));
                }
            }
        }
        if connection.database.is_empty() {
            connection.database = database.ok_or(
                "A database is required: put it in the connection string (…/app) or set Database in #options",
            )?;
        }
        Ok(connection)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_database_comes_from_the_uri_or_the_option() {
        let uri = Connection::parse("mongodb://h1:27017,h2/app?replicaSet=rs", "", "").unwrap();
        assert_eq!(uri.database, "app");
        assert_eq!(uri.timeout_ms, 30_000);
        assert_eq!(uri.max_documents, 1000);
        let option = Connection::parse(
            "mongodb+srv://u:p@cluster.example.net",
            "",
            "Database: shop",
        )
        .unwrap();
        assert_eq!(option.database, "shop");
        let both = Connection::parse("mongodb://h/app", "", "Database: shop").unwrap();
        assert_eq!(both.database, "app");
        assert!(
            Connection::parse("mongodb://h", "", "")
                .unwrap_err()
                .starts_with("A database is required")
        );
    }

    #[test]
    fn options_are_validated() {
        let c = Connection::parse(
            "mongodb://h/app",
            "",
            "Timeout: 5\nConnect-Timeout: 2\nMax-Documents: 50\nbatch-size: 7\nFormat: json\nColumns-Type: a=json",
        )
        .unwrap();
        assert_eq!(
            (c.timeout_ms, c.connect_ms, c.max_documents, c.batch_size),
            (5000, Some(2000), 50, Some(7))
        );
        assert_eq!(c.format, Some(Format::Json));
        assert_eq!(c.columns_type.as_deref(), Some("a=json"));
        let error = |options: &str| Connection::parse("mongodb://h/app", "", options).unwrap_err();
        assert_eq!(error("Timeout: 0"), "Timeout must be 1-3600 seconds");
        assert_eq!(error("Max-Documents: x"), "Max-Documents must be 1-10000");
        assert_eq!(error("Timeout: 1\ntimeout: 2"), "Duplicate option timeout");
        assert!(error("Format: xml").starts_with("Format takes"));
        assert!(error("Columns-Type: a").starts_with("Invalid Columns-Type"));
        assert!(error("Retries: 1").starts_with("Unknown option \"Retries\""));
        for target in [
            "http://h/app",
            "mongodb://",
            "mongodb://h /app",
            "",
            "mongodb://,h/a",
        ] {
            assert!(Connection::parse(target, "", "").is_err(), "{target}");
        }
    }

    #[test]
    fn passwords_never_survive_redaction() {
        let uri = "mongodb://admin:p%40ss@h1:27017/app?authSource=admin&tlsCertificateKeyFilePassword=k3y";
        assert_eq!(
            redact(uri),
            "mongodb://admin:***@h1:27017/app?authSource=admin&tlsCertificateKeyFilePassword=***"
        );
        let text = scrub(&format!("failed {uri}: bad p@ss and k3y"), uri);
        assert!(!text.contains("p%40ss") && !text.contains("p@ss") && !text.contains("k3y"));
        assert!(text.contains("admin:***@"));
        assert_eq!(redact("mongodb://user@h"), "mongodb://user@h");
        assert_eq!(redact("nonsense"), "mongodb://…");
    }
}

#[cfg(test)]
mod certificate_tests {
    use super::*;

    const X509: &str = "Auth: x509\nCertificate: /keys/my client.pem";

    fn uri(target: &str, headers: &str) -> String {
        Connection::parse(target, headers, "").unwrap().uri
    }

    fn error(target: &str, headers: &str) -> String {
        Connection::parse(target, headers, "").unwrap_err()
    }

    #[test]
    fn x509_is_appended_to_the_query_of_the_target() {
        let tail = "authMechanism=MONGODB-X509&authSource=%24external&tlsCertificateKeyFile=/keys/my%20client.pem";
        assert_eq!(
            uri("mongodb+srv://c0.example.net/app", X509),
            format!("mongodb+srv://c0.example.net/app?{tail}")
        );
        assert_eq!(
            uri("mongodb://h:1/app?retryWrites=true", X509),
            format!("mongodb://h:1/app?retryWrites=true&{tail}")
        );
        assert_eq!(
            uri("mongodb://h/app?", X509),
            format!("mongodb://h/app?{tail}")
        );
        assert_eq!(
            Connection::parse("mongodb://h", X509, "Database: d")
                .unwrap()
                .uri,
            format!("mongodb://h/?{tail}")
        );
        assert_eq!(
            uri(
                "mongodb://CN=me@h/app",
                "Auth: X509\nCertificate: C:\\k.pem"
            )
            .split_once('?')
            .unwrap()
            .1,
            "authMechanism=MONGODB-X509&authSource=%24external&tlsCertificateKeyFile=C%3A%5Ck.pem"
        );
        assert_eq!(uri("mongodb://h/app", ""), "mongodb://h/app");
        assert_eq!(uri("mongodb://h/app", "Auth: scram"), "mongodb://h/app");
    }

    #[test]
    fn the_certificate_password_is_a_secret() {
        let built = uri(
            "mongodb://h/app",
            &format!("{X509}\nCertificatePassword: p@ss w"),
        );
        assert!(built.ends_with("&tlsCertificateKeyFilePassword=p%40ss%20w"));
        assert!(redact(&built).ends_with("tlsCertificateKeyFilePassword=***"));
        let text = scrub(&format!("failed {built}: p@ss w"), &built);
        assert!(!text.contains("p@ss") && !text.contains("p%40ss"));
        assert!(text.contains("/keys/my%20client.pem"));
    }

    #[test]
    fn headers_are_validated() {
        let target = "mongodb://h/app";
        assert!(error(target, "Auth: x509").starts_with("Auth: x509 needs a Certificate"));
        assert_eq!(
            error(target, "Certificate: /k.pem"),
            "Certificate needs Auth: x509"
        );
        assert_eq!(
            error(target, "Auth: scram\nCertificate: /k.pem"),
            "Certificate needs Auth: x509"
        );
        assert_eq!(error(target, "Auth: kerberos"), "Auth takes x509 or scram");
        assert_eq!(
            error(target, "Auth: x509\nauth: x509"),
            "Duplicate header auth"
        );
        assert_eq!(
            error(target, "Cert: /k.pem"),
            "Unknown header \"Cert\"; MongoDB takes Auth, Certificate, CertificatePassword"
        );
        assert_eq!(
            error("mongodb://u:pw@h/app", X509),
            "Auth: x509 does not take a password in the #target"
        );
        for name in [
            "authMechanism=PLAIN",
            "authSource=admin",
            "tlsCertificateKeyFile=/a",
        ] {
            let message = error(&format!("mongodb://h/app?{name}"), X509);
            assert!(
                message.contains("sets") && !message.contains("PLAIN"),
                "{message}"
            );
        }
    }
}
