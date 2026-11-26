//! Templates panel: inserting template bodies and checks from `@` and section
//! markers. UI-behaviour tests.

use services_contracts::templates::Library;
use services_kernel::editor::Completion;
use services_sdk::refs::Root;
use services_shell::language::Modules;
use std::collections::BTreeMap;

fn suggest(
    service: &str,
    field: &str,
    source: &str,
    line: usize,
    ch: usize,
    templates: &BTreeMap<String, String>,
) -> Option<Completion> {
    let library = Library::from([(service.to_owned(), templates.clone())]);
    services_kernel::editor::templates::suggest(
        &Modules,
        service,
        field,
        source,
        line,
        ch,
        &library,
        (&Root::default(), ""),
    )
}

#[test]
fn lists_section_templates_and_normalizes_inline_sections() {
    services_shell::link();
    let templates = BTreeMap::from([(
        "payload".into(),
        "@my_create_book comment\n{\n  \"id\": 123\n}\n@my_create_book2 second\n{\"id\":1,}".into(),
    )]);
    let completion = suggest("artemis", "", "#check test\n#payload @", 1, 10, &templates).unwrap();
    assert_eq!((completion.from, completion.to), (8, 10));
    assert_eq!(completion.candidates[0].text, " @my_create_book");
    assert_eq!(
        completion.candidates[0].label.as_deref(),
        Some("@my_create_book")
    );
    assert!(suggest("artemis", "", "#payload\n@m", 1, 2, &templates).is_none());
    assert!(suggest("artemis", "", "#metadata\n@", 1, 1, &templates).is_none());
    assert!(suggest("artemis", "", "#payload\nmail@", 1, 5, &templates).is_none());
}

#[test]
fn full_checks_and_unicode_are_literal() {
    services_shell::link();
    let templates = BTreeMap::from([(
        "check".into(),
        "@готово full\n#check new\n#target\n/books\n#payload\n{}".into(),
    )]);
    let completion = suggest("artemis", "", "#check @гот", 0, 11, &templates).unwrap();
    assert_eq!(completion.from, 0);
    assert_eq!(completion.candidates[0].text, "#check new @готово");
    assert!(completion.candidates[0].literal);
    assert!(suggest("artemis", "", "@", 0, 1, &templates).is_none());
}

#[test]
fn check_header_uses_description_without_copying_template_definition() {
    services_shell::link();
    let body = r#"#target
https://fakerestapi.azurewebsites.net/api/v1/Books/123
#action
PUT
#metadata
Content-Type: application/json
Accept: application/json
#payload
{
  "id": 123,
  "title": "Example Book Title",
  "description": "Sample description of the book.",
  "pageCount": 320,
  "excerpt": "Sample excerpt from the book.",
  "publishDate": "2024-01-15T00:00:00Z"
}"#;
    let templates = BTreeMap::from([("check".into(), format!("@test1 mmm\n{body}"))]);
    let source = "#check @test1";
    let completion = suggest("artemis", "", source, 0, source.len(), &templates).unwrap();
    assert_eq!((completion.from, completion.to), (0, source.len()));
    assert_eq!(completion.candidates[0].text, "#check mmm @test1");
    assert_eq!(completion.candidates[0].label.as_deref(), Some("@test1"));
    assert_eq!(completion.candidates[0].detail, "mmm");
    assert!(suggest("artemis", "", "@test1", 0, 6, &templates).is_none());
}

#[test]
fn check_names_fall_back_to_template_name_and_keep_explicit_headers() {
    services_shell::link();
    for (definition, expected) in [
        ("@test1\n#target\n/books", "#check @test1"),
        (
            "@test1 Create a book\n#target\n/books",
            "#check Create a book @test1",
        ),
        (
            "@test1 mmm\n  #CHECK Existing\n#target\n/books",
            "#check Existing @test1",
        ),
        (
            "@test1 mmm\n#request Existing\n#target\n/books",
            "#check Existing @test1",
        ),
        ("@test1 mmm\n#checker\n#target\n/books", "#check mmm @test1"),
    ] {
        let templates = BTreeMap::from([("check".into(), definition.into())]);
        let completion = suggest("artemis", "", "#check @", 0, 8, &templates).unwrap();
        assert_eq!(completion.candidates[0].text, expected, "{definition}");
    }
}

