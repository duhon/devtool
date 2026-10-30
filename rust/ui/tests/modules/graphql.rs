//! GraphQL module: schema-aware completion, operation folding, object-field
//! ranges and syntax coloring. UI-behaviour tests.

use crate::support::tokens;
use serde_json::{Value, json};
use services_ui::completion::{self, prepare_insertion, suggest};
use services_ui::graphql::{object_fields, operation_fold};
use services_ui::syntax::{Language, State};

#[test]
fn existing_separator_completion_preserves_its_suffix_and_other_requests() {
    let source = "#check first\n#endpoint\nurl\n#query\nquery { viewer { name } }\n#check second\n  #endpoint\nurl\n#variables\n{}\n#query\nquery { viewer { name } }";
    let result = suggest("graphql", "", source, 6, 6, &[], &Value::Null);
    assert_eq!((result.from, result.to), (2, 11));
    assert_eq!(
        result
            .candidates
            .iter()
            .map(|candidate| candidate.text.as_str())
            .collect::<Vec<_>>(),
        ["#endpoint"]
    );
    let result = suggest("graphql", "", source, 5, 4, &[], &Value::Null);
    assert_eq!((result.from, result.to), (0, 6));
    assert_eq!(
        result
            .candidates
            .iter()
            .map(|candidate| candidate.text.as_str())
            .collect::<Vec<_>>(),
        ["#check"]
    );
    let source = "#check first\n#ENDPOINT\nurl\n#";
    let result = suggest("graphql", "", source, 3, 1, &[], &Value::Null);
    assert!(
        !result
            .candidates
            .iter()
            .any(|candidate| candidate.text == "#endpoint")
    );
}

#[test]
fn completed_graphql_operations_offer_separators_without_polluting_field_hints() {
    let prefix = "#check first\n#endpoint\nhttps://example.test/graphql\n#query\n";
    for operation in [
        "query Q { viewer { name } }",
        "mutation M { update { name } }",
        "subscription S { changed { name } }",
        "{ viewer { name } }",
    ] {
        let source = format!("{prefix}{operation}\n");
        let result = suggest(
            "graphql",
            "",
            &source,
            source.split('\n').count() - 1,
            0,
            &[],
            &Value::Null,
        );
        assert_eq!(
            result
                .candidates
                .iter()
                .map(|candidate| candidate.text.as_str())
                .collect::<Vec<_>>(),
            ["#check", "#headers", "#variables"]
        );
        assert!(result.automatic);
    }
    for body in [
        "query Q {\n",
        "query Q { viewer {\n",
        "query Q($filter: Input = { nested: {} }\n",
    ] {
        let source = format!("{prefix}{body}");
        let result = suggest(
            "graphql",
            "",
            &source,
            source.split('\n').count() - 1,
            0,
            &[],
            &Value::Null,
        );
        assert!(
            !result
                .candidates
                .iter()
                .any(|candidate| candidate.text.starts_with('#'))
        );
    }
}

#[test]
fn nested_graphql_snippets_preserve_indent_and_place_cursor_inside() {
    let d = json!({"queryType":"Query","types":{
        "Query":{"fields":[{"name":"viewer","type":"User"}]},
        "User":{"fields":[{"name":"address","type":"Address"}]},
        "Address":{"fields":[{"name":"city","type":"String"}]}
    }});
    for indent in ["    ", "\t\t"] {
        let source = format!("{{\n  viewer {{\n{indent}ad");
        let r = suggest("graphql", "query", &source, 2, indent.len() + 2, &[], &d);
        assert_eq!(r.candidates[0].text, "address");
        let source = format!("{{\n  viewer {{\n{indent}address ");
        let r = suggest("graphql", "query", &source, 2, indent.len() + 8, &[], &d);
        let expected = format!("{{\n{indent}  \n{indent}}}");
        assert_eq!(r.candidates.len(), 3);
        assert_eq!(r.candidates[0].text, expected);
        assert_eq!(
            prepare_insertion("graphql", &expected, false),
            (expected.clone(), Some(format!("{{\n{indent}  ").len()))
        );
    }
}

#[test]
fn variables_offer_only_json_templates() {
    let hints = [
        "query",
        "mutation",
        "subscription",
        "fragment",
        "__typename",
        "products",
        "skus",
        "https://fixture.invalid/graphql",
    ]
    .map(|text| (text.into(), "GraphQL".into()));
    let discovery = json!({
        "queryType":"Query",
        "types":{"Query":{"fields":[{"name":"products","type":"String"}]}},
        "fields":[{"name":"skus","kind":"array"}],
        "bodySchemas":{"GET /":[{"name":"products","kind":"string"}]}
    });
    for (field, prefix) in [
        ("variables", ""),
        ("", "#check g\n#variables\n"),
        ("", "#variables\n"),
    ] {
        let cases: &[(&str, &[&str])] = &[
            ("", &["JSON object"]),
            ("  ", &["JSON object"]),
            ("query", &[]),
            ("pro", &[]),
            ("{\n  ", &["JSON property"]),
            ("{\n  \"sk", &[]),
            ("{\"skus\": ", &["JSON string", "JSON object", "JSON array"]),
            (
                "{\"skus\": [",
                &["JSON string", "JSON object", "JSON array"],
            ),
            (
                "{\"skus\": [\"fixture\", ",
                &["JSON string", "JSON object", "JSON array"],
            ),
            ("{\"skus\": [{", &["JSON property"]),
            (
                "{\"skus\": [[",
                &["JSON string", "JSON object", "JSON array"],
            ),
            (
                "{\"a\": 1, \"b\": true, \"c\": false, \"d\": null, ",
                &["JSON property"],
            ),
            ("{\"skus\": [\"fixture\"]}", &[]),
            ("{\"skus\": [\"fixture\"]}\n", &[]),
            ("{\"skus\": \"", &[]),
            ("{\"skus\": \"brace } colon : comma , [ escaped \\\"", &[]),
            ("{\"skus\"", &[]),
            ("{\"skus\": tru", &[]),
            ("{\"skus\": 123", &[]),
            ("{\"skus\": true ", &[]),
            ("{\"skus\": [}", &[]),
            ("{\"skus\" \"", &[]),
            ("{\"skus\": : ", &[]),
            ("{\"skus\": nope, ", &[]),
            ("[]", &[]),
        ];
        for &(body, details) in cases {
            let head = format!("{prefix}{body}");
            let line = head.split('\n').count() - 1;
            let before = head.split('\n').next_back().unwrap();
            let source = if field.is_empty() {
                format!("{head}\n#query\nquery {{ products }}")
            } else {
                head.clone()
            };
            let result = suggest(
                "graphql",
                field,
                &source,
                line,
                before.encode_utf16().count(),
                &hints,
                &discovery,
            );
            assert_eq!(
                result
                    .candidates
                    .iter()
                    .map(|c| c.detail.as_str())
                    .collect::<Vec<_>>(),
                details,
                "{source}"
            );
            assert_eq!(result.from, before.encode_utf16().count());
            assert_eq!(result.to, result.from);
            for candidate in result.candidates {
                let expected = match candidate.detail.as_str() {
                    "JSON object" => json_block(before, '{', '}'),
                    "JSON array" => json_block(before, '[', ']'),
                    _ => "\"\"".into(),
                };
                assert_eq!(candidate.text, expected, "{source}");
                assert!(
                    prepare_insertion("graphql", &candidate.text, field.is_empty())
                        .1
                        .is_some()
                );
            }
        }
    }
}

#[test]
fn json_templates_preserve_existing_values_and_delimiters() {
    for (marked, count) in [
        ("{|}", 1),
        ("{\n  |\"existing\": 1\n}", 0),
        ("{\"😀имя\": |}", 3),
        ("{\"key\": |\"existing\"}", 0),
        ("{\"key\": |false}", 0),
        ("{\"key\": |{}}", 0),
        ("{\"key\": \n  |[]}", 0),
        ("{\"key\": [|]}", 3),
        ("{\"key\": [|\"existing\"]}", 0),
        ("|{}", 0),
        ("{\"a\": \"\\\"}:,[\", |}", 1),
    ] {
        for (field, prefix) in [("variables", ""), ("", "#check g\n#variables\n\n")] {
            let offset = prefix.len() + marked.find('|').unwrap();
            let source = format!("{prefix}{}", marked.replace('|', ""));
            let source = if field.is_empty() {
                format!("{source}\n#query\nquery {{ products }}")
            } else {
                source
            };
            let before = &source[..offset];
            let line = before.bytes().filter(|c| *c == b'\n').count();
            let ch = before.rsplit('\n').next().unwrap().encode_utf16().count();
            let result = suggest("graphql", field, &source, line, ch, &[], &Value::Null);
            assert_eq!(result.candidates.len(), count, "{source}");
            assert_eq!((result.from, result.to), (ch, ch));
            if count == 3 {
                for candidate in result.candidates {
                    let mut inserted = source.clone();
                    inserted.insert_str(offset, &candidate.text);
                    let body = inserted
                        .strip_prefix(prefix)
                        .unwrap()
                        .split("\n#query")
                        .next()
                        .unwrap();
                    assert!(serde_json::from_str::<Value>(body).is_ok(), "{inserted}");
                }
            }
        }
    }
}

#[test]
fn nested_selection_completes_fields_of_the_resolved_type() {
    let d = json!({"queryType":"Query","types":{"Query":{"fields":[{"name":"viewer","type":"User"}]},"User":{"fields":[{"name":"id","type":"ID"},{"name":"name","type":"String"}]}}});
    let r = suggest("graphql", "query", "query { viewer {\n na", 1, 3, &[], &d);
    assert_eq!(r.candidates[0].text, "name");
}

/// The empty JSON block the editor inserts, indented like the current line.
fn json_block(before: &str, open: char, close: char) -> String {
    let indent: String = before
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();
    format!("{open}\n{indent}  \n{indent}{close}")
}

fn named(name: &str) -> Value {
    json!({"kind":"SCALAR","name":name})
}

