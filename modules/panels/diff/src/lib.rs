//! The Diff panel: two texts side by side with their differences marked, either of which
//! the user can take over the other's piece. Opened by its button it is empty; another
//! module opens it with `diff.open` and gets the right text back when the user is done.
//! The texts are not saved with the session.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use services_sdk::diff::{self, Change, Diff as Comparison, Mode, Options, Side};
use services_sdk::ui::{self, EditorAction, EditorMark};
use services_sdk::{Cmd, Event, Module, cmd};

const DOCUMENT: &str = "document";
const DEBOUNCE_MS: u32 = 100;
const LEFT: &str = "Left";
const RIGHT: &str = "Right";

#[derive(Serialize, Deserialize)]
pub enum Msg {
    Left,
    Right,
    Mode,
    Ignore,
    Language,
    Next,
    Previous,
    /// The chunk of a side replaces the other side's piece.
    Take(usize, Side),
    Done,
    Stop,
}

pub struct Diff {
    left: String,
    right: String,
    left_title: String,
    right_title: String,
    language: String,
    /// The services the editors can highlight as: `(id, name)`.
    services: Vec<(String, String)>,
    options: Options,
    comparison: Comparison,
    /// The chunk Next and Previous stand on.
    current: usize,
    reveal: u32,
    /// Another module waits for the user's decision.
    pending: bool,
    status: (String, &'static str),
}

impl Default for Diff {
    fn default() -> Self {
        Self {
            left: String::new(),
            right: String::new(),
            left_title: LEFT.into(),
            right_title: RIGHT.into(),
            language: String::new(),
            services: Vec::new(),
            options: Options::default(),
            comparison: Comparison::default(),
            current: 0,
            reveal: 0,
            pending: false,
            status: (String::new(), ""),
        }
    }
}

fn count(chunks: usize) -> String {
    match chunks {
        0 => "Identical".into(),
        1 => "Diff · 1 difference".into(),
        n => format!("Diff · {n} differences"),
    }
}

fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Lines => "lines",
        Mode::Words => "words",
        Mode::Chars => "chars",
    }
}

impl Diff {
    fn compare(&mut self) {
        self.comparison = diff::compare(&self.left, &self.right, self.options);
        self.current = self
            .current
            .min(self.comparison.chunks.len().saturating_sub(1));
    }

    /// Back to an empty panel; the options and the language stay.
    fn clear(&mut self) {
        self.left.clear();
        self.right.clear();
        self.left_title = LEFT.into();
        self.right_title = RIGHT.into();
        self.pending = false;
        self.current = 0;
        self.compare();
    }

    fn open(&mut self, args: &Value) {
        let text = |key: &str| args[key].as_str().unwrap_or_default().to_owned();
        let title = |key: &str, default: &str| {
            Some(text(key))
                .filter(|title| !title.is_empty())
                .unwrap_or_else(|| default.into())
        };
        self.left = text("left");
        self.right = text("right");
        self.left_title = title("left_title", LEFT);
        self.right_title = title("right_title", RIGHT);
        self.language = text("language");
        self.pending = true;
        self.current = 0;
        self.compare();
        self.reveal += 1;
    }

    /// The status line, sent when it changed.
    fn status(&mut self) -> Vec<Cmd> {
        let state = (
            count(self.comparison.chunks.len()),
            if self.pending { "busy" } else { "idle" },
        );
        if state == self.status {
            return Vec::new();
        }
        self.status = state.clone();
        vec![if self.pending {
            cmd::status_stoppable(&state.0, state.1, &Msg::Stop, "diff-stop")
        } else {
            cmd::status(&state.0, state.1)
        }]
    }

    fn marks(&self, side: Side) -> Vec<EditorMark> {
        let text = match side {
            Side::Left => &self.left,
            Side::Right => &self.right,
        };
        let lines: Vec<&str> = text.split('\n').collect();
        let mut marks = Vec::new();
        for (index, row) in self.comparison.rows.iter().enumerate() {
            let (line, ranges, whole, inline) = match side {
                Side::Left => (row.left, &row.left_marks, "removed", "removed-text"),
                Side::Right => (row.right, &row.right_marks, "added", "added-text"),
            };
            let Some(line) = line else {
                continue;
            };
            marks.push(EditorMark::line(
                index,
                match row.change {
                    Change::Same => continue,
                    Change::Changed => "changed",
                    Change::Removed | Change::Added => whole,
                },
            ));
            for range in ranges {
                marks.push(EditorMark::range(
                    index,
                    diff::utf16(lines[line], range.start),
                    diff::utf16(lines[line], range.end),
                    inline,
                ));
            }
        }
        marks
    }

    fn actions(&self, side: Side) -> Vec<EditorAction> {
        let (name, title) = match side {
            Side::Left => ("left", "Replace this piece of the right text with this"),
            Side::Right => ("right", "Replace this piece of the left text with this"),
        };
        self.comparison
            .chunks
            .iter()
            .enumerate()
            .map(|(index, chunk)| {
                EditorAction::new(
                    chunk.rows.start,
                    "Use this",
                    title,
                    format!("diff-take-{name}-{index}"),
                    &Msg::Take(index, side),
                )
            })
            .collect()
    }

