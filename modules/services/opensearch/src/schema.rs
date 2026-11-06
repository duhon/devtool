//! A cluster's index mappings and aliases as the console's REST discovery: API paths with
//! their methods, and JSON body schemas for search, count, document and update requests.
//!
//! A body schema lists fields `{name, kind, type?, format?, enum?, fields?, items?}`, where
//! `kind` is `bare` (number, boolean or unknown), `string`, `object` or `array`.

use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use url::Url;

/// APIs under an index or alias, and their methods.
const INDEX_APIS: [(&str, &[&str]); 11] = [
    ("", &["GET"]),
    ("/_search", &["GET", "POST"]),
    ("/_count", &["GET", "POST"]),
    ("/_mapping", &["GET"]),
    ("/_alias", &["GET"]),
    ("/_bulk", &["POST", "PUT"]),
    ("/_msearch", &["GET", "POST"]),
    ("/_msearch/template", &["GET", "POST"]),
    ("/_doc", &["POST"]),
    ("/_doc/{id}", &["GET", "HEAD", "PUT", "DELETE"]),
    ("/_update/{id}", &["POST"]),
];
/// APIs under the cluster root, and their methods.
const CLUSTER_APIS: [(&str, &[&str]); 11] = [
    ("/", &["GET"]),
    ("/_cluster/health", &["GET"]),
    ("/_cat/indices", &["GET"]),
    ("/_cat/aliases", &["GET"]),
    ("/_mapping", &["GET"]),
    ("/_alias", &["GET"]),
    ("/_search", &["GET", "POST"]),
    ("/_count", &["GET", "POST"]),
    ("/_bulk", &["POST", "PUT"]),
    ("/_msearch", &["GET", "POST"]),
    ("/_msearch/template", &["GET", "POST"]),
];
/// Index selectors that span every index.
const SELECTORS: [&str; 2] = ["*", "_all"];
const HEADERS: [&str; 3] = ["Authorization", "Accept", "Content-Type"];

/// Paths and body schemas for the indices and aliases of the cluster at `base`. Names
/// starting with `.` (system indices) are left out with their fields.
pub fn indices(base: &Url, mappings: &Value, aliases: &Value) -> Value {
    summarize(base, &catalog(mappings, aliases), true)
}

/// Only the paths, for a request to a service API: cached under `scope`, so it does not
/// stand in for the index schema.
pub fn services(base: &Url, scope: &Url) -> Value {
    let mut schema = summarize(base, &BTreeMap::new(), false);
    schema["baseUrl"] = json!(scope.as_str());
    schema
}

/// The fields of an index or alias, by name.
#[derive(Default, PartialEq)]
struct Fields {
    /// Leaf fields with dotted names, multi-fields included: what queries name.
    query: BTreeMap<String, Value>,
    /// The document's fields as nested objects: what document bodies hold.
    document: BTreeMap<String, Value>,
}

/// The fields of every index and alias; an alias spanning several indices merges theirs.
fn catalog(mappings: &Value, aliases: &Value) -> BTreeMap<String, Fields> {
    let mut catalog: BTreeMap<String, Fields> = BTreeMap::new();
    for (index, entry) in visible(mappings) {
        let properties = &entry["mappings"]["properties"];
        let fields = catalog.entry(index.clone()).or_default();
        leaves(properties, "", &mut fields.query);
        merge(&mut fields.document, document(properties));
    }
    for (index, entry) in visible(aliases) {
        let fields = catalog.entry(index.clone()).or_default();
        let query: Vec<_> = fields.query.values().cloned().collect();
        let document: Vec<_> = fields.document.values().cloned().collect();
        for (alias, _) in visible(&entry["aliases"]) {
            let fields = catalog.entry(alias.clone()).or_default();
            merge(&mut fields.query, query.iter().cloned());
            merge(&mut fields.document, document.iter().cloned());
        }
    }
    catalog
}

/// The entries of an object whose names do not start with `.`.
fn visible(object: &Value) -> impl Iterator<Item = (&String, &Value)> {
    object
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(name, _)| !name.starts_with('.'))
}