#[test]
fn operation_folds_include_multiline_headers_but_not_input_defaults_alone() {
    let reference: Value =
        serde_json::from_str(include_str!("../fixtures/graphql-explore.json")).unwrap();
    let document = reference["query"]
        .as_str()
        .unwrap()
        .replace("query ExploreCountries(", "query ExploreCountries (")
        .replace(
            "$filter: CountryFilterInput = { continent: { eq: \"EU\" } }",
            "$filter: CountryFilterInput = {\n    continent: {\n      eq: \"EU\"\n    }\n  }",
        );
    let source = &document[document.find("query ExploreCountries").unwrap()..];
    let (from, to) = operation_fold(source).unwrap();
    assert_eq!(&source[..from], "query ExploreCountries");
    assert_eq!(to, source.trim_end().len());
    assert!(source[from..to].contains("$filter: CountryFilterInput"));
    assert!(source[from..to].contains("...LanguageDetails"));
    for kind in ["query", "mutation", "subscription"] {
        let source = format!(
            "{kind} Q(\n  $input: Input = {{ nested: {{ value: \"🦀 }}\" }} }}\n) {{\n  field {{ name }}\n}}\nfragment Later on User {{ name }}"
        );
        let (from, to) = operation_fold(&source).unwrap();
        assert_eq!(&source[..from], format!("{kind} Q"));
        assert_eq!(&source[to..], "\nfragment Later on User { name }");
    }
    let source = "query Q($input: Input = { nested: { value: \"🦀 }\" } }) { viewer { name } }";
    let (from, to) = operation_fold(source).unwrap();
    assert_eq!(&source[from..to], " viewer { name } ");
    assert_eq!(
        operation_fold("query Q($input: Input = {\n nested: {}\n})"),
        None
    );
    assert_eq!(
        operation_fold("query Q\nfragment Later on User { name }"),
        None
    );
    assert_eq!(operation_fold("query Q {\n viewer { name }"), None);
    assert_eq!(operation_fold("fragment U on User {\n name\n}"), None);
}

#[test]
fn operation_folds_ignore_comments_block_strings_and_directive_inputs() {
    let source = "query Q(\n  $text: String = \"\"\"🦀 ) { }\nquery Fake { }\n\"\"\"\n  # ) { }\n) @trace(input: { label: \"}\" }) {\n  # }\n  viewer(text: \"}\") { name }\n}\nquery Next { viewer { name } }";
    let (from, to) = operation_fold(source).unwrap();
    assert_eq!(&source[..from], "query Q");
    assert_eq!(&source[to..], "\nquery Next { viewer { name } }");
    assert_eq!(source[from..to].chars().last(), Some('}'));
    let source = "query\nQ($input: Input = {}) {\n viewer { name }\n}";
    let (from, to) = operation_fold(source).unwrap();
    assert_eq!(&source[..from], "query");
    assert_eq!(to, source.len());
}

fn required(ty: Value) -> Value {
    json!({"kind":"NON_NULL","ofType":ty})
}

fn list(ty: Value) -> Value {
    json!({"kind":"LIST","ofType":ty})
}

fn schema() -> Value {
    json!({"queryType":"Query","mutationType":"Mutation","subscriptionType":"Subscription","types":{
        "Query":{"kind":"OBJECT","fields":[
            {"name":"viewer","type":named("User"),"isObject":true},
            {"name":"user","type":named("User"),"isObject":true,"args":[
                {"name":"id","type":required(named("ID"))},
                {"name":"filter","type":named("Filter")},
                {"name":"state","type":named("State")},
                {"name":"active","type":named("Boolean")},
                {"name":"ids","type":required(list(required(named("ID"))))},
                {"name":"optionalId","type":required(named("ID")),"defaultValue":"\"default\""}
            ]}
        ]},
        "Mutation":{"kind":"OBJECT","fields":[{"name":"updateUser","type":named("User"),"isObject":true,"args":[{"name":"input","type":required(named("Filter"))}]}]},
        "Subscription":{"kind":"OBJECT","fields":[{"name":"userChanged","type":named("User"),"isObject":true,"args":[{"name":"id","type":named("ID")}]}]},
        "User":{"kind":"OBJECT","fields":[{"name":"id","type":named("ID")},{"name":"name","type":named("String")},{"name":"address","type":named("Address"),"isObject":true}]},
        "Address":{"kind":"OBJECT","fields":[{"name":"city","type":named("String")}]},
        "Filter":{"kind":"INPUT_OBJECT","inputFields":[
            {"name":"state","type":named("State")},
            {"name":"tags","type":list(required(named("String")))},
            {"name":"nested","type":named("Nested")}
        ]},
        "Nested":{"kind":"INPUT_OBJECT","inputFields":[{"name":"active","type":required(named("Boolean"))}]},
        "State":{"kind":"ENUM","enumValues":["OPEN","CLOSED"]},
        "ID":{"kind":"SCALAR"},"String":{"kind":"SCALAR"},"Boolean":{"kind":"SCALAR"}
    }})
}

fn complete(source: &str) -> Vec<String> {
    at(source, "query", &schema())
        .candidates
        .into_iter()
        .map(|c| c.text)
        .collect()
}

fn at(source: &str, field: &str, schema: &Value) -> completion::Completion {
    let lines: Vec<_> = source.split('\n').collect();
    completion::suggest(
        "graphql",
        field,
        source,
        lines.len() - 1,
        lines.last().unwrap().encode_utf16().count(),
        &[],
        schema,
    )
}

fn marked_at(source: &str, field: &str, schema: &Value) -> completion::Completion {
    let (before, after) = source.split_once('|').unwrap();
    let lines: Vec<_> = before.split('\n').collect();
    completion::suggest(
        "graphql",
        field,
        &format!("{before}{after}"),
        lines.len() - 1,
        lines.last().unwrap().encode_utf16().count(),
        &[],
        schema,
    )
}

fn explore_schema() -> Value {
    let mut schema = countries_schema();
    schema["types"]["Query"]["fields"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "name":"countries","type":list(required(named("Country"))),
            "args":[{"name":"filter","type":named("CountryFilterInput")}]
        }));
    schema["types"]["Country"]["fields"]
        .as_array_mut()
        .unwrap()
        .extend([
            json!({"name":"continent","type":required(named("Continent"))}),
            json!({"name":"languages","type":required(list(required(named("Language"))))}),
        ]);
    schema["types"]["Continent"] = json!({"kind":"OBJECT","fields":[{"name":"code","type":"ID"},{"name":"name","type":"String"}]});
    schema["types"]["Language"] = json!({"kind":"OBJECT","fields":[{"name":"code","type":"ID"},{"name":"name","type":"String"},{"name":"native","type":"String"}]});
    schema["types"]["CountryFilterInput"] = json!({"kind":"INPUT_OBJECT","inputFields":[{"name":"code","type":"StringQueryOperatorInput"},{"name":"continent","type":"StringQueryOperatorInput"}]});
    schema["types"]["StringQueryOperatorInput"] = json!({"kind":"INPUT_OBJECT","inputFields":[{"name":"eq","type":"String"},{"name":"in","type":list(named("String"))}]});
    schema
}

#[test]
fn explore_reference_retains_all_scopes_object_colors_and_fragment_choices() {
    let reference: Value =
        serde_json::from_str(include_str!("../fixtures/graphql-explore.json")).unwrap();
    let source = reference["query"].as_str().unwrap();
    let schema = explore_schema();
    assert_eq!(
        object_names(source, &schema, false),
        [
            "continent",
            "languages",
            "country",
            "country",
            "countries",
            "languages"
        ]
    );
    for (needle, prefix, expected) in [
        ("  continent {", "  cont", "continent"),
        ("    languages {", "    lang", "languages"),
        ("    ...CountryDetails", "    ...Cou", "CountryDetails"),
        (
            "      ...LanguageDetails",
            "      ...Lang",
            "LanguageDetails",
        ),
        (
            "  $filter: CountryFilterInput = { continent: { eq:",
            "  $filter: CountryFilterInput = { continent: { e",
            "eq",
        ),
    ] {
        let at = source.find(needle).unwrap() + prefix.len();
        let marked = format!("{}|{}", &source[..at], &source[at..]);
        assert!(
            marked_at(&marked, "query", &schema)
                .candidates
                .iter()
                .any(|candidate| candidate.text == expected),
            "{needle}"
        );
    }
    let console = format!(
        "#endpoint\nhttps://countries.fixture.invalid/graphql\n#variables\n{}\n#query\n{source}",
        reference["variables"]
    );
    assert_eq!(
        object_names(&console, &schema, true),
        object_names(source, &schema, false)
    );
    let marked = console.replacen("$hideCurrency)", "$|hideCurrency)", 1);
    let marked = format!(
        "{marked}\n#check other\n#query\nquery Other($outside: Boolean!) {{ country(code: \"PL\") {{ code }} }}"
    );
    let names: Vec<_> = marked_at(&marked, "", &schema)
        .candidates
        .into_iter()
        .map(|candidate| candidate.text)
        .collect();
    assert!(names.contains(&"$hideCurrency".into()) && names.contains(&"$withLanguages".into()));
    assert!(!names.contains(&"$outside".into()) && !names.contains(&"$primaryCode".into()));
}

#[test]
fn directives_are_advertised_only_at_valid_attachment_sites() {
    for source in [
        "{ viewer ",
        "{ viewer { name ",
        "{ user(id: \"1\", ids: []) ",
        "{ viewer { ... on User ",
        "fragment U on User { name }\nquery { viewer { ...U ",
    ] {
        let result = at(source, "query", &schema());
        for name in ["@include()", "@skip()"] {
            assert!(
                result
                    .candidates
                    .iter()
                    .any(|candidate| candidate.text == name),
                "{source}: {name}"
            );
        }
    }
    for source in [
        "{ ",
        "{ viewer { ",
        "{ viewer { rename: ",
        "{ viewer { name } ",
        "query Q ",
        "fragment U on User ",
        "{ user ",
        "{ viewer @include ",
    ] {
        assert!(
            !at(source, "query", &schema())
                .candidates
                .iter()
                .any(|candidate| candidate.text.starts_with('@')),
            "{source}"
        );
    }
    let result = at("{ viewer { name @include(if: true) ", "query", &schema());
    assert!(result.automatic && result.automatic_directives_only);
    let directives: Vec<_> = result
        .candidates
        .iter()
        .filter(|candidate| candidate.text.starts_with('@'))
        .map(|candidate| candidate.text.as_str())
        .collect();
    assert_eq!(directives, ["@skip()"]);
    assert!(
        at("{ viewer { name ", "query", &schema())
            .candidates
            .iter()
            .any(|candidate| candidate.text == "address"),
        "Manual completion must retain sibling fields while automatic menus show only directives"
    );
    for candidate in ["@include()", "@skip()", "include()", "skip()"] {
        assert_eq!(
            completion::prepare_insertion("graphql", candidate, false),
            (candidate.into(), Some(candidate.len() - 1))
        );
    }
}

