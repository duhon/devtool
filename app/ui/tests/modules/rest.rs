//! REST runs in the Kernel (`services-module-rest`, linked natively): the console colors,
//! completes, folds, builds, exports and imports its checks through the module.

use serde_json::{Value, json};
use services_kernel::Effect;
use services_ui::completion::suggest;
use services_ui::editor::highlight::{Highlighter, Style};
use services_ui::{console, editor::fold, kernel, services};

fn lines(text: &str) -> Vec<String> {
    text.split('\n').map(str::to_owned).collect()
}

/// The (fragment, style) runs of `line`, cut at the spans' UTF-16 ends.
fn runs(highlighter: &mut Highlighter, lines: &[String], line: usize) -> Vec<(String, Style)> {
    let units: Vec<u16> = lines[line].encode_utf16().collect();
    let mut start = 0;
    let mut out = Vec::new();
    for span in highlighter.spans(lines, line).iter() {
        out.push((
            String::from_utf16(&units[start..span.end]).unwrap(),
            span.style,
        ));
        start = span.end;
    }
    assert_eq!(start, units.len(), "{:?}", lines[line]);
    out
}

fn styled(runs: &[(String, Style)], text: &str) -> Style {
    runs.iter()
        .find(|(fragment, _)| fragment == text)
        .unwrap_or_else(|| panic!("{text:?} in {runs:?}"))
        .1
}

#[test]
fn the_module_colors_checks_with_utf16_spans_and_the_console_overlays_variables() {
    let source = lines(
        "#check Привет 😀\n#endpoint\nGET https://пример.рф/{{:host}}/pets/{id}\n#headers\nX-Имя😀: {{:token}}\n#body\n{\"имя\": \"значение\"}\n#responce OK\n{\"ok\": true}",
    );
    let mut highlighter = Highlighter::new("console-rest");
    assert_eq!(
        runs(&mut highlighter, &source, 0),
        [("#check Привет 😀".into(), Some("comment"))]
    );
    assert_eq!(
        runs(&mut highlighter, &source, 1),
        [("#endpoint".into(), Some("comment"))]
    );
    let endpoint = runs(&mut highlighter, &source, 2);
    assert_eq!(styled(&endpoint, "пример.рф"), Some("def"));
    assert_eq!(styled(&endpoint, "{id}"), Some("rest-path-param"));
    assert_eq!(styled(&endpoint, "{{"), Some("gw-placeholder"));
    assert_eq!(styled(&endpoint, "host}}"), Some("gw-placeholder"));
    let header = runs(&mut highlighter, &source, 4);
    assert_eq!(styled(&header, "X-Имя😀"), Some("attribute"));
    assert_eq!(styled(&header, "token}}"), Some("gw-placeholder"));
    let body = runs(&mut highlighter, &source, 6);
    assert_eq!(styled(&body, "\"имя\""), Some("string property"));
    assert_eq!(styled(&body, "\"значение\""), Some("string"));
    assert_eq!(
        runs(&mut highlighter, &source, 7),
        [("#responce OK".into(), Some("comment"))]
    );
    assert_eq!(
        styled(&runs(&mut highlighter, &source, 8), "true"),
        Some("atom")
    );
}

#[test]
fn module_completions_merge_with_console_markers_options_and_variables() {
    let hints = [("{{:token}}".to_owned(), "secret".to_owned())];
    let at = |source: &str, line: usize, ch: usize| suggest("rest", "", source, line, ch, &hints);
    let texts = |source: &str, line: usize, ch: usize| {
        at(source, line, ch)
            .candidates
            .into_iter()
            .map(|candidate| candidate.text)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        texts("#check\n#", 1, 1),
        ["#endpoint", "#headers", "#body", "#options", "#check"]
    );
    // A used section is not offered again; `#check` always is.
    assert_eq!(
        texts("#check\n#endpoint\nGET https://a/\n#", 3, 1),
        ["#headers", "#body", "#options", "#check"]
    );
    assert!(texts("#check\n#options\n", 2, 0).contains(&"Repeat: 5s".to_owned()));
    let variables = at("#check\n#endpoint\nGET https://a\n#headers\nX: {{", 4, 5);
    assert_eq!((variables.from, variables.to), (3, 5));
    assert_eq!(variables.candidates[0].text, "{{:");
    // Without a check only `#check` starts one.
    assert_eq!(texts("#", 0, 1), ["#check"]);
    let header = at("#check\n#endpoint\nGET https://a\n#headers\nAccept-E", 4, 8);
    assert_eq!((header.from, header.to), (0, 8));
    assert!(header.candidates[0].text.starts_with("Accept-Encoding"));
    // The module completes on the last, empty line too.
    let body = "#check\n#endpoint\nPOST https://a/x\n#body\n";
    assert_eq!(texts(body, 4, 0), texts(&format!("{body}\n#check"), 4, 0));
    assert!(!texts(body, 4, 0).is_empty());
}

