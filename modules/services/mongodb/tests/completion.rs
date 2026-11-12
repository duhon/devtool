#![allow(dead_code)]
mod common;

use common::*;
use serde_json::{Value, json};
use services_sdk::wire::event::Kind;

const HEAD: &str =
    "#target\nmongodb://h/gamelib\n#collection\nstores\n#action\naggregate\n#payload\n";

/// The user's pipeline: stores with the inventory they hold, counted and priced.
const PIPELINE: &str = r#"{"pipeline": [
  {"$lookup": {"from": "inventory", "let": {"sid": "$storeId"},
    "pipeline": [{"$match": {"$expr": {"$eq": ["$storeId", "$$sid"]}}}], "as": "available"}},
  {"$addFields": {"count": {"$size": "$available"}, "total": {"$sum": "$available.price"}}},
  {"$project": {"name": 1, "avg": {"$round": [{"$divide": ["$total", "$count"]}, 2]}}}
]}"#;

fn seeded(payload: &str) -> Harness {
    let mut module = Harness::default();
    let plain = format!("{HEAD}{payload}");
    let cmds = module.send(Kind::Document(check(&plain)));
    let tag = cmds
        .iter()
        .find(|cmd| cmd.name == "driver.call")
        .expect("a listing")
        .tag
        .clone();
    let list = json!({"documents": [
        {"name": "stores", "type": "collection"},
        {"name": "inventory", "type": "collection"},
        {"name": "orders", "type": "collection"}], "count": 3, "truncated": false});
    let cmds = module.reply(&tag, list);
    for cmd in cmds.iter().filter(|cmd| cmd.name == "driver.call") {
        let request = &cmd.args()["request"];
        let documents = match request["command"]["find"].as_str() {
            Some("stores") => json!([{"_id": 1, "name": "Main", "storeId": 7, "region": "eu"}]),
            Some("inventory") => json!([{"sku": "a", "storeId": 7, "price": 3, "stock": 2}]),
            other => panic!("unexpected sample {other:?}"),
        };
        module.reply(
            &cmd.tag,
            json!({"documents": documents, "count": 1, "truncated": false}),
        );
    }
    module
}

/// The items offered at `|` of the payload as (label, insert).
fn offer(module: &mut Harness, payload: &str) -> Vec<(String, String)> {
    let text = format!("{HEAD}{payload}");
    let pos = text.find('|').unwrap();
    let text = text.replacen('|', "", 1);
    module
        .complete(&text, pos)
        .items
        .into_iter()
        .map(|item| (item.label, item.insert))
        .collect()
}

fn labels(module: &mut Harness, payload: &str) -> Vec<String> {
    offer(module, payload)
        .into_iter()
        .map(|(label, _)| label)
        .collect()
}

fn insert_of(items: &[(String, String)], label: &str) -> String {
    items
        .iter()
        .find(|(name, _)| name == label)
        .unwrap_or_else(|| panic!("{label} in {items:?}"))
        .1
        .clone()
}