#[test]
fn directive_names_arguments_values_and_tail_syntax_are_contextual() {
    for source in [
        "{ viewer { name @",
        "{ viewer { ... on User @",
        "{ viewer { ... @",
        "fragment U on User { name }\nquery { viewer { ...U @",
    ] {
        assert_eq!(complete(source), ["include()", "skip()"], "{source}");
    }
    for source in [
        "query Q @",
        "fragment U on User @",
        "query Q($yes: Boolean @",
    ] {
        assert!(complete(source).is_empty(), "{source}");
    }
    assert_eq!(complete("{ viewer { name @inc"), ["include()"]);
    assert_eq!(complete("{ viewer { name @include "), ["()"]);
    assert_eq!(complete("{ viewer { name @include("), ["if: "]);
    assert_eq!(complete("{ viewer { name @include(if: "), ["true", "false"]);
    let names = complete(
        "query Q($yes: Boolean!, $default: Boolean = true, $nullable: Boolean, $id: ID!) { viewer { name @include(if: $",
    );
    assert!(names.contains(&"$yes".into()) && names.contains(&"$default".into()));
    assert!(!names.contains(&"$nullable".into()) && !names.contains(&"$id".into()));
    assert_eq!(complete("{ viewer { name @include(if: true) @"), ["skip()"]);
    assert!(complete("{ viewer { name @include(if: true) @skip(if: false) @").is_empty());
    assert_eq!(
        complete("{ viewer @include(if: true) "),
        ["{\n  \n}", "@skip()"]
    );
    assert!(complete("{ viewer { ... on User @include(if: true) { na").contains(&"name".into()));
    assert!(complete("{ viewer { ... @skip(if: false) { na").contains(&"name".into()));
    assert!(
        complete("fragment U on User { name }\nquery { viewer { ...U @include(if: true) na")
            .contains(&"name".into())
    );
    assert!(
        !marked_at(
            "{ viewer @inc|lude(if: true) { name } }",
            "query",
            &schema()
        )
        .candidates
        .iter()
        .any(|candidate| candidate.text == "()")
    );
}

#[test]
fn standalone_dollar_is_not_a_completion_but_variable_names_remain_available() {
    for source in [
        "query Q(",
        "query Q($id: ID!, ",
        "{ user(id: ",
        "{ user(filter: { tags: [",
        "{ viewer { name @skip(if: ",
        "query Q($yes: Boolean!, $id: ID!) { viewer { name @skip(if: ",
    ] {
        assert!(!complete(source).iter().any(|text| text == "$"), "{source}");
    }
    let source = "query Q($yes: Boolean!, $id: ID!) { viewer { name @skip(if: ";
    assert_eq!(complete(source), ["$yes", "true", "false"]);
    assert_eq!(complete(&format!("{source}$")), ["$yes"]);
    assert_eq!(complete(&format!("{source}$ye")), ["$yes"]);
}

#[test]
fn value_variable_prefix_uses_request_names_only_without_declarations() {
    let mut schema = schema();
    schema["requestVariables"] =
        json!({"hideCurrency": false, "primaryCode": "PL", "bad-name": true});
    let names = |source: &str| {
        at(source, "query", &schema)
            .candidates
            .into_iter()
            .map(|candidate| candidate.text)
            .collect::<Vec<_>>()
    };
    let value = "fragment U on User { name @skip(if: ";
    assert_eq!(names(value), ["true", "false"]);
    assert_eq!(
        names(&format!("{value}$")),
        ["$hideCurrency", "$primaryCode"]
    );
    assert_eq!(names(&format!("{value}$hide")), ["$hideCurrency"]);
    assert_eq!(
        names("query Q($id: ID!) { viewer { name @skip(if: $"),
        Vec::<String>::new()
    );
    assert_eq!(names("query Q($flag: Boolean! = "), ["true", "false"]);
    assert!(names("query Q($flag: Boolean! = $").is_empty());
    let marked = "fragment U on User { name @skip(if: $|) }\nquery Q($hideCurrency: Boolean!, $id: ID!) { viewer { ...U } }";
    assert_eq!(
        marked_at(marked, "query", &schema)
            .candidates
            .iter()
            .map(|candidate| candidate.text.as_str())
            .collect::<Vec<_>>(),
        ["$hideCurrency"]
    );
    let console = "#check first\n#variables\n{\"hideCurrency\":false}\n#query\nfragment U on User { name @skip(if: $|) }\n#check other\n#variables\n{\"outside\":true}\n#query\nquery Other($outside: Boolean!) { viewer { name } }";
    assert_eq!(
        marked_at(console, "", &schema)
            .candidates
            .iter()
            .map(|candidate| candidate.text.as_str())
            .collect::<Vec<_>>(),
        ["$hideCurrency"]
    );
}

#[test]
fn leading_fragments_use_later_operation_declarations_without_guessing_variable_types() {
    let reference: Value =
        serde_json::from_str(include_str!("../fixtures/graphql-explore.json")).unwrap();
    let source = reference["query"].as_str().unwrap();
    let marked = source.replacen("$hideCurrency)", "$|hideCurrency)", 1);
    let result = marked_at(&marked, "query", &explore_schema());
    let names: Vec<_> = result
        .candidates
        .iter()
        .map(|candidate| candidate.text.as_str())
        .collect();
    assert_eq!(names, ["$withLanguages", "$hideCurrency"]);
    let multiple = format!(
        "{marked}\nquery Other($withLanguages: Boolean!) {{ country(code: \"PL\") {{ ...CountrySummary }} }}"
    );
    assert_eq!(
        marked_at(&multiple, "query", &explore_schema())
            .candidates
            .iter()
            .map(|candidate| candidate.text.as_str())
            .collect::<Vec<_>>(),
        ["$withLanguages"]
    );
}

#[test]
fn schema_directives_respect_locations_repeatability_and_argument_types() {
    let mut schema = schema();
    // Declaring schema directives replaces the built-ins, so restate @include/@skip.
    let mut directives = Vec::from(["include", "skip"].map(|name| {
        json!({"name":name,"locations":["FIELD","FRAGMENT_SPREAD","INLINE_FRAGMENT"],
            "isRepeatable":false,"args":[{"name":"if","type":required(named("Boolean"))}]})
    }));
    directives.extend([
        json!({"name":"trace","locations":["FIELD"],"isRepeatable":true,
            "args":[{"name":"label","type":required(named("String"))}]}),
        json!({"name":"deprecated","locations":["FIELD_DEFINITION"],"args":[]}),
        json!({"name":"operation","locations":["QUERY"],"args":[]}),
        json!({"name":"fragmentOnly","locations":["FRAGMENT_DEFINITION"],"args":[]}),
    ]);
    schema["directives"] = json!(directives);
    let names = |source| {
        at(source, "query", &schema)
            .candidates
            .into_iter()
            .map(|candidate| candidate.text)
            .collect::<Vec<_>>()
    };
    assert_eq!(names("{ viewer @"), ["include()", "skip()", "trace()"]);
    assert_eq!(names("query Q @"), ["operation"]);
    assert_eq!(names("fragment U on User @"), ["fragmentOnly"]);
    assert_eq!(names("{ viewer @trace("), ["label: "]);
    assert_eq!(names("{ viewer @trace(label: "), ["\"\""]);
    assert!(names("{ viewer @trace(label: \"one\") @").contains(&"trace()".into()));
    assert!(names("query Q @operation { viewer { na").contains(&"name".into()));
    assert!(names("fragment U on User @fragmentOnly { na").contains(&"name".into()));
}

#[test]
fn operation_hints_are_single_per_document_but_fragments_and_requests_are_repeatable() {
    for operation in [
        "query Q { viewer { name } }",
        "mutation M { updateUser(input: {}) { name } }",
        "subscription S { userChanged { id } }",
        "{ viewer { name } }",
    ] {
        assert!(
            at(&format!("{operation}\n"), "query", &schema())
                .candidates
                .is_empty(),
            "{operation}"
        );
        let marked = format!("fragment U on User {{ name }}\n|\n{operation}");
        assert_eq!(
            marked_at(&marked, "query", &schema())
                .candidates
                .iter()
                .map(|candidate| candidate.text.as_str())
                .collect::<Vec<_>>(),
            ["fragment"]
        );
        let marked = format!("fragment U on User {{ name }}\nmu|\n{operation}");
        assert!(
            marked_at(&marked, "query", &schema()).candidates.is_empty(),
            "{operation}"
        );
        let console = format!("#check first\n#query\n{operation}\n#check second\n#query\n");
        assert!(
            at(&console, "", &schema())
                .candidates
                .iter()
                .any(|candidate| candidate.text == "query")
        );
    }
    let source = "# query Phantom { }\nfragment query on User { name }\nfragment mutation on Query { user(id: \"subscription { mutation }\") { name } }\n";
    let result = at(source, "query", &schema());
    for name in ["query", "mutation", "subscription", "fragment", "{\n  \n}"] {
        assert!(
            result
                .candidates
                .iter()
                .any(|candidate| candidate.text == name)
        );
    }
    assert!(
        marked_at("que|ry Q { viewer { name } }", "query", &schema())
            .candidates
            .iter()
            .any(|candidate| candidate.text == "query")
    );
    assert_eq!(
        marked_at("|\nquery Q($id:", "query", &schema())
            .candidates
            .iter()
            .map(|candidate| candidate.text.as_str())
            .collect::<Vec<_>>(),
        ["fragment"]
    );
}

#[test]
fn document_starts_with_operations_even_without_discovery() {
    for (source, field) in [
        ("", "query"),
        (
            "#check g\n#endpoint\nhttps://fixture.invalid/graphql\n#query\n",
            "",
        ),
    ] {
        let result = at(source, field, &Value::Null);
        let items: Vec<_> = result.candidates.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(&items[..3], ["query", "mutation", "subscription"]);
        assert_eq!(items[3], "fragment");
        assert_eq!(items[4], "{\n  \n}");
    }
    assert_eq!(complete("qu"), vec!["query"]);
    assert!(complete("query").is_empty());
    assert!(
        complete("query ")
            .iter()
            .all(|text| text.starts_with(['(', '{']))
    );
    assert!(
        complete("query GetUser ")
            .iter()
            .all(|text| text.starts_with(['(', '{']))
    );
    assert!(complete("query GetUser").is_empty());
}