#[test]
fn templates_are_only_suggested_after_a_section_marker_on_the_same_line() {
    services_shell::link();
    let templates = BTreeMap::from([("payload".into(), "@example Body\n{}".into())]);
    for source in [
        "@",
        "#payload\n@",
        "#payload\n@exa",
        "#payload\nemail@",
        "#payload\ntext @",
        "#payload name @",
        "#unknown @",
    ] {
        let line = source.lines().count() - 1;
        let ch = source.lines().last().unwrap().encode_utf16().count();
        assert!(
            suggest("artemis", "", source, line, ch, &templates).is_none(),
            "{source}"
        );
    }
    assert!(suggest("artemis", "payload", "#payload @", 0, 10, &templates).is_none());
    let completion = suggest("artemis", "", "  #payload \t@", 0, 13, &templates).unwrap();
    assert_eq!(completion.candidates[0].text, " @example");
    assert_eq!((completion.from, completion.to), (10, 13));
}

#[test]
fn space_after_section_marker_offers_templates_without_an_at_sign() {
    services_shell::link();
    let templates = BTreeMap::from([
        ("payload".into(), "@example Body\n{}".into()),
        (
            "check".into(),
            "@create Create book\n#target\n/books\n#action\nPUT\n#payload\n{}".into(),
        ),
    ]);
    let completion = suggest("artemis", "", "#payload ", 0, 9, &templates).unwrap();
    assert!(completion.automatic);
    assert_eq!((completion.from, completion.to), (8, 9));
    assert_eq!(completion.candidates[0].text, " @example");
    assert_eq!(completion.candidates[0].label.as_deref(), Some("@example"));
    assert_eq!(
        suggest("artemis", "", "#check ", 0, 7, &templates)
            .unwrap()
            .candidates[0]
            .text,
        "#check Create book @create"
    );
    let existing = suggest("artemis", "", "#payload @example", 0, 9, &templates).unwrap();
    assert_eq!((existing.from, existing.to), (8, 17));
    // Complete calls expand once picked, and are not offered again once typed.
    assert!(completion.candidates[0].expand);
    assert!(suggest("artemis", "", "#payload @example", 0, 17, &templates).is_none());
    assert!(
        suggest(
            "artemis",
            "",
            "#check Create book @create",
            0,
            26,
            &templates
        )
        .is_none()
    );
    for source in [
        "#payload",
        "#payload\n ",
        "#payload\ntext ",
        "#payload text ",
        "#unknown ",
    ] {
        let line = source.lines().count() - 1;
        let ch = source.lines().last().unwrap().encode_utf16().count();
        assert!(
            suggest("artemis", "", source, line, ch, &templates).is_none(),
            "{source}"
        );
    }
}

const SPEC: &str = "@host(service: {{:services}}) Host name\n{{service}}.{{:base_domain}}\n\n@url_shared(service: {{:services}}, type: {{.*.type}} = \"qa\") Service URL\nhttps://{{@host(service)}}.{{.var.region}}.{{type}}.example\n\n@order Zero-parameter template\n{ \"x\": 1 }";

fn spec_library() -> Library {
    Library::from([
        (
            "rest".to_owned(),
            BTreeMap::from([
                ("target".to_owned(), SPEC.to_owned()),
                ("payload".to_owned(), SPEC.to_owned()),
            ]),
        ),
        (
            "*".to_owned(),
            BTreeMap::from([("*".to_owned(), "@glob Global only\nG".to_owned())]),
        ),
    ])
}

fn dictionary() -> Root {
    let parse = |text| services_sdk::refs::parse_lines(text).unwrap();
    Root::new(vec![
        (
            "var".into(),
            parse("type: qa\nregion: eu\nbase_domain: site.example"),
        ),
        (
            "dev".into(),
            parse(
                "type: dev\nservices:\n  1: catalog\n  2: checkout\nenvs:\n  qa: a.example\n  prod: b.example",
            ),
        ),
    ])
}

