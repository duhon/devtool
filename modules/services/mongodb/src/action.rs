//! The command a check runs: `#action` names it, `#collection` is what it works on and
//! `#payload` holds its fields.

use crate::value::{self, as_f64};
use serde_json::{Map, Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Find,
    Aggregate,
    CountDocuments,
    Insert,
    Update,
    Delete,
    Indexes,
    /// A collection command that takes its fields as written; `renames` are friendly names.
    Collection(&'static [(&'static str, &'static str)]),
    /// A database command: its value is `1`.
    Database,
    Raw,
}

const FILTER_AS_QUERY: &[(&str, &str)] = &[("filter", "query")];
const MODIFY_RENAMES: &[(&str, &str)] = &[("filter", "query"), ("projection", "fields")];

const ACTIONS: [(&str, Kind, &str); 25] = [
    ("find", Kind::Find, "Documents of a collection"),
    ("aggregate", Kind::Aggregate, "An aggregation pipeline"),
    (
        "count",
        Kind::Collection(FILTER_AS_QUERY),
        "Count with the count command",
    ),
    (
        "countDocuments",
        Kind::CountDocuments,
        "Count through an aggregation",
    ),
    (
        "distinct",
        Kind::Collection(FILTER_AS_QUERY),
        "Distinct values of a key",
    ),
    ("insert", Kind::Insert, "Insert documents"),
    ("update", Kind::Update, "Update documents"),
    ("delete", Kind::Delete, "Delete documents"),
    (
        "findAndModify",
        Kind::Collection(MODIFY_RENAMES),
        "Modify and return one document",
    ),
    ("createIndexes", Kind::Indexes, "Create indexes"),
    ("dropIndexes", Kind::Collection(&[]), "Drop indexes"),
    (
        "listIndexes",
        Kind::Collection(&[]),
        "Indexes of a collection",
    ),
    ("drop", Kind::Collection(&[]), "Drop a collection"),
    ("create", Kind::Collection(&[]), "Create a collection"),
    (
        "collStats",
        Kind::Collection(&[]),
        "Statistics of a collection",
    ),
    (
        "listCollections",
        Kind::Database,
        "Collections of the database",
    ),
    ("listDatabases", Kind::Database, "Databases of the server"),
    ("ping", Kind::Database, "Check the connection"),
    ("dbStats", Kind::Database, "Statistics of the database"),
    ("serverStatus", Kind::Database, "Status of the server"),
    ("buildInfo", Kind::Database, "Version of the server"),
    ("hello", Kind::Database, "The server's role and topology"),
    (
        "connectionStatus",
        Kind::Database,
        "Authenticated users and roles",
    ),
    ("dropDatabase", Kind::Database, "Drop the database"),
    (
        "runCommand",
        Kind::Raw,
        "The payload is the whole command document",
    ),
];

/// Actions with a collection, then those without one.
pub fn actions(with_collection: bool) -> impl Iterator<Item = (&'static str, &'static str)> {
    ACTIONS
        .iter()
        .filter(move |(_, kind, _)| {
            matches!(kind, Kind::Raw) || with_collection != matches!(kind, Kind::Database)
        })
        .map(|(name, _, detail)| (*name, *detail))
}

pub fn names() -> impl Iterator<Item = &'static str> {
    ACTIONS.iter().map(|(name, _, _)| *name)
}

/// How the answer of a statement is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    /// The documents of a cursor, the reply of anything else.
    Auto,
    /// A count: no documents mean zero.
    Count,
    /// The documents in a field of the reply.
    Rows(&'static str),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Statement {
    pub command: Value,
    /// The database to run in, when not the check's.
    pub database: Option<String>,
    pub view: View,
}

pub fn int_value(n: i64) -> Value {
    match i32::try_from(n) {
        Ok(n) => json!({"$numberInt": n.to_string()}),
        Err(_) => json!({"$numberLong": n.to_string()}),
    }
}

pub fn names_command() -> Value {
    json!({"listCollections": 1, "nameOnly": true, "authorizedCollections": true})
}