fn summarize(base: &Url, catalog: &BTreeMap<String, Fields>, bodies: bool) -> Value {
    let mut all = Fields::default();
    for fields in catalog.values() {
        merge(&mut all.query, fields.query.values().cloned());
        merge(&mut all.document, fields.document.values().cloned());
    }
    let mut apis = Apis::default();
    for (name, fields) in catalog {
        // Fields equal to the merged ones come from the `{index}` template below.
        let own = bodies && fields != &all;
        apis.index(&index_path(base, name), own.then_some(fields));
    }
    let mount = base.path().trim_end_matches('/');
    // Concrete paths precede the template: the console takes the first matching schema.
    for selector in SELECTORS {
        apis.index(&format!("{mount}/{selector}"), None);
    }
    apis.index(&format!("{mount}/{{index}}"), bodies.then_some(&all));
    apis.cluster(mount, bodies.then_some(&all));
    json!({
        "baseUrl": base.as_str().trim_end_matches('/'),
        "basePath": mount,
        "paths": apis.methods.keys().collect::<Vec<_>>(),
        "methodsByPath": apis.methods,
        "headers": HEADERS,
        "bodySchemas": apis.bodies,
    })
}

/// The path of an index or alias under the cluster root, encoded like a request URL.
fn index_path(base: &Url, name: &str) -> String {
    let mut url = base.clone();
    if let Ok(mut segments) = url.path_segments_mut() {
        segments.pop_if_empty().push(name);
    }
    url.path().to_owned()
}

/// Paths with their methods and body schemas, in the order they are added.
#[derive(Default)]
struct Apis {
    methods: Map<String, Value>,
    bodies: Map<String, Value>,
}

impl Apis {
    /// The index APIs under `prefix`, with bodies for `fields` when given.
    fn index(&mut self, prefix: &str, fields: Option<&Fields>) {
        for (api, methods) in INDEX_APIS {
            let path = format!("{prefix}{api}");
            self.methods.insert(path.clone(), json!(methods));
            let Some(fields) = fields else { continue };
            let document = || fields.document.values().cloned().collect::<Vec<_>>();
            for method in methods {
                let body = match (api, *method) {
                    ("/_search", _) => search(&fields.query),
                    ("/_count", _) => vec![query(&fields.query)],
                    ("/_doc", "POST") | ("/_doc/{id}", "PUT") => document(),
                    ("/_update/{id}", _) => vec![
                        object("doc", document()),
                        json!({"name":"doc_as_upsert","kind":"bare","type":"boolean"}),
                        object("upsert", document()),
                    ],
                    _ => continue,
                };
                self.bodies.insert(format!("{method} {path}"), json!(body));
            }
        }
    }

    /// The cluster APIs under `mount`, with search and count bodies for `fields` when given.
    fn cluster(&mut self, mount: &str, fields: Option<&Fields>) {
        for (api, methods) in CLUSTER_APIS {
            let path = format!("{mount}{api}");
            self.methods.insert(path.clone(), json!(methods));
            let body = match (api, fields) {
                ("/_search", Some(fields)) => search(&fields.query),
                ("/_count", Some(fields)) => vec![query(&fields.query)],
                _ => continue,
            };
            for method in methods {
                self.bodies.insert(format!("{method} {path}"), json!(body));
            }
        }
    }
}

/// A body field for a mapped field.
fn field(name: &str, mapping: &Value) -> Value {
    let kind = mapping["type"].as_str().unwrap_or("object");
    let mut field = match kind {
        "byte" | "short" | "integer" | "long" | "unsigned_long" | "token_count" => {
            json!({"kind":"bare","type":"integer"})
        }
        "half_float" | "float" | "double" | "scaled_float" => {
            json!({"kind":"bare","type":"number"})
        }
        "boolean" => json!({"kind":"bare","type":"boolean"}),
        "object" => json!({"kind":"object","fields":document(&mapping["properties"])}),
        "nested" => json!({
            "kind":"array",
            "items":{"kind":"object","fields":document(&mapping["properties"])}
        }),
        _ => json!({"kind":"string"}),
    };
    field["name"] = json!(name);
    if matches!(kind, "date" | "date_nanos") {
        field["format"] = json!("date-time");
    }
    field
}