fn object_names(source: &str, schema: &Value, console: bool) -> Vec<String> {
    let lines: Vec<_> = source.split('\n').collect();
    object_fields(source, schema, console.then_some("graphql"))
        .into_iter()
        .map(|(line, from, to)| {
            String::from_utf16(
                &lines[line]
                    .encode_utf16()
                    .skip(from)
                    .take(to - from)
                    .collect::<Vec<_>>(),
            )
            .unwrap()
        })
        .collect()
}

#[test]
fn object_field_ranges_follow_schema_wrappers_aliases_and_fragments() {
    let mut schema = fragment_schema();
    schema["types"]["Query"]["fields"][0]["type"] = required(list(required(named("Product"))));
    schema["types"]["Product"]["fields"][2]["type"] = list(required(named("Image")));
    let source = "query ($images: Int) { p: products(skus: [\"images\"]) { sku i: images(limit: $images) { url } thumbnail { url } __typename } }";
    assert_eq!(
        object_names(source, &schema, false),
        ["products", "images", "thumbnail"]
    );
    let source = "fragment ProductFields on Product { sku images { url } }\nquery { products { ...ProductFields ... on SimpleProductView { price { amount } } } }";
    assert_eq!(
        object_names(source, &schema, false),
        ["images", "products", "price"]
    );
    schema["types"]["Query"]["fields"]
        .as_array_mut()
        .unwrap()
        .extend([
            json!({"name":"tags","type":list(named("String"))}),
            json!({"name":"states","type":list(named("State"))}),
            json!({"name":"quantity","type":"TierCondition"}),
        ]);
    schema["types"]["State"] = json!({"kind":"ENUM","enumValues":["OPEN"]});
    assert_eq!(
        object_names("{ tags states quantity { __typename } }", &schema, false),
        ["quantity"]
    );
    assert!(object_fields(source, &Value::Null, None).is_empty());
}

#[test]
fn object_field_ranges_survive_incomplete_tokens_and_use_utf16_columns() {
    let schema = product_schema();
    for source in [
        "{ products(skus: [\"unfinished",
        "{ products(skus: [\"escape\\",
        "{ products # comment at EOF",
        "{ products(skus: [\"\"\"unfinished\nblock",
    ] {
        assert_eq!(
            object_names(source, &schema, false),
            ["products"],
            "{source}"
        );
    }
    let source = "{ products(skus: [\"🦀\"]) { sku alias: images";
    assert_eq!(object_names(source, &schema, false), ["products", "images"]);
    let start = source[..source.rfind("images").unwrap()]
        .encode_utf16()
        .count();
    assert_eq!(
        object_fields(source, &schema, None).last(),
        Some(&(0, start, start + 6))
    );
}

#[test]
fn console_object_fields_exclude_variables_headers_and_response_sections() {
    let source = "#check first\n#variables\n{\"images\":{},\"products\":{}}\n#query\nfragment Fields on Product { images { url } }\nquery { products { sku ...Fields } }\n#headers\nimages: products\n#response OK\n{ products { images } }\n#check second\n#query\n{ products { # GraphQL comment\n images\n} }";
    assert_eq!(
        object_names(source, &product_schema(), true),
        ["images", "products", "products", "images"]
    );
    assert_eq!(
        object_fields(source, &product_schema(), Some("graphql"))
            .iter()
            .map(|range| range.0)
            .collect::<Vec<_>>(),
        [4, 5, 12, 13]
    );
}

#[test]
fn operation_roots_and_anonymous_queries_use_the_right_fields() {
    assert!(complete("{ ").contains(&"viewer".into()));
    assert!(complete("query Get { ").iter().any(|text| text == "user"));
    let mutation = complete("mutation Update { ");
    assert!(mutation.iter().any(|text| text == "updateUser"));
    assert!(!mutation.iter().any(|text| text.starts_with("viewer")));
    let subscription = complete("subscription Watch { ");
    assert!(subscription.iter().any(|text| text == "userChanged"));
    assert!(
        !subscription
            .iter()
            .any(|text| text.starts_with("updateUser"))
    );
}

fn countries_schema() -> Value {
    let mut schema = schema();
    schema["types"]["Query"]["fields"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "name":"country","type":"Country","args":[{"name":"code","type":required(named("ID"))}]
        }));
    schema["types"]["Country"] = json!({"kind":"OBJECT","fields":[
        {"name":"code","type":required(named("ID"))},
        {"name":"name","type":required(named("String"))},
        {"name":"capital","type":"String"},{"name":"currency","type":"String"}
    ]});
    schema["requestVariables"] = json!({"first":"PL","second":"DE"});
    schema
}

#[test]
fn named_fragment_definitions_precede_operation_suggestions() {
    let schema = countries_schema();
    let names = |source: &str| {
        at(source, "query", &schema)
            .candidates
            .into_iter()
            .map(|c| c.text)
            .collect::<Vec<_>>()
    };
    assert_eq!(names("frag"), ["fragment"]);
    assert!(names("fragment ").is_empty());
    assert_eq!(names("fragment CountryFields "), ["on "]);
    assert_eq!(names("fragment CountryFields o"), ["on "]);
    let types = names("fragment CountryFields on ");
    assert!(types.contains(&"Country".into()) && types.contains(&"User".into()));
    assert!(!types.contains(&"Filter".into()) && !types.contains(&"ID".into()));
    assert_eq!(names("fragment CountryFields on Coun"), ["Country"]);
    assert_eq!(names("fragment CountryFields on Country "), ["{\n  \n}"]);
    assert_eq!(
        names("fragment CountryFields on Country {\n  cap"),
        ["capital"]
    );
    assert!(names("fragment CountryFields on ID ").is_empty());
    assert!(names("fragment on ").is_empty());
    let prefix = "fragment CountryFields on Country { code name capital currency }\n";
    assert!(names(prefix).contains(&"fragment".into()));
    assert!(names(prefix).contains(&"query".into()));
    assert_eq!(
        names(&format!("{prefix}fragment CountryFields ")),
        Vec::<String>::new()
    );
    assert!(
        !names(&format!(
            "{prefix}query {{ country(code: \"PL\") {{ ...CountryFields }} }}\n"
        ))
        .contains(&"fragment".into())
    );
    let vars = names(&format!("{prefix}query CompareCountries($"));
    assert!(vars.contains(&"$first: ".into()) && vars.contains(&"$second: ".into()));
}

#[test]
fn field_suggestions_include_complete_compatible_fragment_spreads() {
    let schema = countries_schema();
    let definitions = "fragment UserFields on User { id name }\nfragment CountryFields on Country { code name capital currency }\n";
    let query = "query { firstCountry: country(code: \"PL\") { ";
    let source = format!("{definitions}{query}");
    let result = at(&source, "query", &schema);
    assert!(result.candidates.iter().any(|candidate| {
        candidate.text == "...CountryFields" && candidate.detail == "Fragment on Country"
    }));
    for name in ["code", "name"] {
        assert!(
            result
                .candidates
                .iter()
                .any(|candidate| candidate.text == name)
        );
    }
    assert!(
        !result
            .candidates
            .iter()
            .any(|candidate| candidate.text == "... on ")
    );
    assert!(
        !result
            .candidates
            .iter()
            .any(|candidate| candidate.text == "...UserFields")
    );
    for prefix in ["Cou", ".", ".."] {
        assert!(
            at(&format!("{source}{prefix}"), "query", &schema)
                .candidates
                .iter()
                .any(|candidate| candidate.text == "...CountryFields"),
            "{prefix}"
        );
    }
    assert!(
        !at(&format!("{source}...CountryFields\n  "), "query", &schema)
            .candidates
            .iter()
            .any(|candidate| candidate.text == "...CountryFields")
    );
    assert!(
        at(
            &format!("{source}...CountryFields }} secondCountry: country(code: \"DE\") {{ "),
            "query",
            &schema
        )
        .candidates
        .iter()
        .any(|candidate| candidate.text == "...CountryFields")
    );
    let console = format!("#check countries\n#query\n{source}");
    assert!(
        at(&console, "", &schema)
            .candidates
            .iter()
            .any(|candidate| candidate.text == "...CountryFields")
    );
    let other = format!("{console}}} }}\n#check other\n#query\nquery {{ country(code: \"DE\") {{ ");
    assert!(
        !at(&other, "", &schema)
            .candidates
            .iter()
            .any(|candidate| candidate.text == "...CountryFields")
    );
}

#[test]
fn direct_fragment_spreads_replace_whole_prefixes_and_respect_existing_syntax() {
    let schema = countries_schema();
    let definitions = "fragment CountryFields on Country { code }\n";
    for selection in ["|", ".|", "..|", "..|.", "Cou|ntryFields"] {
        let source = format!("{definitions}query {{ country(code: \"PL\") {{ {selection} }} }}");
        let (before, after) = source.split_once('|').unwrap();
        let lines: Vec<_> = before.split('\n').collect();
        let original = format!("{before}{after}");
        let result = completion::suggest(
            "graphql",
            "query",
            &original,
            lines.len() - 1,
            lines.last().unwrap().encode_utf16().count(),
            &[],
            &schema,
        );
        let candidate = result
            .candidates
            .iter()
            .find(|candidate| candidate.text == "...CountryFields")
            .unwrap();
        let mut edited = lines.last().unwrap().to_string() + after;
        edited.replace_range(result.from..result.to, &candidate.text);
        assert_eq!(
            edited, "query { country(code: \"PL\") { ...CountryFields } }",
            "{selection}"
        );
    }
    for selection in [
        "alias: |",
        "Cou|ntryFields { code }",
        "Cou|ntryFields(code: \"PL\")",
        "Cou|ntryFields: code",
    ] {
        let source = format!("{definitions}query {{ country(code: \"PL\") {{ {selection} }} }}");
        let (before, after) = source.split_once('|').unwrap();
        let lines: Vec<_> = before.split('\n').collect();
        let original = format!("{before}{after}");
        let result = completion::suggest(
            "graphql",
            "query",
            &original,
            lines.len() - 1,
            lines.last().unwrap().encode_utf16().count(),
            &[],
            &schema,
        );
        assert!(
            !result
                .candidates
                .iter()
                .any(|candidate| candidate.text == "...CountryFields"),
            "{selection}"
        );
    }
}

