//! REST module: path/body completion from discovery, schema cache, endpoint
//! coloring, query parameters and history. UI-behaviour tests.

use crate::support::tokens_for;
use serde_json::{Value, json};
use services_ui::completion::{discovery_endpoint, prepare_insertion, suggest};
use services_ui::discovery::{LazyCache, body_position, key, reuses_schema};
use services_ui::format::{console_response, history_snapshot};
use services_ui::profiles;
use services_ui::syntax::{Language, State};

#[test]
fn post_fields_match_a_trailing_slash_after_an_open_quote() {
    let discovery = json!({"bodySchemas":{"POST /api/v1/Books":[
        {"name":"id","kind":"bare"},{"name":"title","kind":"string"},
        {"name":"description","kind":"string"},{"name":"pageCount","kind":"bare"},
        {"name":"excerpt","kind":"string"},{"name":"publishDate","kind":"string"}
    ],"GET /api/v1/Books":[{"name":"wrongMethod","kind":"string"}]}});
    for suffix in ["", "/"] {
        let prefix = format!(
            "#check test1\n#endpoint\nPOST https://fakerestapi.azurewebsites.net/api/v1/Books{suffix}\n#headers\nContent-Type: application/json\nAccept: application/json\n#body\n"
        );
        for body in ["{\n  ", "{\n  \""] {
            let source = format!("{prefix}{body}");
            let result = suggest(
                "rest",
                "",
                &source,
                8,
                source.lines().last().unwrap().encode_utf16().count(),
                &[],
                &discovery,
            );
            assert_eq!(
                result
                    .candidates
                    .iter()
                    .map(|candidate| candidate.text.as_str())
                    .collect::<Vec<_>>(),
                vec![
                    "\"id\": ",
                    "\"title\": \"\"",
                    "\"description\": \"\"",
                    "\"pageCount\": ",
                    "\"excerpt\": \"\"",
                    "\"publishDate\": \"\""
                ],
                "{suffix:?}: {body:?}"
            );
        }
        let mut form = discovery.clone();
        form["requestEndpoint"] = json!(format!(
            "POST https://fakerestapi.azurewebsites.net/api/v1/Books{suffix}"
        ));
        assert_eq!(
            suggest("rest", "body", "{\n  \"", 1, 3, &[], &form)
                .candidates
                .len(),
            6
        );
    }
}

#[test]
fn empty_body_offers_a_required_only_json_template() {
    let fields = json!([
        {"name":"name","kind":"string","required":true},
        {"name":"count","kind":"bare","type":"integer","required":true},
        {"name":"enabled","kind":"bare","type":"boolean","required":true},
        {"name":"tags","kind":"array","required":true},
        {"name":"settings","kind":"object","required":true,"fields":[
            {"name":"locale","kind":"string","required":true},
            {"name":"optionalNested","kind":"string"}
        ]},
        {"name":"optional","kind":"string"},
        {"name":"readOnly","kind":"string","required":true,"readOnly":true}
    ]);
    let discovery = json!({
        "requestEndpoint":"POST http://{{var:HOST}}/token",
        "bodySchemas":{"POST /token":fields}
    });
    let hints = vec![("{{var:HOST}}".into(), "fixture.invalid".into())];
    for field in ["", "body"] {
        let source = if field.is_empty() {
            "#check token\n#endpoint\nPOST http://{{var:HOST}}/token\n#body\n  "
        } else {
            "  "
        };
        let result = suggest(
            "rest",
            field,
            source,
            if field.is_empty() { 4 } else { 0 },
            2,
            &hints,
            &discovery,
        );
        assert_eq!(result.candidates.len(), 2);
        let template = &result.candidates[0];
        assert_eq!(template.detail, "Required fields");
        assert_eq!(
            serde_json::from_str::<Value>(&template.text).unwrap(),
            json!({
                "name":"","count":0,"enabled":false,"tags":[],"settings":{"locale":""}
            })
        );
        assert!(template.text.contains("\n    \""));
        assert_eq!(result.candidates[1].text, "{\n    \n  }");
        let (text, cursor) = prepare_insertion("rest", &template.text, field.is_empty());
        assert_eq!(&text[cursor.unwrap() - 1..cursor.unwrap() + 1], "\"\"");
    }
    let existing = suggest(
        "rest",
        "body",
        "\n{\"name\":\"kept\"}",
        0,
        0,
        &hints,
        &discovery,
    );
    assert!(
        !existing
            .candidates
            .iter()
            .any(|c| c.detail == "Required fields"),
        "A template must not be offered in front of an existing body"
    );
    for discovery in [
        json!({"requestEndpoint":"GET http://fixture.invalid/token","bodySchemas":{"POST /token":fields}}),
        json!({"requestEndpoint":"POST http://fixture.invalid/token","bodySchemas":{"POST /token":[{"name":"optional","kind":"string"}]}}),
        json!({"warning":"No schema"}),
    ] {
        let result = suggest("rest", "body", "", 0, 0, &[], &discovery);
        assert_eq!(result.candidates.len(), 1);
        assert_eq!(result.candidates[0].detail, "JSON object");
    }
    let result = suggest("rest", "body", "{\n  ", 1, 2, &hints, &discovery);
    assert!(
        result
            .candidates
            .iter()
            .any(|c| c.text == "\"optional\": \"\"")
    );
    assert!(
        !result
            .candidates
            .iter()
            .any(|c| c.text.contains("readOnly"))
    );
    assert!(
        !result
            .candidates
            .iter()
            .any(|c| c.detail == "Required fields")
    );
}

