//! Functions panel: the jq definitions it saves are what `{{$name(args)}}` runs at send time.

use serde_json::json;
use services_kernel::modules::{Modules, registry};
use services_module_functions::{Functions, Msg};
use services_sdk::{Event, Module};
use std::collections::BTreeMap;

#[test]
fn saved_definitions_resolve_in_a_request_body() {
    services_shell::link();
    let mut panel = Functions::with("alpha", json!([]));
    let commands = panel.update(Event::Ui {
        msg: Msg::Edit,
        value: Some("def pick(k): .tree[k];".into()),
    });
    // As the Runtime does with the panel's `editor.contribute`.
    for command in commands.iter().filter(|c| c.name == "editor.contribute") {
        registry::contribute("functions", &command.args());
    }
    registry::contribute(
        "variables",
        &json!({"tree": {"first": [["tree", {"a": "x"}]]}}),
    );
    let mut modules = Modules::new("5.1.0", BTreeMap::new(), Box::new(|| 0.0));
    let body = r#"{"q": "{{$pick("a")}}"}"#;
    let resolved = modules.resolve_in(body, true, Some("alpha")).unwrap();
    assert_eq!(resolved.text, r#"{"q": "x"}"#);
    let error = modules.resolve_in(body, true, Some("beta")).unwrap_err();
    assert!(error.contains("$pick"), "{error}");
}