#[test]
fn direct_fragment_spreads_preserve_dependency_and_subscription_guards() {
    let schema = countries_schema();
    for source in [
        "fragment A on Country { ",
        "fragment A on Country { ...B }\nfragment B on Country { ",
        "fragment A on Country { ...B }\nfragment B on Country { ...A }\nquery { country(code: \"PL\") { ",
        "fragment A on Country { ...Missing }\nquery { country(code: \"PL\") { ",
    ] {
        assert!(
            !at(source, "query", &schema)
                .candidates
                .iter()
                .any(|candidate| matches!(candidate.text.as_str(), "...A" | "...B")),
            "{source}"
        );
    }
    let source = "fragment Meta on Subscription { kind: __typename }\nfragment Wrapped on Subscription { ... on Subscription { ...Meta } }\nfragment Safe on Subscription { userChanged { __typename } }\nsubscription { ";
    let result = at(source, "query", &schema);
    assert!(
        result
            .candidates
            .iter()
            .any(|candidate| candidate.text == "...Safe")
    );
    assert!(
        !result
            .candidates
            .iter()
            .any(|candidate| matches!(candidate.text.as_str(), "...Meta" | "...Wrapped"))
    );
    let source = "fragment Inline on Product { sku }\nfragment Simple on SimpleProductView { price { amount } }\nquery { products { ";
    let result = at(source, "query", &fragment_schema());
    for name in ["...Inline", "...Simple"] {
        assert!(
            result
                .candidates
                .iter()
                .any(|candidate| candidate.text == name)
        );
    }
}

#[test]
fn spreads_offer_preceding_compatible_fragments_and_can_be_reused_in_aliases() {
    let schema = countries_schema();
    let definitions = "fragment UserFields on User { id name }\nfragment CountryFields on Country { code name capital currency }\n";
    let query = "query CompareCountries($first: ID!, $second: ID!) { firstCountry: country(code: $first) { ...";
    let source = format!("{definitions}{query}");
    let result = at(&source, "query", &schema);
    assert!(
        result
            .candidates
            .iter()
            .any(|c| c.text == "CountryFields" && c.detail == "Fragment on Country")
    );
    assert!(!result.candidates.iter().any(|c| c.text == "UserFields"));
    assert!(!result.candidates.iter().any(|c| c.text == " on "));
    assert_eq!(
        at(&format!("{source}Cou"), "query", &schema).candidates[0].text,
        "CountryFields"
    );
    assert!(!at(&format!("{source}CountryFields"), "query", &schema).automatic);
    let used = at(&format!("{source}CountryFields ..."), "query", &schema);
    assert!(!used.candidates.iter().any(|c| c.text == "CountryFields"));
    let reused = at(
        &format!("{source}CountryFields }} secondCountry: country(code: $second) {{ ..."),
        "query",
        &schema,
    );
    assert!(reused.candidates.iter().any(|c| c.text == "CountryFields"));
    let future = format!("{source}|\n}} }}\nfragment Future on Country {{ code }}");
    let (before, after) = future.split_once('|').unwrap();
    let full = format!("{before}{after}");
    let lines: Vec<_> = before.split('\n').collect();
    let result = completion::suggest(
        "graphql",
        "query",
        &full,
        lines.len() - 1,
        lines.last().unwrap().encode_utf16().count(),
        &[],
        &schema,
    );
    assert!(result.candidates.iter().any(|c| c.text == "CountryFields"));
    assert!(!result.candidates.iter().any(|c| c.text == "Future"));
    let console = format!(
        "#check countries\n#variables\n{{\"first\":\"PL\",\"second\":\"DE\"}}\n#query\n{definitions}{query}"
    );
    assert!(
        at(&console, "", &schema)
            .candidates
            .iter()
            .any(|c| c.text == "CountryFields")
    );
    let other = format!(
        "{console}CountryFields }} }}\n#check other\n#query\nquery {{ country(code: \"PL\") {{ ..."
    );
    assert!(
        !at(&other, "", &schema)
            .candidates
            .iter()
            .any(|c| c.text == "CountryFields")
    );
}

#[test]
fn named_fragment_graphs_exclude_cycles_and_respect_subscription_typename() {
    let schema = countries_schema();
    for source in [
        "fragment A on Country { ...",
        "fragment A on Country { ...B }\nfragment B on Country { ...",
        "fragment A on Country { ...B }\nfragment B on Country { ...A }\nquery { country(code: \"PL\") { ...",
        "fragment A on Country { ...Missing }\nquery { country(code: \"PL\") { ...",
    ] {
        let result = at(source, "query", &schema);
        assert!(
            !result
                .candidates
                .iter()
                .any(|c| c.text == "A" || c.text == "B"),
            "{source}"
        );
    }
    let forward = "fragment A on Country { ...B }\nfragment B on Country { code }\nquery { country(code: \"PL\") { ...";
    assert!(
        at(forward, "query", &schema)
            .candidates
            .iter()
            .any(|c| c.text == "A")
    );
    let source = "fragment Meta on Subscription { kind: __typename }\nfragment Wrapped on Subscription { ... on Subscription { ...Meta } }\nfragment Safe on Subscription { userChanged { __typename } }\nsubscription { ...";
    let result = at(source, "query", &schema);
    assert!(result.candidates.iter().any(|c| c.text == "Safe"));
    assert!(
        !result
            .candidates
            .iter()
            .any(|c| c.text == "Meta" || c.text == "Wrapped")
    );
    let source = "fragment Shared on Country { code }\nfragment A on Country { ...Shared }\nfragment B on Country { ...Shared }\nfragment Both on Country { ...A ...B }\nquery { country(code: \"PL\") { ...";
    assert!(
        at(source, "query", &schema)
            .candidates
            .iter()
            .any(|c| c.text == "Both")
    );
    let source = "fragment Inline on Product { sku }\nfragment Simple on SimpleProductView { price { amount } }\nquery { products { ...";
    let result = at(source, "query", &fragment_schema());
    assert!(result.candidates.iter().any(|c| c.text == "Inline"));
    assert!(result.candidates.iter().any(|c| c.text == "Simple"));
}

#[test]
fn named_fragment_replacement_preserves_spread_suffixes_and_existing_syntax() {
    let schema = countries_schema();
    for (source, expected) in [
        ("fragment CountryFields on Cou|ntry { code }", "Country"),
        (
            "fragment CountryFields on Country { code }\nquery { country(code: \"PL\") { ...Cou|ntryFields } }",
            "CountryFields",
        ),
    ] {
        let (before, after) = source.split_once('|').unwrap();
        let original = format!("{before}{after}");
        let lines: Vec<_> = before.split('\n').collect();
        let result = completion::suggest(
            "graphql",
            "query",
            &original,
            lines.len() - 1,
            lines.last().unwrap().encode_utf16().count(),
            &[],
            &schema,
        );
        let candidate = result
            .candidates
            .iter()
            .find(|c| c.text == expected)
            .unwrap();
        let mut edited = lines.last().unwrap().to_string() + after.split('\n').next().unwrap();
        let line = edited.clone();
        edited.replace_range(result.from..result.to, &candidate.text);
        assert_eq!(edited, line, "{source}");
    }
    for source in [
        "fragment CountryFields |on Country { code }",
        "fragment CountryFields on Country |{ code }",
    ] {
        let (before, after) = source.split_once('|').unwrap();
        let original = format!("{before}{after}");
        assert!(
            completion::suggest("graphql", "query", &original, 0, before.len(), &[], &schema)
                .candidates
                .is_empty(),
            "{source}"
        );
    }
}

#[test]
fn variable_definitions_offer_only_input_types_and_correct_wrappers() {
    let types = complete("query Get($id: ");
    for ty in ["ID", "Boolean", "Filter", "State", "["] {
        assert!(types.contains(&ty.into()), "{ty}");
    }
    for ty in ["User", "Query", "Mutation", "Subscription", "viewer"] {
        assert!(!types.contains(&ty.into()), "{ty}");
    }
    assert!(!types.contains(&"Int".into()) && !types.contains(&"Float".into()));
    assert_eq!(complete("query Get($id: ID!) "), vec!["{\n  \n}"]);
    assert!(complete("query Get($id: [ID ").contains(&"]".into()));
    assert!(!complete("query Get($id: [ID! ").contains(&"!".into()));
    assert!(complete("query Get($id: [ID!]! ").contains(&"=".into()));
    assert!(!complete("query Get($id: [ID!]! ").contains(&"!".into()));
    assert_eq!(complete("query Get($"), Vec::<String>::new());
    let tail = complete("query Get($id: ID = null ");
    assert!(!tail.contains(&"=".into()) && !tail.contains(&"!".into()));
}

#[test]
fn alias_fields_automatically_open_after_colon_space_but_not_after_completed_scalars() {
    for source in [
        "query { viewer { displayName: ",
        "fragment Summary on User { displayName: ",
        "query { viewer { name otherName: ",
    ] {
        let result = at(source, "query", &schema());
        assert!(result.automatic, "{source}");
        assert!(
            result
                .candidates
                .iter()
                .any(|candidate| candidate.text == "name"),
            "{source}"
        );
        assert!(
            !result
                .candidates
                .iter()
                .any(|candidate| candidate.text.starts_with("...")),
            "{source}"
        );
        let console = format!("#check aliases\n#query\n{source}");
        assert!(at(&console, "", &schema()).automatic, "{source}");
    }
    for source in [
        "query { viewer { displayName:",
        "query { viewer { displayName: name",
    ] {
        assert!(!at(source, "query", &schema()).automatic, "{source}");
    }
    for source in [
        "query { viewer { displayName: name ",
        "fragment Summary on User { displayName: name ",
    ] {
        let result = at(source, "query", &schema());
        assert!(
            result.automatic && result.automatic_directives_only,
            "{source}"
        );
    }
    for source in ["fragment Summary on ", "fragment Summary on C"] {
        let result = at(source, "query", &countries_schema());
        assert!(
            result.automatic
                && result
                    .candidates
                    .iter()
                    .any(|candidate| candidate.text == "Country")
        );
    }
}

#[test]
fn field_arguments_are_isolated_and_used_names_are_hidden() {
    let args = complete("{ user(");
    assert!(args.contains(&"id: ".into()) && args.contains(&"filter: ".into()));
    assert!(!args.contains(&")".into()) && !args.contains(&"name".into()));
    let partial = complete("{ user(i");
    assert!(partial.contains(&"id: ".into()) && partial.contains(&"ids: ".into()));
    assert!(!partial.contains(&"name".into()));
    let used = complete("{ user(id: \"x\", ");
    assert!(!used.contains(&"id: ".into()));
    assert!(used.contains(&"filter: ".into()));
    assert!(!used.contains(&")".into()), "ids is still required");
    assert!(complete("{ user(id: \"x\", ids: [] ").contains(&")".into()));
    assert_eq!(
        complete("{ user ").len(),
        1,
        "required arguments come before a selection set"
    );
}

