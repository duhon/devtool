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
