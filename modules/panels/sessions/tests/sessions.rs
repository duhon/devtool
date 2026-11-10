use serde_json::json;
use services_module_sessions::{Msg, Sessions};
use services_sdk::{Event, Module};

#[test]
fn the_panel_switches_copies_renames_and_deletes_through_the_workspace() {
    let mut sessions = Sessions::default();
    let commands = sessions.update(Event::Shown);
    assert_eq!(commands[0].name, "workspace.list");
    let library = |saving| {
        json!({"current": 2, "saving": saving, "sessions": [
            {"id": 2, "name": "Work", "used": 1_700_000_000_000u64},
            {"id": 3, "name": "Work-1"},
            {"id": 4, "name": "Work-7"},
        ]})
    };
    let commands = sessions.update(Event::Reply {
        tag: "list".into(),
        result: Ok(library(true)),
    });
    // While the workspace saves, the list is read again until it is saved.
    assert_eq!(commands[0].name, "timer.after");
    let view = format!("{:?}", sessions.view());
    assert!(
        view.contains("session-copy-0") && view.contains("Immutable") && view.contains("Work"),
        "{view}"
    );
    sessions.update(Event::Reply {
        tag: "list".into(),
        result: Ok(library(false)),
    });
    let ui = |sessions: &mut Sessions, msg, value: Option<&str>| {
        sessions.update(Event::Ui {
            msg,
            value: value.map(str::to_owned),
        })
    };
    // A session created from another is named after it, one past the largest number.
    let commands = ui(&mut sessions, Msg::Copy(2), None);
    assert_eq!(commands[0].name, "workspace.copy");
    assert_eq!(commands[0].args(), json!({"source": 2, "name": "Work-8"}));
    let commands = ui(&mut sessions, Msg::Copy(0), None);
    assert_eq!(
        commands[0].args(),
        json!({"source": 0, "name": "Temporary-1"})
    );
    // Clicking the current row does nothing; another row switches to it.
    assert!(ui(&mut sessions, Msg::Switch(2), None).is_empty());
    assert_eq!(
        ui(&mut sessions, Msg::Switch(3), None)[0].args(),
        json!({"id": 3})
    );
    // An empty or unchanged name is not sent.
    assert!(ui(&mut sessions, Msg::Rename(2), Some(" ")).is_empty());
    assert!(ui(&mut sessions, Msg::Rename(2), Some("Work")).is_empty());
    let commands = ui(&mut sessions, Msg::Rename(2), Some(" Home "));
    assert_eq!(commands[0].name, "workspace.rename");
    assert_eq!(commands[0].args(), json!({"id": 2, "name": "Home"}));
    let commands = ui(&mut sessions, Msg::Delete(2), None);
    assert_eq!(commands[0].args(), json!({"id": 2}));
    let commands = sessions.update(Event::Reply {
        tag: "delete".into(),
        result: Err(services_sdk::Failure::new("busy")),
    });
    assert_eq!(commands[0].name, "workspace.list");
    assert_eq!(commands[1].args()["text"], "Sessions · ERROR · busy");
}