#[test]
fn variables_are_scoped_and_filtered_by_input_type_and_nullability() {
    let header =
        "query Get($id: ID!, $text: String, $other: ID, $ids: [ID!]!, $fallback: ID = \"x\")";
    let vars = complete(&format!("{header} {{ user(id: $"));
    assert_eq!(vars, vec!["$id", "$fallback"]);
    assert_eq!(complete(&format!("{header} {{ user(ids: $")), vec!["$ids"]);
    assert_eq!(
        complete(&format!("{header} {{ user(optionalId: $")),
        vec!["$id", "$other", "$fallback"]
    );
    assert!(
        !complete("query Get($id: ID = $")
            .iter()
            .any(|text| text.starts_with('$'))
    );
    assert!(
        !complete("query Old($id: ID!) { viewer { id } }\nquery New { user(id: $")
            .contains(&"$id".into())
    );
}

#[test]
fn input_objects_enums_booleans_and_lists_follow_argument_types() {
    assert_eq!(complete("{ user(id: "), vec!["\"\""]);
    assert_eq!(complete("{ user(state: "), vec!["null", "OPEN", "CLOSED"]);
    assert_eq!(complete("{ user(active: "), vec!["null", "true", "false"]);
    let input = complete("{ user(filter: { ");
    assert!(input.contains(&"state: ".into()) && input.contains(&"tags: ".into()));
    assert!(!input.contains(&"viewer".into()) && !input.contains(&"name".into()));
    assert_eq!(complete("{ user(filter: { state: O"), vec!["OPEN"]);
    assert_eq!(
        complete("{ user(filter: { nested: { active: "),
        vec!["true", "false"]
    );
    let list = complete("{ user(filter: { tags: [");
    assert!(list.contains(&"\"\"".into()) && list.contains(&"]".into()));
    assert!(!list.contains(&"null".into()), "list elements are non-null");
    let next = complete("{ user(filter: { tags: [\"one\", ");
    assert!(next.contains(&"\"\"".into()) && next.contains(&"]".into()));
}

#[test]
fn aliases_arguments_and_string_delimiters_preserve_selection_scope() {
    for source in [
        "{ person: user(id: \"{ ) # false\", ids: []) { na",
        "query Get($id: ID!) { user(id: $id, ids: []) { na",
        "{ user(id: \"\"\"{ ) # multiline\n string }\"\"\", ids: []) { na",
    ] {
        assert_eq!(complete(source), vec!["name"], "{source}");
    }
    assert_eq!(complete("{ viewer { id na"), vec!["name"]);
    let alias = complete("{ viewer { primary: name ");
    assert!(
        alias.contains(&"name".into()),
        "an alias does not reserve the unaliased response name"
    );
    assert_eq!(
        complete("{ viewer { name ")
            .iter()
            .filter(|text| *text == "name")
            .count(),
        0
    );
    assert!(complete("{ user(id: \"unfinished").is_empty());
    assert!(complete("{ viewer { # name").is_empty());
    assert_eq!(complete("{ viewer { # name\n na"), vec!["name"]);
    assert_eq!(complete("\u{feff}{ viewer { # name\r na"), vec!["name"]);
}

#[test]
fn snippets_keep_the_cursor_inside_arguments_and_inputs() {
    let candidate = complete("{ user ")
        .into_iter()
        .find(|text| text == "()")
        .unwrap();
    let (text, cursor) = completion::prepare_insertion("graphql", &candidate, false);
    assert_eq!(&text[..cursor.unwrap()], "(");
    let source = "{ user(filter: ";
    let candidate = complete(source)
        .into_iter()
        .find(|text| text.starts_with('{'))
        .unwrap();
    let (text, cursor) = completion::prepare_insertion("graphql", &candidate, false);
    assert_eq!(&text[..cursor.unwrap()], "{\n  ");
}

fn product_schema() -> Value {
    json!({"queryType":"Query","types":{
        "Query":{"fields":[{"name":"products","type":"Product","args":[
            {"name":"skus","type":list(named("String"))}
        ]},{"name":"lookup","type":"Product","args":[
            {"name":"id","type":required(named("ID"))}
        ]},{"name":"total","type":"Int","args":[{"name":"precision","type":named("Int")}]}]},
        "Product":{"fields":[{"name":"sku","type":"String"},{"name":"skuLabel","type":"String"},
            {"name":"images","type":"Image","args":[{"name":"limit","type":named("Int")}]},
            {"name":"thumbnail","type":"Image"}]},
        "Image":{"fields":[{"name":"url","type":"String"}]},
        "String":{"kind":"SCALAR"},"Int":{"kind":"SCALAR"}
    }})
}

fn cursor(source: &str) -> completion::Completion {
    let (before, after) = source.split_once('|').unwrap();
    let source = format!("{before}{after}");
    completion::suggest(
        "graphql",
        "query",
        &source,
        before.split('\n').count() - 1,
        before
            .split('\n')
            .next_back()
            .unwrap()
            .encode_utf16()
            .count(),
        &[],
        &product_schema(),
    )
}

#[test]
fn fields_offer_arguments_or_selection_after_inserting_the_name() {
    for (candidate, cursor) in [("()", 1), (" ()", 2), ("products()", 9)] {
        let (text, target) = completion::prepare_insertion("graphql", candidate, false);
        assert_eq!(text, candidate);
        assert_eq!(target, Some(cursor));
        assert!(!text.contains('\n'));
    }
    assert_eq!(
        completion::prepare_insertion("mysql", "NOW()", false),
        ("NOW()".into(), None)
    );
    assert_eq!(
        completion::prepare_insertion("graphql", "https://fixture.invalid/query()", false),
        ("https://fixture.invalid/query()".into(), None)
    );
    let schema = product_schema();
    let names = |source: &str| {
        at(source, "query", &schema)
            .candidates
            .into_iter()
            .map(|c| c.text)
            .collect::<Vec<_>>()
    };
    assert_eq!(names("query "), vec!["{\n  \n}", "()"]);
    assert_eq!(names("query ($skus: [String])"), vec![" {\n  \n}"]);
    let products = names("query ($skus: [String]) {\n  pro");
    assert_eq!(products, vec!["products"]);
    assert_eq!(
        names("query ($skus: [String]) {\n  products "),
        vec!["{\n    \n  }", "()", "@include()", "@skip()"]
    );
    assert_eq!(names("{ look"), vec!["lookup"]);
    assert_eq!(names("{ lookup "), vec!["()"]);
    assert_eq!(
        names("query ($skus: [String]) { products(skus: $s"),
        vec!["$skus"]
    );
    assert_eq!(
        names("query ($skus: [String]) {\n  products(skus: $skus)"),
        vec![" {\n    \n  }", "@include()", "@skip()"]
    );
    let images = names("query ($skus: [String]) {\n  products(skus: $skus) {\n    im");
    assert_eq!(images, vec!["images"]);
    assert_eq!(
        names("query ($skus: [String]) {\n  products(skus: $skus) {\n    images "),
        vec!["{\n      \n    }", "()", "@include()", "@skip()"]
    );
    assert_eq!(names("{ products { th"), vec!["thumbnail"]);
    assert_eq!(
        names("{ products { thumbnail "),
        vec!["{\n  \n}", "@include()", "@skip()"]
    );
    assert!(images.iter().all(|text| !text.contains("(\n")));
    let nested = names(
        "query ($skus: [String]) {\n  products(skus: $skus) {\n    sku\n    images {\n      ",
    );
    assert_eq!(nested, vec!["url", "__typename"]);
}

#[test]
fn automatic_field_hints_wait_for_space_or_newline() {
    for source in [
        "query { products|",
        "query { products { images|",
        "query { products { sku|",
        "query { products { sku } }|",
    ] {
        assert!(!cursor(source).automatic, "{source}");
    }
    let scalar = cursor("query { products { sku |");
    assert!(scalar.automatic && scalar.automatic_directives_only);
    for source in [
        "query |",
        "query { products |",
        "query { products { images |",
        "query { products { sku\n  |",
        "query { products { sk|",
    ] {
        assert!(cursor(source).automatic, "{source}");
    }
    let next = cursor("query { products { sku\n  |");
    assert!(!next.candidates.iter().any(|c| c.text == "sku"));
    assert!(next.candidates.iter().any(|c| c.text == "images"));
    let scalar_args = cursor("query { total |");
    assert_eq!(
        scalar_args
            .candidates
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>(),
        ["()", "@include()", "@skip()"]
    );
}

#[test]
fn existing_arguments_and_selections_are_never_duplicated() {
    for source in [
        "query { products |(skus: []) { sku } }",
        "query { products | # existing args\n (skus: []) { sku } }",
        "query |($skus: [String]) { products(skus: $skus) { sku } }",
        "query ($skus: [String]) |{ products(skus: $skus) { sku } }",
        "query { total |(precision: 1) }",
        "query { products(skus: |[]) { sku } }",
    ] {
        assert!(cursor(source).candidates.is_empty(), "{source}");
    }
    for source in [
        "query { products(skus: []) |{ sku } }",
        "query { products(skus: []) | # existing body\n { sku } }",
    ] {
        assert_eq!(
            cursor(source)
                .candidates
                .iter()
                .map(|candidate| candidate.text.as_str())
                .collect::<Vec<_>>(),
            ["@include()", "@skip()"]
        );
    }
    for source in [
        "query ($skus: [String]) { products(skus: $skus) |",
        "# 😀\n\nquery { products(skus: []) |",
    ] {
        let result = cursor(source);
        assert_eq!(result.candidates.len(), 3, "{source}");
        assert_eq!(result.candidates[0].detail, "Selection set", "{source}");
    }
    for source in ["query |{ products { sku } }", "query { products |{ sku } }"] {
        let result = cursor(source);
        assert_eq!(
            result
                .candidates
                .iter()
                .map(|c| c.text.as_str())
                .collect::<Vec<_>>(),
            if source.starts_with("query |") {
                vec!["()"]
            } else {
                vec!["()", "@include()", "@skip()"]
            }
        );
    }
}

