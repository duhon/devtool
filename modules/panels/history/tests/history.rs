use serde_json::json;
use services_module_history::{History, Redactor};
use services_sdk::{DriverCall, Module};

fn record() -> serde_json::Value {
    json!({
        "service": "rest", "checkId": "019a1234-5678-7890-abcd-012345678901",
        "sessionId": "019a1234-5678-7890-abcd-012345678902",
        "title": "#check token=t0k", "secrets": ["s3cr3t"],
        "source": "#check token=t0k\n#endpoint GET https://u:pw@x.test/?k=s3cr3t\n#headers\nAuthorization: Bearer abc",
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
    let commands = History.after_response(&call, &record);
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].name, "history.insert");
    let args = commands[0].args();
    assert_eq!(
        args["text"],
        "#check token=[redacted]\n#endpoint GET https://[redacted]@x.test/?k=[redacted]\n#headers\nAuthorization: [redacted]\n[redacted] [redacted]\n\n#responce OK\n{\"a\":1}"
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
        assert!(History.after_response(&call, &run).is_empty());
    }
}

#[test]
fn cookies_and_bearer_tokens_are_masked_by_their_parts() {
    let redactor = Redactor::new(&json!(null), "Cookie: a=1; b=two", Vec::new());
    assert_eq!(redactor.text("a=1 two"), "a=[redacted] [redacted]");
}
