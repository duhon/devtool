//! The editors' [`Language`]: what the Kernel's modules and the console document add to an
//! editor widget — module highlighting, folds and completion, and a console's response
//! regions, checks and edit history.
use serde_json::{Value, json};
use services_kernel::document::{
    self, ClipboardFragment, EditHistory, ResponseRegion, checks::Previous, identity::Identity,
};
use services_kernel::editor::{self as language, Services};
use services_kernel::modules::registry as kernel;
use services_sdk::{Check, Completions, Fold as ModuleFold, Line, LineColumns, Token};
use services_widgets::editor::extension::{
    self, Candidate, Completion, Extension, Languages, Payload, Refresh, Tokens, Viewport,
};
use services_widgets::editor::model::{Context, Session};
use services_widgets::editor::{Change, Fold, Position, TextChange, Transaction};
use std::any::Any;
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;

/// The running modules, as the Kernel's editor language reads them.
pub struct Modules;

impl Services for Modules {
    fn is_service(&self, service: &str) -> bool {
        kernel::is_service(service)
    }

    fn markers(&self, service: &str) -> Vec<String> {
        services_kernel::requests::markers(service)
    }

    fn highlight(
        &self,
        service: &str,
        state: Vec<u8>,
        lines: Vec<String>,
        columns: Vec<LineColumns>,
    ) -> Option<Vec<Line>> {
        kernel::with(|modules| modules.highlight(service, state, lines, columns)).ok()
    }

    fn fold(&self, service: &str, check: Check) -> Vec<ModuleFold> {
        kernel::with(|modules| modules.fold(service, check)).unwrap_or_default()
    }

    fn marks(&self, service: &str, check: Check) -> Vec<Line> {
        kernel::with(|modules| modules.marks(service, check)).unwrap_or_default()
    }

    fn complete(&self, service: &str, check: Check, pos: u32) -> Option<Completions> {
        kernel::with(|modules| modules.complete(service, check, pos)).ok()
    }
}

thread_local! {
    /// The console fragment last copied, so pasting it restores its response regions.
    static CLIPBOARD: RefCell<Option<ClipboardFragment>> = const { RefCell::new(None) };
}

#[derive(Default)]
struct State {
    regions: Vec<ResponseRegion>,
    /// `regions` by header line.
    sorted: Vec<ResponseRegion>,
    checks: Vec<Identity>,
    check_lines: Vec<usize>,
    /// What the next synchronization takes.
    pending_regions: Option<Vec<ResponseRegion>>,
    pending_checks: Option<Vec<Identity>>,
    history: Option<EditHistory>,
    /// The line an insertion starts on and the check lines, as they were before it.
    inserting: Option<(Previous, Vec<usize>)>,
    /// The module folds of the check at a line, until the text changes.
    module_folds: Option<(usize, Folds)>,
    /// The module marks of a check, its `#check` line and the line after its last, until
    /// the text changes.
    module_marks: Option<(usize, usize, Rc<[Line]>)>,
}

/// Folded line ranges, start and end.
type Folds = Rc<[(usize, usize)]>;

/// One editor's language: its service, the request section it edits and, for a console,
/// its document.
pub struct Language {
    /// The service whose module highlights and folds the text (`console-<service>` modes).
    module: Option<String>,
    /// The service completion asks.
    service: String,
    /// The request section the editor edits; empty for a console.
    field: String,
    /// The template section the text belongs to.
    section: Option<String>,
    /// A saved run: its `#responce` line heads the response of the check above it.
    snapshot: bool,
    state: RefCell<State>,
}

impl Language {
    /// An editor in `mode` completing `field` of `service`, in template `section`.
    pub fn new(mode: &str, service: &str, field: &str, section: &str) -> Self {
        Self {
            module: mode.strip_prefix("console-").map(str::to_owned),
            service: service.to_owned(),
            field: field.to_owned(),
            section: (!section.is_empty()).then(|| section.to_owned()),
            snapshot: false,
            state: RefCell::default(),
        }
    }

    /// The console document of `service`; `scope` scopes the ids of its checks.
    pub fn console(service: &str, scope: &str) -> Self {
        let language = Self::new(&format!("console-{service}"), service, "", "");
        let mut history = EditHistory::default();
        history.scope = scope.to_owned();
        language.state.borrow_mut().history = Some(history);
        language
    }