#[test]
fn replacing_partial_names_preserves_existing_suffixes_and_colons() {
    for (source, expected) in [
        ("query { pro|ducts(skus: []) { sku } }", "products"),
        ("query { products(sk|us: []) { sku } }", "skus"),
        (
            "query ($skus: [String]) { products(skus: $sk|us) { sku } }",
            "$skus",
        ),
        ("que|ry { products { sku } }", "query"),
    ] {
        let result = cursor(source);
        let candidate = result
            .candidates
            .iter()
            .find(|c| c.text == expected)
            .unwrap();
        let original = source.replace('|', "");
        let mut edited = original.clone();
        edited.replace_range(result.from..result.to, &candidate.text);
        assert_eq!(edited, original, "{source}");
    }
}

#[test]
fn right_context_preserves_existing_types_and_delimiters() {
    for source in [
        "query ($skus: | [String]) { products { sku } }",
        "query ($skus: | String) { products { sku } }",
    ] {
        assert!(cursor(source).candidates.is_empty(), "{source}");
    }
    for (source, duplicate) in [
        ("query ($skus: String |!) { products { sku } }", "!"),
        ("query ($skus: String | ) { products { sku } }", ")"),
        ("query ($skus: [String | ]) { products { sku } }", "]"),
        ("query { products(skus: [] | ) { sku } }", ")"),
        ("query { products(skus: [\"one\" | ]) { sku } }", "]"),
    ] {
        assert!(
            !cursor(source)
                .candidates
                .iter()
                .any(|c| c.text == duplicate),
            "{source}"
        );
    }
    assert!(
        cursor("query ($skus: String |) { products { sku } }")
            .candidates
            .iter()
            .any(|c| c.text == ")")
    );
}

#[test]
fn typename_is_an_implicit_scalar_in_selection_sets() {
    for source in [
        "query {\n  __ty",
        "mutation {\n  __ty",
        "{ viewer {\n  __ty",
        "subscription { userChanged {\n  __ty",
        "#check g\n#query\n{ viewer {\n  __ty",
    ] {
        let result = at(
            source,
            if source.starts_with('#') { "" } else { "query" },
            &schema(),
        );
        assert_eq!(result.candidates.len(), 1, "{source}");
        assert_eq!(result.candidates[0].text, "__typename", "{source}");
        assert_eq!(result.candidates[0].detail, "String!", "{source}");
        assert_eq!(
            completion::prepare_insertion("graphql", "__typename", false),
            ("__typename".into(), None)
        );
    }
    for source in [
        "subscription { __ty",
        "{ viewer { __typename\n  __ty",
        "query __ty",
        "query ($value: __ty",
        "{ user(__ty",
        "{ user(filter: { __ty",
    ] {
        assert!(!complete(source).contains(&"__typename".into()), "{source}");
    }
    assert_eq!(
        complete("{ viewer { kind: __typename\n  __ty"),
        ["__typename"]
    );
    assert!(!at("{ viewer { __typename", "query", &schema()).automatic);
    assert!(at("{ viewer { __typename ", "query", &schema()).automatic_directives_only);
    assert!(at("{ viewer { __typename\n  ", "query", &schema()).automatic);
}

fn fragment_schema() -> Value {
    let mut schema = product_schema();
    schema["types"]["Product"]["kind"] = json!("INTERFACE");
    schema["types"]["Product"]["possibleTypes"] =
        json!(["SimpleProductView", "ComplexProductView"]);
    for (name, ty) in [
        (
            "SimpleProductView",
            json!({"kind":"OBJECT","fields":[
                {"name":"sku","type":"String"}, {"name":"price","type":"Price"}]}),
        ),
        (
            "ComplexProductView",
            json!({"kind":"OBJECT","fields":[
                {"name":"sku","type":"String"}, {"name":"options","type":"Option"}]}),
        ),
        (
            "Priced",
            json!({"kind":"INTERFACE","possibleTypes":["SimpleProductView","Unrelated"],
                "fields":[{"name":"price","type":"Price"}]}),
        ),
        (
            "Price",
            json!({"kind":"OBJECT","fields":[
                {"name":"amount","type":"Int"}, {"name":"quantity","type":"TierCondition"}]}),
        ),
        (
            "TierCondition",
            json!({"kind":"UNION","possibleTypes":["ProductViewTierRangeCondition","FixedQuantity"]}),
        ),
        (
            "ProductViewTierRangeCondition",
            json!({"kind":"OBJECT","fields":[
                {"name":"lt","type":"Int"}, {"name":"gte","type":"Int"}]}),
        ),
        (
            "FixedQuantity",
            json!({"kind":"OBJECT","fields":[{"name":"value","type":"Int"}]}),
        ),
        (
            "Option",
            json!({"kind":"OBJECT","fields":[{"name":"values","type":"OptionValue"}]}),
        ),
        (
            "OptionValue",
            json!({"kind":"INTERFACE","possibleTypes":["ProductViewOptionValueProduct"],
                "fields":[{"name":"title","type":"String"}]}),
        ),
        (
            "ProductViewOptionValueProduct",
            json!({"kind":"OBJECT","fields":[
                {"name":"product","type":"SimpleProductView"}, {"name":"quantity","type":"Int"}]}),
        ),
        (
            "Unrelated",
            json!({"kind":"OBJECT","fields":[{"name":"secret","type":"String"}]}),
        ),
    ] {
        schema["types"][name] = ty;
    }
    schema
}

#[test]
fn type_condition_hints_require_another_compatible_composite_type() {
    let mut abstract_without_alternatives = fragment_schema();
    abstract_without_alternatives["types"]["Product"]["possibleTypes"] = json!([]);
    for (schema, body, available) in [
        (json!({"queryType":"Missing","types":{}}), "{ ", false),
        (countries_schema(), "{ country(code: \"PL\") { ", false),
        (product_schema(), "{ products { images { ", false),
        (abstract_without_alternatives, "{ products { ", false),
        (fragment_schema(), "{ products { ", true),
        (
            fragment_schema(),
            "{ products { ... on SimpleProductView { ",
            true,
        ),
        (
            fragment_schema(),
            "{ products { ... on SimpleProductView { price { quantity { ",
            true,
        ),
    ] {
        for (field, prefix) in [("query", ""), ("", "#check types\n#query\n")] {
            for tail in ["", ".", "..", "... ", "... o"] {
                let result = at(&format!("{prefix}{body}{tail}"), field, &schema);
                assert_eq!(
                    result
                        .candidates
                        .iter()
                        .any(|candidate| matches!(candidate.text.trim(), "... on" | "on")),
                    available,
                    "{prefix}{body}{tail}: {:?}",
                    result.candidates
                );
            }
        }
    }
    let schema = countries_schema();
    assert_eq!(
        at("{ country(code: \"PL\") { ... on ", "query", &schema)
            .candidates
            .iter()
            .map(|candidate| candidate.text.as_str())
            .collect::<Vec<_>>(),
        ["Country"]
    );
    assert_eq!(
        at("fragment New ", "query", &schema)
            .candidates
            .iter()
            .map(|candidate| candidate.text.as_str())
            .collect::<Vec<_>>(),
        ["on "]
    );
}

#[test]
fn inline_fragment_steps_offer_only_compatible_composite_types() {
    let schema = fragment_schema();
    let names = |source| {
        let mut names = at(source, "query", &schema)
            .candidates
            .into_iter()
            .map(|c| c.text)
            .collect::<Vec<_>>();
        names.sort();
        names
    };
    for source in ["{ products { .", "{ products { .."] {
        assert_eq!(names(source), ["... on "], "{source}");
    }
    assert_eq!(names("{ products { ..."), [" on ", "@include()", "@skip()"]);
    for source in ["{ products { ... ", "{ products { ... o"] {
        let expected = if source.ends_with('o') {
            vec!["on "]
        } else {
            vec!["@include()", "@skip()", "on "]
        };
        assert_eq!(names(source), expected, "{source}");
    }
    assert_eq!(
        names("{ products { ... on "),
        [
            "ComplexProductView",
            "Priced",
            "Product",
            "SimpleProductView"
        ]
    );
    assert_eq!(names("{ products { ... on Sim"), ["SimpleProductView"]);
    assert!(!at("{ products { ... on SimpleProductView", "query", &schema).automatic);
    assert_eq!(
        names("{ products { ... on SimpleProductView "),
        ["@include()", "@skip()", "{\n  \n}"]
    );
    assert_eq!(
        names("{ products { ... on SimpleProductView { pr"),
        ["price"]
    );
    assert!(!names("{ products { ... on SimpleProductView {\n  ").contains(&"options".into()));
    assert_eq!(
        names("{ products { ... on SimpleProductView { price { quantity { ... on "),
        [
            "FixedQuantity",
            "ProductViewTierRangeCondition",
            "TierCondition"
        ]
    );
    assert_eq!(
        names(
            "{ products { ... on SimpleProductView { price { quantity { ... on ProductViewTierRangeCondition { g"
        ),
        ["gte"]
    );
    assert_eq!(
        names("{ products { ... on ComplexProductView { options { values { ... on "),
        ["OptionValue", "ProductViewOptionValueProduct"]
    );
    assert_eq!(
        names(
            "{ products { ... on ComplexProductView { options { values { ... on ProductViewOptionValueProduct { product { pr"
        ),
        ["price"]
    );
    assert_eq!(
        names(
            "{ products { ... on SimpleProductView { price { amount } } ... on ComplexProductView { op"
        ),
        ["options"]
    );
    assert_eq!(
        names("{ products { ... on SimpleProductView { price { amount } }\n im"),
        ["images"]
    );
    assert_eq!(names("{ products { ... { sk"), ["sku", "skuLabel"]);
    assert_eq!(names("{ products { ...Shared\n im"), ["images"]);
}

#[test]
fn typename_and_fragment_conditions_obey_selection_scope() {
    let schema = fragment_schema();
    assert_eq!(
        at("{ products { ", "query", &schema)
            .candidates
            .iter()
            .find(|candidate| candidate.text == "... on ")
            .unwrap()
            .detail,
        "Type condition"
    );
    for source in [
        "{ products { __ty",
        "{ products { ... on SimpleProductView { __ty",
        "{ products { ... on SimpleProductView { price { quantity { __ty",
        "{ products { ... on SimpleProductView { price { quantity { ... on ProductViewTierRangeCondition { __ty",
    ] {
        assert_eq!(
            at(source, "query", &schema)
                .candidates
                .iter()
                .map(|c| c.text.as_str())
                .collect::<Vec<_>>(),
            ["__typename"],
            "{source}"
        );
    }
    assert!(complete("subscription { ... on Subscription {\n __ty").is_empty());
    assert_eq!(
        complete("subscription { ... on Subscription { userChanged { __ty"),
        ["__typename"]
    );
    let prefix = "#check fragment\n#query\n";
    let source = format!("{prefix}{{ products {{ ... on Sim");
    assert_eq!(
        at(&source, "", &schema).candidates[0].text,
        "SimpleProductView"
    );
    for source in ["query ($value: ... on ", "{ products(skus: ... on "] {
        assert!(
            at(source, "query", &schema).candidates.is_empty(),
            "{source}"
        );
    }
}

