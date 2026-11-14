//! Terminal module behaviour: endpoints and env become `process` driver calls, exit codes
//! and stderr set the outcome, shell highlighting and completion.

use serde_json::{Value, json};
use services_module_terminal::{
    INCOMPLETE, Terminal, build_command, outcome, parse_argv, render, request,
};
use services_sdk::lang::marker;
use services_sdk::process::Exit;
use services_sdk::{Check, Module, Section};

/// A check as the Kernel builds it: sections split at marker lines.
fn check(text: &str) -> Check {
    let mut sections: Vec<Section> = vec![];
    let mut body: Vec<&str> = vec![];
    let mut current: Option<(String, String, u32)> = Some(("leftover".into(), String::new(), 0));
    let mut close = |current: Option<(String, String, u32)>, body: &mut Vec<&str>| {
        if let Some((name, head, line)) = current {
            let mut text = head;
            if !body.is_empty() {
                text = format!("{text}\n{}", body.join("\n"));
            }
            body.clear();
            let text = text.trim().to_owned();
            if name != "leftover" || !text.is_empty() {
                sections.push(Section { name, text, line });
            }
        }
    };
    for (i, line) in text.split('\n').enumerate() {
        if let Some((name, rest)) = marker(line) {
            close(current.take(), &mut body);
            let head = if name == "target" {
                rest.to_owned()
            } else {
                String::new()
            };
            current = Some((name, head, i as u32));
        } else {
            body.push(line);
        }
    }
    close(current, &mut body);
    Check {
        columns: Vec::new(),
        service: "terminal".into(),
        text: text.into(),
        sections,
        options: vec![],
    }
}

fn build(text: &str) -> Result<Value, String> {
    request(&check(text)).map(|request| request.call().request())
}

#[test]
fn argv_quotes_and_env_match_the_shell_contract() {
    assert_eq!(
        parse_argv("ssh 'some host' -i \"a b\\\"c\" x\\ y").unwrap(),
        vec!["ssh", "some host", "-i", "a b\"c", "x y"]
    );
    assert_eq!(parse_argv("sh -c ''").unwrap(), vec!["sh", "-c", ""]);
    assert_eq!(
        parse_argv("sh 'bad").unwrap_err(),
        "Unterminated single quote in #target"
    );
    assert_eq!(
        parse_argv("sh \"bad").unwrap_err(),
        "Unterminated double quote in #target"
    );
    assert_eq!(
        build_command("printf \"$VALUE\"", &[("VALUE".into(), "a'b".into())]),
        "export VALUE='a'\\''b'; printf \"$VALUE\""
    );
}

#[test]
fn checks_become_process_driver_calls() {
    let call = request(&check(
        "#check indexer\n#target\ndocker exec fixture bash -c\n#payload\nbin/magento indexer:status\n#metadata\nA: 1\nB: {{store:B}}",
    ))
    .unwrap()
    .call();
    assert_eq!(call.driver, "process");
    let request: Value = serde_json::from_slice(&call.request).unwrap();
    assert_eq!(
        request,
        json!({"program":"docker","args":["exec","fixture","bash","-c",
            "export A='1' B='{{store:B}}'; bin/magento indexer:status"],
            "env":[],"text":true})
    );
    assert_eq!(
        build("#target\nbash -c\necho hi").unwrap()["args"],
        json!(["-c", "echo hi"])
    );
    assert_eq!(
        build("#target\n\n#payload\nls").unwrap_err(),
        "#target must not be empty"
    );
    assert_eq!(
        build("#target\nbash -c").unwrap_err(),
        "command is required"
    );
}

