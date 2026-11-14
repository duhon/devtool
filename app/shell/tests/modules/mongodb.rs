//! MongoDB runs in the Kernel: the console colors its checks the same whether the editor
//! reached a line by the exact pass or restarted there after a scroll.

use services_shell::language::Language;
use services_widgets::editor::highlight::{Highlighter, Span};
use std::rc::Rc;

fn highlighter() -> Highlighter {
    Highlighter::with_extension(
        "console-mongodb",
        Rc::new(Language::new("console-mongodb", "mongodb", "", "")),
    )
}

fn ends(spans: &[Span]) -> Vec<(usize, String)> {
    spans
        .iter()
        .map(|span| (span.end, format!("{:?}", span.style)))
        .collect()
}

#[test]
fn a_line_reached_cold_is_colored_like_the_exact_pass() {
    services_shell::link();
    let check = r##"#check
#target mongodb://localhost/shop
#payload
{
  "pipeline": [
    {"$match": {"tags": {"$in": ["a", "b"]}}},
    {"$lookup": {"from": "stores", "as": "store"}}
  ]
}"##;
    let text = vec![check; 40].join("\n");
    let lines: Vec<String> = text.split('\n').map(str::to_owned).collect();

    let mut exact = highlighter();
    exact.work(&lines, lines.len(), || true);
    let expected: Vec<_> = (0..lines.len())
        .map(|line| ends(&exact.spans(&lines, line)))
        .collect();

    for line in (0..lines.len()).step_by(7) {
        let mut cold = highlighter();
        assert_eq!(
            ends(&cold.spans(&lines, line)),
            expected[line],
            "line {line}"
        );
    }
}

#[test]
fn options_offer_the_console_then_the_module_and_a_saved_hint_is_the_default() {
    services_shell::link();
    let details = |lines: &str, hints: &[(String, String)]| -> Vec<(String, String)> {
        let source = format!("#check\n#target mongodb://localhost/shop\n#options\n{lines}");
        let line = source.lines().count() - 1 + usize::from(lines.is_empty());
        services_kernel::editor::completion::suggest(
            &services_shell::language::Modules,
            "mongodb",
            "",
            &source,
            line,
            lines.len(),
            hints,
        )
        .candidates
        .into_iter()
        .map(|candidate| (candidate.text, candidate.detail))
        .filter(|(text, _)| !text.starts_with('#'))
        .collect()
    };
    let options = |lines: &str, hints: &[(String, String)]| -> Vec<String> {
        details(lines, hints)
            .into_iter()
            .map(|(text, _)| text)
            .collect()
    };
    let saved = [("Repeat: 10s".to_owned(), "Saved hint #options".to_owned())];
    let badges = |lines: &str, hints: &[(String, String)]| -> Vec<(String, Option<char>)> {
        let source = format!("#check\n#target mongodb://localhost/shop\n#options\n{lines}");
        let line = source.lines().count() - 1 + usize::from(lines.is_empty());
        services_kernel::editor::completion::suggest(
            &services_shell::language::Modules,
            "mongodb",
            "",
            &source,
            line,
            lines.len(),
            hints,
        )
        .candidates
        .into_iter()
        .map(|candidate| (candidate.text, candidate.badge))
        .collect()
    };
    let labels = badges("", &saved);
    assert!(labels.contains(&("Repeat: 10s".into(), Some('u'))));
    assert!(labels.contains(&("Format: ".into(), Some('m'))));
    assert!(badges("", &[]).contains(&("Repeat: 5s".into(), Some('g'))));
    let plain = options("", &[]);
    assert_eq!(plain[..2], ["Timeout: 30", "Repeat: 5s"]);
    assert!(plain.contains(&"Format: ".to_owned()), "{plain:?}");
    let with = options("", &saved);
    assert_eq!(with[..2], ["Timeout: 30", "Repeat: 10s"]);
    assert!(!with.contains(&"Repeat: 5s".to_owned()));
    assert!(
        !options("Repeat: 1m", &saved)
            .iter()
            .any(|t| t.starts_with("Repeat:"))
    );

    let grouped = |lines: &str| -> Vec<(String, Option<String>, bool)> {
        let source = format!("#check\n#target mongodb://localhost/shop\n#options\n{lines}");
        let line = source.split('\n').count() - 1;
        services_kernel::editor::completion::suggest(
            &services_shell::language::Modules,
            "mongodb",
            "",
            &source,
            line,
            lines.len(),
            &[],
        )
        .candidates
        .into_iter()
        .map(|candidate| (candidate.text, candidate.label, candidate.continue_hint))
        .collect()
    };
    let all = grouped("");
    assert!(
        all.contains(&("Format: ".into(), Some("Format: json | table".into()), true)),
        "{all:?}"
    );
    let values = grouped("Format: ");
    assert!(
        values.iter().any(|(text, ..)| text == "Format: table"),
        "{values:?}"
    );
    assert!(
        !values.iter().any(|(text, ..)| text == "Format: "),
        "{values:?}"
    );

    let saved = [("Format: json".to_owned(), "Saved hint #options".to_owned())];
    let typed = badges("Format: ", &saved);
    assert_eq!(typed[0], ("Format: json".into(), Some('u')), "{typed:?}");
    assert!(
        typed.contains(&("Format: table".into(), Some('m'))),
        "{typed:?}"
    );
}