/// The statement of a check; the three texts are trimmed or empty.
pub fn build(collection: &str, action: &str, payload: &str) -> Result<Statement, String> {
    if action.is_empty() {
        return Err("An #action is required, for example find".into());
    }
    let Some((name, kind, _)) = ACTIONS
        .iter()
        .find(|(name, _, _)| name.eq_ignore_ascii_case(action))
    else {
        return Err(format!(
            "Unknown action '{action}'. Supported: {}; any other command goes through runCommand",
            names().collect::<Vec<_>>().join(", ")
        ));
    };
    let payload = match payload {
        "" => None,
        text => Some(value::parse(text).map_err(|error| format!("Invalid payload: {error}"))?),
    };
    if !matches!(kind, Kind::Database | Kind::Raw) && collection.is_empty() {
        return Err(format!("{name} needs a #collection"));
    }
    let target = if matches!(kind, Kind::Database) {
        json!(1)
    } else {
        json!(collection)
    };
    let plain = |command: Map<String, Value>| Statement {
        command: Value::Object(command),
        database: None,
        view: View::Auto,
    };
    let command = |fields: Map<String, Value>| {
        let mut command = Map::from_iter([((*name).to_owned(), target.clone())]);
        command.extend(fields.into_iter().filter(|(key, _)| key != name));
        command
    };
    match kind {
        Kind::Raw => match payload {
            Some(Value::Object(map)) if !map.is_empty() => {
                Ok(plain(map))
            }
            _ => Err("runCommand takes the command document as the payload, with the command name as its first key".into()),
        },
        Kind::Database => {
            let mut statement = plain(command(fields(name, payload, None)?));
            if *name == "listDatabases" {
                statement.database = Some("admin".into());
                statement.view = View::Rows("databases");
            }
            Ok(statement)
        }
        Kind::Find | Kind::Collection(_) => {
            let renames: &[(&str, &str)] = match kind {
                Kind::Collection(renames) => renames,
                _ => &[],
            };
            let mut map = fields(name, payload, None)?;
            for (friendly, native) in renames {
                if let Some(value) = map.remove(*friendly) {
                    if map.contains_key(*native) {
                        return Err(format!("Use either {friendly} or {native}, not both"));
                    }
                    map.insert((*native).into(), value);
                }
            }
            if *name == "distinct" && !map.contains_key("key") {
                return Err("distinct needs a \"key\" in the payload".into());
            }
            if *name == "findAndModify" && !map.contains_key("update") && !map.contains_key("remove")
            {
                return Err("findAndModify needs \"update\" or \"remove\" in the payload".into());
            }
            Ok(plain(command(map)))
        }
        Kind::Aggregate => {
            let mut map = fields(name, payload, Some("pipeline"))?;
            if !matches!(map.get("pipeline"), Some(Value::Array(_))) {
                return Err("aggregate needs a \"pipeline\" array (or the array as the payload)".into());
            }
            map.entry("cursor").or_insert_with(|| json!({}));
            Ok(plain(command(map)))
        }
        Kind::CountDocuments => {
            let mut map = fields(name, payload, None)?;
            if let Some(key) = map.keys().find(|key| !["filter", "skip", "limit"].contains(&key.as_str())) {
                return Err(format!("countDocuments takes filter, skip and limit, not {key}"));
            }
            let mut pipeline = Vec::new();
            if let Some(filter) = map.remove("filter") {
                pipeline.push(json!({"$match": filter}));
            }
            for stage in ["skip", "limit"] {
                if let Some(n) = map.remove(stage)
                    && as_f64(&n) != Some(0.0)
                {
                    pipeline.push(json!({ format!("${stage}"): n }));
                }
            }
            pipeline.push(json!({"$group": {"_id": int_value(1), "n": {"$sum": int_value(1)}}}));
            let mut statement = plain(Map::from_iter([
                ("aggregate".to_owned(), target),
                ("pipeline".to_owned(), Value::Array(pipeline)),
                ("cursor".to_owned(), json!({})),
            ]));
            statement.view = View::Count;
            Ok(statement)
        }
        Kind::Insert => {
            let mut map = match payload {
                Some(Value::Array(documents)) => Map::from_iter([("documents".into(), Value::Array(documents))]),
                Some(Value::Object(map)) if map.contains_key("documents") => map,
                Some(Value::Object(document)) => Map::from_iter([("documents".into(), json!([document]))]),
                _ => return Err("insert takes {\"documents\": […]}, an array of documents or one document".into()),
            };
            if !matches!(map.get("documents"), Some(Value::Array(items)) if !items.is_empty()) {
                return Err("insert needs at least one document in \"documents\"".into());
            }
            map.remove(*name);
            Ok(plain(command(map)))
        }
        Kind::Update => {
            let mut map = fields(name, payload, None)?;
            if !map.contains_key("updates") {
                let spec = statement_spec(
                    &mut map,
                    &[("filter", "q"), ("q", "q"), ("update", "u"), ("u", "u")],
                    &["upsert", "multi", "arrayFilters", "collation", "hint", "c"],
                )?;
                for key in ["q", "u"] {
                    if !spec.contains_key(key) {
                        let friendly = if key == "q" { "filter" } else { "update" };
                        return Err(format!("update needs \"{friendly}\" in the payload"));
                    }
                }
                map.insert("updates".into(), json!([spec]));
            }
            Ok(plain(command(map)))
        }
        Kind::Delete => {
            let mut map = fields(name, payload, None)?;
            if !map.contains_key("deletes") {
                let mut spec = statement_spec(
                    &mut map,
                    &[("filter", "q"), ("q", "q")],
                    &["limit", "collation", "hint"],
                )?;
                if !spec.contains_key("q") {
                    return Err("delete needs \"filter\" in the payload; use {} to match every document".into());
                }
                spec.entry("limit").or_insert_with(|| int_value(1));
                map.insert("deletes".into(), json!([spec]));
            }
            Ok(plain(command(map)))
        }
        Kind::Indexes => {
            let mut map = match payload {
                Some(Value::Array(specs)) => Map::from_iter([("indexes".into(), Value::Array(specs))]),
                Some(Value::Object(map)) if map.contains_key("indexes") => map,
                Some(Value::Object(spec)) if spec.contains_key("key") => {
                    Map::from_iter([("indexes".into(), json!([spec]))])
                }
                _ => return Err("createIndexes takes {\"indexes\": […]}, an array of index specs or one spec with a \"key\"".into()),
            };
            let Some(Value::Array(specs)) = map.get_mut("indexes") else {
                return Err("\"indexes\" must be an array".into());
            };
            for spec in specs {
                let Some(spec) = spec.as_object_mut() else {
                    return Err("An index spec is an object with a \"key\"".into());
                };
                let Some(Value::Object(keys)) = spec.get("key") else {
                    return Err("An index spec needs a \"key\" object, for example {\"key\": {\"name\": 1}}".into());
                };
                if !spec.contains_key("name") {
                    let name = index_name(keys);
                    spec.insert("name".into(), json!(name));
                }
            }
            Ok(plain(command(map)))
        }
    }
}

