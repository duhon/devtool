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
            Some("stores") => json!([
                {"_id": 1, "name": "Main", "storeId": 7, "region": "eu", "tags": ["big", "open"], "stars": {"$numberInt": "5"}},
                {"_id": 2, "name": "Side", "storeId": 8, "region": "us", "tags": ["open"], "stars": {"$numberInt": "3"}}]),
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
    assert_eq!(sort[..2], ["1", "-1"]);
    let types = labels(
        &mut module,
        r#"{"pipeline": [{"$match": {"name": {"$type": "|"}}}]}"#,
    );
    assert!(types.contains(&"string".into()) && types.contains(&"objectId".into()));
    let exists = labels(
        &mut module,
        r#"{"pipeline": [{"$match": {"name": {"$exists": |}}}]}"#,
    );
    assert_eq!(exists[..2], ["true", "false"]);
    let project = labels(&mut module, r#"{"pipeline": [{"$project": {"name": |}}]}"#);
    assert!(project.contains(&"1".into()) && project.contains(&"false".into()));
    let upsert = labels(&mut module, "{\"upsert\": |}");
    assert_eq!(upsert[..2], ["true", "false"]);
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

const BLOCK: &str = "{\n  $0\n}";

#[test]
fn stage_values_offer_their_skeleton_and_the_fallbacks() {
    let mut module = seeded("");
    for stage in ["$addFields", "$set", "$facet", "$replaceRoot", "$match"] {
        let items = offer(
            &mut module,
            &format!(r#"{{"pipeline": [{{"{stage}": |}}]}}"#),
        );
        assert!(!items.is_empty(), "{stage}");
        let labels: Vec<&str> = items.iter().map(|(label, _)| label.as_str()).collect();
        for fallback in ["{}", "[]", "\"\""] {
            assert!(labels.contains(&fallback), "{stage}: {labels:?}");
        }
    }
    let lookup = offer(&mut module, "{\"pipeline\": [{\"$lookup\": |}]}");
    assert_eq!(
        insert_of(
            &lookup,
            "{\"from\": \"\", \"localField\": \"\", \"foreignField\": \"\", \"as\": \"\"}"
        ),
        "{\n  \"from\": \"$0\",\n  \"localField\": \"\",\n  \"foreignField\": \"\",\n  \"as\": \"\"\n}"
    );
    assert!(lookup.iter().any(
        |(label, insert)| label == "$lookup (pipeline)" && insert.contains("\"pipeline\": []")
    ));
    let facet = offer(
        &mut module,
        "{\n  \"pipeline\": [\n    {\n      \"$facet\": |\n    }\n  ]\n}",
    );
    assert_eq!(
        facet.iter().find(|(label, _)| label == "{}").unwrap().1,
        "{\n        $0\n      }"
    );
    let count = offer(&mut module, r#"{"pipeline": [{"$count": |}]}"#);
    assert_eq!(count[0], ("\"\"".to_owned(), "\"$0\"".to_owned()));
}

#[test]
fn filter_values_offer_blocks_and_literals() {
    let mut module = seeded("");
    let items = offer(&mut module, r#"{"pipeline": [{"$match": {"rating": |}}]}"#);
    assert_eq!(insert_of(&items, "{}"), BLOCK);
    assert_eq!(insert_of(&items, "[]"), "[\n  $0\n]");
    assert_eq!(insert_of(&items, "\"\""), "\"$0\"");
    for literal in ["true", "false", "null"] {
        insert_of(&items, literal);
    }
}

#[test]
fn array_operators_offer_an_array_block() {
    let mut module = seeded("");
    for key in ["$in", "$nin", "$all"] {
        let items = offer(
            &mut module,
            &format!(r#"{{"filter": {{"x": {{"{key}": |}}}}}}"#),
        );
        assert_eq!(items[0].1, "[\n  $0\n]", "{key}");
    }
    for key in ["$and", "$or", "$nor"] {
        let items = offer(&mut module, &format!(r#"{{"filter": {{"{key}": |}}}}"#));
        assert_eq!(items[0].1, "[\n  $0\n]", "{key}");
    }
}

#[test]
fn expression_operators_offer_their_container_before_paths() {
    let mut module = seeded("");
    let stage = |op: &str| format!(r#"{{"pipeline": [{{"$addFields": {{"n": {{"{op}": |}}}}}}]}}"#);
    let round = offer(&mut module, &stage("$round"));
    assert_eq!(round[0].1, "[\"$0\", 2]");
    assert!(round.iter().any(|(label, _)| label == "{}"));
    let divide = offer(&mut module, &stage("$divide"));
    assert_eq!(divide[0].1, "[\n  $0\n]");
    let cond = offer(&mut module, &stage("$cond"));
    assert!(cond[0].1.starts_with("{\n  \"if\": $0"), "{:?}", cond[0]);
    let size = offer(&mut module, &stage("$size"));
    assert_eq!(size[0].1, "\"$0\"");
    assert!(size.iter().any(|(label, _)| label == "$storeId"));
    assert_eq!(insert_of(&size, "{}"), BLOCK);
    let sum = offer(&mut module, &stage("$sum"));
    assert!(sum.iter().any(|(label, _)| label == "$region"));
    assert_eq!(insert_of(&sum, "[]"), "[\n  $0\n]");
}

#[test]
fn array_elements_offer_expression_blocks() {
    let mut module = seeded("");
    let items = offer(
        &mut module,
        r#"{"pipeline": [{"$addFields": {"n": {"$round": [|]}}}]}"#,
    );
    assert_eq!(insert_of(&items, "{}"), BLOCK);
    assert_eq!(insert_of(&items, "\"\""), "\"$0\"");
    assert!(items.iter().any(|(label, _)| label == "$region"));
}

#[test]
fn update_and_group_values_are_objects() {
    let mut module = seeded("");
    let set = offer(&mut module, r#"{"update": {"$set": |}}"#);
    assert_eq!(set[0].1, BLOCK);
    let group = offer(
        &mut module,
        r#"{"pipeline": [{"$group": {"_id": "$region", "total": |}}]}"#,
    );
    assert_eq!(group[0].1, BLOCK);
}

#[test]
fn typed_words_are_replaced_without_doubling_quotes_or_brackets() {
    let mut module = seeded("");
    let text = format!("{HEAD}{{\"pipeline\": [{{\"$mat\"}}]}}");
    let pos = text.find("$mat").unwrap() + 4;
    let reply = module.complete(&text, pos);
    let item = reply
        .items
        .iter()
        .find(|item| item.label == "$match")
        .unwrap();
    assert_eq!(item.insert, "$match\": {\n  $0\n}");
    assert_eq!((reply.from, reply.to), (pos as u32 - 4, pos as u32 + 1));

    let items = offer(&mut module, r#"{"pipeline": [{"$match": {"pi|"}}]}"#);
    assert!(items.iter().all(|(_, insert)| !insert.ends_with("\"\"")));

    let closed = offer(&mut module, r#"{"filter": {"x": {"$in": [|]}}}"#);
    assert_eq!(insert_of(&closed, "{}"), BLOCK);
    let value = offer(&mut module, r#"{"filter": {"x": "|"}}"#);
    assert!(
        value
            .iter()
            .all(|(_, insert)| !insert.starts_with(['{', '[']))
    );
}

#[test]
fn comparisons_offer_the_sampled_values() {
    let mut module = seeded("");
    let equality = offer(&mut module, r#"{"filter": {"region": |}}"#);
    assert_eq!(
        equality[..2],
        [
            ("\"eu\"".to_owned(), "\"eu\"".to_owned()),
            ("\"us\"".to_owned(), "\"us\"".to_owned())
        ]
    );
    assert!(equality.iter().any(|(label, _)| label == "{}"));
    let numbers = offer(&mut module, r#"{"filter": {"stars": {"$gte": |}}}"#);
    assert_eq!(
        numbers[..2],
        [("5".into(), "5".into()), ("3".into(), "3".into())]
    );
    let in_match = offer(
        &mut module,
        r#"{"pipeline": [{"$match": {"region": {"$ne": |}}}]}"#,
    );
    assert_eq!(in_match[0].1, "\"eu\"");
    let many = labels(&mut module, r#"{"filter": {"name": |}}"#);
    assert_eq!(many[..2], ["\"Main\"", "\"Side\""]);
    for skipped in ["_id", "unknown"] {
        let none = labels(&mut module, &format!(r#"{{"filter": {{"{skipped}": |}}}}"#));
        assert_eq!(none[0], "{}", "{skipped}");
    }
}

#[test]
fn lists_offer_the_sampled_values_of_the_field() {
    let mut module = seeded("");
    let tags = offer(&mut module, r#"{"filter": {"tags": {"$in": [|]}}}"#);
    assert_eq!(
        tags[..2],
        [
            ("\"big\"".into(), "\"big\"".into()),
            ("\"open\"".into(), "\"open\"".into())
        ]
    );
    assert!(tags.iter().any(|(label, _)| label == "{}"));
    let all = labels(&mut module, r#"{"filter": {"tags": {"$all": [|]}}}"#);
    assert_eq!(all[0], "\"big\"");
    let stars = labels(&mut module, r#"{"filter": {"stars": {"$nin": [|]}}}"#);
    assert_eq!(stars[..2], ["5", "3"]);
    let plain = labels(&mut module, r#"{"filter": {"tags": {"$size": |}}}"#);
    assert!(!plain.contains(&"\"big\"".to_owned()));
}

#[test]
fn typed_sampled_strings_keep_their_quotes() {
    let mut module = seeded("");
    let open = offer(&mut module, r#"{"filter": {"region": "e|"}}"#);
    assert_eq!(open[0], ("eu".into(), "eu".into()));
    let unclosed = offer(&mut module, "{\"filter\": {\"region\": \"e|");
    assert_eq!(unclosed[0], ("eu".into(), "eu\"".into()));
    let element = offer(&mut module, r#"{"filter": {"tags": {"$in": ["op|"]}}}"#);
    assert_eq!(element[0], ("open".into(), "open".into()));
}

/// The payload after each item is picked at `|`, with the cursor mark filled by `filler`.
fn picked(module: &mut Harness, payload: &str) -> Vec<(String, String, String)> {
    let text = format!("{HEAD}{payload}");
    let pos = text.find('|').unwrap();
    let text = text.replacen('|', "", 1);
    let reply = module.complete(&text, pos);
    reply
        .items
        .into_iter()
        .map(|item| {
            let result = format!(
                "{}{}{}",
                &text[..reply.from as usize],
                item.insert,
                &text[reply.to as usize..]
            );
            (item.label, item.insert, result[HEAD.len()..].to_owned())
        })
        .collect()
}

/// Every offered item leaves strict JSON, whatever fills the cursor.
fn all_valid(module: &mut Harness, payload: &str) -> Vec<(String, String, String)> {
    let items = picked(module, payload);
    assert!(!items.is_empty(), "{payload}");
    for (label, _, text) in &items {
        let valid = ["", "0"]
            .iter()
            .any(|filler| serde_json::from_str::<Value>(&text.replace("$0", filler)).is_ok());
        assert!(valid, "{label} gives invalid JSON:\n{text}");
    }
    items
}

#[test]
fn a_value_followed_by_nothing_gets_the_comma_with_the_next_member() {
    let mut module = seeded("");
    let after_string = all_valid(&mut module, r#"{"filter": {"tags": {"$in": ["big"|]}}}"#);
    let open = after_string
        .iter()
        .find(|(label, ..)| label == "\"open\"")
        .unwrap();
    assert_eq!(open.1, ",\n  \"open\"");
    assert!(after_string.iter().all(|(label, ..)| label != ","));
    assert!(after_string.iter().any(|(label, ..)| label == "{}"));

    let after_number = all_valid(&mut module, r#"{"filter": {"stars": 5|}}"#);
    let key = after_number
        .iter()
        .find(|(label, ..)| label == "$or")
        .unwrap();
    assert_eq!(key.1, ",\n  \"$or\": [\n    {\n      $0\n    }\n  ]");
    assert!(after_number.iter().all(|(label, ..)| label != ","));

    let after_object = all_valid(&mut module, "{\n  \"filter\": {\n    \"a\": {}\n  }|\n}");
    assert!(
        after_object
            .iter()
            .all(|(_, insert, _)| insert.starts_with(",\n  "))
    );

    let after_stage = all_valid(&mut module, "{\"pipeline\": [\n  {\"$match\": {}}|\n]}");
    let stage = after_stage
        .iter()
        .find(|(label, ..)| label == "$sort")
        .unwrap();
    assert_eq!(
        stage.1,
        ",\n  {\n    \"$sort\": {\n      \"$0\": 1\n    }\n  }"
    );
}

#[test]
fn a_member_typed_after_a_value_replaces_its_quotes() {
    let mut module = seeded("");
    let items = all_valid(&mut module, r#"{"filter": {"a": 1 "$o|"}}"#);
    let or = items.iter().find(|(label, ..)| label == "$or").unwrap();
    assert_eq!(
        or.2,
        "{\"filter\": {\"a\": 1,\n  \"$or\": [\n    {\n      $0\n    }\n  ]}}"
    );
    let open = picked(&mut module, "{\"filter\": {\"a\": 1 \"$o|");
    assert!(open.iter().any(|(label, ..)| label == "$or"));
}

#[test]
fn picks_before_an_existing_sibling_end_with_a_comma() {
    let mut module = seeded("");
    let stages = all_valid(
        &mut module,
        "{\"pipeline\": [\n  {\"$match\": {}},\n  |\n  {\"$sort\": {}}\n]}",
    );
    let (_, insert, _) = stages.iter().find(|(label, ..)| label == "$limit").unwrap();
    assert!(insert.ends_with("},"), "{insert}");
    let keys = all_valid(&mut module, "{\n  \"filter\": {},\n  |\n  \"limit\": 1\n}");
    assert!(keys.iter().all(|(_, insert, _)| insert.ends_with(',')));
    let last = all_valid(&mut module, "{\n  \"filter\": {},\n  |\n}");
    assert!(last.iter().all(|(_, insert, _)| !insert.ends_with(',')));
}

#[test]
fn an_existing_comma_or_an_empty_container_needs_none() {
    let mut module = seeded("");
    let before_comma = picked(
        &mut module,
        r#"{"filter": {"tags": {"$in": ["big"| , "open"]}}}"#,
    );
    assert!(before_comma.is_empty());
    for payload in [
        r#"{"filter": {"tags": {"$in": [|]}}}"#,
        "{\n  \"filter\": {|}\n}",
        "{\"filter\": {\"a\": 1, |}}",
    ] {
        let items = all_valid(&mut module, payload);
        assert!(
            items.iter().all(|(_, insert, _)| !insert.starts_with(',')),
            "{payload}"
        );
    }
}

#[test]
fn blanks_between_a_value_and_the_comma_are_replaced() {
    let mut module = seeded("");
    let items = all_valid(&mut module, r#"{"filter": {"stars": 5 |}}"#);
    let or = items.iter().find(|(label, ..)| label == "$or").unwrap();
    assert!(
        or.2.starts_with("{\"filter\": {\"stars\": 5,\n"),
        "{}",
        or.2
    );
}

#[test]
fn the_comma_sticks_to_the_value_whatever_lies_between() {
    let mut module = seeded("");
    let ends = "{\n  \"filter\": {\"tags\": {\"$in\": [\n    \"big\"|\n  ]}}\n}";
    let blank = "{\n  \"filter\": {\"tags\": {\"$in\": [\n    \"big\"\n    |\n  ]}}\n}";
    let trailing = "{\n  \"filter\": {\"tags\": {\"$in\": [\n    \"big\"\n    |  \n  ]}}\n}";
    let mut results = Vec::new();
    for payload in [ends, blank, trailing] {
        let items = all_valid(&mut module, payload);
        let open = items
            .iter()
            .find(|(label, ..)| label == "\"open\"")
            .expect("a sampled value");
        results.push(open.2.clone());
    }
    for result in &results {
        assert!(result.contains("\"big\",\n    \"open\"\n  ]"), "{result}");
    }
}

#[test]
fn keys_after_a_value_on_a_blank_line_keep_the_comma_glued() {
    let mut module = seeded("");
    let items = all_valid(&mut module, "{\n  \"pipeline\": []\n  |\n}");
    let limit = items
        .iter()
        .find(|(label, ..)| label == "allowDiskUse")
        .unwrap();
    assert!(
        limit
            .2
            .starts_with("{\n  \"pipeline\": [],\n  \"allowDiskUse\": "),
        "{}",
        limit.2
    );
    let before_sibling = all_valid(
        &mut module,
        "{\"pipeline\": [\n  {\"$match\": {}}\n  |\n  {\"$sort\": {}}\n]}",
    );
    assert!(
        before_sibling
            .iter()
            .all(|(_, _, text)| text.contains("{\"$match\": {}},\n"))
    );
}

#[test]
fn sampled_values_already_in_the_list_are_not_offered_again() {
    let mut module = seeded("");
    let strings = labels(&mut module, r#"{"filter": {"tags": {"$in": ["big"|]}}}"#);
    assert!(strings.contains(&"\"open\"".to_owned()));
    assert!(!strings.contains(&"\"big\"".to_owned()));
    let numbers = labels(&mut module, r#"{"filter": {"stars": {"$in": [5, |]}}}"#);
    assert!(numbers.contains(&"3".to_owned()));
    assert!(!numbers.contains(&"5".to_owned()));
}

#[test]
fn a_new_stage_after_a_closed_one_is_a_whole_element_with_the_comma_glued() {
    let mut module = seeded("");
    let base = "{\"pipeline\": [\n  {\"$match\": {}}\n  ";
    for typed in [
        "|",
        "{|",
        "{\"|",
        "{\"$ma|",
        "\"|",
        "\"$ma|",
        "{|}",
        "{\"|\"}",
        "{\"$ma|\"}",
    ] {
        let items = picked(&mut module, &format!("{base}{typed}\n]}}"));
        assert!(!items.is_empty(), "{typed}");
        assert_ne!(items[0].0, ",", "{typed}");
        let stage = items.iter().find(|(label, ..)| label == "$match").unwrap();
        assert!(
            stage
                .2
                .starts_with("{\"pipeline\": [\n  {\"$match\": {}},\n  {\n    \"$match\""),
            "{typed}: {}",
            stage.2
        );
        assert!(serde_json::from_str::<Value>(&stage.2.replace("$0", "")).is_ok());
    }
}

#[test]
fn literals_are_offered_in_every_element_and_expression_position() {
    let mut module = seeded("");
    for payload in [
        r#"{"pipeline": [{"$match": {"$expr": {"$eq": [|]}}}]}"#,
        r#"{"pipeline": [{"$match": {"$expr": {"$and": [|]}}}]}"#,
        r#"{"pipeline": [{"$project": {"a": {"$ifNull": ["$x", |]}}}]}"#,
        r#"{"pipeline": [{"$project": {"a": {"$gt": |}}}]}"#,
        r#"{"pipeline": [{"$project": {"a": {"$cond": [|]}}}]}"#,
        r#"{"filter": {"a": {"$in": [|]}}}"#,
    ] {
        let found = labels(&mut module, payload);
        for literal in ["true", "false", "null"] {
            assert!(
                found.contains(&literal.to_owned()),
                "{literal} in {payload}"
            );
        }
    }
}

#[test]
fn a_literal_after_a_value_carries_the_comma_and_replaces_the_typed_word() {
    let mut module = seeded("");
    let base = "{\"pipeline\": [{\"$match\": {\"$expr\": {\"$eq\": [\n  \"$storeId\"";
    for typed in ["|", "\n  |", " tr|", "\n  tr|", "\n  |  "] {
        let items = picked(&mut module, &format!("{base}{typed}\n]}}}}}}]}}"));
        let item = items
            .iter()
            .find(|(label, ..)| label == "true")
            .unwrap_or_else(|| panic!("true for {typed:?}"));
        assert!(
            item.2.contains("\"$storeId\",\n  true\n]"),
            "{typed:?}: {}",
            item.2
        );
        assert!(serde_json::from_str::<Value>(&item.2).is_ok());
    }
}

fn complete_at(module: &mut Harness, text: &str) -> Vec<(String, String)> {
    let pos = text.find('|').unwrap();
    module
        .complete(&text.replacen('|', "", 1), pos)
        .items
        .into_iter()
        .map(|item| (item.label, item.insert))
        .collect()
}

#[test]
fn metadata_has_a_marker_names_and_values() {
    let mut module = Harness::default();
    let marker = complete_at(&mut module, "#target\nmongodb://h/app\n#m|");
    assert!(
        marker.iter().any(|(label, _)| label == "#metadata"),
        "{marker:?}"
    );
    let next = complete_at(&mut module, "#target\nmongodb://h/app\n|");
    assert!(
        next.iter().any(|(label, _)| label == "#metadata"),
        "{next:?}"
    );

    let names = complete_at(&mut module, "#target\nmongodb://h/app\n#metadata\n|");
    let found: Vec<&str> = names.iter().map(|(label, _)| label.as_str()).collect();
    assert_eq!(
        found,
        ["Auth: x509", "Certificate: ", "CertificatePassword: "]
    );
    let used = complete_at(
        &mut module,
        "#target\nmongodb://h/app\n#metadata\nAuth: x509\n|",
    );
    assert!(
        !used.iter().any(|(label, _)| label.starts_with("Auth")),
        "{used:?}"
    );
    assert!(
        used.iter().any(|(label, _)| label == "Certificate: "),
        "{used:?}"
    );
}
