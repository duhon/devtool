#![allow(dead_code)]
mod common;

use common::*;
use serde_json::{Value, json};
use services_sdk::Cmd;

const TARGET: &str = "mongodb://app:s3cret%21@db1.example:27017,db2.example/shop?replicaSet=rs0";

fn call(cmds: &[Cmd]) -> Value {
    let call = cmds
        .iter()
        .find(|cmd| cmd.name == "driver.call")
        .unwrap_or_else(|| panic!("No driver call: {cmds:?}"));
    assert_eq!(call.args()["driver"], "mongodb");
    call.args()["request"].clone()
}

fn result(cmds: &[Cmd]) -> Value {
    cmds.iter()
        .find(|cmd| cmd.name == "run.result")
        .unwrap_or_else(|| panic!("No result: {cmds:?}"))
        .args()
}

fn started(module: &mut Harness, text: &str, vars: &[(&str, &str)]) -> Vec<Cmd> {
    let cmds = module.run(text);
    if cmds.iter().any(|cmd| cmd.name == "vars.resolve") {
        resolve(module, &cmds, vars)
    } else {
        cmds
    }
}

fn text(collection: &str, action: &str, payload: &str) -> String {
    format!("#target\n{TARGET}\n#collection\n{collection}\n#action\n{action}\n#payload\n{payload}")
}

fn answer(documents: Value) -> Value {
    let count = documents.as_array().map_or(0, Vec::len);
    json!({"documents": documents, "count": count, "truncated": false,
        "timing": {"firstByteUs": 900, "totalUs": 2500}})
}

#[test]
fn a_find_becomes_the_driver_request_and_a_table() {
    let mut module = Harness::default();
    let cmds = started(
        &mut module,
        &format!(
            "#target\n{TARGET}\n#collection\nusers\n#action\nfind\n#options\nMax-Documents: 50\nBatch-Size: 10\n#payload\n{{\"filter\": {{\"age\": {{\"$gt\": 21}}}}, \"sort\": {{\"name\": 1}}, \"limit\": 2}}"
        ),
        &[],
    );
    let request = call(&cmds);
    assert_eq!(request["uri"], TARGET);
    assert_eq!(request["database"], "shop");
    assert_eq!(request["maxDocuments"], 50);
    assert_eq!(request["batchSize"], 10);
    assert_eq!(request["timeoutMs"], 3_600_000);
    assert_eq!(
        request["command"],
        json!({"find": "users", "filter": {"age": {"$gt": {"$numberInt": "21"}}},
            "sort": {"name": {"$numberInt": "1"}}, "limit": {"$numberInt": "2"}})
    );
    let tag = cmds
        .iter()
        .find(|cmd| cmd.name == "driver.call")
        .unwrap()
        .tag
        .clone();
    let cmds = module.reply(
        &tag,
        answer(json!([
            {"_id": {"$oid": "65f0c0ffee0000000000000a"}, "name": "Ann", "age": {"$numberInt": "30"}},
            {"_id": {"$oid": "65f0c0ffee0000000000000b"}, "name": "Bob", "age": {"$numberInt": "41"}}
        ])),
    );
    let done = result(&cmds);
    assert_eq!(done["outcome"], "OK");
    let body = done["body"].as_str().unwrap();
    assert!(body.contains("ObjectId(\"65f0c0ffee0000000000000a\")") && body.contains("Bob"));
    assert!(cmds.iter().any(|cmd| {
        cmd.name == "status.set"
            && cmd.args()["text"]
                .as_str()
                .unwrap()
                .starts_with("2 documents")
    }));
    assert!(done["columns"].to_string().contains("number"));
}

#[test]
fn the_database_comes_from_the_uri_and_variables_are_resolved() {
    let mut module = Harness::default();
    let cmds = started(
        &mut module,
        "#target\nmongodb://{{HOST}}:27017/{{DB}}\n#collection\n{{COLL}}\n#action\ncountDocuments\n#payload\n{\"filter\": {\"n\": \"{{N}}\"}}",
        &[
            ("HOST", "h.example"),
            ("DB", "metrics"),
            ("COLL", "events"),
            ("N", "a b"),
        ],
    );
    let request = call(&cmds);
    assert_eq!(request["uri"], "mongodb://h.example:27017/metrics");
    assert_eq!(request["database"], "metrics");
    assert_eq!(request["command"]["aggregate"], "events");
    assert_eq!(
        request["command"]["pipeline"][0],
        json!({"$match": {"n": "a b"}})
    );
}

