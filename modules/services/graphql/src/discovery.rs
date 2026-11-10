//! Introspection: when the user asks for hints (or to discover) the check's endpoint is
//! asked for its schema through the `http` driver; a schema is kept per endpoint and
//! headers, and a connection is introspected once until asked again.

use serde_json::{Map, Value, json};
use services_sdk::http::{Request, Response};
use services_sdk::lang::headers;
use services_sdk::{Check, Cmd, Failure, cmd};

/// Schemas and connections remembered.
const CACHE: usize = 16;
const ATTEMPTED: usize = 64;
const TAG: &str = "discover:";

pub const INTROSPECTION_QUERY: &str = r#"
query IntrospectionQuery {
  __schema {
    queryType { name }
    mutationType { name }
    subscriptionType { name }
    directives {
      name locations isRepeatable
      args { name defaultValue type { ...CompletionType } }
    }
    types {
      kind name
      possibleTypes { name }
      fields {
        name type { ...CompletionType }
        args { name defaultValue type { ...CompletionType } }
      }
      inputFields { name defaultValue type { ...CompletionType } }
      enumValues { name }
    }
  }
}
fragment CompletionType on __Type {
  kind name
  ofType { kind name ofType { kind name ofType { kind name
    ofType { kind name ofType { kind name ofType { kind name } } }
  } } }
}
"#;

/// What a check connects with: its endpoint URL and headers.
#[derive(Clone, Debug, PartialEq)]
pub struct Connection {
    pub url: String,
    pub headers: Vec<(String, String)>,
}

impl Connection {
    pub fn of(check: &Check) -> Option<Self> {
        let url = check.text_of("target").trim();
        let web = url.split_once("://").is_some_and(|(scheme, _)| {
            matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https")
        });
        (web || url.starts_with("{{")).then(|| Self {
            url: url.to_owned(),
            headers: headers::pairs(check.text_of("headers")),
        })
    }
}

#[derive(Debug, Default)]
pub struct Discovery {
    /// `(connection, schema)`, most recent last.
    schemas: Vec<(Connection, Value)>,
    attempted: Vec<Connection>,
    /// The connection being introspected and its run.
    run: Option<(u64, Connection)>,
    runs: u64,
    /// The latest other connection hints asked for during a run.
    pending: Option<Connection>,
}

/// The request that sends `query` (and `variables`) to `connection`.
pub fn request(connection: &Connection, query: &str, variables: Option<Value>) -> Request {
    let mut body = json!({ "query": query });
    if let Some(variables) = variables {
        body["variables"] = variables;
    }
    let mut headers = vec![("Content-Type".to_owned(), "application/json".to_owned())];
    for (name, value) in &connection.headers {
        headers.retain(|(set, _)| !set.eq_ignore_ascii_case(name));
        headers.push((name.clone(), value.clone()));
    }
    Request {
        headers,
        body: Some(body.to_string()),
        ..Request::new("POST", &connection.url)
    }
}

/// The introspection answer as completion uses it: `{queryType, mutationType,
/// subscriptionType, directives, types: {name: {kind, fields, possibleTypes, inputFields,
/// enumValues}}}`.
pub fn schema(response: &Response) -> Result<Value, String> {
    if response.status >= 400 {
        return Err(format!("GraphQL discovery HTTP {}", response.status));
    }
    let data: Value = serde_json::from_str(response.body.as_deref().unwrap_or(""))
        .map_err(|e| format!("Invalid GraphQL introspection JSON: {e}"))?;
    if let Some(errors) = data.get("errors").filter(|errors| {
        !errors.is_null() && errors.as_array().is_none_or(|errors| !errors.is_empty())
    }) {
        let messages = errors
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|error| error["message"].as_str())
            .collect::<Vec<_>>()
            .join("; ");
        return Err(format!(
            "GraphQL introspection failed: {}",
            if messages.is_empty() {
                "server returned errors"
            } else {
                &messages
            }
        ));
    }
    let schema = &data["data"]["__schema"];
    if !schema.is_object() {
        return Err("GraphQL introspection returned no schema".into());
    }
    let names = |value: &Value| -> Vec<Value> {
        value
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|value| value["name"].as_str())
            .map(|name| json!(name))
            .collect()
    };
    let mut types = Map::new();
    for t in schema["types"].as_array().into_iter().flatten() {
        let Some(name) = t["name"].as_str() else {
            continue;
        };
        let fields: Vec<_> = t["fields"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|field| {
                let mut named = &field["type"];
                for _ in 0..32 {
                    if named["kind"] == "NON_NULL" || named["kind"] == "LIST" {
                        named = &named["ofType"];
                    } else {
                        break;
                    }
                }
                json!({
                    "name": field["name"],
                    "type": field["type"],
                    "typeName": named["name"],
                    "isObject": matches!(named["kind"].as_str(), Some("OBJECT" | "INTERFACE" | "UNION")),
                    "args": field["args"].as_array().cloned().unwrap_or_default(),
                })
            })
            .collect();
        types.insert(
            name.into(),
            json!({
                "kind": t["kind"],
                "fields": fields,
                "possibleTypes": names(&t["possibleTypes"]),
                "inputFields": t["inputFields"].as_array().cloned().unwrap_or_default(),
                "enumValues": names(&t["enumValues"]),
            }),
        );
    }
    if schema["queryType"]["name"]
        .as_str()
        .and_then(|name| types.get(name))
        .and_then(|root| root["fields"].as_array())
        .is_none_or(|fields| fields.is_empty())
    {
        return Err("GraphQL introspection returned no query fields".into());
    }
    Ok(json!({
        "queryType": schema["queryType"]["name"],
        "mutationType": schema["mutationType"]["name"],
        "subscriptionType": schema["subscriptionType"]["name"],
        "directives": schema["directives"].as_array().cloned().unwrap_or_default(),
        "types": types,
    }))
}