    fn column(&self, side: Side) -> ui::Node {
        let (title, text, key, msg) = match side {
            Side::Left => (&self.left_title, &self.left, "diff-left", Msg::Left),
            Side::Right => (&self.right_title, &self.right, "diff-right", Msg::Right),
        };
        let line = self
            .comparison
            .chunks
            .get(self.current)
            .map_or(0, |chunk| chunk.rows.start);
        ui::column(vec![
            ui::text(title.clone())
                .variant("muted")
                .key(format!("{key}-title")),
            ui::editor(
                self.language.clone(),
                self.comparison.aligned(side, text),
                &msg,
            )
            .key(key)
            .label(format!("Diff · {title}"))
            .debounce(DEBOUNCE_MS)
            .ghosts()
            .scroll_group("diff")
            .marks(self.marks(side))
            .line_actions(self.actions(side))
            .reveal(line, self.reveal),
        ])
        .variant("diff-column")
    }
}

impl Module for Diff {
    type Msg = Msg;

    fn update(&mut self, event: Event<Msg>) -> Vec<Cmd> {
        let mut commands = Vec::new();
        match event {
            Event::Shown => commands.push(cmd::document_read(DOCUMENT)),
            Event::Reply {
                tag,
                result: Ok(document),
            } if tag == DOCUMENT => {
                self.services = document["services"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|service| {
                        let id = service["id"].as_str()?.to_owned();
                        let name = service["name"].as_str().unwrap_or(&id).to_owned();
                        Some((id, name))
                    })
                    .collect();
            }
            Event::ToolCall { name, args, .. } if name == "diff.open" => self.open(&args),
            Event::Ui { msg, value } => match msg {
                Msg::Left | Msg::Right => {
                    let text = diff::strip(&value.unwrap_or_default());
                    let side = if matches!(msg, Msg::Left) {
                        &mut self.left
                    } else {
                        &mut self.right
                    };
                    if *side != text {
                        *side = text;
                        self.compare();
                    }
                }
                Msg::Mode => {
                    self.options.mode = match value.as_deref() {
                        Some("words") => Mode::Words,
                        Some("chars") => Mode::Chars,
                        _ => Mode::Lines,
                    };
                    self.compare();
                }
                Msg::Ignore => {
                    self.options.ignore_whitespace = value.as_deref() == Some("true");
                    self.compare();
                }
                Msg::Language => self.language = value.unwrap_or_default(),
                Msg::Next | Msg::Previous => {
                    let chunks = self.comparison.chunks.len();
                    if chunks > 0 {
                        self.current = match msg {
                            Msg::Next => (self.current + 1) % chunks,
                            _ => (self.current + chunks - 1) % chunks,
                        };
                        self.reveal += 1;
                    }
                }
                Msg::Take(chunk, from) => {
                    let text = self.comparison.take(chunk, from, &self.left, &self.right);
                    match from {
                        Side::Left => self.right = text,
                        Side::Right => self.left = text,
                    }
                    self.compare();
                }
                Msg::Done if self.pending => {
                    commands.push(cmd::diff_done(&self.right));
                    self.clear();
                }
                Msg::Stop if self.pending => {
                    commands.push(cmd::diff_cancel());
                    self.clear();
                }
                Msg::Done | Msg::Stop => {}
            },
            _ => {}
        }
        commands.extend(self.status());
        commands
    }

    fn view(&self) -> Option<ui::Node> {
        let chunks = self.comparison.chunks.len();
        let mut languages = vec![ui::Choice::new("", "Plain text")];
        languages.extend(
            self.services
                .iter()
                .map(|(id, name)| ui::Choice::new(id.clone(), name.clone())),
        );
        let mut toolbar = vec![
            ui::button("Previous", &Msg::Previous)
                .disabled(chunks == 0)
                .key("diff-previous"),
            ui::button("Next", &Msg::Next)
                .disabled(chunks == 0)
                .key("diff-next"),
            ui::text(if chunks == 0 {
                "0 of 0".to_owned()
            } else {
                format!("{} of {chunks}", self.current + 1)
            })
            .key("diff-counter"),
            ui::select(
                vec![
                    ui::Choice::new("lines", "Lines"),
                    ui::Choice::new("words", "Words"),
                    ui::Choice::new("chars", "Characters"),
                ],
                mode_name(self.options.mode),
                &Msg::Mode,
            )
            .key("diff-mode"),
            ui::checkbox(
                "Ignore whitespace",
                self.options.ignore_whitespace,
                &Msg::Ignore,
            )
            .key("diff-ignore"),
            ui::select(languages, self.language.clone(), &Msg::Language).key("diff-language"),
            ui::spacer(),
        ];
        if self.pending {
            toolbar.push(ui::button("Done", &Msg::Done).key("diff-done"));
        }
        Some(ui::panel(
            "Diff",
            vec![
                ui::row(toolbar).key("diff-toolbar"),
                ui::row(vec![self.column(Side::Left), self.column(Side::Right)])
                    .variant("diff-columns")
                    .key("diff-columns"),
            ],
            Vec::new(),
        ))
    }
}

services_sdk::export!(Diff);
