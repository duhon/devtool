#![allow(dead_code)]
mod common;
mod support;

use services_sdk::wire::{self, answer, request};

fn highlight(module: &mut common::Harness, text: &str) -> Vec<Vec<(String, String)>> {
    let lines: Vec<String> = text.split('\n').map(str::to_owned).collect();
    let answer::Kind::Highlighted(done) =
        module.call(request::Kind::Highlight(wire::HighlightRequest {
            state: vec![],
            lines: lines.clone(),
            columns: vec![],
        }))
    else {
        panic!("Expected lines")
    };
    lines
        .iter()
        .zip(done.lines)
        .map(|(line, done)| {
            done.tokens
                .iter()
                .map(|token| {
                    let start = token.start as usize;
                    (
                        line[start..start + token.len as usize].to_owned(),
                        token.kind.clone(),
                    )
                })
                .collect()
        })
        .collect()
}

fn pair(text: &str, kind: &str) -> (String, String) {
    (text.to_owned(), kind.to_owned())
}

#[test]
fn sections_target_and_action_are_styled_by_their_parts() {
    let mut module = common::Harness::default();
    let lines = highlight(
        &mut module,
        "#target\nk8s://dev/default\n#action\nrollout status deploy/web",
    );
    assert_eq!(lines[0], [pair("#target", "comment")]);
    assert_eq!(
        lines[1],
        [
            pair("k8s://", "keyword"),
            pair("dev", "def"),
            pair("default", "variable-2")
        ]
    );
    assert_eq!(
        lines[3],
        [
            pair("rollout", "keyword"),
            pair("status", "keyword"),
            pair("deploy", "type"),
            pair("web", "string")
        ]
    );
}

#[test]
fn metadata_options_and_both_payload_syntaxes_are_styled() {
    let mut module = common::Harness::default();
    let text = "#metadata\nTail: 10\n#payload\napiVersion: v1\nkind: Pod\n---\n{\"kind\": \"Pod\", \"n\": 1}\n#options\nFormat: json";
    let lines = highlight(&mut module, text);
    assert_eq!(lines[1][0].0, "Tail");
    assert_eq!(lines[1].last().unwrap().0, "10");
    assert_eq!(
        lines[3],
        [pair("apiVersion", "property"), pair("v1", "string")]
    );
    assert_eq!(lines[5], [pair("---", "operator")]);
    assert!(
        lines[6]
            .iter()
            .any(|(text, kind)| text == "\"kind\"" && kind.contains("property")),
        "{:?}",
        lines[6]
    );
    assert_eq!(lines[7], [pair("#options", "comment")]);
    assert!(!lines[8].is_empty());
}