#[test]
fn body_fields_resolve_variable_urls_in_console_and_form() {
    let discovery = json!({"bodySchemas":{
        "POST /":[{"name":"rootOnly","kind":"string"}],
        "POST /rest/default/V1/integration/admin/token":[
            {"name":"username","kind":"string"},
            {"name":"password","kind":"string"}
        ],
        "GET /rest/default/V1/integration/admin/token":[
            {"name":"wrongMethod","kind":"string"}
        ]
    }});
    let hints = profiles::hints(&json!({
        "order":["var"],
        "profiles":{"var":{
            "magento":"store.fixture.invalid",
            "base":"http://store.fixture.invalid",
            "store":"default",
            "token":"http://store.fixture.invalid/rest/default/V1/integration/admin/token"
        }},
        "env":{"MAGENTO":"http://store.fixture.invalid"}
    }));
    for endpoint in [
        "http://{{var:magento}}/rest/default/V1/integration/admin/token",
        "http://{{:magento}}/rest/default/V1/integration/admin/token",
        "http://{{var:magento}}/rest/{{var:store}}/V1/integration/admin/token",
        "{{var:base}}/rest/default/V1/integration/admin/token",
        "{{env:MAGENTO}}/rest/default/V1/integration/admin/token",
        "{{var:token}}",
    ] {
        for suffix in ["", "/"] {
            let endpoint = format!("{endpoint}{suffix}");
            for body in ["{\n  ", "{\n  \""] {
                let source = format!(
                    "#check magento\n#endpoint\nPOST {endpoint}\n#headers\nContent-Type: application/json\n#body\n{body}"
                );
                let ch = body.lines().last().unwrap().encode_utf16().count();
                let console = suggest("rest", "", &source, 7, ch, &hints, &discovery);
                let mut form_discovery = discovery.clone();
                form_discovery["requestEndpoint"] = json!(format!("POST {endpoint}"));
                let form = suggest("rest", "body", body, 1, ch, &hints, &form_discovery);
                for result in [console, form] {
                    assert_eq!(
                        result
                            .candidates
                            .iter()
                            .map(|c| c.text.as_str())
                            .collect::<Vec<_>>(),
                        vec!["\"username\": \"\"", "\"password\": \"\""],
                        "{endpoint}: {body:?}"
                    );
                }
            }
        }
    }
    for endpoint in [
        "http://{{missing:magento}}/rest/default/V1/integration/admin/token",
        "{{var:base",
    ] {
        let mut discovery = discovery.clone();
        discovery["requestEndpoint"] = json!(format!("POST {endpoint}"));
        let result = suggest("rest", "body", "{\n  ", 1, 2, &hints, &discovery);
        assert!(
            result.candidates.is_empty(),
            "An unresolved URL must not borrow a schema from a previous connection"
        );
    }
}