/// The payload's fields; an array is taken as `shorthand`.
fn fields(
    action: &str,
    payload: Option<Value>,
    shorthand: Option<&str>,
) -> Result<Map<String, Value>, String> {
    match (payload, shorthand) {
        (None, _) => Ok(Map::new()),
        (Some(Value::Object(map)), _) => Ok(map),
        (Some(array @ Value::Array(_)), Some(key)) => Ok(Map::from_iter([(key.into(), array)])),
        _ => Err(format!("The payload of {action} is a JSON object")),
    }
}

/// Moves the friendly or native `names` and the `options` of a statement out of `map`.
fn statement_spec(
    map: &mut Map<String, Value>,
    names: &[(&str, &str)],
    options: &[&str],
) -> Result<Map<String, Value>, String> {
    let mut spec = Map::new();
    for (given, native) in names {
        if let Some(value) = map.remove(*given)
            && spec.insert((*native).into(), value).is_some()
        {
            return Err(format!(
                "Use either the long or the short name of \"{native}\", not both"
            ));
        }
    }
    for option in options {
        if let Some(value) = map.remove(*option) {
            spec.insert((*option).into(), value);
        }
    }
    Ok(spec)
}

/// The default index name: each key and its direction joined with `_`.
fn index_name(keys: &Map<String, Value>) -> String {
    keys.iter()
        .map(|(key, direction)| {
            let direction = match direction {
                Value::String(kind) => kind.clone(),
                other => as_f64(other).map_or_else(|| "1".into(), |n| format!("{n}")),
            };
            format!("{key}_{direction}")
        })
        .collect::<Vec<_>>()
        .join("_")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(collection: &str, action: &str, payload: &str) -> Value {
        build(collection, action, payload)
            .unwrap_or_else(|error| panic!("{action} {payload}: {error}"))
            .command
    }

    fn fails(collection: &str, action: &str, payload: &str) -> String {
        build(collection, action, payload).unwrap_err()
    }

    fn int(n: i64) -> Value {
        int_value(n)
    }

    #[test]
    fn find_takes_its_fields_as_written() {
        assert_eq!(
            command(
                "users",
                "find",
                r#"{"filter": {"age": {"$gt": 21}}, "sort": {"name": 1}, "limit": 10}"#
            ),
            json!({"find": "users", "filter": {"age": {"$gt": int(21)}},
                "sort": {"name": int(1)}, "limit": int(10)})
        );
        assert_eq!(command("users", "find", ""), json!({"find": "users"}));
        assert_eq!(command("users", "FIND", "{}"), json!({"find": "users"}));
        let first = command("u", "find", r#"{"find": "other", "limit": 1}"#);
        assert_eq!(first, json!({"find": "u", "limit": int(1)}));
    }

    #[test]
    fn aggregate_takes_an_object_or_the_pipeline() {
        let expected =
            json!({"aggregate": "a", "pipeline": [{"$match": {"x": int(1)}}], "cursor": {}});
        assert_eq!(
            command("a", "aggregate", r#"{"pipeline": [{"$match": {"x": 1}}]}"#),
            expected
        );
        assert_eq!(
            command("a", "aggregate", r#"[{"$match": {"x": 1}}]"#),
            expected
        );
        assert!(fails("a", "aggregate", "{}").contains("pipeline"));
    }

    #[test]
    fn counts() {
        let counted = build("a", "countDocuments", r#"{"filter": {"x": 1}, "limit": 5}"#).unwrap();
        assert_eq!(counted.view, View::Count);
        assert_eq!(
            counted.command["pipeline"],
            json!([{"$match": {"x": int(1)}}, {"$limit": int(5)},
                {"$group": {"_id": int(1), "n": {"$sum": int(1)}}}])
        );
        assert_eq!(
            command("a", "count", r#"{"filter": {"x": 1}}"#),
            json!({"count": "a", "query": {"x": int(1)}})
        );
        assert!(fails("a", "countDocuments", r#"{"sort": {}}"#).contains("not sort"));
        assert_eq!(
            command("a", "distinct", r#"{"key": "k", "filter": {}}"#),
            json!({"distinct": "a", "key": "k", "query": {}})
        );
        assert!(fails("a", "distinct", "{}").contains("key"));
    }

    #[test]
    fn insert_accepts_three_shapes() {
        let expected = json!({"insert": "a", "documents": [{"_id": int(1)}]});
        assert_eq!(
            command("a", "insert", r#"{"documents": [{"_id": 1}]}"#),
            expected
        );
        assert_eq!(command("a", "insert", r#"[{"_id": 1}]"#), expected);
        assert_eq!(command("a", "insert", r#"{"_id": 1}"#), expected);
        assert_eq!(
            command(
                "a",
                "insert",
                r#"{"documents": [{"a": 1}], "ordered": false}"#
            )["ordered"],
            false
        );
        assert!(fails("a", "insert", "[]").contains("at least one"));
    }

    #[test]
    fn update_and_delete_build_their_statements() {
        let update = command(
            "a",
            "update",
            r#"{"filter": {"a": 1}, "update": {"$set": {"b": 2}}, "upsert": true, "multi": true}"#,
        );
        assert_eq!(
            update,
            json!({"update": "a", "updates": [{"q": {"a": int(1)}, "u": {"$set": {"b": int(2)}},
                "upsert": true, "multi": true}]})
        );
        let short = command("a", "update", r#"{"q": {}, "u": {"$inc": {"n": 1}}}"#);
        assert_eq!(short["updates"][0]["q"], json!({}));
        assert!(fails("a", "update", r#"{"filter": {}}"#).contains("\"update\""));
        assert!(
            fails("a", "update", r#"{"filter": {}, "q": {}, "update": {}}"#).contains("not both")
        );
        assert_eq!(
            command("a", "delete", r#"{"filter": {"a": 1}}"#),
            json!({"delete": "a", "deletes": [{"q": {"a": int(1)}, "limit": int(1)}]})
        );
        assert_eq!(
            command("a", "delete", r#"{"filter": {}, "limit": 0}"#)["deletes"][0]["limit"],
            int(0)
        );
        assert!(fails("a", "delete", "{}").contains("filter"));
    }

    #[test]
    fn find_and_modify_and_indexes() {
        let modified = command(
            "a",
            "findAndModify",
            r#"{"filter": {"a": 1}, "update": {"$set": {"b": 1}}, "new": true, "projection": {"b": 1}}"#,
        );
        assert_eq!(modified["query"], json!({"a": int(1)}));
        assert_eq!(modified["fields"], json!({"b": int(1)}));
        assert_eq!(modified["new"], true);
        assert!(fails("a", "findAndModify", "{}").contains("remove"));
        let index = command(
            "a",
            "createIndexes",
            r#"{"key": {"a": 1, "b": -1}, "unique": true}"#,
        );
        assert_eq!(index["indexes"][0]["name"], "a_1_b_-1");
        assert_eq!(index["indexes"][0]["unique"], true);
        assert_eq!(
            command(
                "a",
                "createIndexes",
                r#"[{"key": {"a": "text"}, "name": "t"}]"#
            )["indexes"][0]["name"],
            "t"
        );
        assert!(fails("a", "createIndexes", r#"[{"name": "x"}]"#).contains("key"));
    }

    #[test]
    fn database_actions_and_raw_commands() {
        assert_eq!(command("", "ping", ""), json!({"ping": 1}));
        assert_eq!(
            command("ignored", "listCollections", r#"{"nameOnly": true}"#)["listCollections"],
            1
        );
        let databases = build("", "listDatabases", "").unwrap();
        assert_eq!(databases.database.as_deref(), Some("admin"));
        assert_eq!(databases.view, View::Rows("databases"));
        assert_eq!(
            command("", "runCommand", r#"{"dbStats": 1, "scale": 1024}"#),
            json!({"dbStats": int(1), "scale": int(1024)})
        );
        assert_eq!(command("a", "listIndexes", ""), json!({"listIndexes": "a"}));
    }

    #[test]
    fn problems_are_clear() {
        let unknown = fails("a", "frobnicate", "");
        assert!(
            unknown.contains("Unknown action 'frobnicate'") && unknown.contains("findAndModify")
        );
        assert!(unknown.contains("runCommand"));
        assert!(fails("", "find", "").contains("needs a #collection"));
        assert!(fails("a", "", "").contains("#action is required"));
        assert!(fails("a", "find", r#"{"limit": }"#).starts_with("Invalid payload: "));
        assert!(fails("a", "find", "[1]").contains("JSON object"));
        assert!(fails("", "runCommand", "{}").contains("command document"));
    }
}
