use super::http;
use crate::core::error::{Error, Result, required, text};
use serde_json::{Map, Value, json};
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
pub async fn send(client: &reqwest::Client, params: &Value) -> Result<Value> {
    let query = required(params, "query")?;
    let mut body = json!({"query":query});
    if let Some(v) = params
        .get("variables")
        .filter(|v| !v.is_null() && **v != "")
    {
        body["variables"] = if let Some(v) = v.as_str() {
            serde_json::from_str(v).map_err(|_| Error::new("variables must be valid JSON"))?
        } else {
            v.clone()
        };
    }
    let mut headers = json!({"Content-Type":"application/json"});
    headers
        .as_object_mut()
        .unwrap()
        .extend(http::headers(params)?.as_object().unwrap().clone());
    http::request(
        client,
        "POST",
        &text(params, "url"),
        &headers,
        Some(body.to_string()),
    )
    .await
}
pub async fn discover(client: &reqwest::Client, params: &Value) -> Result<Value> {
    let mut params = params.clone();
    params["query"] = json!(INTROSPECTION_QUERY);
    params.as_object_mut().map(|p| p.remove("variables"));
    let result = send(client, &params).await?;
    let failure = |message: String| {
        let mut error = Error::new(message);
        error.debug = result.get("debug").cloned().map(Box::new);
        error.duration_ms = result["durationMs"].as_u64().map(u128::from);
        error
    };
    if let Some(status) = result["status"].as_u64().filter(|status| *status >= 400) {
        return Err(failure(format!("GraphQL discovery HTTP {status}")));
    }
    let data: Value = serde_json::from_str(result["body"].as_str().unwrap_or(""))
        .map_err(|e| failure(format!("Invalid GraphQL introspection JSON: {e}")))?;
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
        return Err(failure(format!(
            "GraphQL introspection failed: {}",
            if messages.is_empty() {
                "server returned errors"
            } else {
                &messages
            }
        )));
    }
    let schema = &data["data"]["__schema"];
    if !schema.is_object() {
        return Err(failure("GraphQL introspection returned no schema".into()));
    }
    let mut types = Map::new();
    for t in schema["types"].as_array().into_iter().flatten() {
        let Some(name) = t["name"].as_str() else {
            continue;
        };
        let fields:Vec<_> = t["fields"].as_array().into_iter().flatten().map(|field| {
            let mut named = &field["type"];
            for _ in 0..32 {
                if named["kind"] == "NON_NULL" || named["kind"] == "LIST" { named=&named["ofType"]; } else {break}
            }
            json!({"name":field["name"],"type":field["type"],"typeName":named["name"],"isObject":matches!(named["kind"].as_str(),Some("OBJECT"|"INTERFACE"|"UNION")),"args":field["args"].as_array().cloned().unwrap_or_default()})
        }).collect();
        types.insert(name.into(), json!({
            "kind":t["kind"],"fields":fields,
            "possibleTypes":t["possibleTypes"].as_array().into_iter().flatten().filter_map(|value|value["name"].as_str()).collect::<Vec<_>>(),
            "inputFields":t["inputFields"].as_array().cloned().unwrap_or_default(),
            "enumValues":t["enumValues"].as_array().into_iter().flatten().filter_map(|value|value["name"].as_str()).collect::<Vec<_>>()
        }));
    }
    if schema["queryType"]["name"]
        .as_str()
        .and_then(|name| types.get(name))
        .and_then(|root| root["fields"].as_array())
        .is_none_or(|fields| fields.is_empty())
    {
        return Err(failure(
            "GraphQL introspection returned no query fields".into(),
        ));
    }
    Ok(
        json!({"queryType":schema["queryType"]["name"],"mutationType":schema["mutationType"]["name"],"subscriptionType":schema["subscriptionType"]["name"],"directives":schema["directives"].as_array().cloned().unwrap_or_default(),"types":types}),
    )
}