#[test]
fn problems_fail_the_run_with_a_reason() {
    let mut module = Harness::default();
    let run = |module: &mut Harness, text: &str| {
        let cmds = module.run(text);
        let cmds = if cmds.iter().any(|cmd| cmd.name == "vars.resolve") {
            resolve(module, &cmds, &[])
        } else {
            cmds
        };
        result(&cmds)
    };
    let failed = run(&mut module, &text("users", "find", "{\"limit\": }"));
    assert_eq!(failed["outcome"], "ERROR");
    assert!(
        failed["error"]
            .as_str()
            .unwrap()
            .starts_with("Invalid payload: ")
    );
    assert!(
        failed["error"]
            .as_str()
            .unwrap()
            .contains("(line 1, column")
    );
    let error = run(&mut module, &text("users", "frobnicate", ""))["error"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(error.contains("Unknown action 'frobnicate'") && error.contains("runCommand"));
    assert!(
        run(&mut module, &text("", "find", ""))["error"]
            .as_str()
            .unwrap()
            .contains("#collection")
    );
    assert!(
        run(&mut module, "#target\nhttp://x\n#action\nping")["error"]
            .as_str()
            .unwrap()
            .contains("mongodb://")
    );
    let options = format!("#target\n{TARGET}\n#options\nRetries: 3\n#action\nping");
    assert!(
        run(&mut module, &options)["error"]
            .as_str()
            .unwrap()
            .starts_with("Unknown option")
    );
}

#[test]
fn passwords_never_reach_results_debug_or_errors() {
    let mut module = Harness::default();
    let cmds = started(&mut module, &text("users", "find", ""), &[]);
    let tag = cmds
        .iter()
        .find(|cmd| cmd.name == "driver.call")
        .unwrap()
        .tag
        .clone();
    let failed = module.fail(
        &tag,
        &format!("Server selection failed for {TARGET}: authentication failed for s3cret!"),
    );
    let shown = result(&failed).to_string();
    assert!(!shown.contains("s3cret"), "{shown}");
    assert!(shown.contains("app:***@db1.example"));
    let cmds = started(&mut module, &text("users", "find", ""), &[]);
    let tag = cmds
        .iter()
        .find(|cmd| cmd.name == "driver.call")
        .unwrap()
        .tag
        .clone();
    let ok = module.reply(&tag, answer(json!([])));
    let text = result(&ok).to_string();
    assert!(!text.contains("s3cret"), "{text}");
    assert_eq!(result(&ok)["body"], "Empty set");
}

#[test]
fn a_server_error_shows_the_documents_read_before_it() {
    let mut module = Harness::default();
    let cmds = started(
        &mut module,
        &text("users", "aggregate", "[{\"$match\": {}}]"),
        &[],
    );
    let tag = cmds
        .iter()
        .find(|cmd| cmd.name == "driver.call")
        .unwrap()
        .tag
        .clone();
    let mut response = answer(json!([{"name": "Ann"}]));
    response["error"] =
        json!({"code": 13, "codeName": "Unauthorized", "message": "not authorized"});
    let done = result(&module.reply(&tag, response));
    assert_eq!(done["outcome"], "ERROR");
    let body = done["body"].as_str().unwrap();
    assert!(body.contains("Ann") && body.ends_with("ERROR 13 (Unauthorized): not authorized"));
}

fn at(text: &str) -> (String, usize) {
    let pos = text.find('|').unwrap();
    (text.replacen('|', "", 1), pos)
}

fn labels(module: &mut Harness, text: &str) -> Vec<String> {
    let (text, pos) = at(text);
    module
        .complete(&text, pos)
        .items
        .into_iter()
        .map(|item| item.label)
        .collect()
}

fn statement(collection: &str, action: &str, payload: &str) -> String {
    format!(
        "#target\nmongodb://h/app\n#collection\n{collection}\n#action\n{action}\n#payload\n{payload}"
    )
}

#[test]
fn static_completion_follows_the_sections() {
    let mut module = Harness::default();
    let actions = labels(
        &mut module,
        "#target\nmongodb://h/app\n#collection\nusers\n#action\n|",
    );
    assert!(actions.contains(&"find".into()) && actions.contains(&"runCommand".into()));
    assert!(!actions.contains(&"ping".into()), "{actions:?}");
    let database = labels(&mut module, "#target\nmongodb://h/app\n#action\n|");
    assert!(database.contains(&"ping".into()) && !database.contains(&"find".into()));
    let keys = labels(&mut module, &statement("users", "find", "{\"|}"));
    assert!(
        keys.contains(&"filter".into()) && keys.contains(&"limit".into()),
        "{keys:?}"
    );
    let update = labels(&mut module, &statement("users", "update", "{|}"));
    assert!(update.contains(&"upsert".into()), "{update:?}");
    let operators = labels(
        &mut module,
        &statement("users", "find", "{\"filter\": {\"age\": {$|}}}"),
    );
    assert!(operators.contains(&"$gt".into()), "{operators:?}");
    let stages = labels(
        &mut module,
        &statement("users", "aggregate", "{\"pipeline\": [{$gr|}]}"),
    );
    assert!(stages.contains(&"$group".into()), "{stages:?}");
    let shorthand = labels(&mut module, &statement("users", "aggregate", "[{\"|\"}]"));
    assert!(shorthand.contains(&"$match".into()), "{shorthand:?}");
    let set = labels(
        &mut module,
        &statement("users", "update", "{\"update\": {$|}}"),
    );
    assert!(set.contains(&"$set".into()), "{set:?}");
    let command = labels(&mut module, &statement("", "runCommand", "{|}"));
    assert!(command.contains(&"dbStats".into()), "{command:?}");
    let options = labels(
        &mut module,
        "#target\nmongodb://h/app\n#options\nMa|\n#payload\n{}",
    );
    assert!(
        options.contains(&"Max-Documents: 1000".into()),
        "{options:?}"
    );
    assert_eq!(
        labels(&mut module, "#target\nmongodb+|"),
        ["mongodb+srv://"]
    );
}

#[test]
fn discovery_feeds_collection_and_field_completion_and_is_cached() {
    let mut module = Harness::default();
    let text = "#target\nmongodb://h/app\n#collection\n|";
    let (plain, pos) = at(text);
    let cmds = module.hints(&plain, pos);
    let request = call(&cmds);
    assert_eq!(request["database"], "app");
    assert_eq!(request["command"]["listCollections"], 1);
    assert_eq!(request["command"]["nameOnly"], true);
    assert_eq!(request["timeoutMs"], 5000);
    assert!(
        module
            .hints(&plain, pos)
            .iter()
            .all(|cmd| cmd.name != "driver.call")
    );
    let tag = cmds
        .iter()
        .find(|cmd| cmd.name == "driver.call")
        .unwrap()
        .tag
        .clone();
    let done = module.reply(
        &tag,
        answer(json!([{"name": "orders", "type": "collection"}, {"name": "users", "type": "collection"}])),
    );
    assert!(done.iter().any(|cmd| cmd.name == "editor.relight"));
    assert_eq!(labels(&mut module, text), ["orders", "users"]);
    assert!(
        module
            .hints(&plain, pos)
            .iter()
            .all(|cmd| cmd.name != "driver.call")
    );
    let forced = module.send(services_sdk::wire::event::Kind::Discover(check(&plain)));
    assert_eq!(call(&forced)["command"]["listCollections"], 1);

    let text = statement("users", "find", "{\"filter\": {\"|\"}}");
    let (plain, pos) = at(&text);
    let cmds = module.hints(&plain, pos);
    let request = call(&cmds);
    assert_eq!(request["command"]["find"], "users");
    assert_eq!(request["command"]["limit"], 20);
    let tag = cmds
        .iter()
        .find(|cmd| cmd.name == "driver.call")
        .unwrap()
        .tag
        .clone();
    module.reply(
        &tag,
        answer(json!([{"_id": {"$oid": "65f0c0ffee0000000000000a"}, "name": "Ann", "address": {"city": "P"}}])),
    );
    let fields = labels(&mut module, &text);
    assert!(
        fields.contains(&"name".into()) && fields.contains(&"address.city".into()),
        "{fields:?}"
    );
    let sort = labels(
        &mut module,
        &statement("users", "find", "{\"sort\": {\"|\"}}"),
    );
    assert!(sort.contains(&"name".into()), "{sort:?}");
}

#[test]
fn discovery_fails_soft_and_waits_before_retrying() {
    let mut module = Harness::default();
    let (plain, pos) = at(&format!("#target\n{TARGET}\n#collection\n|"));
    let cmds = module.hints(&plain, pos);
    let tag = cmds
        .iter()
        .find(|cmd| cmd.name == "driver.call")
        .unwrap()
        .tag
        .clone();
    let failed = module.fail(&tag, &format!("cannot connect to {TARGET}"));
    let status = failed
        .iter()
        .find(|cmd| cmd.name == "status.set")
        .unwrap()
        .args();
    assert_eq!(status["indicator"], "error");
    assert!(
        failed
            .iter()
            .all(|cmd| !cmd.args().to_string().contains("s3cret"))
    );
    assert!(
        module
            .hints(&plain, pos)
            .iter()
            .all(|cmd| cmd.name != "driver.call")
    );
    let again = module.send(services_sdk::wire::event::Kind::Discover(check(&plain)));
    assert!(again.iter().any(|cmd| cmd.name == "driver.call"));
}

#[test]
fn discovery_resolves_variables_of_the_target() {
    let mut module = Harness::default();
    let (plain, pos) = at("#target\nmongodb://{{HOST}}/app\n#collection\n|");
    let cmds = module.hints(&plain, pos);
    let cmds = resolve(&mut module, &cmds, &[("HOST", "vars.example")]);
    assert_eq!(call(&cmds)["uri"], "mongodb://vars.example/app");
}

#[test]
fn highlighting_styles_the_sections_and_the_response() {
    use services_sdk::Module;
    let module = services_module_mongodb::Mongodb::default();
    let lines: Vec<String> = [
        "#target",
        "mongodb://h/app",
        "#action",
        "find",
        "#payload",
        "{\"filter\": {\"age\": {\"$gt\": 1}}}",
        "#responce",
        "{",
        "  \"ok\": 1",
        "}",
    ]
    .map(String::from)
    .to_vec();
    let out = module.highlight(&[], &lines, &[]);
    assert_eq!(out[3].tokens[0].kind, "keyword");
    let kinds: Vec<&str> = out[5].tokens.iter().map(|t| t.kind.as_str()).collect();
    assert!(
        kinds.contains(&"string property") && kinds.contains(&"number"),
        "{kinds:?}"
    );
    assert!(out[8].tokens.iter().any(|t| t.kind.contains("property")));
}
