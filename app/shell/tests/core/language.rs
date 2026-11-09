//! The editors' language: a console's responses and folds through the editor widget.

use serde_json::json;
use services_kernel::document::ResponseRegion;
use services_shell::language::Language;
use services_widgets::editor::extension::Extension;
use services_widgets::editor::highlight::Highlighter;
use services_widgets::editor::model::{Session, Setup};
use services_widgets::editor::{Position, Transaction, fold};
use std::rc::Rc;

fn lines(text: &str) -> Vec<String> {
    text.split('\n').map(str::to_owned).collect()
}

fn at(line: usize, ch: usize) -> Position {
    Position { line, ch }
}

#[test]
fn checks_responses_and_brackets_fold() {
    services_shell::link();
    let text = "#check a\n{\n  \"x\": \"}\"\n}\n#response 200\n{}\n#check b\nx";
    let doc = lines(text);
    let language = Rc::new(Language::new("console-none", "none", "", ""));
    language.set_document(
        &[ResponseRegion {
            request: 0,
            header: 4,
            end: 6,
            columns: Vec::new(),
        }],
        &[],
    );
    language.synchronize(text, 0);
    let mut highlighter = Highlighter::with_extension("console-none", language);
    let fold = |h: &mut Highlighter, line| fold::range(h, &doc, line).map(|f| (f.from, f.to));
    assert_eq!(fold(&mut highlighter, 0), Some((at(0, 8), at(5, 2))));
    assert_eq!(fold(&mut highlighter, 4), Some((at(4, 13), at(5, 2))));
    // Inside a check only its module folds.
    assert_eq!(fold(&mut highlighter, 1), None);
    assert_eq!(fold(&mut highlighter, 7), None);
    assert_eq!(fold(&mut highlighter, 6), Some((at(6, 8), at(7, 1))));
    let mut json = Highlighter::new("json");
    assert_eq!(fold(&mut json, 1), Some((at(1, 1), at(3, 0))));
}

#[test]
fn edits_keep_the_responses_set_directly() {
    services_shell::link();
    let setup = Setup {
        mode: "console-mysql".into(),
        document: true,
        ..Setup::default()
    };
    let language = Rc::new(Language::console("mysql", ""));
    let mut session = Session::new(
        setup,
        language.clone(),
        "#check A\n#query\nselect 1\n#responce OK\n{\n}",
    );
    let region = ResponseRegion {
        request: 0,
        header: 3,
        end: 6,
        columns: Vec::new(),
    };
    session.set_option("servicesResponses", json!([region]));
    session.apply(
        Transaction::replace(at(5, 0), at(5, 0), "  ", "+input"),
        0.0,
    );
    assert_eq!(language.regions(), [region]);
}
