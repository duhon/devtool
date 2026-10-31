//! Terminal module: shell coloring, form validation and history output.
//! UI-behaviour tests.

use crate::support::tokens;
use serde_json::json;
use services_ui::format::{check_response, history_snapshot, status};
use services_ui::syntax::{Language, State};

#[test]
fn shell_commands_variables_and_flags() {
    let t = tokens(
        Language::Shell,
        "echo \"$HOME\" --flag && printf {{:NAME}}",
        &mut State::default(),
    );
    assert!(t.contains(&("echo".into(), Some("builtin"))));
    assert!(t.contains(&("$HOME".into(), Some("variable-2"))));
    assert!(t.contains(&("--flag".into(), Some("attribute"))));
    assert!(t.contains(&(":".into(), Some("gw-placeholder-colon"))));
}

#[test]
fn terminal_history_matches_joined_console_output_without_event_or_exit_metadata() {
    let request = "#check indexer status\n#endpoint\ndocker exec fixture bash -c\n#command\nbin/magento indexer:status";
    let first = "+----+-------+\n| ID | Title |\n";
    let last = "+----+-------+\n| 1  | Ready |\n+----+-------+\n";
    let warning = "warning: é\n";
    let events = [
        json!({"type":"stdout","data":first}),
        json!({"type":"stderr","data":warning}),
        json!({"type":"stdout","data":last}),
    ]
    .map(|event| event.to_string())
    .join("\n");
    let result = json!({"code":0,"signal":null,"durationMs":3645,"cancelled":false});
    let incomplete = json!({"code":0,"outputIncomplete":true,"warning":"Detached processes may still be running."});
    let incomplete_snapshot =
        format!("{request}\n\n#responce WARNING\n#terminal-output\n{events}\n\n{incomplete}");
    assert_eq!(
        history_snapshot("terminal", &incomplete_snapshot, "6:00:14 PM", true).unwrap(),
        format!(
            "{request}\n#responce 6:00:14 PM · WARNING\n{first}{last}{warning}Warning: Detached processes may still be running."
        )
    );
    assert_eq!(
        check_response("terminal", &incomplete, false, "output", ""),
        "output\nWarning: Detached processes may still be running."
    );
    let snapshot = format!("{request}\n\n#responce OK\n#terminal-output\n{events}\n\n{result}");
    let display = history_snapshot("terminal", &snapshot, "6:00:14 PM", true).unwrap();
    let stdout = format!("{first}{last}");
    assert_eq!(
        display,
        format!("{request}\n#responce 6:00:14 PM · OK\n{stdout}{warning}")
    );
    assert!(display.ends_with(&check_response(
        "terminal", &result, false, &stdout, warning
    )));
    assert!(!display.contains("#terminal-output"));
    assert!(!display.contains("\"type\""));
    assert!(!display.contains("durationMs"));
    assert!(snapshot.contains("#terminal-output"));
    let partial = format!(
        "{request}\n\n#responce OK\n#terminal-output\n{}\n{{\"type\":\"stdout\",\"data\":",
        json!({"type":"stdout","data":first})
    );
    assert!(
        history_snapshot("terminal", &partial, "6:00:14 PM", false)
            .unwrap()
            .ends_with(first)
    );
    assert!(history_snapshot("terminal", &partial, "6:00:14 PM", true).is_err());
    let empty = format!("{request}\n\n#responce OK\n{result}");
    assert!(
        history_snapshot("terminal", &empty, "6:00:14 PM", true)
            .unwrap()
            .ends_with(" · OK\n")
    );
    let failed =
        format!("{request}\n\n#responce ERROR\n{{\"error\":\"could not start\",\"debug\":{{}}}}");
    assert!(
        history_snapshot("terminal", &failed, "6:00:14 PM", true)
            .unwrap()
            .ends_with("could not start")
    );
}

#[test]
fn a_failed_command_status_shows_its_exit_code_and_last_stderr_line() {
    let failed = json!({"code":1,"signal":null,"durationMs":35,"outcome":"ERROR",
        "stderr":"first\nls: /x: No such file or directory\n\n"});
    assert_eq!(
        status("terminal", &failed),
        "35ms · ERROR · exit 1 · ls: /x: No such file or directory"
    );
    let silent = json!({"code":1,"signal":null,"durationMs":35,"outcome":"ERROR"});
    assert_eq!(status("terminal", &silent), "35ms · ERROR · exit 1");
    let killed =
        json!({"code":null,"signal":"SIGKILL","durationMs":35,"outcome":"ERROR","stderr":"bye\n"});
    assert_eq!(status("terminal", &killed), "35ms · ERROR · SIGKILL · bye");
    let ok = json!({"code":0,"signal":null,"durationMs":35,"outcome":"OK"});
    assert_eq!(status("terminal", &ok), "35ms · OK");
    let warned =
        json!({"code":0,"signal":null,"durationMs":35,"outcome":"WARNING","stderr":"warning: x\n"});
    assert_eq!(status("terminal", &warned), "35ms · WARNING · warning: x");
    let long = json!({"code":2,"outcome":"ERROR","stderr":"é".repeat(200)});
    let shown = status("terminal", &long);
    assert!(shown.starts_with("ERROR · exit 2 · é"), "{shown}");
    assert!(
        shown.ends_with("é…") && shown.chars().count() == "ERROR · exit 2 · ".chars().count() + 160,
        "{shown}"
    );
    assert_eq!(
        check_response(
            "terminal",
            &failed,
            false,
            "",
            "ls: /x: No such file or directory\n"
        ),
        "ls: /x: No such file or directory\n",
        "The stderr tail must not repeat in the response body"
    );
}
