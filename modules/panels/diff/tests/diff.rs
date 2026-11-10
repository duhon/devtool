use serde_json::json;
use services_module_diff::{Diff, Msg};
use services_sdk::diff::GHOST;
use services_sdk::ui::{Editor, node::Kind};
use services_sdk::{Cmd, Event, Module};

fn ui(diff: &mut Diff, msg: Msg, value: Option<&str>) -> Vec<Cmd> {
    diff.update(Event::Ui {
        msg,
        value: value.map(str::to_owned),
    })
}

fn open(diff: &mut Diff, left: &str, right: &str) {
    diff.update(Event::ToolCall {
        tag: "open".into(),
        name: "diff.open".into(),
        args: json!({"left": left, "right": right, "left_title": "Mine", "right_title": "Theirs"}),
    });
}

fn status(commands: &[Cmd]) -> Option<serde_json::Value> {
    commands
        .iter()
        .rev()
        .find(|command| command.name == "status.set")
        .map(Cmd::args)
}

fn editors(diff: &Diff) -> Vec<Editor> {
    fn walk(node: &services_sdk::ui::Node, out: &mut Vec<Editor>) {
        match &node.kind {
            Some(Kind::Editor(editor)) => out.push(editor.clone()),
            Some(Kind::PanelFrame(frame)) => frame.body.iter().for_each(|node| walk(node, out)),
            Some(Kind::Row(children) | Kind::Column(children)) => {
                children.children.iter().for_each(|node| walk(node, out))
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(&diff.view().expect("view"), &mut out);
    out
}

#[test]
fn an_empty_panel_is_identical() {
    let mut diff = Diff::default();
    let commands = diff.update(Event::Shown);
    assert_eq!(commands[0].name, "document.read");
    let commands = ui(&mut diff, Msg::Left, Some("a"));
    assert_eq!(status(&commands).unwrap()["text"], "Diff · 1 difference");
    let commands = ui(&mut diff, Msg::Right, Some("a"));
    assert_eq!(status(&commands).unwrap()["text"], "Identical");
}

#[test]
fn texts_align_with_ghost_lines_and_typing_strips_them() {
    let mut diff = Diff::default();
    ui(&mut diff, Msg::Left, Some("a\nb\nc"));
    ui(&mut diff, Msg::Right, Some("a\nc"));
    let shown = editors(&diff);
    assert_eq!(shown[0].text, "a\nb\nc");
    assert_eq!(shown[1].text, format!("a\n{GHOST}\nc"));
    assert_eq!(shown[1].actions.len(), 1);
    // The editor sends back what it shows; the ghost is not text.
    ui(&mut diff, Msg::Right, Some(&shown[1].text));
    assert_eq!(editors(&diff)[1].text, format!("a\n{GHOST}\nc"));
    let commands = ui(&mut diff, Msg::Right, Some(&format!("a\n{GHOST}b\nc")));
    assert_eq!(status(&commands).unwrap()["text"], "Identical");
    assert_eq!(editors(&diff)[1].text, "a\nb\nc");
}

#[test]
fn taking_a_chunk_makes_the_texts_equal() {
    let mut diff = Diff::default();
    ui(&mut diff, Msg::Left, Some("a\nb\nc"));
    ui(&mut diff, Msg::Right, Some("a\nx\nc"));
    let commands = ui(
        &mut diff,
        Msg::Take(0, services_sdk::diff::Side::Left),
        None,
    );
    assert_eq!(status(&commands).unwrap()["text"], "Identical");
    assert_eq!(editors(&diff)[1].text, "a\nb\nc");
}

#[test]
fn next_and_previous_wrap_around_the_differences() {
    let mut diff = Diff::default();
    ui(&mut diff, Msg::Left, Some("a\nb\nc\nd\ne"));
    ui(&mut diff, Msg::Right, Some("a\nB\nc\nD\ne"));
    let line = |diff: &Diff| editors(diff)[0].reveal_line;
    ui(&mut diff, Msg::Next, None);
    assert_eq!(line(&diff), 3);
    ui(&mut diff, Msg::Next, None);
    assert_eq!(line(&diff), 1);
    ui(&mut diff, Msg::Previous, None);
    assert_eq!(line(&diff), 3);
}

#[test]
fn modes_and_whitespace_change_the_comparison() {
    let mut diff = Diff::default();
    ui(&mut diff, Msg::Left, Some("a  b"));
    ui(&mut diff, Msg::Right, Some("a b"));
    assert!(
        editors(&diff)[0]
            .marks
            .iter()
            .all(|mark| mark.class == "changed")
    );
    let commands = ui(&mut diff, Msg::Ignore, Some("true"));
    assert_eq!(status(&commands).unwrap()["text"], "Identical");
    ui(&mut diff, Msg::Ignore, Some("false"));
    ui(&mut diff, Msg::Mode, Some("chars"));
    assert!(
        editors(&diff)[0]
            .marks
            .iter()
            .any(|mark| mark.class == "removed-text")
    );
}

#[test]
fn an_opened_comparison_runs_until_done_or_stopped() {
    let mut diff = Diff::default();
    open(&mut diff, "a\nb", "a\nc");
    let commands = ui(&mut diff, Msg::Left, Some("a\nb"));
    let running = status(&commands);
    assert!(running.is_none(), "unchanged status is not sent again");
    let commands = diff.update(Event::Hidden);
    assert!(commands.is_empty());

    // The state stays while hidden.
    assert_eq!(editors(&diff)[0].text, "a\nb");
    let commands = ui(&mut diff, Msg::Right, Some("a\nb"));
    let status = status(&commands).unwrap();
    assert_eq!(status["indicator"], "busy");
    assert_eq!(status["stop"]["testid"], "diff-stop");

    let commands = ui(&mut diff, Msg::Done, None);
    assert_eq!(commands[0].name, "diff.done");
    assert_eq!(commands[0].args()["text"], "a\nb");
    assert_eq!(commands[1].args()["indicator"], "idle");
    assert_eq!(editors(&diff)[0].text, "");

    open(&mut diff, "x", "y");
    let commands = ui(&mut diff, Msg::Stop, None);
    assert_eq!(commands[0].name, "diff.cancel");
    assert_eq!(editors(&diff)[1].text, "");
    assert!(ui(&mut diff, Msg::Stop, None).is_empty());
}
