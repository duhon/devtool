#![allow(dead_code)]
mod common;

use common::*;
use serde_json::{Value, json};
use services_sdk::Cmd;
use services_sdk::wire::event::Kind;

fn calls(cmds: &[Cmd]) -> Vec<Value> {
    cmds.iter()
        .filter(|cmd| cmd.name == "driver.call")
        .map(|cmd| cmd.args()["request"].clone())
        .collect()
}

fn document(module: &mut Harness, text: &str) -> Vec<Cmd> {
    module.send(Kind::Document(check(text)))
}

const BY_VARIABLE: &str = "#target\nmongodb://{{var:mongo}}/gamelib\n#collection\nstores\n#action\naggregate\n#payload\n[]";

#[test]
fn a_target_followed_by_a_section_discovers_the_collections_on_its_own() {
    let mut module = Harness::default();
    let cmds = document(&mut module, "#target\nmongodb://h/app\n#collection\nus");
    let requests = calls(&cmds);
    assert_eq!(requests.len(), 1, "{cmds:?}");
    assert_eq!(requests[0]["command"]["listCollections"], 1);
    assert_eq!(requests[0]["uri"], "mongodb://h/app");
    assert!(
        calls(&document(
            &mut module,
            "#target\nmongodb://h/app\n#collection\nuse"
        ))
        .is_empty()
    );
}

#[test]
fn a_target_still_being_typed_is_left_alone() {
    let mut module = Harness::default();
    assert!(calls(&document(&mut module, "#target\nmongodb://h")).is_empty());
    assert!(calls(&document(&mut module, "#target")).is_empty());
    assert!(calls(&document(&mut module, "#target\nnot a uri\n#action\nfind")).is_empty());
    let typing = "#target\nmongodb://h/app";
    let (text, pos) = (typing, typing.len());
    assert!(calls(&module.hints(text, pos)).is_empty());
}

#[test]
fn a_target_ended_by_a_newline_discovers_when_the_cursor_moves_on() {
    let mut module = Harness::default();
    let text = "#target\nmongodb://h/app\n";
    assert_eq!(calls(&module.hints(text, text.len())).len(), 1);
}

#[test]
fn a_target_with_a_variable_is_resolved_first_then_discovered() {
    let mut module = Harness::default();
    let cmds = document(&mut module, BY_VARIABLE);
    assert!(calls(&cmds).is_empty());
    let cmds = resolve(&mut module, &cmds, &[("var:mongo", "vars.example:27017")]);
    let requests = calls(&cmds);
    assert_eq!(requests.len(), 1, "{cmds:?}");
    assert_eq!(requests[0]["uri"], "mongodb://vars.example:27017/gamelib");
    assert_eq!(requests[0]["command"]["listCollections"], 1);
}

#[test]
fn typing_the_action_asks_again_without_repeating_known_work() {
    let mut module = Harness::default();
    let cmds = document(&mut module, BY_VARIABLE);
    let cmds = resolve(&mut module, &cmds, &[("var:mongo", "vars.example:27017")]);
    assert_eq!(calls(&cmds).len(), 1);
    let typing = BY_VARIABLE.replace("aggregate", "agg");
    assert!(calls(&document(&mut module, &typing)).is_empty());
    let pos = typing.find("agg").unwrap() + 3;
    assert!(calls(&module.hints(&typing, pos)).is_empty());
}

#[test]
fn the_first_listing_finds_the_collections_to_sample() {
    let mut module = Harness::default();
    let cmds = document(
        &mut module,
        "#target\nmongodb://h/app\n#collection\nstores\n#action\nfind",
    );
    let list = cmds
        .iter()
        .find(|cmd| cmd.name == "driver.call")
        .unwrap()
        .tag
        .clone();
    assert_eq!(calls(&cmds).len(), 1, "The listing comes first: {cmds:?}");
    let cmds = module.reply(
        &list,
        json!({"documents": [{"name": "stores", "type": "collection"}, {"name": "inventory", "type": "collection"}],
            "count": 2, "truncated": false}),
    );
    let requests = calls(&cmds);
    assert_eq!(requests.len(), 1, "{cmds:?}");
    assert_eq!(requests[0]["command"]["find"], "stores");
}

#[test]
fn ctrl_space_in_the_action_resolves_the_variable_and_discovers() {
    let mut module = Harness::default();
    let pos = BY_VARIABLE.find("aggregate").unwrap() + 3;
    let cmds = module.hints(BY_VARIABLE, pos);
    let cmds = resolve(&mut module, &cmds, &[("var:mongo", "vars.example:27017")]);
    assert_eq!(calls(&cmds)[0]["command"]["listCollections"], 1, "{cmds:?}");
}