#[test]
fn checks_and_sections_fold_where_the_module_says() {
    let source = lines("#check\n#endpoint\nGET https://a\n#body\n{\n  \"a\": 1\n}");
    let mut highlighter = Highlighter::new("console-rest");
    let fold = |highlighter: &mut Highlighter, line| {
        fold::range(highlighter, &source, line)
            .map(|fold| ((fold.from.line, fold.from.ch), (fold.to.line, fold.to.ch)))
    };
    assert_eq!(fold(&mut highlighter, 0), Some(((0, 6), (6, 1))));
    assert_eq!(fold(&mut highlighter, 1), Some(((1, 9), (2, 13))));
    assert_eq!(fold(&mut highlighter, 4), Some(((4, 1), (6, 0))));
    assert_eq!(fold(&mut highlighter, 2), None);
}

#[test]
fn a_run_is_the_modules_http_call_with_the_console_options() {
    let (call, options) = console::parse_run(
        "rest",
        "#endpoint\nPOST https://a/x\n#headers\nX: y\n#options\nRepeat: 5s\n#body\n{}",
    )
    .unwrap();
    assert_eq!(
        call,
        json!({"driver":"http","stream":false,"request":{"method":"POST",
            "url":"https://a/x","headers":[["X","y"]],"body":"{}","text":true}})
    );
    assert_eq!(options.repeat, Some(5000));
    // before_request passes the Variables panel, which reports unknown refs.
    assert_eq!(
        console::parse_run("rest", "#endpoint\nGET https://a/{{:x}}").unwrap_err(),
        "Missing environment variable(s): x"
    );
    assert_eq!(
        console::parse_run("rest", "#endpoint\nhttps://a").unwrap_err(),
        "#endpoint must be METHOD URL"
    );
    assert!(
        console::parse_run("rest", "#endpoint\nGET https://a\n#options\nRepeat: soon").is_err()
    );
}

#[test]
fn curl_export_and_import_go_through_the_module() {
    assert!(console::transfers("rest"));
    assert_eq!(
        console::export(
            "rest",
            "#endpoint\nPOST https://a\n#options\nRepeat: 5s\n#body\n{}"
        )
        .unwrap(),
        "curl \\\n  --request POST \\\n  --url 'https://a' \\\n  --data '{}'"
    );
    assert_eq!(
        console::import("rest", "curl -X POST https://a -d '{}'").unwrap(),
        "#endpoint\nPOST https://a\n#body\n{}"
    );
}

#[test]
fn rest_is_a_kernel_tab_ordered_by_its_manifest() {
    assert!(services::contains("rest"));
    assert_eq!(services::label("rest"), "REST");
    assert_eq!(services::default_service(), "rest");
    assert_eq!(
        console::markers("rest"),
        ["endpoint", "headers", "body", "options"]
    );
    assert_eq!(
        services::template_sections("rest"),
        ["check", "endpoint", "headers", "body", "options"]
    );
}

#[test]
fn host_wasm_lists_declare_kernel_services_and_what_to_load() {
    let entry = json!({"id":"rest","kind":"service","name":"REST","version":"3.7.0",
        "description":"REST","abi":1,"app":">=3.7","drivers":{"http":"1"},"order":1,
        "digest":"abc"});
    let parsed = kernel::parse(&json!({"wasm":[entry]})).unwrap();
    assert_eq!(parsed[0].1, "abc");
    assert!(kernel::parse(&json!({"wasm":[{"id":"rest"}]})).is_err());
    assert!(kernel::parse(&json!({"modules":[]})).unwrap().is_empty());
    kernel::declare(parsed);
    let (_, wanted) = kernel::retain();
    assert_eq!(
        wanted
            .iter()
            .map(|(manifest, digest)| (manifest.id.as_str(), digest.as_str()))
            .collect::<Vec<_>>(),
        [("rest", "abc")]
    );
    assert!(kernel::declare(Vec::new()));
    assert!(!services::contains("rest"));
    assert!(!services::ids().contains(&"rest".to_owned()));
}

