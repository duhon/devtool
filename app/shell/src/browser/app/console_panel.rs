//! The console panel of [`App`]: the runs the Kernel reports and their responses in the
//! document, check statuses, discovery, completions and the console view.
use super::*;

impl App {
    /// The console editor of `service`, showing its document from the first use.
    pub(super) fn console_editor(&self, service: &str) -> Handle {
        let key = format!("console:{service}");
        if let Some(editor) = self.editors.borrow().get(&key) {
            return editor.clone();
        }
        let scope = format!("{}:{service}", self.workspace_id);
        let setup = Setup {
            mode: format!("console-{service}"),
            scope: scope.clone(),
            document: true,
            ..Setup::default()
        };
        let text = self.docs.get(service).map_or("", String::as_str);
        let editor = editor::kept(
            &format!("{}:{key}", self.session_id),
            setup,
            || Rc::new(Language::console(service, &scope)),
            text,
        );
        if self.console_editor_stale(service, &editor) {
            editor.with(|session| {
                self.sync_console_editor(service, session);
                // Nothing has seen the editor yet.
                session.take_events();
            });
        }
        self.editors.borrow_mut().insert(key, editor.clone());
        editor
    }
    pub(super) fn export_editor(&self, text: &str) -> Handle {
        self.export_editor
            .borrow_mut()
            .get_or_insert_with(|| {
                let setup = Setup {
                    mode: "shell".into(),
                    ..Setup::default()
                };
                Handle::with_extension(setup, Rc::new(Language::new("shell", "", "", "")), text)
            })
            .clone()
    }
    /// The anchor of `line` in the console editor of `service`, which follows the line
    /// through edits.
    pub(super) fn anchor(&self, service: &str, line: usize) -> Option<Anchor> {
        self.console_editor(service)
            .read(|session| session.anchor(line))
    }
    /// The line of `anchor` in the console editor of `service`.
    pub(super) fn anchor_line(&self, service: &str, anchor: Anchor) -> Option<usize> {
        self.console_editor(service)
            .read(|session| session.anchor_line(anchor))
    }
    /// The line of the check of `run` now.
    fn run_line(&self, run: &Run) -> Option<usize> {
        match run.anchor {
            Some(anchor) => self.anchor_line(&run.view.service, anchor),
            None => Some(run.view.line),
        }
    }
    fn console_editor_stale(&self, service: &str, editor: &Handle) -> bool {
        editor.read(|session| {
            !session.pending()
                && (*session.source() != *self.docs.get(service).map_or("", String::as_str)
                    || Language::of(session).is_none_or(|language| {
                        language.regions() != self.regions(service)
                            || language.checks()
                                != self.checks.get(service).map_or(&[][..], Vec::as_slice)
                    }))
        })
    }
    fn regions(&self, service: &str) -> &[services_kernel::document::ResponseRegion] {
        self.response_regions
            .get(service)
            .map_or(&[], Vec::as_slice)
    }
    /// Shows the document of `service` with its responses and checks.
    fn sync_console_editor(
        &self,
        service: &str,
        session: &mut services_widgets::editor::model::Session,
    ) {
        self.sync_console_session(service, session, None);
    }
    fn sync_console_session(
        &self,
        service: &str,
        session: &mut services_widgets::editor::model::Session,
        preserve_after: Option<usize>,
    ) {
        if let Some(language) = Language::of(session) {
            language.set_document(
                self.regions(service),
                self.checks.get(service).map_or(&[], Vec::as_slice),
            );
        }
        session.sync(
            self.docs.get(service).map_or("", String::as_str),
            preserve_after,
            js_sys::Date::now(),
        );
    }
    /// Brings the editors up to the documents changed outside them; their events come back
    /// as messages.
    pub(super) fn sync_editors(&self, ctx: &Context<Self>) {
        let editors = self.editors.borrow().clone();
        for (key, editor) in editors {
            if let Some(service) = key.strip_prefix("console:") {
                if self.console_editor_stale(service, &editor) {
                    editor.with(|session| self.sync_console_editor(service, session));
                }
                if service == self.console_service {
                    editor.set_context(EditorContext {
                        hints: self.completions(service),
                        templates: self.template_entries(service),
                        discovery: self.editor_discovery(service),
                    });
                }
                if editor.pending() {
                    ctx.link()
                        .send_message(Msg::ConsoleEditor(service.into(), EditorMsg::Flush));
                }
            }
        }
        match &self.export_text {
            Some(text) => self.export_editor(text).set_text(text),
            None => {
                self.export_editor.borrow_mut().take();
            }
        }
    }
    /// Handles what the console editor of `service` reported.
    pub(super) fn console_events(
        &mut self,
        ctx: &Context<Self>,
        service: &str,
        events: Vec<EditorEvent>,
    ) -> bool {
        let mut render = false;
        for event in events {
            let msg = match event {
                EditorEvent::Changed(text) => Msg::ConsoleText(text),
                EditorEvent::Edited(edit) => {
                    let Some(edit) = edit.get::<DocumentEdit>() else {
                        continue;
                    };
                    Msg::ConsoleEdited(service.into(), edit.clone())
                }
                EditorEvent::EditError(error) => Msg::EditorError(error),
                EditorEvent::Run { source, line } => Msg::ConsoleRun(source, line),
                EditorEvent::Discover => Msg::Discover,
                EditorEvent::Hints(hints) => {
                    let Some((check, pos)) = hints.get::<(services_sdk::Check, u32)>() else {
                        continue;
                    };
                    Msg::KernelHints(check.clone(), *pos)
                }
                EditorEvent::Viewport(viewport) => Msg::Scroll(
                    service.into(),
                    services_kernel::document::CheckViewport {
                        top: viewport.top,
                        visible_cursor: viewport.visible_cursor,
                    },
                ),
                EditorEvent::Cursor(_) => continue,
            };
            render |= Component::update(self, ctx, msg);
        }
        render
    }
    pub(super) fn active_console_line(&self, service: &str) -> Option<usize> {
        let checks: Vec<_> = self
            .checks
            .get(service)?
            .iter()
            .map(|check| check.line)
            .collect();
        self.viewports
            .get(service)
            .copied()
            .unwrap_or_default()
            .active_check(&checks)
            .map(|index| checks[index])
    }
    pub(super) fn console_run(&self, service: &str, line: Option<usize>) -> Option<&Run> {
        self.console_run_entry(service, line).map(|(_, run)| run)
    }
    pub(super) fn console_run_entry(
        &self,
        service: &str,
        line: Option<usize>,
    ) -> Option<(&str, &Run)> {
        self.runs
            .iter()
            .filter(|(_, run)| {
                run.view.service == service && line.is_some() && self.run_line(run) == line
            })
            .max_by(|(_, a), (_, b)| a.view.started.total_cmp(&b.view.started))
            .map(|(key, run)| (key.as_str(), run))
    }
    pub(super) fn check_status(
        &self,
        service: &str,
        line: Option<usize>,
    ) -> (Option<&Run>, Option<&ModuleStatus>) {
        let run = self.console_run(service, line);
        let editor = self.console_editor(service);
        let discovery = self
            .module_status
            .get(service)
            .into_iter()
            .chain(self.module_history.get(service).into_iter().flatten())
            .filter(|status| line.is_some() && status.check_line(&editor) == line)
            .max_by(|a, b| a.started.total_cmp(&b.started));
        if run.is_some_and(Run::active)
            || discovery
                .is_none_or(|status| run.is_some_and(|run| run.view.started >= status.started))
        {
            (run, None)
        } else {
            (None, discovery)
        }
    }
    pub(super) fn selected_status(&self, service: &str) -> (Option<&Run>, Option<&ModuleStatus>) {
        self.check_status(service, self.active_console_line(service))
    }
    pub(super) fn check_state(
        &self,
        service: &str,
        line: usize,
    ) -> services_widgets::indicator::Indicator {
        let (run, discovery) = self.check_status(service, Some(line));
        if let Some(run) = run {
            services_widgets::indicator::Indicator::status(&run.view.status, run.active())
        } else if let Some(status) = discovery {
            services_widgets::indicator::Indicator::status(&status.text, status.request.is_some())
        } else {
            services_widgets::indicator::Indicator::Ready
        }
    }
    pub(super) fn check_indicator(
        &self,
        service: &str,
        line: usize,
    ) -> services_widgets::indicator::Indicator {
        let state = self.check_state(service, line);
        let visible = !self.hidden
            && self.console_service == service
            && self.active_console_line(service) == Some(line);
        let (run, discovery) = self.check_status(service, Some(line));
        let signal = run
            .map(|run| run.signal)
            .or_else(|| discovery.map(|status| status.signal))
            .unwrap_or_default();
        signal.indicator(state, visible)
    }
    /// Counts how long each status on screen has been there and returns how much longer the
    /// nearest one must stay to count as seen; statuses off screen start a new count.
    pub(super) fn observe_console_status(&mut self) -> Option<f64> {
        use services_widgets::indicator::sooner;
        let now = js_sys::Date::now();
        let service = self.console_service.clone();
        let (run_key, discovery) = if self.hidden {
            (None, None)
        } else {
            let line = self.active_console_line(&service);
            let (run, discovery) = self.selected_status(&service);
            let run_key = run
                .and_then(|_| self.console_run_entry(&service, line))
                .map(|(key, _)| key.to_owned());
            (run_key, discovery.map(|status| (status.started, line)))
        };
        let editor = self.console_editor(&service);
        let mut wait = None;
        for (key, run) in &mut self.runs {
            if run_key.as_ref() == Some(key) {
                let state = Indicator::status(&run.view.status, run.view.running);
                wait = sooner(wait, run.signal.observe(state, now));
            } else {
                run.signal.hide();
            }
        }
        let discoveries =
            self.module_status
                .iter_mut()
                .chain(
                    self.module_history
                        .iter_mut()
                        .flat_map(|(owner, statuses)| {
                            statuses.iter_mut().map(move |status| (owner, status))
                        }),
                );
        for (owner, status) in discoveries {
            if *owner == service
                && discovery.is_some_and(|(started, line)| {
                    status.started == started && status.check_line(&editor) == line
                })
            {
                let state = Indicator::status(&status.text, status.request.is_some());
                wait = sooner(wait, status.signal.observe(state, now));
            } else {
                status.signal.hide();
            }
        }
        wait
    }
    pub(super) fn service_indicator(
        &self,
        service: &str,
    ) -> (services_widgets::indicator::Indicator, String) {
        let source = self.docs.get(service).map(String::as_str).unwrap_or("");
        let regions = self
            .response_regions
            .get(service)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        services_widgets::indicator::summary(
            services_kernel::document::check_lines(source, regions)
                .into_iter()
                .map(|line| self.check_indicator(service, line)),
        )
    }
    pub(super) fn status_owner(&self, service: &str) -> CheckOwner {
        let line = self.active_console_line(service);
        CheckOwner {
            line,
            anchor: line.and_then(|line| self.anchor(service, line)),
        }
    }
    pub(super) fn set_module_status(
        &mut self,
        service: &str,
        text: String,
        debug: Value,
        owner: CheckOwner,
    ) {
        let editor = self.console_editor(service);
        let line = owner.check_line(&editor);
        let status = ModuleStatus {
            request: None,
            text,
            debug,
            started: js_sys::Date::now(),
            owner,
            signal: services_widgets::indicator::Signal::default(),
        };
        let history = self.module_history.entry(service.into()).or_default();
        history.retain(|previous| {
            previous.request.is_some()
                || previous
                    .check_line(&editor)
                    .is_some_and(|previous| Some(previous) != line)
        });
        if let Some(previous) = self.module_status.insert(service.into(), status)
            && (previous.request.is_some()
                || previous
                    .check_line(&editor)
                    .is_some_and(|previous| Some(previous) != line))
        {
            history.push(previous);
        }
    }
    /// A check to run from the console document of `service`: the check whose header is at
    /// `line`, with what History keeps of it.
    pub(super) fn run_request(
        &self,
        service: &str,
        line: usize,
        source: Source,
        origin: Origin,
    ) -> RunRequest {
        let doc = self.docs.get(service).map_or("", String::as_str);
        let identity = self
            .checks
            .get(service)
            .and_then(|checks| checks.iter().find(|check| check.line == line));
        let history = identity.and_then(|check| {
            let block = services_kernel::document::request_at(doc, line, self.regions(service))?;
            Some(json!({"checkId":check.id,"sessionId":self.workspace_id,
                "title":doc.lines().nth(line).unwrap_or("#check"),"createdAt":check.created_at,
                "source":doc.lines().skip(line).take(block.end-line).collect::<Vec<_>>().join("\n")}))
        });
        RunRequest {
            service: service.to_owned(),
            source,
            line,
            check: identity.map(|check| check.id.clone()),
            title: doc.lines().nth(line).unwrap_or("#check").to_owned(),
            history,
            origin,
        }
    }
    /// The Kernel reports a run: a new one replaces the finished run of its check.
    pub(super) fn run_changed(&mut self, view: RunView) {
        let key = view.key.clone();
        if let Some(run) = self.runs.get_mut(&key) {
            run.view = view;
        } else {
            // Completed runs are replaced only for this check; concurrent sibling checks
            // keep their own response anchors.
            self.prune_console_runs(&view.service, view.line);
            let anchor = self.anchor(&view.service, view.line);
            self.runs.insert(
                key.clone(),
                Run {
                    view,
                    anchor,
                    signal: Default::default(),
                },
            );
        }
        self.agent_run_ended(&key);
    }
    fn prune_console_runs(&mut self, service: &str, line: usize) {
        let editor = self.console_editor(service);
        self.runs.retain(|_, run| {
            run.view.service != service
                || run.active()
                || (if let Some(anchor) = run.anchor {
                    editor.read(|session| session.anchor_line(anchor))
                } else {
                    Some(run.view.line)
                })
                .is_some_and(|previous| previous != line)
        });
    }
    pub(super) fn persist_console(&mut self) {
        for (service, source) in &self.docs {
            services_kernel::document::identity::normalize(
                source,
                self.response_regions
                    .get(service)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
                self.checks.entry(service.clone()).or_default(),
            );
        }
        let value = json!({"version":1,"service":self.console_service,"documents":self.docs,"order":self.console_order,"responses":self.response_regions,
            "workspaceId":self.workspace_id,"checkIdentities":self.checks});
        let result = self.sessions.borrow_mut().update(|s| {
            let mut value = value;
            services_kernel::storage::keep_unloaded(&s.console, &mut value);
            s.console = value
        });
        if let Err(e) = result {
            self.session_error("Console save", e);
        }
    }
    /// The request text of the check at `line`.
    fn check_source(&self, service: &str, line: Option<usize>) -> Option<String> {
        services_kernel::document::request_at(
            self.docs.get(service)?,
            line?,
            self.response_regions
                .get(service)
                .map(Vec::as_slice)
                .unwrap_or(&[]),
        )
        .filter(|block| Some(block.start) == line)
        .map(|block| block.text)
    }
    /// The Kernel hears what the checks of the repeating runs of `service` are now: an edit
    /// ends a repeat.
    pub(super) fn observe_repeats(&self, service: &str) {
        let repeating: Vec<(String, Option<String>)> = self
            .runs
            .iter()
            .filter(|(_, run)| run.view.service == service && run.view.repeat.is_some())
            .map(|(key, run)| (key.clone(), self.check_source(service, self.run_line(run))))
            .collect();
        for (key, source) in repeating {
            self.kernel.check_changed(&key, source.as_deref());
        }
    }
    /// What the run `key` shows under its check now.
    pub(super) fn show_response(
        &mut self,
        key: &str,
        service: &str,
        status: &str,
        body: &str,
        columns: Vec<(String, String)>,
    ) {
        let Some(line) = self.runs.get(key).and_then(|run| self.run_line(run)) else {
            return;
        };
        let editor = self.console_editor(service);
        let doc = self.docs.entry(service.into()).or_default();
        let preserve_after = services_kernel::document::check_lines(
            doc,
            self.response_regions
                .get(service)
                .map(Vec::as_slice)
                .unwrap_or(&[]),
        )
        .into_iter()
        .find(|next| *next > line)
        .map(|next| {
            doc.split_inclusive('\n')
                .take(next)
                .map(str::len)
                .sum::<usize>()
                - 1
        });
        let old_doc = doc.clone();
        *doc = services_kernel::document::replace_response(
            doc,
            line,
            status,
            body,
            columns,
            self.response_regions.entry(service.into()).or_default(),
        );
        services_kernel::document::identity::remap_text(
            &old_doc,
            doc,
            self.checks.entry(service.into()).or_default(),
        );
        // Move the anchors now: Yew may batch several replies into one render.
        editor.with(|session| self.sync_console_session(service, session, preserve_after));
        self.persist_console();
    }
    /// What completion in the editor of `service` reads besides hints: its module's status
    /// (with its details) and how many variable resolutions the module got; a change reopens
    /// hints.
    fn editor_discovery(&self, service: &str) -> Value {
        let resolutions = services_kernel::modules::registry::with(|modules| {
            modules.get(service).map_or(0, |module| module.resolutions)
        });
        let Some(status) = services_kernel::modules::registry::status(service) else {
            return if resolutions == 0 {
                Value::Null
            } else {
                json!({"module":service,"resolutions":resolutions})
            };
        };
        let mut value = json!({"module":service,"status":status.text,"indicator":status.indicator,
            "resolutions":resolutions});
        if let (Some(value), Some(detail)) = (value.as_object_mut(), status.detail.as_object()) {
            value.extend(detail.clone());
        }
        value
    }
    pub(super) fn template_entries(&self, service: &str) -> BTreeMap<String, String> {
        self.templates.get(service).cloned().unwrap_or_default()
    }
    pub(super) fn completions(&self, service: &str) -> Vec<(String, String)> {
        let mut hints = self.vars.clone();
        if !services_kernel::modules::services::contains(service) {
            return hints;
        }
        hints.push(("#check".into(), "New check block".into()));
        for marker in services_kernel::requests::markers(service) {
            hints.push((format!("#{marker}"), String::new()));
        }
        if let Some(text) = self.hints.get(service) {
            hints.extend(services_kernel::editor::saved_hints(text));
        }
        let mut seen = BTreeSet::new();
        hints.retain(|hint| seen.insert(hint.clone()));
        hints
    }
    pub(super) fn console_tabs(&self, ctx: &Context<Self>) -> Html {
        html! {for self.console_order.iter().map(|s|{
            let select=s.clone();let drag=s.clone();let drop=s.clone();
            let (state,title)=self.service_indicator(s);
            html!{<button data-testid={format!("console-service-{s}")} draggable="true" ondragstart={ctx.link().callback(move |_|Msg::Drag(drag.clone()))} ondragover={Callback::from(|e:DragEvent|e.prevent_default())} ondrop={ctx.link().callback(move|e:DragEvent|{e.prevent_default();Msg::Drop(drop.clone())})} class={classes!("tab-button",if &self.console_service==s{"active"}else{""})} onclick={ctx.link().callback(move |_|Msg::ConsoleService(select.clone()))}>{services_kernel::modules::services::label(s)}{indicator(state,title)}</button>}
        })}
    }
    /// Import and Export stay red while their failure is in the banner.
    /// The Transfer toggle's light: red while the service's last Export or Import failed.
    pub(super) fn transfer_indicator(
        &self,
        service: &str,
    ) -> services_widgets::indicator::Indicator {
        let failed = self
            .transfers
            .get(service)
            .is_some_and(|status| status.failed);
        if failed {
            services_widgets::indicator::Indicator::Error
        } else {
            services_widgets::indicator::Indicator::Ready
        }
    }
    pub(super) fn view_console(&self, ctx: &Context<Self>) -> Html {
        let service = &self.console_service;
        let edited_service = service.clone();
        let (run, check_discovery) = self.selected_status(service);
        let state = self
            .active_console_line(service)
            .map(|line| self.check_state(service, line));
        let busy = self.busy_reasons();
        let (status, debug, running, started) = if let Some(discovery) = check_discovery {
            (
                discovery.text.clone(),
                discovery.debug.clone(),
                discovery.request.is_some(),
                Some(discovery.started),
            )
        } else {
            (
                run.map(|Run { view: run, .. }| {
                    let repeat = run
                        .repeat
                        .as_ref()
                        .filter(|_| !run.running)
                        .map(|repeat| {
                            format!(
                                " · Repeat: run {} done, next in {}",
                                repeat.count,
                                interval(repeat.every)
                            )
                        })
                        .unwrap_or_default();
                    format!(
                        "Request · {}{repeat} · {}",
                        if run.stopping && run.running {
                            "Stopping…"
                        } else {
                            &run.status
                        },
                        run.title
                    )
                })
                .unwrap_or_default(),
                run.map(|run| run.view.debug.clone())
                    .unwrap_or(json!({"note":"No debug info yet - run a request first."})),
                run.is_some_and(Run::active),
                run.map(|run| run.view.started),
            )
        };
        let checks: Vec<_> = self
            .checks
            .get(service)
            .into_iter()
            .flatten()
            .map(|check| services_widgets::check_rail::Item {
                line: check.line,
                title: check.title.clone(),
                state: self.check_indicator(service, check.line),
            })
            .collect();
        let active = self.active_console_line(service);
        let layout = self.sessions.borrow().library.snapshot().layout.clone();
        html! {<div class="console-editors">
            <div class={classes!("console-workspace",self.side.is_some().then_some("has-tools"))}>
                <div class="console-top-bar">
                    <div id="tab-strip" class="console-tab-strip">
                        {self.console_tabs(ctx)}
                    </div>
                </div>
                <services_widgets::check_rail::CheckRail items={checks} {active}
                    width={layout.check_menu_width} onwidth={ctx.link().callback(Msg::CheckMenuWidth)}
                    onselect={ctx.link().callback(Msg::Check)} onerror={ctx.link().callback(Msg::EditorError)}/>
                <div class="console-workspace-body">
                    <div class="console-editor-host">
                        <editor::View key={format!("console:{service}")} handle={self.console_editor(service)}
                            on={ctx.link().callback(move |msg|Msg::ConsoleEditor(edited_service.clone(),msg))}
                            testid="console-editor" label={format!("Console · {}",services_kernel::modules::services::label(service))}/>
                    </div>
                </div>
                <div class="console-status-row">
                    <div class="console-status-region">
                        <ConsoleStatus key={format!("console:{service}:{}",started.unwrap_or(0.0))}
                            text={status} {debug} {running} {started} {state} onstop={ctx.link().callback(|_|Msg::Stop)}/>
                    </div>
                </div>
                <div class="console-right-pane" hidden={self.side.is_none()}>
                    if let Some(id)=self.side.clone() {
                        <services_widgets::resizer::PaneResizer key={id.clone()} panel={id.clone()} minimum={320}
                            width={layout.panel_widths.get(&id).copied()}
                            onwidth={{let id=id.clone(); ctx.link().callback(move |width|Msg::PanelWidth(id.clone(),width))}}
                            onerror={ctx.link().callback(Msg::EditorError)}/>
                    }
                    <aside class="console-tool-panel" data-testid="console-tool-panel" hidden={self.side.is_none()} aria-label="Console tools">
                        {self.view_side(ctx)}
                    </aside>
                    <div class="console-status-region console-right-status" hidden={self.side.is_none()}>
                        {self.view_side_status(ctx)}
                    </div>
                </div>
            <div class="console-session-name" data-testid="console-session-name" data-session-id={self.sessions.borrow().library.current.to_string()} data-storage-busy={self.sessions.borrow().saving().to_string()} data-busy={(!busy.is_empty()).to_string()} data-busy-reason={busy.join(", ")} title={self.sessions.borrow().library.name().to_owned()}>{self.sessions.borrow().library.name()}</div>
            <nav class="console-toolbar" aria-label="Console tools">
                {self.view_toolbar(ctx,service)}
            </nav>
            </div>
        </div>}
    }
}
/// A repeat's time the way `Repeat:` takes it: 5s, 2m or 1h.
fn interval(ms: u32) -> String {
    match ms / 1000 {
        seconds if seconds.is_multiple_of(3600) => format!("{}h", seconds / 3600),
        seconds if seconds.is_multiple_of(60) => format!("{}m", seconds / 60),
        seconds => format!("{seconds}s"),
    }
}