#[test]
fn body_schema_prefers_exact_paths_and_requires_nonempty_parameters() {
    let discovery = json!({"bodySchemas":{
        "POST /api/v1/Books":[{"name":"book","kind":"string"}],
        "POST /api/v1/Books/":[{"name":"slashBook","kind":"string"}],
        "POST /api/v1/Books/{id}":[{"name":"byId","kind":"string"}]
    }});
    for (path, field) in [
        ("Books", "book"),
        ("Books/", "slashBook"),
        ("Books/42/", "byId"),
    ] {
        let source = format!(
            "#check test1\n#endpoint\nPOST https://fixture.invalid/api/v1/{path}\n#body\n{{\n  "
        );
        let result = suggest("rest", "", &source, 5, 2, &[], &discovery);
        assert_eq!(result.candidates[0].text, format!("\"{field}\": \"\""));
    }
    let mut discovery = discovery;
    discovery["bodySchemas"]
        .as_object_mut()
        .unwrap()
        .remove("POST /api/v1/Books/");
    let source =
        "#check test1\n#endpoint\nPOST https://fixture.invalid/api/v1/Books/\n#body\n{\n  ";
    assert_eq!(
        suggest("rest", "", source, 5, 2, &[], &discovery).candidates[0].text,
        "\"book\": \"\""
    );
}

#[test]
fn enum_fields_are_labeled_and_continue_with_value_choices() {
    let discovery = json!({"requestEndpoint":"POST https://fixture.invalid/item","bodySchemas":{
        "POST /item":[
            {"name":"status","kind":"string","type":"string","enum":["pending","active"]},
            {"name":"priority","kind":"bare","type":"integer","enum":[1,2]},
            {"name":"mixed","kind":"string","enum":[true,null,1,"one"]},
            {"name":"title","kind":"string"}
        ]
    }});
    let result = suggest("rest", "body", "{\n  ", 1, 2, &[], &discovery);
    assert_eq!(
        result
            .candidates
            .iter()
            .map(|candidate| (candidate.detail.as_str(), candidate.continue_hint))
            .collect::<Vec<_>>(),
        vec![
            ("string · enum", true),
            ("integer · enum", true),
            ("enum", true),
            ("string", false)
        ]
    );
    let required_template = |fields: Value| {
        let discovery = json!({"requestEndpoint":"POST https://fixture.invalid/item",
            "bodySchemas":{"POST /item":fields}});
        suggest("rest", "body", "", 0, 0, &[], &discovery)
            .candidates
            .into_iter()
            .find(|candidate| candidate.detail == "Required fields")
            .map(|candidate| serde_json::from_str::<Value>(&candidate.text).unwrap())
    };
    assert_eq!(
        required_template(json!([
            {"name":"status","kind":"string","required":true,"enum":["pending","active"]},
            {"name":"priority","kind":"bare","type":"integer","required":true,"enum":[2,3]},
            {"name":"value","kind":"string","required":true,"enum":[null,true]}
        ])),
        Some(json!({"status":"pending","priority":2,"value":null}))
    );
    assert!(
        required_template(json!([{"name":"impossible","kind":"string","required":true,"enum":[]}]))
            .is_none()
    );
}