#[test]
fn exit_codes_signals_stderr_and_incomplete_output_set_the_outcome() {
    for (exit, expected) in [
        (json!({"pid":1,"code":0,"outputIncomplete":true}), "WARNING"),
        (json!({"pid":1,"code":1,"outputIncomplete":true}), "ERROR"),
        (json!({"pid":1,"code":0}), "OK"),
        (json!({"pid":1,"code":0,"stderr":"\n  \n"}), "OK"),
        (
            json!({"pid":1,"code":0,"stderr":"warning: deprecated\n"}),
            "WARNING",
        ),
        (json!({"pid":1,"code":0,"truncated":true}), "WARNING"),
        (json!({"pid":1,"code":1}), "ERROR"),
        (json!({"pid":1,"code":127}), "ERROR"),
        (json!({"pid":1,"code":null,"signal":"SIGKILL"}), "ERROR"),
    ] {
        assert_eq!(outcome(&Exit::parse(&exit).unwrap()), expected, "{exit}");
        assert_eq!(render(&exit).unwrap().outcome, expected, "{exit}");
    }
}

#[test]
fn output_shows_stdout_then_stderr_then_why_output_is_missing() {
    let rendered =
        render(&json!({"pid":1,"code":0,"stdout":"+----+\n| 1  |\n","stderr":"warning: é\n"}))
            .unwrap();
    assert_eq!(rendered.text, "+----+\n| 1  |\nwarning: é\n");
    assert_eq!(rendered.language, "text");
    let incomplete =
        render(&json!({"pid":1,"code":0,"stdout":"output","outputIncomplete":true})).unwrap();
    assert_eq!(incomplete.text, format!("output\nWarning: {INCOMPLETE}"));
    assert_eq!(render(&json!({"pid":1,"code":0})).unwrap().text, "");
    assert_eq!(render(&json!({"stdout":"piece"})).unwrap().text, "piece");
    assert!(render(&json!({"nonsense":true})).is_err());
}

fn inserts(text: &str) -> Vec<String> {
    let pos = text.find('|').unwrap();
    let text = text.replacen('|', "", 1);
    Terminal::default()
        .complete(&check(&text), pos as u32)
        .items
        .into_iter()
        .map(|item| item.insert)
        .collect()
}

#[test]
fn endpoints_env_names_and_markers_complete() {
    assert_eq!(inserts("#target\n|"), vec!["bash -c", "zsh -c", "ssh "]);
    assert_eq!(inserts("#target\nss|"), vec!["ssh "]);
    assert_eq!(
        inserts("#target\nbash -c\n#metadata\nPATH: /bin\nNO|"),
        vec!["NODE_ENV: production"]
    );
    assert_eq!(
        inserts("#target\nbash -c\n#|"),
        vec!["#payload\n", "#metadata\n"]
    );
    assert!(inserts("#target\nbash -c\n#payload\nls |").is_empty());
}

fn styles(lines: &[&str]) -> Vec<Vec<(String, String)>> {
    let lines: Vec<String> = lines.iter().map(|line| line.to_string()).collect();
    Terminal::default()
        .highlight(&[], &lines, &[])
        .iter()
        .zip(&lines)
        .map(|(line, text)| {
            line.tokens
                .iter()
                .map(|t| {
                    let start = t.start as usize;
                    (
                        text[start..start + t.len as usize].to_owned(),
                        t.kind.clone(),
                    )
                })
                .collect()
        })
        .collect()
}

#[test]
fn shell_commands_variables_and_flags_highlight() {
    let styled = styles(&[
        "#target bash -c",
        "#payload",
        "echo \"$HOME\" --flag && printf {{:NAME}} 'a",
        "b'",
        "#metadata",
        "LANG: en_US.UTF-8",
        "#responce 6:00:14 PM · OK",
        "echo out",
    ]);
    let pair = |a: &str, b: &str| (a.to_owned(), b.to_owned());
    assert_eq!(
        styled[0],
        vec![pair("#target", "comment"), pair("-c", "attribute")]
    );
    for expected in [
        pair("echo", "builtin"),
        pair("$HOME", "variable-2"),
        pair("--flag", "attribute"),
        pair("&&", "operator"),
    ] {
        assert!(styled[2].contains(&expected), "{expected:?}");
    }
    assert_eq!(styled[3], vec![pair("b", "string"), pair("'", "string")]);
    assert_eq!(styled[5][0], pair("LANG", "attribute"));
    assert!(styled[7].is_empty());
}