/// Mapping properties as document fields, objects nested.
fn document(properties: &Value) -> Vec<Value> {
    properties
        .as_object()
        .into_iter()
        .flatten()
        .map(|(name, mapping)| field(name, mapping))
        .collect()
}

/// Mapping properties as leaf fields named by their dotted paths, multi-fields included.
fn leaves(properties: &Value, prefix: &str, out: &mut BTreeMap<String, Value>) {
    for (name, mapping) in properties.as_object().into_iter().flatten() {
        let name = match prefix {
            "" => name.clone(),
            _ => format!("{prefix}.{name}"),
        };
        if mapping["properties"].is_object() {
            leaves(&mapping["properties"], &name, out);
        } else if !matches!(mapping["type"].as_str(), Some("object" | "nested")) {
            out.insert(name.clone(), field(&name, mapping));
        }
        leaves(&mapping["fields"], &name, out);
    }
}

/// Adds fields by name; a name mapped differently by two indices loses its type rather
/// than guessing one.
fn merge(target: &mut BTreeMap<String, Value>, fields: impl IntoIterator<Item = Value>) {
    for field in fields {
        let Some(name) = field["name"].as_str().map(str::to_owned) else {
            continue;
        };
        match target.get(&name) {
            Some(known) if known != &field => {
                target.insert(name.clone(), json!({"name":name,"kind":"bare"}));
            }
            Some(_) => {}
            None => {
                target.insert(name, field);
            }
        }
    }
}

fn object(name: &str, fields: Vec<Value>) -> Value {
    json!({"name":name,"kind":"object","fields":fields})
}

fn array(name: &str, items: Value) -> Value {
    json!({"name":name,"kind":"array","items":items})
}

/// A search body: the query and the usual paging, sorting and source options.
fn search(fields: &BTreeMap<String, Value>) -> Vec<Value> {
    let names: Vec<_> = fields.keys().map(|name| json!(name)).collect();
    let mut sort = names.clone();
    sort.extend([json!("_score"), json!("_doc")]);
    vec![
        query(fields),
        json!({"name":"size","kind":"bare","type":"integer"}),
        json!({"name":"from","kind":"bare","type":"integer"}),
        array("sort", json!({"kind":"string","enum":sort})),
        array("_source", json!({"kind":"string","enum":names})),
        array("search_after", json!({"kind":"bare"})),
        object("aggs", vec![]),
        json!({"name":"track_total_hits","kind":"bare","type":"boolean"}),
        json!({"name":"timeout","kind":"string"}),
    ]
}

/// `query`: the leaf queries, and `bool` with the same leaf queries in its clauses.
fn query(fields: &BTreeMap<String, Value>) -> Value {
    let mut queries = leaf_queries(fields);
    let mut clauses: Vec<_> = ["must", "filter", "should", "must_not"]
        .into_iter()
        .map(|clause| {
            array(
                clause,
                json!({"kind":"object","fields":leaf_queries(fields)}),
            )
        })
        .collect();
    clauses.push(json!({"name":"minimum_should_match","kind":"bare"}));
    queries.push(object("bool", clauses));
    object("query", queries)
}

fn leaf_queries(fields: &BTreeMap<String, Value>) -> Vec<Value> {
    let leaves: Vec<_> = fields.values().cloned().collect();
    let names: Vec<_> = fields.keys().collect();
    vec![
        object("match_all", vec![]),
        object("match", leaves.clone()),
        object("match_phrase", leaves.clone()),
        object("term", leaves),
        object(
            "terms",
            fields
                .iter()
                .map(|(name, field)| array(name, field.clone()))
                .collect(),
        ),
        object(
            "range",
            fields
                .iter()
                .map(|(name, field)| object(name, range(field)))
                .collect(),
        ),
        object(
            "exists",
            vec![json!({"name":"field","kind":"string","type":"string","enum":names})],
        ),
        object(
            "multi_match",
            vec![
                json!({"name":"query","kind":"string"}),
                array("fields", json!({"kind":"string"})),
            ],
        ),
        object(
            "query_string",
            vec![
                json!({"name":"query","kind":"string"}),
                json!({"name":"default_field","kind":"string"}),
                array("fields", json!({"kind":"string"})),
            ],
        ),
    ]
}

