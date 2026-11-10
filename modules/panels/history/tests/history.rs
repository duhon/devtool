use serde_json::json;
use services_module_history::{History, Redactor};
use services_sdk::{DriverCall, Module};

fn record() -> serde_json::Value {
    json!({
        "service": "rest", "checkId": "019a1234-5678-7890-abcd-012345678901",
        "sessionId": "019a1234-5678-7890-abcd-012345678902",
        "title": "#check token=t0k", "secrets": ["s3cr3t"],
        "source": "#check token=t0k\n#target GET https://u:pw@x.test/?k=s3cr3t\n#headers\nAuthorization: Bearer abc",
        "started": 1, "ended": 3, "status": "success", "outcome": "OK", "text": "{\"a\":1}",
    })
}

#[test]
fn a_finished_run_is_saved_with_its_secrets_masked() {
    let call = DriverCall::new(
        "http",
        &json!({"url": "https://x.test/", "headers": [["X-Api-Key", "k3y"]], "body": "{\"password\":\"pw2\"}"}),
    );
    let mut record = record();
    record["source"] = json!(format!("{}\nk3y pw2", record["source"].as_str().unwrap()));
    let commands = History::default().after_response(&call, &record);
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].name, "history.insert");
    let args = commands[0].args();
    assert_eq!(
        args["text"],
        "#check token=[redacted]\n#target GET https://[redacted]@x.test/?k=[redacted]\n#headers\nAuthorization: [redacted]\n[redacted] [redacted]\n\n#responce OK\n{\"a\":1}"
    );
    assert_eq!(args["title"], "#check token=[redacted]");
    assert_eq!(args["module"], "rest");
    assert_eq!(args["status"], "success");
    assert_eq!(args["defer"], false);
}

#[test]
fn runs_without_a_check_or_a_resolved_source_are_not_saved() {
    let call = DriverCall::default();
    for field in ["source", "checkId"] {
        let mut run = record();
        run[field] = serde_json::Value::Null;
        assert!(History::default().after_response(&call, &run).is_empty());
    }
}

#[test]
fn cookies_and_bearer_tokens_are_masked_by_their_parts() {
    let redactor = Redactor::new(&json!(null), "Cookie: a=1; b=two", Vec::new());
    assert_eq!(redactor.text("a=1 two"), "a=[redacted] [redacted]");
}

fn reply(history: &mut History, tag: &str, value: serde_json::Value) -> Vec<services_sdk::Cmd> {
    history.update(services_sdk::Event::Reply {
        tag: tag.into(),
        result: Ok(value),
    })
}

#[test]
fn the_panel_lists_the_current_check_and_shows_a_picked_run_whole() {
    use services_module_history::Msg;
    use services_sdk::Event;
    let mut history = History::default();
    history.update(Event::Init {
        settings: json!({"timezoneOffset": 60}),
    });
    let commands = reply(
        &mut history,
        "document",
        json!({"service": "rest", "checkId": "c1"}),
    );
    assert_eq!(commands[0].name, "history.search");
    assert_eq!(commands[0].args()["checkId"], "c1");
    let list = commands[0].tag.clone();
    let entry = json!({"id": 7, "startedAt": 0.0, "endedAt": 1000.0, "durationMs": 1000.0,
        "status": "OK", "title": "#check one"});
    reply(
        &mut history,
        &list,
        json!({"entries": [entry], "total": 1, "bytes": 10}),
    );
    let commands = history.update(Event::Ui {
        msg: Msg::Select(7),
        value: None,
    });
    let first = commands[0].clone();
    assert_eq!(
        first.args(),
        json!({"op": "get", "entryId": 7, "offset": 0})
    );
    let commands = reply(
        &mut history,
        &first.tag,
        json!({"id": 7, "text": "#check one\n\n", "nextOffset": 12}),
    );
    assert_eq!(commands[0].args()["offset"], 12);
    reply(
        &mut history,
        &commands[0].tag,
        json!({"id": 7, "text": "#responce OK\n{}", "nextOffset": null}),
    );
    let view = serde_json::to_string(&format!("{:?}", history.view())).unwrap();
    assert!(
        view.contains("#check one\\\\n#responce 1:00:01\\\\u{202f}AM · OK\\\\n{}"),
        "{view}"
    );
    assert!(view.contains("1/1/1970, 1:00:00"), "{view}");
    // A run saved under the check reads the list again; another check's does not.
    assert_eq!(
        reply(&mut history, "c1", json!({}))[0].name,
        "history.search"
    );
    assert!(reply(&mut history, "c2", json!({})).is_empty());
}

#[test]
fn a_saved_run_shows_its_body_whole_under_a_timed_outcome() {
    use services_module_history::document;
    let prefix = "#check example";
    let partial = format!("{prefix}\n\n#responce OK\n{{\"debug\":");
    assert_eq!(
        document(&partial, "1:00:00 PM"),
        format!("{prefix}\n#responce 1:00:00 PM · OK\n{{\"debug\":")
    );
    // Only the first response marker belongs to the run.
    let plain = "line one\n\n#response error\nThis is part of the actual response.";
    assert!(document(&format!("{prefix}\n\n#responce OK\n{plain}"), "t").ends_with(plain));
    let large = "é".repeat(300_000);
    assert!(document(&format!("{prefix}\n\n#responce OK\n{large}"), "t").ends_with(&large));
    for (header, outcome) in [
        ("#responce WARNING", "WARNING"),
        ("#responce ERROR", "ERROR"),
        ("#responce NOTICE", "NOTICE"),
        ("#responce Stopped", "Stopped"),
        ("#response error", "ERROR"),
    ] {
        assert_eq!(
            document(&format!("{prefix}\n\n{header}\nbody"), "1:00:00 PM"),
            format!("{prefix}\n#responce 1:00:00 PM · {outcome}\nbody")
        );
    }
    let unanswered = format!("{prefix}\n#target\nx");
    assert_eq!(document(&unanswered, "t"), unanswered);
}
