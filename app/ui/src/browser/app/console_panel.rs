//! The console panel of [`App`]: check runs and their status, execution and responses,
//! discovery, completions and the console view.
use super::*;

impl App {
    /// The console editor of `service`, showing its document from the first use.
    pub(super) fn console_editor(&self, service: &str) -> Handle {
        let key = format!("console:{service}");
        if let Some(editor) = self.editors.borrow().get(&key) {
            return editor.clone();
        }
        let setup = Setup {
            mode: format!("console-{service}"),
            service: service.into(),
            console: true,
            check_scope: format!("{}:{service}", self.workspace_id),
            ..Setup::default()
        };
        let text = self.docs.get(service).map_or("", String::as_str);
        let editor = editor::kept(&format!("{}:{key}", self.session_id), setup, text);
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
    pub(super) fn hints_editor(&self, service: &str) -> Handle {
        let key = format!("hints:{service}");
        if let Some(editor) = self.editors.borrow().get(&key) {
            return editor.clone();
        }
        let setup = Setup {
            mode: "headers".into(),
            service: "hints".into(),
            field: service.into(),
            ..Setup::default()
        };
        let editor = editor::kept(
            &format!("{}:{key}", self.session_id),
            setup,
            &self.hints_text(service),
        );
        self.editors.borrow_mut().insert(key, editor.clone());
        editor
    }
    fn hints_text(&self, service: &str) -> String {
        self.hints_drafts
            .get(service)
            .or_else(|| self.hints.get(service))
            .cloned()
            .unwrap_or_default()
    }
    fn export_editor(&self, text: &str) -> Handle {
        self.export_editor
            .borrow_mut()
            .get_or_insert_with(|| {
                let setup = Setup {
                    mode: "shell".into(),
                    ..Setup::default()
                };
                Handle::new(setup, text)
            })
            .clone()
    }
    /// The anchor of `line` in the console editor of `service`, which follows the line
    /// through edits.
    pub(super) fn anchor(&self, service: &str, line: usize) -> Option<Anchor> {
        self.console_editor(service)
            .read(|session| session.anchor(line))
    }
    /// The line of the `anchor` of a run of `owner` (`console:{service}`).
    pub(super) fn anchor_line(&self, owner: &str, anchor: Anchor) -> Option<usize> {
        self.console_editor(owner.trim_start_matches("console:"))
            .read(|session| session.anchor_line(anchor))
    }
    fn console_editor_stale(&self, service: &str, editor: &Handle) -> bool {
        editor.read(|session| {
            !session.pending()
                && (*session.source() != *self.docs.get(service).map_or("", String::as_str)
                    || session.regions() != self.regions(service)
                    || session.checks() != self.checks.get(service).map_or(&[][..], Vec::as_slice))
        })
    }
    fn regions(&self, service: &str) -> &[crate::document::ResponseRegion] {
        self.response_regions
            .get(service)
            .map_or(&[], Vec::as_slice)
    }
    /// Shows the document of `service` with its responses and checks.
    fn sync_console_editor(&self, service: &str, session: &mut crate::editor::session::Session) {
        self.sync_console_session(service, session, None);
    }
    fn sync_console_session(
        &self,
        service: &str,
        session: &mut crate::editor::session::Session,
        preserve_after: Option<usize>,
    ) {
        session.sync(
            self.docs.get(service).map_or("", String::as_str),
            self.regions(service),
            self.checks.get(service).map_or(&[], Vec::as_slice),
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
            } else if let Some(service) = key.strip_prefix("hints:") {
                editor.set_text(&self.hints_text(service), &[]);
            }
        }
        match &self.export_text {
            Some(text) => self.export_editor(text).set_text(text, &[]),
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
                EditorEvent::Edited(edit) => Msg::ConsoleEdited(service.into(), edit),
                EditorEvent::EditError(error) => Msg::EditorError(error),
                EditorEvent::Run { source, line } => Msg::ConsoleRun(source, line),
                EditorEvent::Discover => Msg::Discover,
                EditorEvent::Hints(check, pos) => Msg::KernelHints(check, pos),
                EditorEvent::Viewport(viewport) => Msg::Scroll(service.into(), viewport),
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
        let owner = format!("console:{service}");
        self.runs
            .iter()
            .filter(|(_, run)| {
                run.editor == owner
                    && line.is_some()
                    && (if let Some(anchor) = run.anchor {
                        self.anchor_line(&owner, anchor)
                    } else {
                        run.line
                    }) == line
            })
            .max_by(|(_, a), (_, b)| a.started.total_cmp(&b.started))
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
            || discovery.is_none_or(|status| run.is_some_and(|run| run.started >= status.started))
        {
            (run, None)
        } else {
            (None, discovery)
        }
    }
    pub(super) fn selected_status(&self, service: &str) -> (Option<&Run>, Option<&ModuleStatus>) {
        self.check_status(service, self.active_console_line(service))
    }
    pub(super) fn check_state(&self, service: &str, line: usize) -> crate::activity::Indicator {
        let (run, discovery) = self.check_status(service, Some(line));
        if let Some(run) = run {
            crate::activity::Indicator::status(&run.status, run.active())
        } else if let Some(status) = discovery {
            crate::activity::Indicator::status(&status.text, status.request.is_some())
        } else {
            crate::activity::Indicator::Ready
        }
    }
    pub(super) fn check_indicator(&self, service: &str, line: usize) -> crate::activity::Indicator {
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
        use crate::activity::sooner;
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
                let state = Indicator::status(&run.status, run.running);
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
        let hints = !self.hidden && self.console_panel == Some(ConsolePanel::Hints);
        for (owner, status) in &mut self.panel_status {
            if hints && *owner == service {
                wait = sooner(wait, status.observe(now));
            } else {
                status.hide();
            }
        }
        if !self.hidden && self.console_panel == Some(ConsolePanel::Sessions) {
            wait = sooner(wait, self.session_status.observe(now));
        } else {
            self.session_status.hide();
        }
        wait
    }
    pub(super) fn hints_indicator(&self, service: &str) -> crate::activity::Indicator {
        self.panel_status
            .get(service)
            .map(|status| {
                status.notification(!self.hidden && self.console_panel == Some(ConsolePanel::Hints))
            })
            .unwrap_or_default()
    }
    /// Where the hints of `service` come from, with Reset for the user's own hints.
    pub(super) fn view_hints_origin(&self, ctx: &Context<Self>, service: &str) -> Html {
        let Some(origin) = self.hints_origins.get(service) else {
            return Html::default();
        };
        let source = origin["source"].as_str();
        let (label, reset) = match (origin["origin"].as_str(), source) {
            (Some("local"), Some(source)) => (
                format!("Yours, over source {source}"),
                Some(format!("Reset to {source}")),
            ),
            (Some("local"), None) => ("Yours".to_owned(), Some("Reset to the module's".to_owned())),
            (Some("source"), Some(source)) => (format!("From source {source}"), None),
            _ => return Html::default(),
        };
        html! {
            <span class="console-panel-origin" data-testid="hints-origin">
                <span class="settings-origin" data-testid="hints-origin-label">{label}</span>
                if let Some(reset) = reset {
                    <button type="button" class="tab-button" data-testid="hints-reset"
                        disabled={self.hints_saving.contains(service)}
                        onclick={ctx.link().callback(|_|Msg::ResetHints)}>{reset}</button>
                }
            </span>
        }
    }
    pub(super) fn service_indicator(&self, service: &str) -> (crate::activity::Indicator, String) {
        let source = self.docs.get(service).map(String::as_str).unwrap_or("");
        let regions = self
            .response_regions
            .get(service)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        crate::activity::summary(
            crate::document::check_lines(source, regions)
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
            signal: crate::activity::Signal::default(),
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
    pub(super) fn execute_console(
        &mut self,
        ctx: &Context<Self>,
        service: &str,
        params: Value,
        line: usize,
        session: Option<String>,
        options: console::RunOptions,
    ) -> Option<String> {
        if self.hidden {
            return None;
        }
        if self.cancel_duplicate_check(ctx, service, line) {
            return None;
        }
        self.run_serial += 1;
        let key = format!("console:{service}:{}", self.run_serial);
        let title = self
            .docs
            .get(service)
            .and_then(|source| source.lines().nth(line))
            .unwrap_or("#check")
            .to_owned();
        // Completed runs are replaced only for this check; concurrent sibling
        // checks retain their own reply routing and response anchors.
        self.prune_console_runs(service, line);
        self.execute(ctx, service, &key, params, line, &options);
        if let Some(run) = self.runs.get_mut(&key) {
            run.agent_session = session;
            run.title = title;
        }
        Some(key)
    }
    /// Shift+Enter on a check that is running stops it; one waiting to repeat stops repeating.
    pub(super) fn cancel_duplicate_check(
        &mut self,
        ctx: &Context<Self>,
        service: &str,
        line: usize,
    ) -> bool {
        let Some((key, running)) = self
            .console_run_entry(service, Some(line))
            .filter(|(_, run)| run.active())
            .map(|(key, run)| (key.to_owned(), run.running))
        else {
            return false;
        };
        if !running {
            self.finish_repeat(ctx, &key, "Repeat stopped");
            return true;
        }
        let message = "Repeated run rejected: this check was already running. Cancellation was requested; no new request was started.";
        if self.kernel_running(&key) {
            if let Some(run) = self.runs.get_mut(&key) {
                run.stopping = true;
                run.debug["duplicateRun"] = json!({
                    "error":message,"phase":"duplicate-run","cancellationRequested":true
                });
                run.duplicate_error = Some(message.to_owned());
            }
            self.kernel_stop(ctx, &key);
            return true;
        }
        let run = self.runs.get_mut(&key).unwrap();
        let stopped = run
            .stream
            .ok_or_else(|| "Running request has no cancellation handle".to_owned())
            .and_then(|id| self.transport.stop(id));
        let message = match stopped {
            Ok(()) => {
                run.stopping = true;
                run.debug["state"] = json!("stopping");
                message.to_owned()
            }
            Err(error) => {
                run.stopping = false;
                let message = format!(
                    "Repeated run rejected: this check was already running. Cancellation failed: {error}"
                );
                run.status = format!("ERROR · {message}");
                message
            }
        };
        run.debug["duplicateRun"] = json!({
            "error":message,"phase":"duplicate-run","cancellationRequested":run.stopping
        });
        run.duplicate_error = Some(message);
        true
    }
    pub(super) fn prune_console_runs(&mut self, service: &str, line: usize) {
        let owner = format!("console:{service}");
        let editor = self.console_editor(service);
        self.runs.retain(|_, run| {
            run.editor != owner
                || run.active()
                || (if let Some(anchor) = run.anchor {
                    editor.read(|session| session.anchor_line(anchor))
                } else {
                    run.line
                })
                .is_some_and(|previous| previous != line)
        });
    }
    pub(super) fn persist_console(&mut self) {
        for (service, source) in &self.docs {
            crate::checks::normalize(
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
            crate::storage::keep_unloaded(&s.console, &mut value);
            s.console = value
        });
        if let Err(e) = result {
            self.session_error("Console save", e);
        }
    }
    pub(super) fn execute(
        &mut self,
        ctx: &Context<Self>,
        service: &str,
        key: &str,
        params: Value,
        line: usize,
        options: &console::RunOptions,
    ) {
        if self.runs.get(key).is_some_and(|r| r.running) {
            self.notice = Some(Notice::new(
                "This request is already running. Stop the current stream first.",
            ));
            return;
        }
        let started = js_sys::Date::now();
        self.runs.insert(
            key.into(),
            Run {
                editor: format!("console:{service}"),
                running: true,
                status: "Running…".into(),
                line: Some(line),
                started,
                debug: run_debug(service, &params, started),
                anchor: self.anchor(service, line),
                ..Default::default()
            },
        );
        let mut message = json!({"service":service,"params":params});
        if let Some(check) = self
            .checks
            .get(service)
            .and_then(|checks| checks.iter().find(|check| check.line == line))
            && let Some(source) = self.docs.get(service)
            && let Some(block) = crate::document::request_at(
                source,
                line,
                self.response_regions
                    .get(service)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
            )
        {
            message["history"] = json!({"checkId":check.id,"sessionId":self.workspace_id,
                "title":source.lines().nth(line).unwrap_or("#check"),"createdAt":check.created_at,
                "source":source.lines().skip(line).take(block.end-line).collect::<Vec<_>>().join("\n")});
            if options.repeat.is_some() {
                message["history"]["defer"] = json!(true);
                message["history"]["startedAt"] = json!(started as u64);
            }
        }
        if let Some(every) = options.repeat {
            let source = self.check_source(service, Some(line)).unwrap_or_default();
            self.runs.get_mut(key).unwrap().repeat = Some(Repeat {
                every,
                until_changed: options.until_changed,
                count: 0,
                changed: false,
                timer: None,
                message: message.clone(),
                last: None,
                source,
                edited: false,
            });
        }
        self.send_run(ctx, service, key, message);
    }
    fn send_run(&mut self, ctx: &Context<Self>, service: &str, key: &str, message: Value) {
        self.kernel_send(ctx, service, key, message);
    }
    /// The request text of the check at `line`.
    fn check_source(&self, service: &str, line: Option<usize>) -> Option<String> {
        crate::document::request_at(
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
    /// Whether the check of a repeating run is gone or its request text changed.
    fn repeat_edited(&self, run: &Run) -> bool {
        let Some(repeat) = &run.repeat else {
            return false;
        };
        let service = run.editor.trim_start_matches("console:");
        let line = run
            .anchor
            .and_then(|anchor| self.anchor_line(&run.editor, anchor));
        repeat.edited || self.check_source(service, line).as_ref() != Some(&repeat.source)
    }
    /// An edit of a repeating check ends its repeat; a run in flight finishes first.
    pub(super) fn stop_edited_repeats(&mut self, ctx: &Context<Self>, service: &str) {
        let owner = format!("console:{service}");
        let edited: Vec<_> = self
            .runs
            .iter()
            .filter(|(_, run)| run.editor == owner && self.repeat_edited(run))
            .map(|(key, run)| (key.clone(), run.running))
            .collect();
        for (key, running) in edited {
            if running {
                if let Some(repeat) = self.runs.get_mut(&key).and_then(|run| run.repeat.as_mut()) {
                    repeat.edited = true;
                }
            } else {
                self.finish_repeat(ctx, &key, "Check edited");
            }
        }
    }
    /// After a run of a repeating check: stop, or wait and run again.
    pub(super) fn continue_repeat(&mut self, ctx: &Context<Self>, key: &str) {
        let hidden = self.hidden;
        let edited = self
            .runs
            .get(key)
            .is_some_and(|run| self.repeat_edited(run));
        let Some(run) = self.runs.get_mut(key) else {
            return;
        };
        let Some(repeat) = run.repeat.as_mut() else {
            return;
        };
        if run.running || repeat.timer.is_some() {
            return;
        }
        let stop = if run.stopping || hidden || run.duplicate_error.is_some() {
            Some("Repeat stopped")
        } else if edited {
            Some("Check edited")
        } else if repeat.until_changed && repeat.changed {
            Some("Response changed")
        } else {
            None
        };
        if let Some(reason) = stop {
            self.finish_repeat(ctx, key, reason);
            return;
        }
        let link = ctx.link().clone();
        let next = key.to_owned();
        let callback = Closure::once_into_js(move || link.send_message(Msg::RepeatRun(next)));
        match window().set_timeout_with_callback_and_timeout_and_arguments_0(
            callback.unchecked_ref(),
            repeat.every as i32,
        ) {
            Ok(id) => repeat.timer = Some(id),
            Err(e) => {
                let reason = format!("Repeat failed: {}", error(e));
                self.finish_repeat(ctx, key, &reason);
            }
        }
    }
    /// The next run of a repeating check, with the request of its first run.
    pub(super) fn repeat_run(&mut self, ctx: &Context<Self>, key: &str) {
        let hidden = self.hidden;
        let edited = self
            .runs
            .get(key)
            .is_some_and(|run| self.repeat_edited(run));
        let line = self
            .runs
            .get(key)
            .and_then(|run| self.anchor_line(&run.editor, run.anchor?));
        let Some(run) = self.runs.get_mut(key) else {
            return;
        };
        let Some(repeat) = run.repeat.as_mut() else {
            return;
        };
        repeat.timer = None;
        let message = repeat.message.clone();
        if hidden || line.is_none() {
            self.finish_repeat(ctx, key, "Repeat stopped");
            return;
        }
        if edited {
            self.finish_repeat(ctx, key, "Check edited");
            return;
        }
        let started = js_sys::Date::now();
        run.line = line;
        run.running = true;
        run.stopping = false;
        run.status = "Running…".into();
        run.started = started;
        run.result = Value::Null;
        run.stdout.clear();
        run.stderr.clear();
        run.debug = json!({"operation":"request","service":message["service"],
            "params":message["params"],"started":started,"state":"running"});
        let service = run.editor.trim_start_matches("console:").to_owned();
        self.send_run(ctx, &service, key, message);
    }
    /// Ends a repeat that is not running, and saves its last run to History.
    pub(super) fn finish_repeat(&mut self, ctx: &Context<Self>, key: &str, reason: &str) {
        let Some(run) = self.runs.get_mut(key) else {
            return;
        };
        let Some(mut repeat) = run.repeat.take() else {
            return;
        };
        if let Some(id) = repeat.timer.take() {
            window().clear_timeout_with_handle(id);
        }
        let runs = if repeat.count == 1 { "run" } else { "runs" };
        run.status = format!("{} · {reason} after {} {runs}", run.status, repeat.count);
        if repeat.count > 0 && repeat.message.get("history").is_some() {
            self.transport.request(
                json!({"service":"history","op":"commit","checkId":repeat.message["history"]["checkId"]}),
                false,
                ctx.link().callback(Msg::RepeatSaved),
            );
        }
    }
    pub(super) fn repeat_saved(&mut self, reply: Reply) {
        let error = match reply {
            Reply::Result(value) if value["saved"] == true => None,
            Reply::Result(value) if value["empty"] == true => return,
            Reply::Result(value) | Reply::Exit(value) | Reply::Error(value) => Some(
                value["error"]
                    .as_str()
                    .unwrap_or("History could not be saved")
                    .to_owned(),
            ),
            Reply::Data(_) => return,
        };
        self.history_revision = self.history_revision.wrapping_add(1);
        if let Some(error) = error {
            self.history_error = Some(error);
            self.history_error_seen =
                self.console_panel == Some(ConsolePanel::History) && !self.hidden;
        }
    }
    pub(super) fn client_error(
        &mut self,
        ctx: &Context<Self>,
        service: &str,
        message: String,
        line: usize,
    ) {
        if self.cancel_duplicate_check(ctx, service, line) {
            return;
        }
        self.prune_console_runs(service, line);
        self.run_serial += 1;
        let key = format!("console:{service}:{}", self.run_serial);
        self.runs.insert(
            key.clone(),
            Run {
                editor: format!("console:{service}"),
                started: js_sys::Date::now(),
                line: Some(line),
                title: self
                    .docs
                    .get(service)
                    .and_then(|source| source.lines().nth(line))
                    .unwrap_or("")
                    .into(),
                anchor: self.anchor(service, line),
                ..Default::default()
            },
        );
        self.receive(
            &key,
            service,
            Reply::Error(json!({"error":message,"phase":"validation"})),
        );
    }
    pub(super) fn receive(&mut self, key: &str, service: &str, reply: Reply) {
        if let Reply::Result(value) | Reply::Exit(value) | Reply::Error(value) = &reply
            && value.get("history").is_some()
            && value["history"]["deferred"] != true
        {
            self.history_revision = self.history_revision.wrapping_add(1);
            if value["history"]["saved"] == false {
                self.history_error = Some(
                    value["history"]["error"]
                        .as_str()
                        .unwrap_or("History could not be saved")
                        .to_owned(),
                );
                self.history_error_seen =
                    self.console_panel == Some(ConsolePanel::History) && !self.hidden;
            }
        }
        let editor = self.console_editor(service);
        let run = self.runs.entry(key.into()).or_default();
        let reply = match (reply, run.duplicate_error.as_ref()) {
            (Reply::Result(value) | Reply::Exit(value) | Reply::Error(value), Some(error)) => {
                Reply::Error(json!({
                    "error":error,"phase":"duplicate-run","cancellationRequested":run.stopping,
                    "request":run.debug,"previousOperation":value
                }))
            }
            (reply, _) => reply,
        };
        match reply {
            Reply::Data(event) => {
                let data = event["data"].as_str().unwrap_or("");
                if event["type"] == "stderr" {
                    run.stderr.push_str(data);
                } else {
                    run.stdout.push_str(data);
                }
            }
            Reply::Result(result) | Reply::Exit(result) => {
                if let Some(repeat) = run.repeat.as_mut() {
                    repeat.count += 1;
                }
                run.running = false;
                run.stream = None;
                run.status = if result["cancelled"] == true {
                    format!(
                        "{} · Stopped · {}",
                        time(),
                        result["warning"]
                            .as_str()
                            .unwrap_or("Request stopped before dispatch")
                    )
                } else {
                    format!("{} · {}", time(), format::status(&result))
                };
                run.debug = result.clone();
                run.result = result;
            }
            Reply::Error(e) => {
                if let Some(repeat) = run.repeat.as_mut() {
                    repeat.count += 1;
                }
                run.running = false;
                run.stream = None;
                run.status = format!("{} · ERROR · {}", time(), err_text(&e));
                run.debug = e.clone();
                run.result = e;
            }
        }
        let anchored_line = run
            .anchor
            .and_then(|anchor| editor.read(|session| session.anchor_line(anchor)));
        let response_line = if run.anchor.is_some() {
            anchored_line
        } else {
            run.line
        };
        if let Some(line) = response_line {
            let body = format::check_response(&run.result, &run.stdout, &run.stderr);
            let outcome = if run.running {
                "Running"
            } else {
                format::outcome(&run.result)
            };
            let mut response_status = format!("{} · {outcome}", time());
            if let Some(repeat) = run.repeat.as_mut() {
                let count = repeat.count + u32::from(run.running);
                response_status.push_str(&format!(" · Run {count}"));
                if !run.running {
                    repeat.changed = repeat.last.as_ref().is_some_and(|last| *last != body);
                    repeat.last = Some(body.clone());
                }
            }
            let doc = self.docs.entry(service.into()).or_default();
            let preserve_after = crate::document::check_lines(
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
            *doc = crate::document::replace_response(
                doc,
                line,
                &response_status,
                &body,
                self.response_regions.entry(service.into()).or_default(),
            );
            crate::checks::remap_text(
                &old_doc,
                doc,
                self.checks.entry(service.into()).or_default(),
            );
            // Move the anchors now: Yew may batch several replies into one render.
            editor.with(|session| self.sync_console_session(service, session, preserve_after));
            self.persist_console();
        }
    }
    /// What completion in the editor of `service` reads besides hints: its module's status
    /// (with its details) and how many variable resolutions the module got; a change reopens
    /// hints.
    fn editor_discovery(&self, service: &str) -> Value {
        let resolutions = self.kernel.resolutions.get(service).copied().unwrap_or(0);
        let Some(status) = crate::kernel::status(service) else {
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
        let mut hints = profiles::hints(&self.vars);
        if !crate::services::contains(service) {
            return hints;
        }
        hints.push(("#check".into(), "New check block".into()));
        for marker in console::markers(service) {
            hints.push((format!("#{marker}"), String::new()));
        }
        if let Some(text) = self.hints.get(service) {
            hints.extend(crate::completion::saved_hints(text));
        }
        let mut seen = BTreeSet::new();
        hints.retain(|hint| seen.insert(hint.clone()));
        hints
    }
    pub(super) fn console_tabs(&self, ctx: &Context<Self>) -> Html {
        html! {for self.console_order.iter().map(|s|{
            let select=s.clone();let drag=s.clone();let drop=s.clone();
            let (state,title)=self.service_indicator(s);
            html!{<button data-testid={format!("console-service-{s}")} draggable="true" ondragstart={ctx.link().callback(move |_|Msg::Drag(drag.clone()))} ondragover={Callback::from(|e:DragEvent|e.prevent_default())} ondrop={ctx.link().callback(move|e:DragEvent|{e.prevent_default();Msg::Drop(drop.clone())})} class={classes!("tab-button",if &self.console_service==s{"active"}else{""})} onclick={ctx.link().callback(move |_|Msg::ConsoleService(select.clone()))}>{crate::services::label(s)}{indicator(state,title)}</button>}
        })}
    }
    pub(super) fn view_hints_status(&self, ctx: &Context<Self>, service: &str) -> Html {
        let status = self.panel_status.get(service).cloned().unwrap_or_default();
        html! {<ConsoleStatus key={service.to_owned()} text={status.text} debug={status.debug}
        running={status.running} started={(status.started>0.0).then_some(status.started)}
        testid="console-hints-status" stop_testid="console-hints-stop"
        onstop={self.hints_timers.contains_key(service).then(||ctx.link().callback(|_|Msg::StopHints))}/>}
    }
    /// Import and Export stay red while their failure is in the banner.
    pub(super) fn transfer_indicator(
        &self,
        service: &str,
        transfer: Transfer,
    ) -> crate::activity::Indicator {
        let failed = self
            .notice
            .as_ref()
            .and_then(|notice| notice.source.as_ref())
            == Some(&(service.to_owned(), transfer));
        if failed {
            crate::activity::Indicator::Error
        } else {
            crate::activity::Indicator::Ready
        }
    }
    pub(super) fn profile_indicator(&self, dictionary: bool) -> Html {
        let (state, title) = self
            .profile_activity
            .get(&dictionary)
            .cloned()
            .unwrap_or_default();
        indicator(state, title)
    }
    pub(super) fn view_console(&self, ctx: &Context<Self>) -> Html {
        let service = &self.console_service;
        let edited_service = service.clone();
        let hints_service = service.clone();
        let (run, check_discovery) = self.selected_status(service);
        let state = self
            .active_console_line(service)
            .map(|line| self.check_state(service, line));
        let supported = console::transfers(service);
        let (status, debug, running, started) = if let Some(discovery) = check_discovery {
            (
                discovery.text.clone(),
                discovery.debug.clone(),
                discovery.request.is_some(),
                Some(discovery.started),
            )
        } else {
            (
                run.map(|run| {
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
                run.map(|run| run.debug.clone())
                    .unwrap_or(json!({"note":"No debug info yet - run a request first."})),
                run.is_some_and(Run::active),
                run.map(|run| run.started),
            )
        };
        let checks: Vec<_> = self
            .checks
            .get(service)
            .into_iter()
            .flatten()
            .map(|check| crate::browser::check_rail::Item {
                line: check.line,
                title: check.title.clone(),
                state: self.check_indicator(service, check.line),
            })
            .collect();
        let active = self.active_console_line(service);
        let layout = self.sessions.borrow().library.snapshot().layout.clone();
        html! {<div class="console-editors">
            <div class={classes!("console-workspace",self.console_panel.is_some().then_some("has-tools"))}>
                <div class="console-top-bar">
                    <div id="tab-strip" class="console-tab-strip">
                        {self.console_tabs(ctx)}
                    </div>
                    if self.console_panel==Some(ConsolePanel::Hints) {
                        <div class="console-panel-heading" data-testid="console-hints-heading">
                            <span class="tab-button active" data-testid="console-hints-title">{format!("{} completion hints",crate::services::label(service))}{indicator(self.hints_indicator(service),"Hints status".into())}</span>
                            {self.view_hints_origin(ctx, service)}
                        </div>
                    }
                    if self.console_panel==Some(ConsolePanel::Sessions) {
                        <div class="console-panel-heading"><span class="tab-button active">{"Sessions"}{indicator(crate::activity::Indicator::Ready,"Sessions".into())}</span></div>
                    }
                    if self.console_panel==Some(ConsolePanel::Settings) {
                        <div class="console-panel-heading">
                            <div class="console-panel-tabs" data-testid="settings-tabs" role="tablist" aria-label="Settings">
                                {for self.settings_tabs().iter().map(|&tab|{
                                    let active=tab==self.visible_settings_tab();
                                    html!{<button type="button" role="tab" aria-selected={active.to_string()} class={classes!("tab-button",active.then_some("active"))}
                                        data-testid={format!("settings-tab-{}",tab.key())} onclick={ctx.link().callback(move|_|Msg::SettingsTab(tab))}>
                                        {tab.label()}{indicator(self.settings_tab_indicator(tab),format!("{} status",tab.label()))}</button>}
                                })}
                            </div>
                        </div>
                    }
                    <div class="console-panel-heading" ref={self.console_panel_header.clone()} data-testid="console-variables-heading"
                        hidden={!matches!(self.console_panel,Some(ConsolePanel::Variables | ConsolePanel::Dictionary | ConsolePanel::Agent | ConsolePanel::Templates | ConsolePanel::History))}></div>
                </div>
                <crate::browser::check_rail::CheckRail items={checks} {active}
                    width={layout.check_menu_width} onwidth={ctx.link().callback(Msg::CheckMenuWidth)}
                    onselect={ctx.link().callback(Msg::Check)} onerror={ctx.link().callback(Msg::EditorError)}/>
                <div class="console-workspace-body">
                    <div class="console-editor-host">
                        <editor::View key={format!("console:{service}")} handle={self.console_editor(service)}
                            on={ctx.link().callback(move |msg|Msg::ConsoleEditor(edited_service.clone(),msg))}
                            testid="console-editor" label={format!("Console · {}",crate::services::label(service))}/>
                    </div>
                </div>
                <div class="console-status-row">
                    <div class="console-status-region">
                        <ConsoleStatus key={format!("console:{service}:{}",started.unwrap_or(0.0))}
                            text={status} {debug} {running} {started} {state} onstop={ctx.link().callback(|_|Msg::Stop)}/>
                    </div>
                </div>
                <div class="console-right-pane" hidden={self.console_panel.is_none()}>
                    if let Some(panel)=self.console_panel {
                        <crate::browser::pane_resizer::PaneResizer key={panel.key()} panel={panel.key()} minimum={panel.minimum()}
                            width={layout.panel_widths.get(panel.key()).copied()}
                            onwidth={ctx.link().callback(move |width|Msg::PanelWidth(panel.key().into(),width))}
                            onerror={ctx.link().callback(Msg::EditorError)}/>
                    }
                    <aside class="console-tool-panel" data-testid="console-tool-panel" hidden={self.console_panel.is_none()} aria-label="Console tools">
                        if self.settings_visited {
                            <section class="console-settings" data-testid="settings-panel" aria-label="Application settings" hidden={self.console_panel!=Some(ConsolePanel::Settings)}>
                                // Every tab stays mounted so its drafts, checks and model catalog survive switching.
                                if self.transport.is_native() {
                                    <div role="tabpanel" data-testid="settings-group-modules" hidden={self.visible_settings_tab()!=SettingsTab::Modules}>
                                    <crate::browser::module_settings::ModuleSettings transport={self.transport.clone()}
                                        check={self.sources_check.clone()} checking={self.sources_checking}
                                        oncheck={ctx.link().callback(Msg::CheckSources)}
                                        app_busy={self.updates.busy()}
                                        app_installed={(self.updates.phase==UpdatePhase::Installed).then(||self.updates.target.as_ref().map(|(slug,_)|slug.clone())).flatten()}
                                        onapp={ctx.link().callback(Msg::InstallUpdate)}
                                        onreload={ctx.link().callback(|_|Msg::ReloadUpdate)}
                                        onstatus={ctx.link().callback(|status|Msg::SettingsStatus(SettingsTab::Modules,status))}
                                        onforget={ctx.link().callback(Msg::ForgetModule)}
                                        onchange={ctx.link().callback(|_|Msg::ModulesChanged)}/>
                                    </div>
                                    <div role="tabpanel" data-testid="settings-group-sources" hidden={self.visible_settings_tab()!=SettingsTab::Sources}>
                                    <crate::browser::sources_settings::SourcesSettings transport={self.transport.clone()}
                                        check={self.sources_check.clone()} checking={self.sources_checking}
                                        oncheck={ctx.link().callback(Msg::CheckSources)}
                                        onapplied={ctx.link().callback(|_|Msg::SourcesApplied)}
                                        onstatus={ctx.link().callback(|status|Msg::SettingsStatus(SettingsTab::Sources,status))}/>
                                    </div>
                                }
                                <div role="tabpanel" data-testid="settings-group-agent" hidden={self.visible_settings_tab()!=SettingsTab::Agent}>
                                <crate::browser::agent_settings::AgentSettings transport={self.transport.clone()}
                                    onstatus={ctx.link().callback(|status|Msg::SettingsStatus(SettingsTab::Agent,status))}/>
                                </div>
                            </section>
                        }
                        if self.console_panel==Some(ConsolePanel::Sessions) {
                            {self.view_sessions(ctx)}
                        }
                        if self.console_panel==Some(ConsolePanel::Hints) {
                            <div class="console-hints-editor">
                                <editor::View key={format!("hints:{service}")} handle={self.hints_editor(service)}
                                    on={ctx.link().callback(move |msg|Msg::HintsEditor(hints_service.clone(),msg))}
                                    testid="console-hints" label={format!("Hints · {}",crate::services::label(service))}/>
                            </div>
                        }
                        if self.templates_visited {
                            <crate::browser::templates::Templates key={format!("templates:{}", self.templates_epoch)} transport={self.transport.clone()} owner={service.clone()}
                                visible={self.console_panel==Some(ConsolePanel::Templates)} foreground={!self.hidden}
                                header={self.console_panel_header.clone()} footer={self.console_panel_footer.clone()}
                                onchange={ctx.link().callback(Msg::TemplatesChanged)} onactivity={ctx.link().callback(Msg::TemplatesActivity)}/>
                        }
                        if self.history_visited {
                            <crate::browser::history::History transport={self.transport.clone()} service={service.clone()}
                                check_id={self.active_console_line(service).and_then(|line|self.checks.get(service)?.iter().find(|check|check.line==line)).map(|check|check.id.clone()).unwrap_or_default()}
                                visible={self.console_panel==Some(ConsolePanel::History)} foreground={!self.hidden}
                                revision={self.history_revision} error={self.history_error.clone()}
                                header={self.console_panel_header.clone()} footer={self.console_panel_footer.clone()}
                                onactivity={ctx.link().callback(Msg::HistoryActivity)}/>
                        }
                        if self.variables_visited {
                            <ConsoleVariables key="variables" transport={self.transport.clone()} owner={service.clone()} visible={self.console_panel==Some(ConsolePanel::Variables)}
                                foreground={!self.hidden}
                                suspended={self.hidden}
                                header={self.console_panel_header.clone()}
                                footer={self.console_panel_footer.clone()}
                                stop_generation={self.agent_stop_generation}
                                onvars={ctx.link().callback(Msg::PanelVars)}
                                onactivity={ctx.link().callback(|activity|Msg::ProfileActivity(false,activity))}/>
                        }
                        if self.dictionary_visited {
                            <ConsoleVariables key="dictionary" dictionary={true} transport={self.transport.clone()} owner={service.clone()} visible={self.console_panel==Some(ConsolePanel::Dictionary)}
                                foreground={!self.hidden}
                                suspended={self.hidden}
                                header={self.console_panel_header.clone()}
                                footer={self.console_panel_footer.clone()}
                                stop_generation={self.agent_stop_generation}
                                onvars={ctx.link().callback(Msg::PanelVars)}
                                onactivity={ctx.link().callback(|activity|Msg::ProfileActivity(true,activity))}/>
                        }
                        if self.agent_visited {
                            <AgentPanel transport={self.transport.clone()} owner={service.clone()} visible={self.console_panel==Some(ConsolePanel::Agent)}
                                console_session={self.sessions.borrow().library.current}
                                session_names={self.session_names()}
                                foreground={!self.hidden}
                                stop_generation={self.agent_stop_generation} onactivity={ctx.link().callback(Msg::AgentActivity)}
                                header={self.console_panel_header.clone()}
                                footer={self.console_panel_footer.clone()}
                                approval={self.agent_run.as_ref().filter(|run|!run.started).map(|run|run.approval.clone())}
                                stop_error={self.agent_stop_error.clone()}
                                onworkspace={ctx.link().callback(Msg::AgentTool)} onfinished={ctx.link().callback(Msg::AgentFinished)}
                                onapprove={ctx.link().callback(Msg::AgentApprove)}/>
                        }
                    </aside>
                    <div class="console-status-region console-right-status" hidden={self.console_panel.is_none()}>
                        if self.console_panel==Some(ConsolePanel::Settings) {
                            if let Some(status)=self.settings_operation() {
                                <ConsoleStatus testid="settings-update-status" text={status.text.clone()}
                                    debug={status.debug.clone()} running={status.running}
                                    started={Some(status.started)} state={status.indicator()}/>
                            } else {
                                <ConsoleStatus testid="settings-update-status" text={self.updates.text().to_owned()}
                                    debug={self.updates.status.debug.clone()} running={self.updates.status.running}
                                    started={(self.updates.status.started>0.0).then_some(self.updates.status.started)}
                                    state={self.updates.indicator()}/>
                            }
                        }
                        if self.console_panel==Some(ConsolePanel::Hints) {
                            {self.view_hints_status(ctx,service)}
                        }
                        if self.console_panel==Some(ConsolePanel::Sessions) {
                            {{let status=self.sessions_status();html!{<ConsoleStatus testid="console-sessions-status" text={status.text.clone()} debug={status.debug.clone()} running={false} started={None} state={status.indicator()}/>}}}
                        }
                        <div ref={self.console_panel_footer.clone()} hidden={matches!(self.console_panel,Some(ConsolePanel::Hints|ConsolePanel::Sessions|ConsolePanel::Settings))}></div>
                    </div>
                </div>
            <div class="console-session-name" data-testid="console-session-name" data-session-id={self.sessions.borrow().library.current.to_string()} data-storage-busy={self.sessions.borrow().saving().to_string()} title={self.sessions.borrow().library.name().to_owned()}>{self.sessions.borrow().library.name()}</div>
            <nav class="console-toolbar" aria-label="Console tools">
                <button class={classes!("console-hints-toggle",(self.console_panel==Some(ConsolePanel::Sessions)).then_some("active"))}
                    data-testid="console-sessions-toggle" aria-pressed={(self.console_panel==Some(ConsolePanel::Sessions)).to_string()}
                    onclick={ctx.link().callback(|_|Msg::ToggleSessions)}>{"Sessions"}{indicator(if self.sessions.borrow().saving(){crate::activity::Indicator::Running}else if self.sessions.borrow().save_error().is_some(){crate::activity::Indicator::Error}else{self.session_status.notification(self.console_panel==Some(ConsolePanel::Sessions)&&!self.hidden)},"Sessions".into())}</button>
                <button class={classes!("console-hints-toggle",(self.console_panel==Some(ConsolePanel::Agent)).then_some("active"))} data-testid="console-agent-toggle"
                    aria-pressed={(self.console_panel==Some(ConsolePanel::Agent)).to_string()}
                    onclick={ctx.link().callback(|_|Msg::ToggleAgent)}>{"Agent"}{{
                       let (state,title)=crate::activity::summary(self.agent_activity.iter().map(|activity|activity.state));
                       indicator(state,title)
                    }}</button>
                if supported{<button class="console-import-toggle" data-testid="console-export" onclick={ctx.link().callback(|_|Msg::Export)}>{"Export"}{indicator(self.transfer_indicator(service,Transfer::Export),"Export status".into())}</button><button class="console-import-toggle" data-testid="console-import" onclick={ctx.link().callback(|_|Msg::Import)}>{"Import"}{indicator(self.transfer_indicator(service,Transfer::Import),"Import status".into())}</button>}
                <button class={classes!("console-hints-toggle",(self.console_panel==Some(ConsolePanel::Templates)).then_some("active"))}
                    data-testid="console-templates-toggle" aria-pressed={(self.console_panel==Some(ConsolePanel::Templates)).to_string()}
                    onclick={ctx.link().callback(|_|Msg::ToggleTemplates)}>{"Templates"}{indicator(self.templates_activity,"Templates status".into())}</button>
                <button class={classes!("console-hints-toggle",(self.console_panel==Some(ConsolePanel::History)).then_some("active"))}
                    data-testid="console-history-toggle" aria-pressed={(self.console_panel==Some(ConsolePanel::History)).to_string()}
                    onclick={ctx.link().callback(|_|Msg::ToggleHistory)}>{"History"}{indicator(if self.history_error.is_some() && !self.history_error_seen {
                        crate::activity::Indicator::Error
                    }else{self.history_activity},"History status".into())}</button>
                <button class={classes!("console-hints-toggle",(self.console_panel==Some(ConsolePanel::Hints)).then_some("active"))} data-testid="console-hints-toggle" aria-pressed={(self.console_panel==Some(ConsolePanel::Hints)).to_string()} onclick={ctx.link().callback(|_|Msg::ToggleHints)}>{"Hints"}{indicator(self.hints_indicator(service),"Hints status".into())}</button>
                <button class={classes!("console-hints-toggle",(self.console_panel==Some(ConsolePanel::Variables)).then_some("active"))} data-testid="console-variables-toggle" aria-pressed={(self.console_panel==Some(ConsolePanel::Variables)).to_string()} onclick={ctx.link().callback(|_|Msg::ToggleVariables)}>{"Variables"}{self.profile_indicator(false)}</button>
                <button class={classes!("console-hints-toggle",(self.console_panel==Some(ConsolePanel::Dictionary)).then_some("active"))} data-testid="console-dictionary-toggle" aria-pressed={(self.console_panel==Some(ConsolePanel::Dictionary)).to_string()} onclick={ctx.link().callback(|_|Msg::ToggleDictionary)}>{"Dictionary"}{self.profile_indicator(true)}</button>
                <button class={classes!("console-hints-toggle",(self.console_panel==Some(ConsolePanel::Settings)).then_some("active"))}
                    data-testid="console-settings-toggle" aria-pressed={(self.console_panel==Some(ConsolePanel::Settings)).to_string()}
                    onclick={ctx.link().callback(|_|Msg::ToggleSettings)}>{"Settings"}{indicator(self.settings_notification(),"Settings status".into())}</button>
            </nav>
            </div>
            if let Some(command)=&self.export_text{<div class="console-hints-overlay"><div class="console-hints-panel">
                <div class="console-hints-panel-header">{"Export - copy this command"}</div>
                <editor::View key="export" handle={self.export_editor(command)} on={ctx.link().callback(Msg::ExportEditor)} label="Export"/>
                <div class="console-hints-panel-actions"><button onclick={ctx.link().callback(|_|Msg::CloseExport)}>{"Close"}</button></div>
            </div></div>}
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

/// A running request as Debug shows it; a Kernel driver call shows its request as `params`,
/// so the URL is visible without unfolding.
fn run_debug(service: &str, params: &Value, started: f64) -> Value {
    let mut debug = json!({"operation":"request","service":service,"params":params,
        "started":started,"state":"running"});
    if let Some(driver) = params["driver"].as_str() {
        debug["driver"] = json!(driver);
        debug["params"] = params["request"].clone();
        debug["stream"] = params["stream"].clone();
    }
    debug
}