#[test]
fn enum_selection_replaces_complete_values_with_valid_json() {
    let values = json!(["pending","active","ки😀\"\\",1,true,null,["a"],{"key":"value"}]);
    let discovery = json!({"requestEndpoint":"POST https://fixture.invalid/item","bodySchemas":{
        "POST /item":[{"name":"status","kind":"string","enum":values},{"name":"title","kind":"string"}]
    }});
    for marked in [
        "{\"status\": |, \"title\":\"kept\"}",
        "{\"status\": \"|\", \"title\":\"kept\"}",
        "{\"status\": \"ac|tive\", \"title\":\"kept\"}",
        "{\"status\": t|rue, \"title\":\"kept\"}",
        "{\"status\": |null, \"title\":\"kept\"}",
        "{\"status\": \"ки😀\\\"|\\\\\", \"title\":\"kept\"}",
    ] {
        let cursor = marked.find('|').unwrap();
        let source = marked.replacen('|', "", 1);
        let result = suggest(
            "rest",
            "body",
            &source,
            0,
            source[..cursor].encode_utf16().count(),
            &[],
            &discovery,
        );
        assert_eq!(
            result.candidates.len(),
            values.as_array().unwrap().len(),
            "{marked}"
        );
        let byte_at = |count: usize| {
            let mut units = 0;
            source
                .chars()
                .take_while(|c| {
                    units += c.len_utf16();
                    units <= count
                })
                .map(char::len_utf8)
                .sum::<usize>()
        };
        for candidate in result.candidates {
            let mut edited = source.clone();
            edited.replace_range(byte_at(result.from)..byte_at(result.to), &candidate.text);
            let parsed: Value = serde_json::from_str(&edited).unwrap();
            assert_eq!(parsed["title"], "kept");
            assert!(values.as_array().unwrap().contains(&parsed["status"]));
            assert!(!candidate.continue_hint);
        }
    }
}

#[test]
fn enum_partial_values_filter_and_keep_adjacent_fields() {
    let discovery = json!({"requestEndpoint":"POST https://fixture.invalid/item","bodySchemas":{
        "POST /item":[{"name":"status","kind":"string","enum":["pending","active"]}]
    }});
    for marked in [
        "{\"status\": \"pe|\", \"other\":0}",
        "{\"status\": pe|, \"other\":0}",
        "{\"status\": \"pe|",
    ] {
        let cursor = marked.find('|').unwrap();
        let source = marked.replacen('|', "", 1);
        let result = suggest("rest", "body", &source, 0, cursor, &[], &discovery);
        assert_eq!(
            result
                .candidates
                .iter()
                .map(|candidate| candidate.text.as_str())
                .collect::<Vec<_>>(),
            vec!["\"pending\""]
        );
        assert_eq!(result.from, 11);
    }
}

#[test]
fn enum_values_work_in_nested_array_items_and_console_sections() {
    let discovery = json!({"requestEndpoint":"POST https://fixture.invalid/item","bodySchemas":{
        "POST /item":[{"name":"entries","kind":"array","items":{"kind":"object","fields":[
            {"name":"status","kind":"string","enum":["pending","active"]},
            {"name":"tags","kind":"array","items":{"kind":"string","enum":["red","blue"]}}
        ]}}]
    }});
    for (body, expected) in [
        (
            "{\"entries\":[{\"status\": ",
            vec!["\"pending\"", "\"active\""],
        ),
        ("{\"entries\":[{\"tags\":[", vec!["\"red\"", "\"blue\""]),
        (
            "{\"entries\":[{\"tags\":[\"red\", ",
            vec!["\"red\"", "\"blue\""],
        ),
    ] {
        for field in ["", "body"] {
            let source = if field.is_empty() {
                format!("#check enums\n#endpoint\nPOST https://fixture.invalid/item\n#body\n{body}")
            } else {
                body.into()
            };
            let result = suggest(
                "rest",
                field,
                &source,
                source.lines().count() - 1,
                body.encode_utf16().count(),
                &[],
                &discovery,
            );
            assert_eq!(
                result
                    .candidates
                    .iter()
                    .map(|candidate| candidate.text.as_str())
                    .collect::<Vec<_>>(),
                expected
            );
        }
    }
}