/// The bounds of a range query on `field`, typed like it.
fn range(field: &Value) -> Vec<Value> {
    let mut bounds: Vec<_> = ["gt", "gte", "lt", "lte"]
        .into_iter()
        .map(|bound| {
            let mut bound_field = field.clone();
            bound_field["name"] = json!(bound);
            bound_field
        })
        .collect();
    bounds.push(json!({"name":"boost","kind":"bare","type":"number"}));
    if field["format"] == "date-time" {
        bounds.extend([
            json!({"name":"format","kind":"string"}),
            json!({"name":"time_zone","kind":"string"}),
        ]);
    }
    bounds
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata() -> (Value, Value) {
        (
            json!({
                "logs": {"mappings":{"properties":{
                    "message":{"type":"text","fields":{"keyword":{"type":"keyword"}}},
                    "status":{"type":"integer"},
                    "created":{"type":"date"},
                    "user":{"properties":{"name":{"type":"keyword"}}},
                    "events":{"type":"nested","properties":{"code":{"type":"long"}}}
                }}},
                "metrics": {"mappings":{"properties":{
                    "cpu":{"type":"double"},
                    "status":{"type":"keyword"}
                }}}
            }),
            json!({
                "logs":{"aliases":{"recent":{},"combined":{}}},
                "metrics":{"aliases":{"combined":{}}}
            }),
        )
    }

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    /// The fields a `match` query offers in a body schema.
    fn matched(body: &Value) -> &Vec<Value> {
        body[0]["fields"][1]["fields"].as_array().unwrap()
    }

    #[test]
    fn fields_are_flattened_merged_across_aliases_and_untyped_on_conflict() {
        let (mappings, aliases) = metadata();
        let schema = indices(
            &url("https://example.test/proxy/opensearch/"),
            &mappings,
            &aliases,
        );
        assert_eq!(schema["baseUrl"], "https://example.test/proxy/opensearch");
        assert_eq!(schema["basePath"], "/proxy/opensearch");
        let bodies = &schema["bodySchemas"];
        let logs = matched(&bodies["GET /proxy/opensearch/logs/_search"]);
        for name in [
            "message",
            "message.keyword",
            "status",
            "user.name",
            "events.code",
        ] {
            assert!(logs.iter().any(|field| field["name"] == name), "{name}");
        }
        assert!(!logs.iter().any(|field| field["name"] == "cpu"));
        // `combined` spans every index: the `{index}` template answers for it.
        assert!(bodies["POST /proxy/opensearch/combined/_search"].is_null());
        let merged = matched(&bodies["POST /proxy/opensearch/{index}/_search"]);
        assert!(merged.iter().any(|field| field["name"] == "cpu"));
        let status = merged.iter().find(|field| field["name"] == "status");
        assert_eq!(status, Some(&json!({"name":"status","kind":"bare"})));
        let document = bodies["PUT /proxy/opensearch/logs/_doc/{id}"]
            .as_array()
            .unwrap();
        let kind = |name: &str| {
            document
                .iter()
                .find(|field| field["name"] == name)
                .map(|field| field["kind"].clone())
        };
        assert_eq!(kind("user"), Some(json!("object")));
        assert_eq!(kind("events"), Some(json!("array")));
        assert_eq!(kind("message.keyword"), None);
    }

    #[test]
    fn an_empty_cluster_still_offers_api_paths_and_the_search_dsl() {
        let schema = indices(&url("http://localhost:9200/"), &json!({}), &json!({}));
        assert_eq!(schema["methodsByPath"]["/_search"], json!(["GET", "POST"]));
        let query = &schema["bodySchemas"]["GET /_search"][0];
        assert_eq!(query["name"], "query");
        assert_eq!(query["fields"][0]["name"], "match_all");
    }

    #[test]
    fn service_apis_get_only_small_path_lists() {
        let base = url("http://localhost:9201/proxy/");
        let schema = services(&base, &url("http://localhost:9201/proxy/_cluster"));
        assert_eq!(schema["baseUrl"], "http://localhost:9201/proxy/_cluster");
        assert_eq!(schema["bodySchemas"], json!({}));
        assert!(serde_json::to_vec(&schema).unwrap().len() < 16 * 1024);
    }

    #[test]
    fn identical_indices_and_aliases_share_the_template_schema() {
        let mut mappings = Map::new();
        let mut aliases = Map::new();
        for index in 0..10 {
            let properties: Map<String, Value> = (0..50)
                .map(|field| (format!("field_{field:03}"), json!({"type":"keyword"})))
                .collect();
            mappings.insert(
                format!("index-{index:02}"),
                json!({"mappings":{"properties":properties}}),
            );
            aliases.insert(
                format!("index-{index:02}"),
                json!({"aliases":{format!("alias-{index:02}"): {}}}),
            );
        }
        let base = url("http://localhost:9201/");
        let schema = indices(&base, &json!(mappings), &json!(aliases));
        assert!(schema["bodySchemas"]["GET /index-01/_search"].is_null());
        assert!(
            schema["paths"]
                .as_array()
                .unwrap()
                .contains(&json!("/alias-09/_search"))
        );
        let query = &schema["bodySchemas"]["GET /{index}/_search"][0]["fields"];
        let bool_query = query
            .as_array()
            .unwrap()
            .iter()
            .find(|field| field["name"] == "bool")
            .unwrap();
        let must_match = &bool_query["fields"][0]["items"]["fields"][1]["fields"];
        assert_eq!(must_match.as_array().unwrap().len(), 50);
        let single = indices(
            &base,
            &json!({"index-00": mappings["index-00"]}),
            &json!({"index-00": aliases["index-00"]}),
        );
        assert_eq!(schema["bodySchemas"], single["bodySchemas"]);
        let bytes = serde_json::to_vec(&schema).unwrap().len();
        assert!(bytes - serde_json::to_vec(&single).unwrap().len() < 32 * 1024);
    }

    #[test]
    fn system_indices_and_their_aliases_stay_hidden_with_their_fields() {
        let mappings = json!({
            "games":{"mappings":{"properties":{"genre":{"type":"keyword"}}}},
            ".plugins-ml-config":{"mappings":{"properties":{"secret":{"type":"keyword"}}}},
            ".custom":{"mappings":{"properties":{"hidden":{"type":"keyword"}}}}
        });
        let aliases = json!({
            "games":{"aliases":{"public":{},".hidden":{}}},
            ".plugins-ml-config":{"aliases":{"system-alias":{},"public":{}}}
        });
        let schema = indices(&url("http://localhost:9200/"), &mappings, &aliases);
        let paths = schema["paths"].as_array().unwrap();
        assert!(paths.contains(&json!("/games/_search")));
        assert!(paths.contains(&json!("/public/_search")));
        assert!(!paths.iter().any(|path| {
            let path = path.as_str().unwrap();
            path.starts_with("/.") || path.starts_with("/system-alias")
        }));
        let bodies = schema["bodySchemas"].to_string();
        assert!(bodies.contains("genre"));
        assert!(!bodies.contains("secret") && !bodies.contains("hidden"));
    }

    #[test]
    fn a_small_index_gives_a_small_schema() {
        let schema = indices(
            &url("http://localhost:9200/"),
            &json!({"games":{"mappings":{"properties":{
                "title":{"type":"text"}, "genre":{"type":"keyword"}, "year":{"type":"integer"}
            }}}}),
            &json!({}),
        );
        let bytes = serde_json::to_vec(&schema).unwrap().len();
        assert!(bytes < 100 * 1024, "{bytes} bytes");
    }
}