fn complete(source: &str, library: &Library) -> Option<Completion> {
    let line = source.lines().count() - 1;
    let ch = source.lines().last().unwrap().encode_utf16().count();
    services_kernel::editor::templates::suggest(
        &Modules,
        "rest",
        "",
        source,
        line,
        ch,
        library,
        (&dictionary(), ""),
    )
}

#[test]
fn at_lists_section_templates_with_signatures_and_searches_descriptions() {
    services_shell::link();
    let library = spec_library();
    let completion = complete("#check\n#target @", &library).unwrap();
    let items: Vec<_> = completion
        .candidates
        .iter()
        .map(|c| {
            (
                c.label.clone().unwrap(),
                c.text.as_str(),
                c.detail.as_str(),
                c.continue_hint,
            )
        })
        .collect();
    assert_eq!(
        items,
        [
            ("@host(service)".into(), " @host(", "Host name", true),
            ("@order".into(), " @order", "Zero-parameter template", false),
            (
                "@url_shared(service, type = \"qa\")".into(),
                " @url_shared(",
                "Service URL",
                true
            ),
        ]
    );
    let found = complete("#check\n#target @zero", &library).unwrap();
    assert_eq!(found.candidates.len(), 1);
    assert_eq!(found.candidates[0].label.as_deref(), Some("@order"));
}

#[test]
fn arguments_offer_domain_options_as_quoted_literals() {
    services_shell::link();
    let library = spec_library();
    let first = complete("#check\n#target @url_shared(", &library).unwrap();
    let texts: Vec<_> = first.candidates.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(texts, ["\"catalog\", ", "\"checkout\", "]);
    assert_eq!(first.from, 20);
    let second = complete("#check\n#target @url_shared(\"catalog\", ", &library).unwrap();
    let texts: Vec<_> = second.candidates.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(texts, ["\"qa\")", "\"dev\")"]);
    let filtered = complete("#check\n#target @url_shared(\"che", &library).unwrap();
    assert_eq!(filtered.candidates.len(), 1);
    assert_eq!(filtered.candidates[0].label.as_deref(), Some("checkout"));
    assert!(complete("#check\n#target @url_shared(\"a\", \"b\", ", &library).is_none());
    // Back at a typed argument (Shift+Tab), a pick replaces it with its comma.
    let at = |source: &str, ch: usize| {
        services_kernel::editor::templates::suggest(
            &Modules,
            "rest",
            "",
            source,
            1,
            ch,
            &library,
            (&dictionary(), ""),
        )
        .unwrap()
    };
    let back = at("#check\n#target @url_shared(\"catalog\", \"dev\")", 20);
    assert_eq!((back.from, back.to), (20, 31));
    assert_eq!(back.candidates[0].text, "\"catalog\", ");
    let last = at("#check\n#target @url_shared(\"catalog\", \"dev\")", 31);
    assert_eq!((last.from, last.to), (31, 37));

    let hash = Library::from([(
        "rest".to_owned(),
        BTreeMap::from([(
            "target".to_owned(),
            "@env(name: {{:envs}}) Pick\n{{name}}".to_owned(),
        )]),
    )]);
    let named = complete("#check\n#target @env(", &hash).unwrap();
    assert_eq!(named.candidates[0].label.as_deref(), Some("qa — a.example"));
    assert_eq!(named.candidates[0].text, "\"a.example\")");
    // A domain that is no list shows why instead of an empty list.
    let nested = Library::from([(
        "rest".to_owned(),
        BTreeMap::from([(
            "target".to_owned(),
            "@env(name: {{:envs}}) Pick\n{{name}}\n\n@bad(name: {{:nope}}) Bad\nx".to_owned(),
        )]),
    )]);
    let root = Root::new(vec![(
        "var".into(),
        services_sdk::refs::parse_lines("envs:\n  qa:\n    1: catalog").unwrap(),
    )]);
    let error = |source: &str| {
        let line = source.lines().count() - 1;
        let ch = source.lines().last().unwrap().encode_utf16().count();
        let completion = services_kernel::editor::templates::suggest(
            &Modules,
            "rest",
            "",
            source,
            line,
            ch,
            &nested,
            (&root, ""),
        )
        .unwrap();
        assert_eq!(completion.candidates.len(), 1);
        assert_eq!(completion.candidates[0].badge, Some('!'));
        completion.candidates[0].label.clone().unwrap()
    };
    assert!(error("#check\n#target @env(").contains("envs.qa is a hash"));
    assert!(error("#check\n#target @bad(").contains("unknown variable"));
}