#[test]
fn scalar_hints_show_actual_types_instead_of_internal_insertion_kinds() {
    let discovery = json!({"requestEndpoint":"POST https://fixture.invalid/item","bodySchemas":{
        "POST /item":[
            {"name":"count","kind":"bare","type":"integer"},
            {"name":"price","kind":"bare","type":"number"},
            {"name":"enabled","kind":"bare","type":"boolean"},
            {"name":"id","kind":"bare","type":"integer","format":"int32"},
            {"name":"legacy","kind":"bare"}
        ]
    }});
    for field in ["", "body"] {
        let source = if field.is_empty() {
            "#check types\n#endpoint\nPOST https://fixture.invalid/item\n#body\n{\n  \""
        } else {
            "{\n  \""
        };
        let result = suggest(
            "rest",
            field,
            source,
            source.lines().count() - 1,
            3,
            &[],
            &discovery,
        );
        assert_eq!(
            result
                .candidates
                .iter()
                .map(|candidate| (candidate.text.as_str(), candidate.detail.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("\"count\": ", "integer"),
                ("\"price\": ", "number"),
                ("\"enabled\": ", "boolean"),
                ("\"id\": ", "int32"),
                ("\"legacy\": ", "value (type unavailable)")
            ]
        );
    }
}

#[test]
fn date_hints_show_the_schema_format_without_choosing_a_value() {
    let discovery = json!({"requestEndpoint":"POST https://fixture.invalid/api/v1/Books/","bodySchemas":{
        "POST /api/v1/Books":[
            {"name":"publishDate","kind":"string","format":"date-time"},
            {"name":"startDate","kind":"string","format":"date"},
            {"name":"title","kind":"string"}
        ]
    }});
    let result = suggest("rest", "body", "{\n  \"", 1, 3, &[], &discovery);
    assert_eq!(
        result
            .candidates
            .iter()
            .map(|candidate| (candidate.text.as_str(), candidate.detail.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("\"publishDate\": \"\"", "date-time (ISO 8601)"),
            ("\"startDate\": \"\"", "date (YYYY-MM-DD)"),
            ("\"title\": \"\"", "string")
        ]
    );
}

#[test]
fn nested_schema_uses_operation_and_path() {
    let d = json!({"requestEndpoint":"POST https://example.test/users/42","bodySchemas":{
        "GET /users/{id}":[{"name":"incorrect","kind":"string"}],
        "POST /users/{id}":[{"name":"address","kind":"object","fields":[{"name":"city","kind":"string"},{"name":"zip","kind":"bare"}]},{"name":"topLevel","kind":"bare"}]
    }});
    let source = "{\"address\": {\n  \"city\": \"Paris\"\n  zi";
    let r = suggest("rest", "body", source, 2, 4, &[], &d);
    assert_eq!(r.candidates.len(), 1);
    assert_eq!(r.candidates[0].text, "\"zip\": ");
    assert_eq!(r.comma, Some((1, 17)));
}

#[test]
fn path_completion_after_correcting_host_reuses_existing_slash() {
    let path = "/api/pets";
    let discovery = json!({"paths":[path],"methodsByPath":{path:["GET"]}});
    for (field, prefix) in [
        ("", "GET https://fixture.invalid"),
        ("", "GET \u{a0}https://пример.рф"),
        ("endpoint", "https://fixture.invalid"),
    ] {
        let endpoint = format!("{prefix}/api/");
        let source = if field.is_empty() {
            format!("#endpoint\n{endpoint}")
        } else {
            endpoint.clone()
        };
        let result = suggest(
            "rest",
            field,
            &source,
            usize::from(field.is_empty()),
            prefix.encode_utf16().count(),
            &[],
            &discovery,
        );
        assert_eq!(result.candidates.len(), 1, "{endpoint}");
        assert_eq!(result.from, prefix.encode_utf16().count());
        assert_eq!(result.to, endpoint.encode_utf16().count());
        let mut units = endpoint.encode_utf16().collect::<Vec<_>>();
        units.splice(
            result.from..result.to,
            result.candidates[0].text.encode_utf16(),
        );
        assert_eq!(
            String::from_utf16(&units).unwrap(),
            format!("{prefix}{path}")
        );
    }
}

#[test]
fn path_completion_preserves_method_spacing_and_entire_host() {
    let path = "/api/v3/pet/findByStatus";
    let discovery = json!({"paths":[path],"methodsByPath":{path:["GET"]}});
    for spacing in [" ", "  ", "\t", "\u{a0}", " \u{a0}", "\u{2003}\t"] {
        let prefix = format!("GET{spacing}https://petstore3.swagger.io");
        let endpoint = format!("{prefix}/api/v3/");
        let source = format!("#check test1\n#endpoint\n{endpoint}");
        let result = suggest(
            "rest",
            "",
            &source,
            2,
            endpoint.encode_utf16().count(),
            &[],
            &discovery,
        );
        assert_eq!(result.candidates.len(), 1, "{spacing:?}");
        assert_eq!(result.from, prefix.encode_utf16().count(), "{spacing:?}");
        let mut units = endpoint.encode_utf16().collect::<Vec<_>>();
        units.splice(
            result.from..result.to,
            result.candidates[0].text.encode_utf16(),
        );
        assert_eq!(
            String::from_utf16(&units).unwrap(),
            format!("{prefix}{path}"),
            "{spacing:?}"
        );
    }
}

#[test]
fn path_candidates_respect_method() {
    let d = json!({"paths":["/alpha","/beta"],"methodsByPath":{"/alpha":["GET"],"/beta":["POST"]}});
    let source = "#check\n#endpoint\nPOST https://example.test/";
    let r = suggest(
        "rest",
        "",
        source,
        2,
        source.lines().last().unwrap().len(),
        &[],
        &d,
    );
    assert_eq!(
        r.candidates
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>(),
        vec!["/beta"]
    );
}

#[test]
fn slash_discovery_only_reads_the_endpoint_at_the_caret() {
    for (service, field, source, expected) in [
        (
            "rest",
            "endpoint",
            "https://fixture.invalid/",
            Some("https://fixture.invalid/"),
        ),
        (
            "rest",
            "",
            "#endpoint\nGET \u{a0}https://пример.рф/api/",
            Some("https://пример.рф/api/"),
        ),
        ("rest", "endpoint", "https://", None),
        ("rest", "endpoint", "https:/", None),
        ("rest", "headers", "Referer: https://fixture.invalid/", None),
        (
            "rest",
            "",
            "#endpoint\nGET https://fixture.invalid/\n#headers\nReferer: https://fixture.invalid/",
            None,
        ),
        (
            "rest",
            "body",
            "{\"url\":\"https://fixture.invalid/\"}",
            None,
        ),
        (
            "rest",
            "",
            "#endpoint\nGET https://fixture.invalid/\n#body\nhttps://fixture.invalid/",
            None,
        ),
        ("graphql", "endpoint", "https://fixture.invalid/", None),
        ("mysql", "endpoint", "mysql://fixture.invalid/", None),
    ] {
        let line = source.split('\n').count() - 1;
        let ch = source.rsplit('\n').next().unwrap().encode_utf16().count();
        assert_eq!(
            discovery_endpoint(service, field, source, line, ch).as_deref(),
            expected,
            "{service} {field}: {source}"
        );
    }
    let source = "#endpoint\nGET https://fixture.invalid/api/";
    let endpoint =
        discovery_endpoint("rest", "", source, 1, "GET https://fixture.invalid".len()).unwrap();
    assert!(
        !endpoint.ends_with('/'),
        "A slash to the right of the caret is not a newly entered slash"
    );
}

#[test]
fn discovered_headers_merge_without_overriding_saved_values() {
    let hints = vec![("X-Auth: token".into(), "Saved hint".into())];
    let d = json!({"headers":["X-Auth","X-Trace"]});
    let r = suggest("rest", "headers", "", 0, 0, &hints, &d);
    assert_eq!(
        r.candidates
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>(),
        vec!["X-Auth: token", "X-Trace: "]
    );
    let r = suggest("rest", "headers", "trace", 0, 5, &hints, &d);
    assert_eq!(r.candidates[0].text, "X-Trace: ");
}

#[test]
fn path_parameters_are_distinct_from_variables_and_query_values() {
    let source = "PUT http://{{var:magento}}/rest/default/V1/categories/{categoryId}/move/{имя-id}?filter={query}";
    let result = tokens_for(Language::Http, source, &mut State::default());
    assert_eq!(
        result
            .iter()
            .filter(|(_, style)| *style == Some("rest-path-param"))
            .map(|(text, _)| text.as_str())
            .collect::<Vec<_>>(),
        vec!["{categoryId}", "{имя-id}"]
    );
    assert!(
        result
            .iter()
            .any(|(_, style)| *style == Some("gw-placeholder"))
    );
    for (language, text) in [
        (Language::Http, "http://test.com/categories/42/move"),
        (Language::Http, "http://test.com/{incomplete"),
        (Language::Http, "http://test.com/{}"),
        (Language::Http, "http://test.com/{not a name}"),
        (Language::Http, "http://test.com/#fragment/{categoryId}"),
        (Language::Grpc, "grpc://test.com/{categoryId}"),
        (Language::Endpoint, "http://test.com/{categoryId}"),
    ] {
        assert!(
            !tokens_for(language, text, &mut State::default())
                .iter()
                .any(|(_, style)| *style == Some("rest-path-param")),
            "{language:?}: {text}"
        );
    }
}

#[test]
fn book_history_matches_the_visible_check_without_changing_stored_body() {
    let body = r#"{
  "id": 2,
  "title": "Example Book Title",
  "description": "Sample description of the book.",
  "pageCount": 320,
  "excerpt": "Sample excerpt from the book.",
  "publishDate": "2024-01-15T00:00:00Z"
}"#;
    let request = format!(
        "#check test1\n#endpoint\nPUT https://fakerestapi.azurewebsites.net/api/v1/Books/2\n#headers\nContent-Type: application/json\nAccept: application/json\n#body\n{body}"
    );
    let raw = serde_json::from_str::<Value>(body).unwrap().to_string();
    for header in ["#response success", "#responce OK"] {
        let snapshot = format!("{request}\n\n\n{header}\n{raw}");
        assert_eq!(
            history_snapshot("rest", &snapshot, "5:47:15 PM", true).unwrap(),
            format!("{request}\n#responce 5:47:15 PM · OK\n{body}")
        );
        assert_eq!(console_response("rest", &json!({"body":raw}), false), body);
        assert!(snapshot.ends_with(&raw));
    }
}

#[test]
fn body_discovery_only_belongs_to_body_content() {
    let source = "#check c\n#endpoint\nPOST http://test.com/site1/rest/v1/product\n#headers\nX-Token: value\n#body\n\n#check next\n#endpoint\nGET http://other.com/";
    for line in 0..source.lines().count() {
        assert_eq!(body_position("rest", "", source, line), line == 6);
    }
    assert!(body_position("rest", "body", "", 0));
    assert!(!body_position("rest", "endpoint", "", 0));
    assert!(!body_position("graphql", "body", "", 0));
}

fn rest_address(url: &str, token: &str) -> String {
    rest_key(url, token)["url"].as_str().unwrap().to_owned()
}

fn rest_key(url: &str, token: &str) -> Value {
    key(
        "rest",
        &json!({"url":url,"headers":{"X-Token":token}}),
        &json!({}),
    )
    .unwrap()
}

#[test]
fn cache_retains_independent_sites_and_ignores_headers() {
    let mut cache = LazyCache::default();
    for (site, token) in [("site1", "a"), ("site2", "a")] {
        let base = format!("http://test.com/{site}/");
        cache.insert(
            &rest_address(&format!("{base}rest/v1/product"), token),
            json!({
                "baseUrl":base,"paths":[format!("/{site}/rest/v1/product")],"token":token
            }),
        );
    }
    for (site, token) in [("site1", "a"), ("site2", "a"), ("site1", "b")] {
        let schema = cache
            .get(&rest_address(
                &format!("http://test.com/{site}/rest/v1/other"),
                token,
            ))
            .unwrap();
        assert!(schema.get("baseUrl").is_none());
        assert_eq!(
            schema["paths"],
            json!([format!("/{site}/rest/v1/product")]),
            "{site} keeps its own schema"
        );
    }
    for url in [
        "http://test.com/site10/rest/v1/product",
        "http://other.com/site1/rest/v1/product",
        "https://test.com/site1/rest/v1/product",
    ] {
        assert!(cache.get(&rest_address(url, "a")).is_none());
    }
    assert!(
        cache
            .get(&rest_address("http://test.com/site2/rest/v1/product", "b"))
            .is_some()
    );
    assert_eq!(
        rest_key("http://test.com/site1/", "a"),
        rest_key("http://test.com/site1/", "b")
    );
}

#[test]
fn cache_reuses_root_paths_and_prefers_specific_bases() {
    let mut cache = LazyCache::default();
    cache.insert(
        &rest_address("http://test.com/", ""),
        json!({
            "baseUrl":"http://test.com/","paths":["/site1/rest/v1/product"],"name":"root"
        }),
    );
    cache.insert(
        &rest_address("http://test.com/site1/rest/v1/product", ""),
        json!({
            "baseUrl":"http://test.com/site1/","paths":["/site1/rest/v1/product"],"name":"site"
        }),
    );
    assert_eq!(
        cache
            .get(&rest_address("http://test.com/site1/rest/v1/product", ""))
            .unwrap()["name"],
        "site"
    );
    assert_eq!(
        cache
            .get(&rest_address("http://test.com/site2/rest/v1/product", ""))
            .unwrap()["name"],
        "root"
    );
}

#[test]
fn failed_rest_prefix_does_not_block_the_next_prefix_or_create_a_schema() {
    let mut cache = LazyCache::default();
    let root = rest_address("http://test.com/", "");
    cache.attempt(&root);
    assert!(cache.attempted(&root));
    assert!(!cache.attempted(&rest_address("http://test.com/site1/", "")));
    assert!(cache.get(&root).is_none());
    cache.insert(&root, json!({"warning":"No schema"}));
    assert!(cache.get(&root).is_none());
}

#[test]
fn cache_canonicalizes_query_fragment_and_resolved_variable_urls() {
    let variables = json!({"profiles":{"var":{"host":"test.com","site":"site1"}},"order":["var"]});
    let resolved = key("rest", &json!({"url":"http://{{var:host}}/{{var:site}}/rest/v1/product?x=1#part","headers":{"X-Token":""}}), &variables).unwrap();
    assert_eq!(
        resolved,
        rest_key("http://test.com/site1/rest/v1/product", "")
    );
    assert_eq!(
        rest_key("http://TEST.com:80", ""),
        rest_key("http://test.com/", "")
    );
}

fn reuses(previous: &Value, next: &Value, schema: &Value) -> bool {
    reuses_schema(
        previous["url"].as_str().unwrap(),
        next["url"].as_str().unwrap(),
        schema,
    )
}

#[test]
fn schema_is_reused_within_its_api_regardless_of_headers() {
    let previous = json!({"url":"https://fixture.invalid/api/v3/","headers":{"X-Token":"first"}});
    let schema = json!({"basePath":"/api/v3","paths":["/api/v3/pets"]});
    for path in ["/api/v3", "/api/v3/pets", "/api/v3/users?limit=2"] {
        let next =
            json!({"url":format!("https://fixture.invalid{path}"),"headers":{"X-Token":"first"}});
        assert!(reuses(&previous, &next, &schema), "{path}");
    }
    for next in [
        json!({"url":"https://fixture.invalid/api/v30/pets","headers":{"X-Token":"first"}}),
        json!({"url":"https://fixture.invalid/other-api/pets","headers":{"X-Token":"first"}}),
        json!({"url":"https://other.invalid/api/v3/pets","headers":{"X-Token":"first"}}),
    ] {
        assert!(!reuses(&previous, &next, &schema), "{next}");
    }
    assert!(reuses(
        &previous,
        &json!({"url":"https://fixture.invalid/api/v3/pets","headers":{"X-Token":"second"}}),
        &schema
    ));
    assert!(!reuses(
        &previous,
        &previous,
        &json!({"warning":"No schema"})
    ));
    assert!(!reuses(
        &previous,
        &previous,
        &json!({"basePath":"/api/v3","paths":[]})
    ));
    assert!(reuses(
        &previous,
        &previous,
        &json!({"basePath":"","paths":["/api/v3/pets"]})
    ));
}