#[test]
fn values_name_the_collections_and_the_fields_they_join() {
    let mut module = seeded(PIPELINE);
    let from = labels(&mut module, r#"[{"$lookup": {"from": "|"}}]"#);
    assert_eq!(from, ["inventory", "orders", "stores"]);
    let local = labels(
        &mut module,
        r#"[{"$lookup": {"from": "inventory", "localField": "|"}}]"#,
    );
    assert!(local.contains(&"storeId".into()) && local.contains(&"region".into()));
    assert!(!local.contains(&"sku".into()), "{local:?}");
    let foreign = labels(
        &mut module,
        r#"[{"$lookup": {"from": "inventory", "foreignField": "|"}}]"#,
    );
    assert!(
        foreign.contains(&"sku".into()) && !foreign.contains(&"region".into()),
        "{foreign:?}"
    );
    let later = labels(
        &mut module,
        r#"[{"$lookup": {"foreignField": "|", "from": "inventory"}}]"#,
    );
    assert!(later.contains(&"sku".into()), "{later:?}");
    let out = labels(&mut module, r#"[{"$out": "|"}]"#);
    assert!(out.contains(&"orders".into()));
}

#[test]
fn field_paths_follow_the_stages_before_the_cursor() {
    let mut module = seeded(PIPELINE);
    let first = labels(&mut module, r#"{"pipeline": [{"$group": {"_id": "$|"}}]}"#);
    assert!(
        first.contains(&"$region".into()) && first.contains(&"$$ROOT".into()),
        "{first:?}"
    );
    assert!(!first.contains(&"$sku".into()));
    let added = labels(
        &mut module,
        r#"{"pipeline": [{"$lookup": {"from": "inventory", "localField": "storeId", "foreignField": "storeId", "as": "available"}},
          {"$addFields": {"count": {"$size": "$available"}}}, {"$project": {"n": "$|"}}]}"#,
    );
    for path in ["$available", "$available.price", "$count", "$name"] {
        assert!(added.contains(&path.into()), "{path} in {added:?}");
    }
    let projected = labels(
        &mut module,
        r#"{"pipeline": [{"$project": {"name": 1}}, {"$addFields": {"x": "$|"}}]}"#,
    );
    assert!(
        projected.contains(&"$name".into()) && !projected.contains(&"$region".into()),
        "{projected:?}"
    );
    let grouped = labels(
        &mut module,
        r#"{"pipeline": [{"$group": {"_id": "$region", "n": {"$sum": 1}}}, {"$sort": {"|": 1}}]}"#,
    );
    assert!(
        grouped.contains(&"n".into()) && !grouped.contains(&"name".into()),
        "{grouped:?}"
    );
}

#[test]
fn a_lookup_pipeline_reads_the_foreign_collection_and_its_variables() {
    let mut module = seeded(PIPELINE);
    let inner = r#"[{"$lookup": {"from": "inventory", "let": {"sid": "$storeId"}, "pipeline": [{"$match": {PLACE}}], "as": "a"}}]"#;
    let keys = labels(&mut module, &inner.replace("PLACE", r#""|""#));
    assert!(
        keys.contains(&"sku".into()) && keys.contains(&"$expr".into()),
        "{keys:?}"
    );
    assert!(!keys.contains(&"region".into()), "{keys:?}");
    let args = labels(
        &mut module,
        &inner.replace("PLACE", r#""$expr": {"$eq": ["$|", "$$sid"]}"#),
    );
    assert!(
        args.contains(&"$sku".into()) && args.contains(&"$$sid".into()),
        "{args:?}"
    );
    assert!(!args.contains(&"$region".into()), "{args:?}");
    let operators = labels(&mut module, &inner.replace("PLACE", r#""$expr": {"|"}"#));
    assert!(
        operators.contains(&"$eq".into()) && operators.contains(&"$and".into()),
        "{operators:?}"
    );
    let other = labels(
        &mut module,
        &inner
            .replace("PLACE", r#""$expr": {"$eq": ["$|"]}"#)
            .replace("$storeId", "$x"),
    );
    assert!(other.contains(&"$$sid".into()));
    let union = labels(
        &mut module,
        r#"[{"$unionWith": {"coll": "inventory", "pipeline": [{"$match": {"|"}}]}}]"#,
    );
    assert!(
        union.contains(&"sku".into()) && !union.contains(&"region".into()),
        "{union:?}"
    );
    let facet = labels(&mut module, r#"[{"$facet": {"a": [{"$match": {"|"}}]}}]"#);
    assert!(facet.contains(&"region".into()), "{facet:?}");
}

#[test]
fn expressions_and_finite_values_are_offered() {
    let mut module = seeded(PIPELINE);
    let round = offer(
        &mut module,
        r#"{"pipeline": [{"$project": {"avg": {"|"}}}]}"#,
    );
    assert_eq!(insert_of(&round, "$round"), "$round\": [\"$0\", 2]");
    assert!(round.iter().any(|(label, _)| label == "$sum"));
    let cond = insert_of(
        &offer(&mut module, r#"{"pipeline": [{"$project": {"a": {"|"}}}]}"#),
        "$cond",
    );
    assert!(cond.contains("\"if\"") && cond.contains("\"then\"") && cond.contains("\"else\""));
    let sort = labels(&mut module, r#"{"pipeline": [{"$sort": {"name": |}}]}"#);
    assert_eq!(sort, ["1", "-1"]);
    let types = labels(
        &mut module,
        r#"{"pipeline": [{"$match": {"name": {"$type": "|"}}}]}"#,
    );
    assert!(types.contains(&"string".into()) && types.contains(&"objectId".into()));
    let exists = labels(
        &mut module,
        r#"{"pipeline": [{"$match": {"name": {"$exists": |}}}]}"#,
    );
    assert_eq!(exists, ["true", "false"]);
    let project = labels(&mut module, r#"{"pipeline": [{"$project": {"name": |}}]}"#);
    assert!(project.contains(&"1".into()) && project.contains(&"false".into()));
    let upsert = labels(&mut module, "{\"upsert\": |}");
    assert_eq!(upsert, ["true", "false"]);
}

#[test]
fn picking_a_stage_inserts_its_skeleton() {
    let mut module = seeded("");
    let stages = offer(&mut module, r#"{"pipeline": [|]}"#);
    assert_eq!(
        insert_of(&stages, "$lookup"),
        "{\n  \"$lookup\": {\n    \"from\": \"$0\",\n    \"localField\": \"\",\n    \"foreignField\": \"\",\n    \"as\": \"\"\n  }\n}"
    );
    assert_eq!(
        insert_of(&stages, "$lookup (pipeline)"),
        "{\n  \"$lookup\": {\n    \"from\": \"$0\",\n    \"let\": {},\n    \"pipeline\": [],\n    \"as\": \"\"\n  }\n}"
    );
    assert_eq!(
        insert_of(&stages, "$group"),
        "{\n  \"$group\": {\n    \"_id\": \"$0\"\n  }\n}"
    );
    assert_eq!(
        insert_of(&stages, "$match"),
        "{\n  \"$match\": {\n    $0\n  }\n}"
    );
    assert_eq!(
        insert_of(&stages, "$unwind"),
        "{\n  \"$unwind\": \"$$0\"\n}"
    );
    let inside = offer(&mut module, r#"{"pipeline": [{"$gr|"}]}"#);
    assert_eq!(
        insert_of(&inside, "$group"),
        "$group\": {\n  \"_id\": \"$0\"\n}"
    );
    let keyed = offer(&mut module, r#"{"pipeline": [{"|": {}}]}"#);
    assert_eq!(insert_of(&keyed, "$lookup"), "$lookup");
    let shape = offer(&mut module, r#"{"pipeline": [{"$lookup": {|}}]}"#);
    assert_eq!(insert_of(&shape, "from"), r#""from": "$0""#);
}

#[test]
fn blocks_follow_the_indentation_of_the_line() {
    let mut module = seeded("");
    let nested = offer(&mut module, "{\n  \"pipeline\": [\n    |\n  ]\n}");
    assert_eq!(
        insert_of(&nested, "$match"),
        "{\n      \"$match\": {\n        $0\n      }\n    }"
    );
    let key = offer(
        &mut module,
        "{\n  \"pipeline\": [\n    {\n      \"|\"\n    }\n  ]\n}",
    );
    assert_eq!(
        insert_of(&key, "$match"),
        "$match\": {\n        $0\n      }"
    );
    let filter = offer(&mut module, "{\n  |\n}");
    assert_eq!(
        insert_of(&filter, "pipeline"),
        "\"pipeline\": [\n    $0\n  ]"
    );
}

#[test]
fn an_empty_payload_starts_with_the_shape_of_the_action() {
    let mut module = seeded("");
    for (action, label, insert) in [
        (
            "aggregate",
            r#"{"pipeline": []}"#,
            "{\n  \"pipeline\": [\n    $0\n  ]\n}",
        ),
        (
            "find",
            r#"{"filter": {}}"#,
            "{\n  \"filter\": {\n    $0\n  }\n}",
        ),
        (
            "update",
            r#"{"filter": {}, "update": {"$set": {}}}"#,
            "{\n  \"filter\": {\n    $0\n  },\n  \"update\": {\n    \"$set\": {}\n  }\n}",
        ),
        (
            "insert",
            r#"{"documents": [{}]}"#,
            "{\n  \"documents\": [\n    {\n      $0\n    }\n  ]\n}",
        ),
        ("distinct", r#"{"key": ""}"#, "{\n  \"key\": \"$0\"\n}"),
    ] {
        let text = format!(
            "#target\nmongodb://h/gamelib\n#collection\nstores\n#action\n{action}\n#payload\n"
        );
        let items = module.complete(&text, text.len()).items;
        let found = items.iter().find(|item| item.label == label);
        assert_eq!(
            found.map(|item| item.insert.as_str()),
            Some(insert),
            "{action}: {items:?}"
        );
    }
}

#[test]
fn keys_of_the_filter_are_the_sampled_fields() {
    let mut module = seeded("");
    let find = "#target\nmongodb://h/gamelib\n#collection\nstores\n#action\nfind\n#payload\n";
    let text = format!("{find}{{\"filter\": {{\"|\"}}}}");
    let pos = text.find('|').unwrap();
    let items = module.complete(&text.replacen('|', "", 1), pos).items;
    let names: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();
    assert!(
        names.contains(&"name") && names.contains(&"$or"),
        "{names:?}"
    );
}

#[test]
fn only_listed_collections_are_sampled_and_at_most_four() {
    let mut module = Harness::default();
    let payload = r#"{"pipeline": [{"$lookup": {"from": "ghost"}}, {"$lookup": {"from": "inventory"}},
        {"$lookup": {"from": "a"}}, {"$lookup": {"from": "b"}}, {"$lookup": {"from": "c"}}, {"$lookup": {"from": "d"}}]}"#;
    let cmds = module.send(Kind::Document(check(&format!("{HEAD}{payload}"))));
    let tag = cmds
        .iter()
        .find(|cmd| cmd.name == "driver.call")
        .unwrap()
        .tag
        .clone();
    let names = ["stores", "inventory", "a", "b", "c", "d"]
        .map(|name| json!({"name": name, "type": "collection"}));
    let cmds = module.reply(
        &tag,
        json!({"documents": names, "count": 6, "truncated": false}),
    );
    let sampled: Vec<Value> = cmds
        .iter()
        .filter(|cmd| cmd.name == "driver.call")
        .map(|cmd| cmd.args()["request"]["command"]["find"].clone())
        .collect();
    assert_eq!(sampled, [json!("stores"), json!("inventory"), json!("a")]);
}