#[test]
fn live_calls_in_a_body_complete_their_names_and_arguments() {
    services_shell::link();
    let library = spec_library();
    let suggest = |source: &str| {
        let line = source.lines().count() - 1;
        let ch = source.lines().last().unwrap().encode_utf16().count();
        services_kernel::editor::completion::suggest(
            &Modules,
            "rest",
            "",
            source,
            line,
            ch,
            &[],
            &library,
            (&dictionary(), ""),
        )
    };
    let names = suggest("#check\n#target\nhttps://{{@");
    assert_eq!(names.candidates[0].text, "{{@glob}}");
    assert_eq!(names.candidates[1].text, "{{@host(");
    assert_eq!((names.from, names.to), (8, 11));
    let args = suggest("#check\n#target\nhttps://{{@host(");
    assert_eq!(args.candidates[0].text, "\"catalog\")}}");
}

fn expand(source: &str, line: usize) -> Option<Result<String, String>> {
    let lines: Vec<String> = source.split('\n').map(str::to_owned).collect();
    services_kernel::editor::expand_call(
        &Modules,
        "rest",
        &lines,
        line,
        &[],
        &spec_library(),
        (&dictionary(), ""),
    )
    .map(|result| {
        result.map(|edit| {
            let mut out = lines.clone();
            let tail = out.split_off(edit.to.line + 1);
            out.truncate(edit.from.line);
            if edit.from.ch > 0 {
                out.push(lines[edit.from.line].clone());
            }
            let mut text: Vec<String> = edit.text.split('\n').map(str::to_owned).collect();
            if edit.from.ch > 0 {
                text.remove(0);
            }
            out.extend(text);
            out.extend(tail);
            out.join("\n")
        })
    })
}

#[test]
fn enter_replaces_the_section_body_and_regenerates_after_edits() {
    services_shell::link();
    let call = "#check\n#target @url_shared(\"checkout\", \"qa\")";
    let expanded =
        format!("{call}\nhttps://checkout.{{{{:base_domain}}}}.{{{{.var.region}}}}.qa.example");
    assert_eq!(expand(call, 1).unwrap().unwrap(), expanded);
    // Edited arguments overwrite manual changes of the body; later sections stay.
    let edited =
        "#check\n#target @url_shared(\"search\")\nhttps://manual\n\n#metadata\nA: b".to_string();
    let regenerated = expand(&edited, 1).unwrap().unwrap();
    assert_eq!(
        regenerated,
        "#check\n#target @url_shared(\"search\")\nhttps://search.{{:base_domain}}.{{.var.region}}.qa.example\n\n#metadata\nA: b"
    );
    // Nothing to change: Enter stays a line break.
    assert!(expand(&expanded, 1).is_none());
}

#[test]
fn enter_reports_wrong_calls_and_leaves_plain_text_alone() {
    services_shell::link();
    let error = |source: &str| expand(source, 1).unwrap().unwrap_err();
    assert!(error("#check\n#target @nope(\"a\")").contains("Unknown template @nope"));
    // Global templates are for live calls only.
    assert!(error("#check\n#target @glob()").contains("Unknown template @glob"));
    // A section template's body may call them.
    let lines: Vec<String> = ["#check", "#target @wrap()"].map(str::to_owned).to_vec();
    let mut library = spec_library();
    library.get_mut("rest").unwrap().insert(
        "target".to_owned(),
        "@wrap Uses a global one\n[{{@glob}}]".to_owned(),
    );
    let edit = services_kernel::editor::expand_call(
        &Modules,
        "rest",
        &lines,
        1,
        &[],
        &library,
        (&dictionary(), ""),
    )
    .unwrap()
    .unwrap();
    assert_eq!(edit.text, "\n[G]");
    assert!(error("#check\n#target @url_shared()").contains("missing argument"));
    assert!(error("#check\n#target @host(\"a\", \"b\")").contains("takes 1 argument"));
    for source in [
        "#check\n#target @file",
        "#check\n#target \\@host(\"a\")",
        "#check\n#target https://a",
        "#check\n#target\n@host(\"a\")",
    ] {
        assert!(expand(source, 1).is_none(), "{source}");
    }
    // A bare name of a known template expands it.
    assert_eq!(
        expand("#check\n#payload @order", 1).unwrap().unwrap(),
        "#check\n#payload @order\n{ \"x\": 1 }"
    );
}