    /// What `language` of a module's UI tree is: a service's checks (`<id>`), one template
    /// section of them (`<id>/<section>`), a saved run of one (`history/<id>`), a
    /// service's completion hints (`hints/<id>`), `headers` lines with variable
    /// completion, or another highlighting mode.
    pub fn of_tree(language: &str) -> (String, Self) {
        let (head, tail) = language.split_once('/').unwrap_or((language, ""));
        let make = |mode: String, service: &str, field: &str, section: &str| {
            let language = Self::new(&mode, service, field, section);
            (mode, language)
        };
        match head {
            "" => make(String::new(), "", "", ""),
            service if kernel::is_service(service) && tail.is_empty() => {
                make(format!("console-{service}"), service, "", "")
            }
            service if kernel::is_service(service) => make(
                format!("console-{service}"),
                "templates",
                "definitions",
                tail,
            ),
            "history" => {
                let (mode, mut language) =
                    make(format!("console-{tail}"), "history", "snapshot", "");
                language.snapshot = true;
                (mode, language)
            }
            "hints" => make("headers".into(), "hints", tail, ""),
            "headers" => make("headers".into(), "vars", "variables", ""),
            mode => make(mode.to_owned(), "", "", ""),
        }
    }

    /// The language of the editor `session`, when it has one.
    pub fn of(session: &Session) -> Option<&Self> {
        session.extension().as_any().downcast_ref()
    }

    /// The service whose module highlights the editor.
    pub fn module(&self) -> Option<&str> {
        self.module.as_deref()
    }

    /// The module's marks of `lines[line]`, asked once per check until the text changes.
    fn marks(&self, service: &str, lines: &[String], line: usize) -> Vec<Token> {
        let mut state = self.state.borrow_mut();
        let cached = |state: &State| {
            state
                .module_marks
                .as_ref()
                .filter(|(check, end, _)| *check < line && line < *end)
                .map(|(check, _, marks)| {
                    marks
                        .get(line - check - 1)
                        .map(|marks| marks.tokens.clone())
                        .unwrap_or_default()
                })
        };
        if let Some(marks) = cached(&state) {
            return marks;
        }
        let Some((check, end, marks)) =
            language::module_marks(&Modules, service, lines, line, &state.regions)
        else {
            return Vec::new();
        };
        state.module_marks = Some((check, end, marks.into()));
        cached(&state).unwrap_or_default()
    }

    pub fn regions(&self) -> Vec<ResponseRegion> {
        self.state.borrow().regions.clone()
    }

    pub fn checks(&self) -> Vec<Identity> {
        self.state.borrow().checks.clone()
    }

    /// The responses and checks the editor takes with its next text.
    pub fn set_document(&self, regions: &[ResponseRegion], checks: &[Identity]) {
        let mut state = self.state.borrow_mut();
        state.pending_regions = Some(regions.to_vec());
        state.pending_checks = Some(checks.to_vec());
    }

    fn document(&self) -> bool {
        self.state.borrow().history.is_some()
    }

    fn output(state: &State, line: usize) -> bool {
        state
            .regions
            .iter()
            .any(|region| line >= region.header && line < region.end)
    }

    fn masked(&self, source: &str) -> String {
        document::mask_output(source, &self.state.borrow().regions)
    }
}

/// The response regions of a saved run: the `#responce` line heads the response of the
/// check above it.
fn snapshot_regions(text: &str) -> Vec<ResponseRegion> {
    let lines: Vec<&str> = text.split('\n').collect();
    let Some(header) = lines.iter().position(|line| line.starts_with("#responce ")) else {
        return Vec::new();
    };
    let request = lines[..header]
        .iter()
        .position(|line| document::is_check(line))
        .unwrap_or(0);
    vec![ResponseRegion {
        request,
        header,
        end: lines.len(),
        columns: Vec::new(),
    }]
}

fn position(position: Position) -> document::Position {
    document::Position {
        line: position.line,
        ch: position.ch,
    }
}

fn widget_position(position: document::Position) -> Position {
    Position {
        line: position.line,
        ch: position.ch,
    }
}

impl Extension for Language {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn synchronize(&self, text: &str, generation: u64) -> Refresh {
        let mut state = self.state.borrow_mut();
        let state = &mut *state;
        let regions = state.pending_regions.take().unwrap_or_else(|| {
            if self.snapshot {
                snapshot_regions(text)
            } else if state.history.is_some() {
                state.regions.clone()
            } else {
                Vec::new()
            }
        });
        let mut refresh = Refresh::default();
        if regions != state.regions {
            let mut sorted = regions.clone();
            sorted.sort_by_key(|region| region.header);
            state.sorted = sorted;
            state.regions = regions;
            refresh.relight = true;
        }
        let checks = state.pending_checks.take();
        if let Some(checks) = &checks
            && *checks != state.checks
        {
            state.checks = checks.clone();
            state.check_lines = checks.iter().map(|check| check.line).collect();
            refresh.redraw = true;
        }
        if let Some(history) = &mut state.history {
            history.synchronize(text, &state.regions, generation);
            if let Some(checks) = &checks {
                history.synchronize_checks(checks, generation);
            }
        }
        refresh
    }

