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
        wait
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
            let check = repeat.message["history"]["checkId"].clone();
            let owner = check.as_str().unwrap_or_default().to_owned();
            self.transport.request(
                json!({"service":"history","op":"commit","checkId":check}),
                false,
                ctx.link()
                    .callback(move |reply| Msg::RepeatSaved(owner.clone(), reply)),
            );
        }
    }
    /// A save to History ended: its panel lists the check again, a failure is a notice.
    pub(super) fn repeat_saved(&mut self, ctx: &Context<Self>, check: &str, reply: Reply) {
        let error = match reply {
            Reply::Result(value) if value["saved"] == true => None,
            Reply::Result(value) if value["empty"] == true || value["deferred"] == true => return,
            Reply::Result(value) | Reply::Exit(value) | Reply::Error(value) => Some(
                value["error"]
                    .as_str()
                    .unwrap_or("History could not be saved")
                    .to_owned(),
            ),
            Reply::Data(_) => return,
        };
        if let Some(error) = error {
            self.notice = Some(Notice::new(format!("History · ERROR · {error}")));
        } else if !check.is_empty() {
            let effects =
                crate::kernel::with(|modules| modules.reply("history", check, Ok(json!({}))));
            self.kernel_effects(ctx, effects);
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
                crate::document::columns_of(&run.result),
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
        let resolutions = crate::kernel::with(|modules| {
            modules.get(service).map_or(0, |module| module.resolutions)
        });
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
        let mut hints = self.vars.clone();
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
    pub(super) fn view_console(&self, ctx: &Context<Self>) -> Html {
        let service = &self.console_service;
        let edited_service = service.clone();
        let (run, check_discovery) = self.selected_status(service);
        let state = self
            .active_console_line(service)
            .map(|line| self.check_state(service, line));
        let supported = console::transfers(service);
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
            <div class={classes!("console-workspace",self.side.is_some().then_some("has-tools"))}>
                <div class="console-top-bar">
                    <div id="tab-strip" class="console-tab-strip">
                        {self.console_tabs(ctx)}
                    </div>
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
                <div class="console-right-pane" hidden={self.side.is_none()}>
                    if let Some(id)=self.side.clone() {
                        <crate::browser::pane_resizer::PaneResizer key={id.clone()} panel={id.clone()} minimum={320}
                            width={layout.panel_widths.get(&id).copied()}
                            onwidth={{let id=id.clone(); ctx.link().callback(move |width|Msg::PanelWidth(id.clone(),width))}}
                            onerror={ctx.link().callback(Msg::EditorError)}/>
                    }
                    <aside class="console-tool-panel" data-testid="console-tool-panel" hidden={self.side.is_none()} aria-label="Console tools">
                        {self.view_side(ctx)}
                    </aside>
                    <div class="console-status-region console-right-status" hidden={self.side.is_none()}>
                        {self.view_side_status()}
                    </div>
                </div>
            <div class="console-session-name" data-testid="console-session-name" data-session-id={self.sessions.borrow().library.current.to_string()} data-storage-busy={self.sessions.borrow().saving().to_string()} data-busy={(!busy.is_empty()).to_string()} data-busy-reason={busy.join(", ")} title={self.sessions.borrow().library.name().to_owned()}>{self.sessions.borrow().library.name()}</div>
            <nav class="console-toolbar" aria-label="Console tools">
                if supported{<button class="console-import-toggle" data-testid="console-export" onclick={ctx.link().callback(|_|Msg::Export)}>{"Export"}{indicator(self.transfer_indicator(service,Transfer::Export),"Export status".into())}</button><button class="console-import-toggle" data-testid="console-import" onclick={ctx.link().callback(|_|Msg::Import)}>{"Import"}{indicator(self.transfer_indicator(service,Transfer::Import),"Import status".into())}</button>}
                {self.view_panel_toggles(ctx)}
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
