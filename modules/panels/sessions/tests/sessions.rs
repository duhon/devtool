use serde_json::json;
use services_module_sessions::{Msg, Sessions};
use services_sdk::{Event, Module};

#[test]
fn the_panel_switches_copies_and_deletes_through_the_workspace() {
    let mut sessions = Sessions::default();
    let commands = sessions.update(Event::Shown);
    assert_eq!(commands[0].name, "workspace.list");
    let commands = sessions.update(Event::Reply {
        tag: "list".into(),
        result: Ok(json!({"current": 2, "sessions": [{"id": 2, "name": "Work"}], "saving": true})),
    });
    // While the workspace saves, the list is read again until it is saved.
    assert_eq!(commands[0].name, "timer.after");
    let view = format!("{:?}", sessions.view());
    assert!(
        view.contains("session-switch-0") && view.contains("Work"),
        "{view}"
    );
    sessions.update(Event::Reply {
        tag: "list".into(),
        result: Ok(json!({"current": 2, "sessions": [{"id": 2, "name": "Work"}], "saving": false})),
    });
    sessions.update(Event::Ui {
        msg: Msg::Name,
        value: Some("Copy".into()),
    });
    let commands = sessions.update(Event::Ui {
        msg: Msg::Copy(2),
        value: None,
    });
    assert_eq!(commands[0].name, "workspace.copy");
    assert_eq!(commands[0].args(), json!({"source": 2, "name": "Copy"}));
    let commands = sessions.update(Event::Ui {
        msg: Msg::Delete(2),
        value: None,
    });
    assert_eq!(commands[0].args(), json!({"id": 2}));
    let commands = sessions.update(Event::Reply {
        tag: "delete".into(),
        result: Err(services_sdk::Failure::new("busy")),
    });
    assert_eq!(commands[0].name, "workspace.list");
    assert_eq!(commands[1].args()["text"], "Sessions · ERROR · busy");
}