#[test]
fn fragment_completion_preserves_existing_spreads_and_selection_sets() {
    let schema = fragment_schema();
    for (source, candidate) in [
        (
            "{ products { .|.. on SimpleProductView { sku } }",
            "... on ",
        ),
        (
            "{ products { ..|. on SimpleProductView { sku } }",
            "... on ",
        ),
        (
            "{ products { ... on Sim|pleProductView { sku } }",
            "SimpleProductView",
        ),
    ] {
        let (before, after) = source.split_once('|').unwrap();
        let original = format!("{before}{after}");
        let result =
            completion::suggest("graphql", "query", &original, 0, before.len(), &[], &schema);
        let text = &result
            .candidates
            .iter()
            .find(|c| c.text == candidate)
            .unwrap()
            .text;
        let mut edited = original.clone();
        edited.replace_range(result.from..result.to, text);
        assert_eq!(edited, original, "{source}");
    }
    let source = "{ products { ... on SimpleProductView  { sku } }";
    let result = completion::suggest(
        "graphql",
        "query",
        source,
        0,
        "{ products { ... on SimpleProductView ".len(),
        &[],
        &schema,
    );
    assert_eq!(
        result
            .candidates
            .iter()
            .map(|candidate| candidate.text.as_str())
            .collect::<Vec<_>>(),
        ["@include()", "@skip()"]
    );
}

#[test]
fn declaration_names_come_from_request_json_without_exposing_values() {
    let prefix = "#variables\n{\"skus\":[\"private-fixture\"],\"locale\":\"en\",\"nested\":{\"hidden\":1},\"bad-key\":1,\"1bad\":2}\n#query\n";
    for fragment in ["query (", "query ($", "query ($s"] {
        let source = format!("{prefix}{fragment}");
        let result = at(&source, "", &Value::Null);
        assert!(result.candidates.iter().any(|c| c.text == "$skus: "));
        assert!(
            result
                .candidates
                .iter()
                .all(|c| !c.text.contains("private-fixture")
                    && !c.detail.contains("private-fixture")
                    && !c.text.contains("hidden")
                    && !c.text.contains("bad"))
        );
        if fragment.ends_with("$s") {
            assert_eq!(result.from, "query (".len());
            assert_eq!(result.candidates[0].text, "$skus: ");
        }
    }
    let used = at(
        &format!("{prefix}query ($skus: [String], $"),
        "",
        &Value::Null,
    );
    assert!(!used.candidates.iter().any(|c| c.text == "$skus: "));
    assert!(used.candidates.iter().any(|c| c.text == "$locale: "));
    let next = at(
        &format!("{prefix}query ($skus: [String] "),
        "",
        &Value::Null,
    );
    assert!(next.candidates.iter().any(|c| c.text == "$locale: "));
    let mut schema = schema();
    schema["requestVariables"] = json!({"skus":["fixture"]});
    assert_eq!(
        at("query ($s", "query", &schema).candidates[0].text,
        "$skus: "
    );
    schema["requestVariables"] = json!("{\"formOnly\":true}");
    assert_eq!(
        at("query ($f", "query", &schema).candidates[0].text,
        "$formOnly: "
    );
    for bad in [json!("unfinished {"), json!([1, 2]), Value::Null] {
        schema["requestVariables"] = bad;
        assert!(at("query ($", "query", &schema).candidates.is_empty());
    }
    schema["requestVariables"] = json!({"skus":["fixture"]});
    let source = "query ($skus: [String]) { products(skus: $skus) { sku } }";
    let result = completion::suggest(
        "graphql",
        "query",
        source,
        0,
        "query ($s".len(),
        &[],
        &schema,
    );
    assert_eq!(result.from, "query (".len());
    assert_eq!(result.to, "query ($skus".len());
    assert_eq!(result.candidates[0].text, "$skus");
    let replaced = format!(
        "{}{}{}",
        &source[..result.from],
        result.candidates[0].text,
        &source[result.to..]
    );
    assert_eq!(
        replaced, source,
        "editing an existing declaration must not duplicate its name or colon"
    );
}

#[test]
fn console_variable_names_are_scoped_to_the_current_request() {
    for first in ["#check first\n", ""] {
        let prefix = format!("{first}#variables\n{{\"firstOnly\":1}}\n#query\nquery ($");
        let source =
            format!("{prefix}\n#check second\n#variables\n{{\"secondOnly\":2}}\n#query\nquery ($");
        let line = prefix.split('\n').count() - 1;
        let first = completion::suggest(
            "graphql",
            "",
            &source,
            line,
            "query ($".len(),
            &[],
            &Value::Null,
        );
        assert_eq!(
            first
                .candidates
                .iter()
                .map(|c| c.text.as_str())
                .collect::<Vec<_>>(),
            ["$firstOnly: "]
        );
        let second = at(&source, "", &Value::Null);
        assert_eq!(
            second
                .candidates
                .iter()
                .map(|c| c.text.as_str())
                .collect::<Vec<_>>(),
            ["$secondOnly: "]
        );
    }
    let source = "#check after\n#query\nquery ($\n#variables\n{\"after\":true}";
    let result = completion::suggest(
        "graphql",
        "",
        source,
        2,
        "query ($".len(),
        &[],
        &Value::Null,
    );
    assert_eq!(result.candidates[0].text, "$after: ");
}

#[test]
fn replacement_offsets_are_utf16_and_do_not_eat_variable_dollars() {
    let source = "query Get($id: ID!) { user(id: \"😀\", optionalId: $i";
    let result = at(source, "query", &schema());
    assert_eq!(
        result.from,
        source[..source.rfind("$i").unwrap()].encode_utf16().count()
    );
    assert_eq!(result.to, source.encode_utf16().count());
    assert_eq!(result.candidates[0].text, "$id");
}

#[test]
fn scopes_and_block_strings() {
    let mut state = State::default();
    let t = tokens(
        Language::Graphql,
        "query($id: ID!) { alias: viewer(id: $id) @skip(if: false) }",
        &mut state,
    );
    assert!(t.contains(&("$id".into(), Some("variable-3"))));
    assert!(t.contains(&("@skip".into(), Some("atom"))));
    assert!(t.contains(&("alias".into(), Some("attribute"))));
    tokens(Language::Graphql, "\"\"\"start", &mut state);
    assert!(state.block_string);
    assert_eq!(
        tokens(Language::Graphql, "end\"\"\"", &mut state)[0].1,
        Some("string")
    );
    assert!(!state.block_string);
}

#[test]
fn fragment_definitions_spreads_and_type_conditions_have_distinct_styles() {
    let t = tokens(
        Language::Graphql,
        "fragment PriceTest on Price { amount }\nquery { products { ... on SimpleProductView { price { ... PriceTest amount } } } }",
        &mut State::default(),
    );
    assert_eq!(
        t.iter()
            .filter(|(name, style)| name == "PriceTest" && *style == Some("graphql-fragment"))
            .count(),
        2
    );
    for name in ["Price", "SimpleProductView"] {
        assert!(t.contains(&(name.into(), Some("graphql-type-condition"))));
    }
    for name in ["amount", "products", "price"] {
        assert!(t.contains(&(name.into(), Some("property"))));
    }
}

#[test]
fn fragment_colors_survive_comments_line_breaks_and_state_copying() {
    let mut state = State::default();
    tokens(Language::Graphql, "fragment # definition", &mut state);
    assert!(
        tokens(Language::Graphql, "PriceTest", &mut state)
            .contains(&("PriceTest".into(), Some("graphql-fragment")))
    );
    tokens(Language::Graphql, "on # type", &mut state);
    let mut state: State = serde_json::from_value(serde_json::to_value(state).unwrap()).unwrap();
    assert!(
        tokens(Language::Graphql, "Price { amount }", &mut state)
            .contains(&("Price".into(), Some("graphql-type-condition")))
    );
    tokens(Language::Graphql, "query { ... # reference", &mut state);
    assert!(
        tokens(Language::Graphql, ", PriceTest", &mut state)
            .contains(&("PriceTest".into(), Some("graphql-fragment")))
    );
    tokens(Language::Graphql, "...", &mut state);
    tokens(Language::Graphql, "# ignored", &mut state);
    tokens(Language::Graphql, "on", &mut state);
    assert!(
        tokens(Language::Graphql, "SimpleProductView { sku } }", &mut state)
            .contains(&("SimpleProductView".into(), Some("graphql-type-condition")))
    );
}

#[test]
fn fragment_colors_do_not_leak_into_values_comments_or_following_fields() {
    let t = tokens(
        Language::Graphql,
        "{ PriceTest: products(skus: [\"... PriceTest\", $PriceTest]) { ...PriceTest sku ... on Product { name } } } # ... on Hidden",
        &mut State::default(),
    );
    assert!(t.contains(&("PriceTest".into(), Some("attribute"))));
    assert!(t.contains(&("\"... PriceTest\"".into(), Some("string"))));
    assert!(t.contains(&("$PriceTest".into(), Some("variable-3"))));
    assert!(t.contains(&("sku".into(), Some("property"))));
    assert!(t.contains(&("name".into(), Some("property"))));
    assert!(t.contains(&("# ... on Hidden".into(), Some("comment"))));
    assert_eq!(
        t.iter()
            .filter(|(_, style)| *style == Some("graphql-fragment"))
            .count(),
        1
    );
    let fields = tokens(
        Language::Graphql,
        "query { fragment PriceTest on ProductViewPrice }",
        &mut State::default(),
    );
    assert!(
        fields.iter().all(|(_, style)| !matches!(
            style,
            Some("graphql-fragment" | "graphql-type-condition")
        ))
    );
    for source in [
        "{ ... } PriceTest",
        "{ ... \"text\" PriceTest",
        "{ ... $var PriceTest",
    ] {
        assert!(
            tokens(Language::Graphql, source, &mut State::default())
                .contains(&("PriceTest".into(), Some("property")))
        );
    }
}