    fn edited(
        &self,
        text: &str,
        changes: &[TextChange],
        generation: u64,
        generations: &BTreeSet<u64>,
    ) -> Option<Result<(Payload, Refresh), String>> {
        let mut state = self.state.borrow_mut();
        let history = state.history.as_mut()?;
        let changes: Vec<document::TextChange> = changes
            .iter()
            .map(|change| document::TextChange {
                from: position(change.from),
                to: position(change.to),
                text: change.text.clone(),
                removed: change.removed.clone(),
                origin: change.origin.clone(),
            })
            .collect();
        let result = CLIPBOARD.with(|clipboard| {
            history.apply(
                text.to_owned(),
                &changes,
                generation,
                &mut clipboard.borrow_mut(),
            )
        });
        history.retain_generations(generations);
        Some(result.map(|edit| {
            let relight = edit.regions != state.regions;
            if relight {
                let mut sorted = edit.regions.clone();
                sorted.sort_by_key(|region| region.header);
                state.sorted = sorted;
                state.regions = edit.regions.clone();
            }
            state.check_lines = edit.checks.iter().map(|check| check.line).collect();
            state.checks = edit.checks.clone();
            (
                Payload::new(edit),
                Refresh {
                    relight,
                    redraw: true,
                },
            )
        }))
    }

    fn invalidate(&self) {
        let mut state = self.state.borrow_mut();
        state.module_folds = None;
        state.module_marks = None;
    }

    fn copy(&self, from: Position, to: Position) {
        let state = self.state.borrow();
        if let Some(history) = &state.history {
            let fragment = history.clipboard(&position(from), &position(to)).ok();
            CLIPBOARD.with(|clipboard| *clipboard.borrow_mut() = fragment);
        }
    }

    fn inserting(&self, lines: &[String], line: usize) {
        let mut state = self.state.borrow_mut();
        let previous = Previous::capture(lines.get(line).map(String::as_str), line, &state.regions);
        state.inserting = Some((previous, state.check_lines.clone()));
    }

    fn inserted(
        &self,
        lines: &[String],
        from: Position,
        inserted: &[String],
        origin: &str,
    ) -> Option<Transaction> {
        let (previous, checks) = self.state.borrow_mut().inserting.take()?;
        if !self.document() {
            return None;
        }
        let pasted = if origin == "paste" {
            let text = inserted.join("\n");
            CLIPBOARD.with(|clipboard| {
                clipboard
                    .borrow()
                    .as_ref()
                    .and_then(|fragment| fragment.pasted_regions(&text))
                    .unwrap_or_default()
                    .to_vec()
            })
        } else {
            Vec::new()
        };
        let lines = document::checks::separate(
            lines,
            position(from),
            inserted,
            origin,
            &checks,
            previous,
            &pasted,
        )?;
        Some(Transaction {
            changes: lines
                .into_iter()
                .map(|line| Change {
                    from: Position { line, ch: 0 },
                    to: Position { line, ch: 0 },
                    text: "\n".into(),
                })
                .collect(),
            origin: origin.into(),
            selection: None,
            join: true,
        })
    }

    fn removal(&self, source: &str, line: usize) -> Option<usize> {
        let text = source.split('\n').nth(line)?;
        if !text
            .trim_start()
            .to_ascii_lowercase()
            .starts_with("#respon")
        {
            return None;
        }
        let state = self.state.borrow();
        Some(document::request_at(source, line, &state.regions)?.response_end)
    }

    fn output(&self, line: usize) -> bool {
        Self::output(&self.state.borrow(), line)
    }

    fn outputs(&self) -> Vec<(usize, usize)> {
        let state = self.state.borrow();
        state
            .regions
            .iter()
            .map(|region| (region.header, region.end))
            .collect()
    }