#[test]
fn check_templates_with_parameters_ask_for_arguments_and_expand_on_enter() {
    services_shell::link();
    let check = "@pair(a: \"p\" | \"q\", b = \"y\") Pair\n#check Own\n#target\n{{@glob}}/{{a}}/{{b}}\n#action\nGET\n@plain Plain\n#target\n/x";
    let templates = BTreeMap::from([("check".to_owned(), check.to_owned())]);
    let names = suggest("rest", "", "#check @", 0, 8, &templates).unwrap();
    assert_eq!(names.candidates[0].text, "#check Pair @pair(");
    assert!(names.candidates[0].continue_hint);
    assert_eq!(names.candidates[1].text, "#check Plain @plain");
    let source = "#check Pair @pair(";
    let args = suggest("rest", "", source, 0, source.len(), &templates).unwrap();
    assert_eq!(args.candidates[0].text, "\"p\", ");
    assert_eq!(args.from, source.len());

    let mut library = spec_library();
    library
        .get_mut("rest")
        .unwrap()
        .insert("check".to_owned(), check.to_owned());
    let expand = |source: &str| {
        let lines: Vec<String> = source.split('\n').map(str::to_owned).collect();
        services_kernel::editor::expand_call(
            &Modules,
            "rest",
            &lines,
            0,
            &[],
            &library,
            (&dictionary(), ""),
        )
        .map(|edit| edit.map(|edit| (edit.from.line, edit.to.line, edit.text)))
    };
    let body = "#target\nG/q/y\n#action\nGET".to_owned();
    assert_eq!(
        expand("#check Pair @pair(\"q\")\n#check next"),
        Some(Ok((0, 0, format!("\n{body}"))))
    );
    // Enter again regenerates the whole check, up to the next one.
    assert_eq!(
        expand("#check Pair @pair(\"q\")\n#target\nold\n#metadata\nA: b\n\n#check next"),
        Some(Ok((1, 4, body.clone())))
    );
    assert!(expand(&format!("#check @pair(\"q\")\n{body}")).is_none());
    assert!(expand("#check Pair @pair()").unwrap().is_err());
    assert!(expand("#check title").is_none());
    // Without parameters the call stays the header too.
    assert_eq!(
        expand("#check Plain @plain"),
        Some(Ok((0, 0, "\n#target\n/x".to_owned())))
    );
    assert!(expand("#check mail@pair(\"q\")").is_none());
}

fn tree() -> Root {
    let parse = |text| services_sdk::refs::parse_lines(text).unwrap();
    Root::new(vec![(
        "tree".into(),
        parse("alpha:\n  one: x\n  two: y\nsecret: abc"),
    )])
}

const DEFS: &str = r#"def pick(k): .tree.alpha[k]; def keys_of(k): .tree[k] | keys; def ref: "{{.a.x}}"; def hidden: .tree.secret; def bad: error("boom");"#;