#[test]
fn discovery_runs_as_module_driver_calls_and_reports_its_status() {
    let document = kernel::check(
        "rest",
        "#endpoint\nGET https://fixture.invalid/api/pets",
        &Value::Null,
    );
    assert!(kernel::broadcast(kernel::document(document)).is_empty());
    let check = kernel::check(
        "rest",
        "#endpoint\nGET https://fixture.invalid/api/pets",
        &Value::Null,
    );
    let effects = kernel::broadcast(kernel::hints(check, 0));
    let drivers = |effects: &[Effect]| -> Vec<(String, Value)> {
        effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::Driver {
                    module,
                    tag,
                    driver,
                    request,
                    ..
                } if module == "rest" && driver == "http" => Some((tag.clone(), request.clone())),
                _ => None,
            })
            .collect()
    };
    let mut pending = drivers(&effects);
    let request = &pending.first().expect("a discovery request").1;
    assert_eq!(request["method"], "GET");
    assert!(
        request["url"]
            .as_str()
            .unwrap()
            .starts_with("https://fixture.invalid/")
    );
    assert_eq!(kernel::status("rest").unwrap().indicator, "busy");
    // Every candidate fails the same way.
    while let Some((tag, _)) = pending.pop() {
        let failure = services_sdk::Failure::new("connection refused");
        let effects = kernel::with(|modules| modules.reply("rest", &tag, Err(failure)));
        pending.extend(drivers(&effects));
    }
    let status = kernel::status("rest").unwrap();
    assert!(status.text.starts_with("Discovery · "), "{}", status.text);
    assert_ne!(status.indicator, "busy");
}

#[test]
fn a_field_after_a_value_puts_its_comma_after_the_value() {
    let source = "#check\n#endpoint\nPOST http://comma.invalid/api/users\n#body\n";
    let check = kernel::check("rest", &source[7..], &Value::Null);
    let mut pending: Vec<(String, String)> = Vec::new();
    let calls = |effects: Vec<Effect>| -> Vec<(String, String)> {
        effects
            .into_iter()
            .filter_map(|effect| match effect {
                Effect::Driver { tag, request, .. } => {
                    Some((tag, request["url"].as_str().unwrap_or("").to_owned()))
                }
                _ => None,
            })
            .collect()
    };
    pending.extend(calls(kernel::broadcast(kernel::hints(check, 0))));
    let spec = json!({"openapi":"3.0.0","paths":{"/api/users":{"post":{"requestBody":{
        "content":{"application/json":{"schema":{"type":"object","properties":{
            "name":{"type":"string"},"age":{"type":"integer"},
            "mood":{"type":"string","enum":["say \"hi\""]}}}}}}}}}});
    while let Some((tag, url)) = pending.pop() {
        let reply = if url == "http://comma.invalid/api/users/openapi.json" {
            json!({"status":200,"statusText":"","body":spec.to_string()})
        } else {
            json!({"status":404,"statusText":"","body":""})
        };
        pending.extend(calls(kernel::with(|modules| {
            modules.reply("rest", &tag, Ok(reply))
        })));
    }
    let source = format!("{source}{{\n  \"name\": \"x\"\n  \n}}");
    let completion = suggest("rest", "", &source, 6, 2, &[]);
    assert_eq!(completion.comma, Some((5, 13)));
    assert_eq!((completion.from, completion.to), (2, 2));
    assert_eq!(completion.candidates[0].text, "\"age\": ");
    // Picking a field with a list of values asks for the value next.
    let mood = &completion.candidates[1];
    assert_eq!(mood.text, "\"mood\": \"$0\"");
    assert!(mood.continue_hint && !mood.literal);
    // A value goes in as the module wrote it, though it ends with `""`.
    let source = format!("{}{{\"mood\": \"\"}}", &source[..source.find('{').unwrap()]);
    let value = &suggest("rest", "", &source, 4, 10, &[]).candidates[0];
    assert_eq!(value.text, "\"say \\\"hi\\\"\"");
    assert!(value.literal);
}
