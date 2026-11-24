use serde_json::{Value, json};
use services_module_prometheus::{
    discovery::Discovery,
    language::{self, Context},
};
use services_sdk::{Check, ColumnType, LineColumns, Section};

fn payload(query: &str) -> Check {
    Check {
        text: format!("#payload\n{query}"),
        sections: vec![Section {
            name: "payload".into(),
            text: query.into(),
            ..Section::default()
        }],
        ..Check::default()
    }
}

#[test]
fn parser_ast_prettifies_and_failure_is_only_advisory() {
    let query = "sum by(job)(rate(http_requests_total{code=~\"5..\"}[5m]))";
    let parsed = language::parse(query).unwrap();
    assert!(matches!(parsed, promql_parser::parser::Expr::Aggregate(_)));
    let formatted = language::prettify(query).unwrap();
    assert!(formatted.contains("sum by (job)"));
    assert_eq!(language::parse(&formatted).unwrap(), parsed);
    assert!(language::parse("rate(up[").is_err());
    let check = payload("rate(up[");
    assert!(
        !language::complete(&check, check.text.len(), &Discovery::default())
            .items
            .is_empty()
    );
    assert!(promql_parser::parser::lexer(query).is_ok());
}

#[test]
fn incomplete_contexts_and_unicode_offsets_do_not_require_full_ast() {
    let cases = [
        (
            "http_requests_total{",
            Context::Labels {
                selector: Some("http_requests_total{}".into()),
            },
        ),
        (
            "http_requests_total{job=\"a\",instance=~\"",
            Context::Values {
                label: "instance".into(),
                selector: Some("http_requests_total{job=\"a\"}".into()),
            },
        ),
        (
            "rate{on=\"x\",job=\"",
            Context::Values {
                label: "job".into(),
                selector: Some("rate{on=\"x\"}".into()),
            },
        ),
        ("sum by (jo", Context::Labels { selector: None }),
        ("up[", Context::Duration),
        ("up offset ", Context::Duration),
        ("up offset 5", Context::Duration),
        ("up{job=\"finished\"", Context::None),
        (
            "up{job=\"unfinished\\\"",
            Context::Values {
                label: "job".into(),
                selector: Some("up{}".into()),
            },
        ),
    ];
    for (query, context) in cases {
        let check = payload(query);
        assert_eq!(
            language::context(&check, check.text.len()),
            context,
            "{query}"
        );
    }
    let check = payload("up{job=\"雪");
    let mid_character = check.text.len() - 1;
    let completions = language::complete(&check, mid_character, &Discovery::default());
    assert!(check.text.is_char_boundary(completions.from as usize));
    assert!(check.text.is_char_boundary(completions.to as usize));
    assert_eq!(
        language::context(&check, mid_character),
        Context::Values {
            label: "job".into(),
            selector: Some("up{}".into())
        }
    );
}

#[test]
fn static_completions_work_before_any_discovery_and_sections_use_sdk() {
    let discovery = Discovery::default();
    let check = payload("ra");
    let completions = language::complete(&check, check.text.len(), &discovery);
    assert!(completions.items.iter().any(|i| i.insert == "rate($0)"));
    let check = payload("group_l");
    assert!(
        language::complete(&check, check.text.len(), &discovery)
            .items
            .iter()
            .any(|i| i.insert == "group_left")
    );
    for (text, expected) in [
        ("#options\nFormat: j", "json"),
        ("#metadata\nQuery-Mode: r", "range"),
        ("#met", "#metadata\n"),
    ] {
        let check = Check {
            text: text.into(),
            ..Check::default()
        };
        assert!(
            language::complete(&check, text.len(), &discovery)
                .items
                .iter()
                .any(|i| i.insert == expected),
            "{text}"
        );
    }
}