#[test]
fn eager_function_calls_are_replaced_by_their_results_on_expansion() {
    services_shell::link();
    let mut library = spec_library();
    library.get_mut("rest").unwrap().insert(
        "target".to_owned(),
        "@e(p) E\n{{$!pick(p)}}/{{$!keys_of(\"alpha\")}}/{{$!ref}}/{{$pick(p)}}\n@bad() B\n<{{$!bad}}>\n@hidden() H\n<{{$!hidden}}>".to_owned(),
    );
    let (root, lines) = (tree(), |source: &str| {
        source.split('\n').map(str::to_owned).collect::<Vec<_>>()
    });
    let expand = |source: &str| {
        services_kernel::editor::expand_call(
            &Modules,
            "rest",
            &lines(source),
            1,
            &[],
            &library,
            (&root, DEFS),
        )
        .unwrap()
        .map(|edit| edit.text)
    };
    // The result is inserted verbatim: a reference it holds stays one.
    assert_eq!(
        expand("#check\n#target @e(\"one\")").unwrap(),
        "\nx/[\"one\",\"two\"]/{{.a.x}}/{{$pick(\"one\")}}"
    );
    // A failing call, or one whose result holds a secret, expands nothing.
    assert!(
        expand("#check\n#target @bad()")
            .unwrap_err()
            .contains("boom")
    );
    assert_eq!(
        expand("#check\n#target @hidden()").unwrap_err(),
        "{{$!hidden()}}: the result holds a secret; use {{$hidden()}} to resolve it at send time"
    );
}

#[test]
fn enter_on_a_line_with_eager_calls_replaces_them() {
    services_shell::link();
    let root = tree();
    let enter = |source: &str, line: usize| {
        let lines: Vec<String> = source.split('\n').map(str::to_owned).collect();
        services_kernel::editor::expand_eager((&lines, line), &[], (&root, DEFS))
    };
    let edit = enter("#check\n#target\nhttps://{{$!pick(\"two\")}}/{{:a}}", 2)
        .unwrap()
        .unwrap();
    assert_eq!(
        (edit.from.line, edit.from.ch, edit.to.line, edit.to.ch),
        (2, 0, 2, 32)
    );
    assert_eq!(edit.text, "https://y/{{:a}}");
    assert!(enter("#check\n#target\nhttps://{{$pick(\"two\")}}", 2).is_none());
    assert!(enter("#check\n#target\nhttps://\\{{$!pick(\"two\")}}", 2).is_none());
    assert!(
        enter("#check\n#target\n{{$!bad}}", 2)
            .unwrap()
            .unwrap_err()
            .contains("boom")
    );
    assert!(
        enter("#check\n#target\n{{$!hidden}}", 2)
            .unwrap()
            .unwrap_err()
            .contains("holds a secret")
    );
    // Inside the arguments, the signature shows with the current parameter marked, past
    // quotes and nested references.
    let suggest = |source: &str| {
        let line = source.lines().count() - 1;
        let ch = source.lines().last().unwrap().encode_utf16().count();
        services_kernel::editor::completion::suggest(
            &Modules,
            "rest",
            "",
            source,
            line,
            ch,
            &[],
            &Library::new(),
            (&root, "def target($s; $e; $n): .;"),
        )
        .candidates
        .into_iter()
        .map(|candidate| (candidate.text, candidate.label.unwrap_or_default()))
        .collect::<Vec<_>>()
    };
    let kinds = |signature: &str| {
        std::iter::once((String::new(), signature.to_owned()))
            .chain([".", ":", "$", "@", "\""].map(|kind| (kind.to_owned(), kind.to_owned())))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        suggest("#check\n#target\n{{$!target(\"a, b\", {{:a}}, "),
        kinds("target($s, $e, «$n»)")
    );
    assert_eq!(
        suggest("#check\n#target\n{{$target(\"{{:a}}\", "),
        kinds("target($s, «$e», $n)")
    );
    assert_eq!(
        suggest("#check\n#target\n{{$target(1, \"x"),
        [(String::new(), "target($s, «$e», $n)".to_owned())]
    );
}