    fn line_class(&self, line: usize) -> Option<&'static str> {
        let state = self.state.borrow();
        state.history.as_ref()?;
        document::checks::band(&state.check_lines, line).map(|odd| {
            if odd {
                "editor-band-even"
            } else {
                "editor-band-odd"
            }
        })
    }

    fn run_line(&self, source: &str, viewport: Viewport) -> Option<usize> {
        let state = self.state.borrow();
        state.history.as_ref()?;
        let checks = document::check_lines(source, &state.regions);
        document::CheckViewport {
            top: viewport.top,
            visible_cursor: viewport.visible_cursor,
        }
        .active_check(&checks)
        .map(|index| checks[index])
    }

    fn start(&self) -> Vec<u8> {
        self.module.as_deref().map_or_else(Vec::new, |service| {
            language::section_state(&Modules, service, self.section.as_deref())
        })
    }

    fn highlight(&self, state: &[u8], lines: &[String], line: usize) -> Option<Tokens> {
        let service = self.module.as_deref()?;
        let text = lines[line].as_str();
        let marks = self.marks(service, lines, line);
        let document = self.state.borrow();
        let index = document
            .sorted
            .partition_point(|region| region.header <= line);
        let region = index
            .checked_sub(1)
            .map(|index| &document.sorted[index])
            .filter(|region| line < region.end);
        Some(language::highlight(
            &Modules,
            service,
            self.section.as_deref(),
            region,
            (line, text),
            state,
            &marks,
        ))
    }

    fn fold(&self, lines: &[String], line: usize) -> Option<Option<Fold>> {
        let fold = |(from, to): (document::Position, document::Position)| Fold {
            from: widget_position(from),
            to: widget_position(to),
        };
        let mut state = self.state.borrow_mut();
        if let Some(range) = language::fold(lines, line, &state.regions) {
            return Some(range.map(fold));
        }
        let service = self.module.as_deref()?;
        let check = (0..line).rev().find(|&at| document::is_check(&lines[at]));
        let folds = match (&state.module_folds, check) {
            (_, None) => return Some(None),
            (Some((at, folds)), Some(check)) if *at == check => folds.clone(),
            _ => {
                let (check, folds) =
                    language::module_folds(&Modules, service, lines, line, &state.regions)?;
                let folds: Rc<[(usize, usize)]> = folds.into();
                state.module_folds = Some((check, folds.clone()));
                folds
            }
        };
        Some(language::module_fold(lines, line, &folds).map(fold))
    }

    fn suggest(&self, source: &str, line: usize, ch: usize, context: &Context) -> Completion {
        let masked = self.masked(source);
        let completion = language::suggest(
            &Modules,
            &self.service,
            &self.field,
            &masked,
            (line, ch),
            &context.hints,
            &context.templates,
        );
        Completion {
            from: completion.from,
            to: completion.to,
            candidates: completion
                .candidates
                .into_iter()
                .map(|candidate| Candidate {
                    continue_hint: candidate.continue_hint
                        || (candidate.text.starts_with('#') && candidate.text != "#check"),
                    text: candidate.text,
                    label: candidate.label,
                    detail: candidate.detail,
                    literal: candidate.literal,
                })
                .collect(),
            comma: completion.comma,
            automatic: completion.automatic,
        }
    }

    fn insertion(&self, candidate: &str, line: &str) -> Option<String> {
        self.field
            .is_empty()
            .then(|| language::insertion(&Modules, &self.service, candidate, line))
    }

    fn hints(&self, source: &str, line: usize, ch: usize) -> Option<Payload> {
        if !self.field.is_empty() || self.output(line) {
            return None;
        }
        let masked = self.masked(source);
        language::hints(&Modules, &self.service, &masked, line, ch).map(Payload::new)
    }

    fn option(&self, name: &str) -> Option<Value> {
        let state = self.state.borrow();
        match name {
            "servicesResponses" => serde_json::to_value(&state.regions).ok(),
            "servicesChecks" => Some(json!(state.check_lines)),
            "servicesCompletion" => Some(json!({"service": self.service, "field": self.field})),
            _ => None,
        }
    }

    fn set_option(&self, name: &str, value: Value) -> bool {
        if name != "servicesResponses" {
            return false;
        }
        self.state.borrow_mut().pending_regions =
            Some(serde_json::from_value(value).unwrap_or_default());
        true
    }
}

/// The editors of modules' UI trees get a [`Language`]; `headers` ones complete variables.
struct Trees;

impl Languages for Trees {
    fn editor(&self, language: &str) -> (String, Rc<dyn Extension>) {
        let (mode, language) = Language::of_tree(language);
        (mode, Rc::new(language))
    }

    fn hints(&self, language: &str) -> Option<Vec<(String, String)>> {
        (language == "headers").then(kernel::variables)
    }
}

/// Makes the editors of modules' UI trees highlight, fold and complete through the Kernel.
pub fn install() {
    extension::install(&Trees);
}

/// Highlights the kept editors of `service` again: its module has just started.
#[cfg(target_arch = "wasm32")]
pub fn relight(service: &str) {
    for handle in services_widgets::editor::kept_handles() {
        if handle.read(|session| Language::of(session).and_then(Language::module) == Some(service))
        {
            handle.with(Session::relight);
        }
    }
}