#[test]
fn dynamic_values_replace_only_quoted_prefix_and_escape_insert() {
    let mut check = payload("up{job=\"a");
    check.sections.push(Section {
        name: "target".into(),
        text: "https://example.invalid".into(),
        ..Section::default()
    });
    let mut discovery = Discovery::default();
    let commands = discovery.hints(&check, check.text.len());
    let resolve = commands.iter().find(|c| c.name == "vars.resolve").unwrap();
    let commands = discovery
        .reply(
            &resolve.tag,
            Ok(Value::String(
                resolve.args()["text"].as_str().unwrap().into(),
            )),
        )
        .unwrap();
    let call = commands.iter().find(|c| c.name == "driver.call").unwrap();
    discovery.reply(&call.tag, Ok(json!({"status":200,"body":json!({"status":"success","data":["a\"b","apple"]}).to_string()}))).unwrap();
    let completions = language::complete(&check, check.text.len(), &discovery);
    assert_eq!(
        &check.text[completions.from as usize..completions.to as usize],
        "a"
    );
    assert!(completions.items.iter().any(|i| i.insert == "a\\\"b"));
}

#[test]
fn highlighting_handles_unfinished_strings_placeholders_and_incremental_tables() {
    let lines: Vec<_> = [
        "#payload",
        "rate(up{job=\"雪",
        "# comment",
        "up{job=\"{{.secret}}\"}",
        "#responce",
        "+------+-------+",
        "| name | value |",
        "+------+-------+",
        "| a    | 12    |",
        "+------+-------+",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    let columns = vec![LineColumns {
        line: 4,
        columns: vec![ColumnType::new("value", "number")],
    }];
    let highlighted = language::highlight(&[], &lines, &columns);
    assert!(highlighted[1].tokens.iter().any(|t| t.kind == "string"));
    assert!(highlighted[2].tokens.iter().any(|t| t.kind == "comment"));
    let placeholder = lines[3].find("{{").unwrap();
    assert!(highlighted[3].tokens.iter().all(|t| {
        let range = t.start as usize..(t.start + t.len) as usize;
        !range.contains(&placeholder)
    }));
    let incremental = language::highlight(&highlighted[7].state, &lines[8..], &[]);
    assert_eq!(incremental, highlighted[8..]);
    for (line, highlighted) in lines.iter().zip(highlighted) {
        for token in highlighted.tokens {
            assert!(line.is_char_boundary(token.start as usize));
            assert!(line.is_char_boundary((token.start + token.len) as usize));
        }
    }
}

#[test]
fn folding_ignores_brackets_inside_strings_and_comments() {
    let check = payload("sum(\n  up{job=\"(\"}\n  # ) ignored\n)");
    let folds = language::fold(&check);
    assert!(folds.iter().any(|f| f.start == 1 && f.end == 4));
    let check = payload("up{job=\"(\")");
    assert!(language::fold(&check).is_empty());
}

#[test]
fn unknown_kernel_markers_remain_promql_comments_in_editor_context() {
    let check = Check {
        text: "#payload\n#comment\nra".into(),
        // The Kernel may split unknown markers into extra sections.
        sections: vec![
            Section {
                name: "payload".into(),
                text: "".into(),
                ..Section::default()
            },
            Section {
                name: "comment".into(),
                text: "ra".into(),
                ..Section::default()
            },
        ],
        ..Check::default()
    };
    assert_eq!(
        language::context(&check, check.text.len()),
        Context::Metrics
    );
    assert!(
        language::complete(&check, check.text.len(), &Discovery::default())
            .items
            .iter()
            .any(|item| item.insert == "rate($0)")
    );
    let check = payload("up{job=\"x\"}\n#comment\nrate{job=\"");
    assert_eq!(
        language::context(&check, check.text.len()),
        Context::Values {
            label: "job".into(),
            selector: Some("rate{}".into()),
        }
    );
}

#[test]
fn multiline_raw_strings_continue_across_highlight_batches_and_marker_lines() {
    let lines: Vec<String> = ["#payload", "up{job=`first", "#metadata", "", "last`} + ra"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let whole = language::highlight(&[], &lines, &[]);
    let continuation = language::highlight(&whole[1].state, &lines[2..], &[]);
    assert_eq!(continuation, whole[2..]);
    assert_eq!(whole[2].tokens[0].kind, "string");
    assert!(whole[4].tokens.iter().any(|t| {
        t.kind == "variable" && &lines[4][t.start as usize..(t.start + t.len) as usize] == "ra"
    }));
    let check = payload("up{job=`first\n#metadata\nlast`} + ra");
    assert_eq!(
        language::context(&check, check.text.len()),
        Context::Metrics
    );
    assert!(
        language::complete(&check, check.text.len(), &Discovery::default())
            .items
            .iter()
            .any(|item| item.insert == "rate($0)")
    );
}