#[test]
fn template_editors_band_each_template() {
    use services_widgets::editor::extension::Extension;
    let text = "\n@a One\nbody\n\\@literal\n@b(x) Two\n  @indented\n@c";
    for language in [
        services_shell::language::Language::new(
            "console-rest",
            "templates",
            "definitions",
            "target",
        ),
        services_shell::language::Language::of_tree("*/*").1,
    ] {
        language.synchronize(text, 0);
        let bands: Vec<_> = (0..7).map(|line| language.line_class(line)).collect();
        let (even, odd) = (Some("editor-band-even"), Some("editor-band-odd"));
        assert_eq!(bands, [None, even, even, even, odd, odd, even]);
    }
    let global = services_shell::language::Language::of_tree("*/*").1;
    let lines: Vec<String> = ["@a(x) A", "https://h/{{x}}//{{y}}"]
        .map(str::to_owned)
        .to_vec();
    let (tokens, _) = global.highlight(&[], &lines, 1).unwrap();
    let kind = |kind: &str| Some(kind.to_owned());
    assert_eq!(tokens, [(10, None), (15, kind("tpl-arg")), (22, None)]);
    let plain = services_shell::language::Language::new("console-rest", "rest", "payload", "");
    plain.synchronize(text, 0);
    assert_eq!(plain.line_class(1), None);
}

#[test]
fn the_built_in_copy_starts_a_check_with_sections_of_another() {
    services_shell::link();
    let mut library = spec_library();
    for (service, sections) in services_kernel::modules::registry::templates() {
        library.entry(service).or_default().extend(sections);
    }
    let defs = services_kernel::modules::registry::function_defs(Some("rest"));
    let checks = serde_json::json!({
        "1": {"number": 1, "title": "item list", "target": "{{:host}}/a", "action": "GET", "response": "{}"},
        "2": {"number": 2, "title": "[v2] item list", "target": "/b", "payload": "{\n  \"x\": 1\n}", "response": ""},
    });
    let mut this = checks.as_object().unwrap().clone();
    this.insert("prev".into(), checks["2"].clone());
    let root = Root::new(vec![(
        "checks".into(),
        serde_json::json!({"rest": checks, "this": this})
            .as_object()
            .unwrap()
            .clone(),
    )]);
    let names = services_kernel::editor::templates::suggest(
        &Modules,
        "rest",
        "",
        "#check @",
        0,
        8,
        &library,
        (&root, &defs),
    )
    .unwrap();
    let copy = names
        .candidates
        .iter()
        .find(|candidate| candidate.text == "#check @copy()")
        .unwrap();
    assert!(!copy.continue_hint);
    // Tab opens it for its arguments instead of taking the defaults.
    assert_eq!(copy.tab.as_deref(), Some("#check @copy("));
    let expand = |source: &str| {
        let lines: Vec<String> = source.split('\n').map(str::to_owned).collect();
        services_kernel::editor::expand_call(
            &Modules,
            "rest",
            &lines,
            0,
            &[],
            &library,
            (&root, &defs),
        )
        .map(|edit| edit.map(|edit| (edit.from.line, edit.from.ch, edit.text)))
    };
    // A copy of a copy takes the next version of the original.
    let copied = "#target\n/b\n#payload\n{\n  \"x\": 1\n}";
    assert_eq!(
        expand("#check @copy()"),
        Some(Ok((
            0,
            0,
            format!("#check [v3] item list @copy()\n{copied}")
        )))
    );
    assert_eq!(
        expand("#check @copy(\"1\")\n#check next"),
        Some(Ok((
            0,
            0,
            "#check [v3] item list @copy(\"1\")\n#target\n{{:host}}/a\n#action\nGET".into()
        )))
    );
    // With a title the call only fills the body, so Enter again changes nothing.
    assert_eq!(
        expand("#check Mine @copy(\"2\")"),
        Some(Ok((0, 22, format!("\n{copied}"))))
    );
    assert_eq!(
        expand(&format!("#check [v3] item list @copy()\n{copied}")),
        None
    );
    // Another original renames a generated title; the same one keeps its version.
    assert_eq!(
        expand("#check [v3] other @copy(\"1\")\n#target\n/b"),
        Some(Ok((
            0,
            0,
            "#check [v3] item list @copy(\"1\")\n#target\n{{:host}}/a\n#action\nGET".into()
        )))
    );
    assert_eq!(
        expand("#check [v7] item list @copy(\"1\")\n#target\n/b"),
        Some(Ok((1, 0, "#target\n{{:host}}/a\n#action\nGET".into())))
    );
    assert!(
        expand("#check @copy(\"9\")")
            .unwrap()
            .unwrap_err()
            .contains("no check 9")
    );
}