impl Discovery {
    /// The schema introspected for the check's connection.
    pub fn schema(&self, check: &Check) -> Option<&Value> {
        let connection = Connection::of(check)?;
        self.schemas
            .iter()
            .rev()
            .find(|(known, _)| *known == connection)
            .map(|(_, schema)| schema)
    }

    fn known(&self, connection: &Connection) -> bool {
        self.attempted.contains(connection) || self.schemas.iter().any(|(c, _)| c == connection)
    }

    /// The user asked for hints: introspect the check's connection unless it is known.
    pub fn hints(&mut self, check: &Check) -> Vec<Cmd> {
        let Some(connection) = Connection::of(check) else {
            return vec![];
        };
        match &self.run {
            Some((_, running)) if *running == connection => vec![],
            Some(_) => {
                self.pending = Some(connection);
                vec![]
            }
            None if self.known(&connection) => vec![],
            None => self.start(connection),
        }
    }

    /// Discovery on demand: the check's connection again.
    pub fn manual(&mut self, check: &Check) -> Vec<Cmd> {
        match Connection::of(check) {
            Some(connection) => self.start(connection),
            None => vec![cmd::status(
                "Discovery · WARNING · Invalid or incomplete service address for discovery",
                "warning",
            )],
        }
    }

    fn start(&mut self, connection: Connection) -> Vec<Cmd> {
        self.runs += 1;
        self.pending = None;
        let call = request(&connection, INTROSPECTION_QUERY, None).call();
        self.run = Some((self.runs, connection));
        vec![
            cmd::status("Discovery…", "busy"),
            cmd::driver(format!("{TAG}{}", self.runs), &call),
        ]
    }

    /// A driver reply; `None` when the tag is not discovery's.
    pub fn reply(&mut self, tag: &str, result: Result<Value, Failure>) -> Option<Vec<Cmd>> {
        let id: u64 = tag.strip_prefix(TAG)?.parse().ok()?;
        let Some((_, connection)) = self.run.take_if(|(run, _)| *run == id) else {
            return Some(vec![]);
        };
        let schema = result
            .map_err(|failure| failure.message)
            .and_then(|value| Response::parse(&value))
            .and_then(|response| schema(&response));
        self.attempted.retain(|known| *known != connection);
        self.attempted.push(connection.clone());
        if self.attempted.len() > ATTEMPTED {
            self.attempted.remove(0);
        }
        let mut cmds = match schema {
            Ok(schema) => {
                let types = schema["types"].as_object().map_or(0, Map::len);
                let cmds = vec![
                    cmd::status_with("Discovery · OK", "ok", &json!({"url": connection.url})),
                    cmd::log(
                        "info",
                        &format!("GraphQL schema of {}: {types} types", connection.url),
                    ),
                    cmd::relight(),
                ];
                self.schemas.retain(|(known, _)| *known != connection);
                self.schemas.push((connection, schema));
                if self.schemas.len() > CACHE {
                    self.schemas.remove(0);
                }
                cmds
            }
            Err(message) => vec![
                cmd::status(&format!("Discovery · ERROR · {message}"), "error"),
                cmd::log("error", &format!("{}: {message}", connection.url)),
            ],
        };
        if let Some(next) = self.pending.take().filter(|next| !self.known(next)) {
            cmds.extend(self.start(next));
        }
        Some(cmds)
    }
}
