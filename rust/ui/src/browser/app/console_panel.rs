//! The console panel of [`App`]: check runs and their status, execution and responses,
//! discovery, completions and the console view.
use super::*;

impl App {
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
                    && (if let Some(anchor) = &run.anchor {
                        crate::browser::editor::anchor_line(&owner, anchor)
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
    ) -> (Option<&Run>, Option<&DiscoveryStatus>) {
        let run = self.console_run(service, line);
        let discovery = self
            .discovery_status
            .get(service)
            .into_iter()
            .chain(self.discovery_history.get(service).into_iter().flatten())
            .filter(|status| line.is_some() && status.check_line(service) == line)
            .max_by(|a, b| a.started.total_cmp(&b.started));
        if run.is_some_and(|run| run.running)
            || discovery.is_none_or(|status| run.is_some_and(|run| run.started >= status.started))
        {
            (run, None)
        } else {
            (None, discovery)
        }
    }
    pub(super) fn selected_status(
        &self,
        service: &str,
    ) -> (Option<&Run>, Option<&DiscoveryStatus>) {
        self.check_status(service, self.active_console_line(service))
    }
    pub(super) fn check_state(&self, service: &str, line: usize) -> crate::activity::Indicator {
        let (run, discovery) = self.check_status(service, Some(line));
        if let Some(run) = run {
            crate::activity::Indicator::status(&run.status, run.running)
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
    pub(super) fn observe_console_status(&mut self) {
        if self.hidden {
            return;
        }
        let service = self.console_service.clone();
        let line = self.active_console_line(&service);
        let (run, discovery) = self.selected_status(&service);
        let run_key = run
            .and_then(|_| self.console_run_entry(&service, line))
            .map(|(key, _)| key.to_owned());
        let discovery_started = discovery.map(|status| status.started);
        if let Some(run) = run_key.and_then(|key| self.runs.get_mut(&key)) {
            run.signal
                .observe(crate::activity::Indicator::status(&run.status, run.running));
        }
        if let Some(started) = discovery_started {
            for status in self
                .discovery_status
                .get_mut(&service)
                .into_iter()
                .chain(
                    self.discovery_history
                        .get_mut(&service)
                        .into_iter()
                        .flatten(),
                )
                .filter(|status| status.started == started && status.check_line(&service) == line)
            {
                status.signal.observe(crate::activity::Indicator::status(
                    &status.text,
                    status.request.is_some(),
                ));
            }
        }
        if self.console_panel == Some(ConsolePanel::Hints)
            && let Some(status) = self.panel_status.get_mut(&service)
        {
            status.observe();
        }
        if self.console_panel == Some(ConsolePanel::Sessions) {
            self.session_status.observe();
        }
    }
    pub(super) fn hints_indicator(&self, service: &str) -> crate::activity::Indicator {
        self.panel_status
            .get(service)
            .map(|status| {
                status.notification(!self.hidden && self.console_panel == Some(ConsolePanel::Hints))
            })
            .unwrap_or_default()
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
    pub(super) fn discovery_owner(&self, service: &str) -> CheckOwner {
        let line = self.active_console_line(service);
        CheckOwner {
            line,
            anchor: line.and_then(|line| {
                crate::browser::editor::anchor(&format!("console:{service}"), line)
            }),
        }
    }
    pub(super) fn set_discovery_status(
        &mut self,
        service: &str,
        text: String,
        params: &Value,
        debug: Value,
        owner: CheckOwner,
    ) {
        let line = owner.check_line(service);
        let status = DiscoveryStatus {
            request: None,
            text,
            debug,
            started: js_sys::Date::now(),
            owner,
            params: params.clone(),
            signal: crate::activity::Signal::default(),
        };
        let history = self.discovery_history.entry(service.into()).or_default();
        history.retain(|previous| {
            previous.request.is_some()
                || previous
                    .check_line(service)
                    .is_some_and(|previous| Some(previous) != line)
        });
        if let Some(previous) = self.discovery_status.insert(service.into(), status)
            && (previous.request.is_some()
                || previous
                    .check_line(service)
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
    ) -> Option<String> {
        if self.hidden {
            return None;
        }
        if self.cancel_duplicate_check(service, line) {
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
        self.execute(ctx, service, &key, params, line);
        if let Some(run) = self.runs.get_mut(&key) {
            run.agent_session = session;
            run.title = title;
        }
        Some(key)
    }
    pub(super) fn cancel_duplicate_check(&mut self, service: &str, line: usize) -> bool {
        let Some(key) = self
            .console_run_entry(service, Some(line))
            .filter(|(_, run)| run.running)
            .map(|(key, _)| key.to_owned())
        else {
            return false;
        };
        let run = self.runs.get_mut(&key).unwrap();
        let message = "Repeated run rejected: this check was already running. Cancellation was requested; no new request was started.";
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
        self.runs.retain(|_, run| {
            run.editor != owner
                || run.running
                || (if let Some(anchor) = &run.anchor {
                    crate::browser::editor::anchor_line(&owner, anchor)
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
    ) {
        if self.runs.get(key).is_some_and(|r| r.running) {
            self.notice = Some(Notice::new(
                "This request is already running. Stop the current stream first.",
            ));
            return;
        }
        let vertical = params["vertical"] == true;
        let started = js_sys::Date::now();
        self.runs.insert(
            key.into(),
            Run {
                editor: format!("console:{service}"),
                running: true,
                status: "Running…".into(),
                line: Some(line),
                vertical,
                started,
                debug: json!({"operation":"request","service":service,"params":params,"started":started,"state":"running"}),
                anchor: crate::browser::editor::anchor(&format!("console:{service}"), line),
                ..Default::default()
            },
        );
        let k = key.to_string();
        let s = service.to_string();
        let callback = ctx
            .link()
            .callback(move |r| Msg::Reply(k.clone(), s.clone(), r));
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
        }
        let id = self.transport.request(
            message,
            crate::services::view(service) == View::Stream,
            callback,
        );
        self.runs.get_mut(key).unwrap().stream = Some(id);
    }
    pub(super) fn client_error(&mut self, service: &str, message: String, line: usize) {
        if self.cancel_duplicate_check(service, line) {
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
                anchor: crate::browser::editor::anchor(&format!("console:{service}"), line),
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
                    format!("{} · {}", time(), format::status(service, &result))
                };
                run.debug = result.clone();
                run.result = result;
            }
            Reply::Error(e) => {
                run.running = false;
                run.stream = None;
                run.status = format!("{} · ERROR · {}", time(), err_text(&e));
                run.debug = e.clone();
                run.result = e;
            }
        }
        let anchored_line = run
            .anchor
            .as_ref()
            .and_then(|a| crate::browser::editor::anchor_line(&run.editor, a));
        let response_line = if run.anchor.is_some() {
            anchored_line
        } else {
            run.line
        };
        if let Some(line) = response_line {
            let body = format::check_response(
                service,
                &run.result,
                run.vertical,
                &run.stdout,
                &run.stderr,
            );
            let outcome = if run.running {
                "Running"
            } else {
                format::outcome(&run.result)
            };
            let response_status = format!("{} · {outcome}", time());
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
            if let Some(region) = self
                .response_regions
                .get_mut(service)
                .and_then(|regions| regions.iter_mut().find(|region| region.request == line))
                && crate::services::view(service) == View::Grid
                && run.result["columns"].is_array()
            {
                region.table = Some(crate::tables::Metadata::from_result(&run.result));
            }
            crate::checks::remap_text(
                &old_doc,
                doc,
                self.checks.entry(service.into()).or_default(),
            );
            if let Err(error) = crate::browser::editor::sync_response(
                &format!("console:{service}"),
                doc,
                self.response_regions
                    .get(service)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
                preserve_after,
            ) {
                let run = self.runs.get_mut(key).unwrap();
                run.status = format!("{} · ERROR · Response display failed: {error}", time());
                run.debug = json!({"error":format!("Response display failed: {error}"),
                    "phase":"response-display","result":run.debug.take()});
            }
            self.persist_console();
        }
    }
    pub(super) fn discover(&mut self, ctx: &Context<Self>, manual: bool) {
        let service = self.current_language().to_string();
        if crate::services::discovery(&service).is_none() {
            return;
        }
        let Some(params) = self.discovery_params(&service) else {
            return;
        };
        self.start_discovery(ctx, service, params, manual, None);
    }
    pub(super) fn discovery_params(&self, service: &str) -> Option<Value> {
        let source = self.docs.get(service).cloned().unwrap_or_default();
        let cursor = self.active_console_line(service).unwrap_or(0);
        let block = crate::document::request_at(
            &source,
            cursor,
            self.response_regions
                .get(service)
                .map(Vec::as_slice)
                .unwrap_or(&[]),
        )
        .map(|b| b.text)
        .unwrap_or(source);
        crate::discovery::params(service, &console::sections(service, &block))
    }
    /// The discovered schema for the current check of `service`.
    pub(super) fn schema(&self, service: &str) -> Option<&Value> {
        if crate::services::lazy_discovery(service) {
            let params = self.discovery_params(service)?;
            let key = crate::discovery::key(service, &params, &self.vars).ok()?;
            self.lazy_schemas
                .get(service)?
                .get(&crate::discovery::address(service, &key))
        } else {
            self.discovery.get(service)
        }
    }
    pub(super) fn start_discovery(
        &mut self,
        ctx: &Context<Self>,
        service: String,
        params: Value,
        manual: bool,
        owner: Option<CheckOwner>,
    ) {
        if self.hidden {
            return;
        }
        let owner = owner.unwrap_or_else(|| self.discovery_owner(&service));
        let key = match crate::discovery::key(&service, &params, &self.vars) {
            Ok(key) => key,
            Err(error) => {
                self.pending_discovery.remove(&service);
                let previous = self.discovered_params.remove(&service).is_some();
                self.discovery.remove(&service);
                let address = crate::discovery::address(&service, &params);
                if manual || previous || !address.trim().is_empty() {
                    self.set_discovery_status(
                            &service,
                            format!("{} · Discovery · WARNING · {error}", time()),
                            &params,
                            json!({"type":"discover","service":service,"params":params,"warning":error}),
                            owner,
                        );
                }
                return;
            }
        };
        let lazy = crate::services::lazy_discovery(&service);
        let address = crate::discovery::address(&service, &key);
        let cache = self.lazy_schemas.get(&service);
        if lazy && cache.and_then(|cache| cache.get(&address)).is_some() {
            self.pending_discovery.remove(&service);
            return;
        }
        if self.discovering.contains(&service) {
            if crate::discovery::follows_connection(&service) {
                if self.discovered_params.get(&service) != Some(&key) {
                    self.pending_discovery.insert(service, (params, owner));
                } else {
                    self.pending_discovery.remove(&service);
                }
            }
            return;
        }
        if self.discovered_params.get(&service) == Some(&key) {
            return;
        }
        if lazy && cache.is_some_and(|cache| cache.attempted(&address)) {
            return;
        }
        if crate::discovery::follows_connection(&service) {
            self.discovery.remove(&service);
        }
        if lazy {
            self.lazy_schemas
                .entry(service.clone())
                .or_default()
                .attempt(&address);
        }
        self.discovered_params.insert(service.clone(), key.clone());
        self.discovering.insert(service.clone());
        self.set_discovery_status(
            &service,
            "Discovery…".into(),
            &params,
            json!({"type":"discover","service":service,"params":params}),
            owner,
        );
        let s = service.clone();
        let p = params.clone();
        let id = self.transport.request(
            json!({"type":"discover","service":service,"params":params}),
            false,
            ctx.link()
                .callback(move |r| Msg::Discovered(s.clone(), p.clone(), key.clone(), r)),
        );
        if let Some(status) = self.discovery_status.get_mut(&service) {
            status.request = Some(id);
        }
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
        if crate::services::has_section(service, "headers") && !self.hints.contains_key(service) {
            for header in [
                "Accept",
                "Authorization",
                "Content-Type",
                "User-Agent",
                "Cookie",
                "X-Request-Id",
            ] {
                hints.push((format!("{header}: "), "Header".into()));
            }
        }
        for keyword in crate::services::get(service)
            .iter()
            .flat_map(|service| &service.console.keywords)
        {
            hints.push((keyword.clone(), String::new()));
        }
        if let Some(text) = self.hints.get(service) {
            for line in text
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
            {
                hints.push((line.into(), "Saved hint".into()));
            }
        }
        if let Some(value) = self.discovery.get(service) {
            collect_discovery(value, "", &mut hints);
        }
        let mut seen = BTreeSet::new();
        hints.retain(|(s, _)| seen.insert(s.clone()));
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
        let owner = format!("console:{service}");
        let edited_service = service.clone();
        let scroll_service = service.clone();
        let (run, check_discovery) = self.selected_status(service);
        let state = self
            .active_console_line(service)
            .map(|line| self.check_state(service, line));
        let discovery = self.schema(service).cloned().unwrap_or(Value::Null);
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
                    format!(
                        "Request · {} · {}",
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
                run.is_some_and(|run| run.running),
                run.map(|run| run.started),
            )
        };
        let document = self.docs.get(service).cloned().unwrap_or_default();
        let regions = self
            .response_regions
            .get(service)
            .cloned()
            .unwrap_or_default();
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
        let scroll_target = self
            .check_target
            .as_ref()
            .filter(|(s, _, _)| s == service)
            .map(|(_, line, serial)| (*line, *serial));
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
                        </div>
                    }
                    if self.console_panel==Some(ConsolePanel::Sessions) {
                        <div class="console-panel-heading"><span class="tab-button active">{"Sessions"}{indicator(crate::activity::Indicator::Ready,"Sessions".into())}</span></div>
                    }
                    if self.console_panel==Some(ConsolePanel::Settings) {
                        <div class="console-panel-heading"><span class="tab-button active" data-testid="settings-title">{"Settings"}{indicator(self.settings_indicator(),"Settings status".into())}</span></div>
                    }
                    <div class="console-panel-heading" ref={self.console_panel_header.clone()} data-testid="console-variables-heading"
                        hidden={!matches!(self.console_panel,Some(ConsolePanel::Variables | ConsolePanel::Dictionary | ConsolePanel::Agent | ConsolePanel::Templates | ConsolePanel::History))}></div>
                </div>
                <crate::browser::check_rail::CheckRail items={checks} {active}
                    width={layout.check_menu_width} onwidth={ctx.link().callback(Msg::CheckMenuWidth)}
                    onselect={ctx.link().callback(Msg::Check)} onerror={ctx.link().callback(Msg::EditorError)}/>
                <div class="console-workspace-body">
                    <div class="console-editor-host">
                        <Editor key={format!("console:{service}")} cache_key={format!("console:{service}")} testid="console-editor" service={service.clone()} {regions} {discovery} value={document} mode={format!("console-{service}")} console={true} checks={self.checks.get(service).cloned().unwrap_or_default()} check_scope={format!("{}:{service}",self.workspace_id)} hints={self.completions(service)} onchange={ctx.link().callback(Msg::ConsoleText)} onedit={ctx.link().callback(move |edit|Msg::ConsoleEdited(edited_service.clone(),edit))} onrun={ctx.link().callback(|(s,l)|Msg::ConsoleRun(s,l))} ondiscover={ctx.link().callback(|_|Msg::Discover)} onauto={ctx.link().callback(move |endpoint|Msg::AutoDiscover(owner.clone(), endpoint))} onscroll={ctx.link().callback(move |viewport|Msg::Scroll(scroll_service.clone(),viewport))} {scroll_target} onerror={ctx.link().callback(Msg::EditorError)}
                            templates={self.template_entries(service)}
                            agent_edit={self.agent_edit.as_ref().filter(|edit|&edit.command.service==service).map(|edit|edit.command.clone())}
                            onagentedit={ctx.link().callback(|(serial,result)|Msg::AgentEdited(serial,result))}/>
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
                                <div class="console-settings-version"><span>{"Current version"}</span><strong data-testid="settings-version">{env!("CARGO_PKG_VERSION")}</strong></div>
                                if let Some(release)=&self.updates.release {
                                    <div class="console-settings-version"><span>{"New version"}</span><strong data-testid="settings-update-version">{&release.version}</strong></div>
                                }
                                <div class="console-settings-actions">
                                if self.updates.phase==UpdatePhase::Installed {
                                    <button type="button" class="tab-button" data-testid="settings-update-reload" onclick={ctx.link().callback(|_|Msg::ReloadUpdate)}>{"Reload extension"}</button>
                                } else if self.transport.is_native() {
                                    <button type="button" class="tab-button" data-testid="settings-update-check" disabled={self.updates.busy() || self.updates.phase==UpdatePhase::Checking}
                                        onclick={ctx.link().callback(|_|Msg::CheckUpdates)}>{"Check for updates"}</button>
                                    if self.updates.phase==UpdatePhase::Available {
                                        <button type="button" class="tab-button" data-testid="settings-update-install" onclick={ctx.link().callback(|_|Msg::InstallUpdate)}>{"Update"}</button>
                                    }
                                    if let Some(source)=self.updates.source.as_ref().filter(|source|source.validate().is_ok()) {
                                        <a class="tab-button" href={format!("https://github.com/{}/releases",source.repository)} target="_blank" rel="noopener noreferrer">{"Open GitHub releases"}</a>
                                    }
                                }
                                </div>
                                if self.transport.is_native() {
                                    <crate::browser::module_settings::ModuleSettings transport={self.transport.clone()}
                                        onstatus={ctx.link().callback(Msg::SettingsStatus)}
                                        onforget={ctx.link().callback(Msg::ForgetModule)}
                                        onchange={ctx.link().callback(|_|Msg::ModulesChanged)}/>
                                }
                                <crate::browser::agent_settings::AgentSettings transport={self.transport.clone()}
                                    onstatus={ctx.link().callback(Msg::SettingsStatus)}/>
                            </section>
                        }
                        if self.console_panel==Some(ConsolePanel::Sessions) {
                            {self.view_sessions(ctx)}
                        }
                        if self.console_panel==Some(ConsolePanel::Hints) {
                            <div class="console-hints-editor">
                                <Editor key={format!("hints:{service}")} cache_key={format!("hints:{service}")} testid="console-hints"
                                    value={self.hints_drafts.get(service).or_else(||self.hints.get(service)).cloned().unwrap_or_default()} mode="headers" onchange={ctx.link().callback(Msg::HintsText)} onerror={ctx.link().callback(Msg::EditorError)}/>
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
                if supported{<button class="console-import-toggle" data-testid="console-export" onclick={ctx.link().callback(|_|Msg::Export)}>{"Export\u{2026}"}{indicator(self.transfer_indicator(service,Transfer::Export),"Export status".into())}</button><button class="console-import-toggle" data-testid="console-import" onclick={ctx.link().callback(|_|Msg::Import)}>{"Import\u{2026}"}{indicator(self.transfer_indicator(service,Transfer::Import),"Import status".into())}</button>}
                <button class={classes!("console-hints-toggle",(self.console_panel==Some(ConsolePanel::Templates)).then_some("active"))}
                    data-testid="console-templates-toggle" aria-pressed={(self.console_panel==Some(ConsolePanel::Templates)).to_string()}
                    onclick={ctx.link().callback(|_|Msg::ToggleTemplates)}>{"Templates"}{indicator(self.templates_activity,"Templates status".into())}</button>
                <button class={classes!("console-hints-toggle",(self.console_panel==Some(ConsolePanel::History)).then_some("active"))}
                    data-testid="console-history-toggle" aria-pressed={(self.console_panel==Some(ConsolePanel::History)).to_string()}
                    onclick={ctx.link().callback(|_|Msg::ToggleHistory)}>{"History"}{indicator(if self.history_error.is_some() && !self.history_error_seen {
                        crate::activity::Indicator::Error
                    }else{self.history_activity},"History status".into())}</button>
                <button class={classes!("console-hints-toggle",(self.console_panel==Some(ConsolePanel::Hints)).then_some("active"))} data-testid="console-hints-toggle" aria-pressed={(self.console_panel==Some(ConsolePanel::Hints)).to_string()} onclick={ctx.link().callback(|_|Msg::ToggleHints)}>{"Hints\u{2026}"}{indicator(self.hints_indicator(service),"Hints status".into())}</button>
                <button class={classes!("console-hints-toggle",(self.console_panel==Some(ConsolePanel::Variables)).then_some("active"))} data-testid="console-variables-toggle" aria-pressed={(self.console_panel==Some(ConsolePanel::Variables)).to_string()} onclick={ctx.link().callback(|_|Msg::ToggleVariables)}>{"Variables"}{self.profile_indicator(false)}</button>
                <button class={classes!("console-hints-toggle",(self.console_panel==Some(ConsolePanel::Dictionary)).then_some("active"))} data-testid="console-dictionary-toggle" aria-pressed={(self.console_panel==Some(ConsolePanel::Dictionary)).to_string()} onclick={ctx.link().callback(|_|Msg::ToggleDictionary)}>{"Dictionary"}{self.profile_indicator(true)}</button>
                <button class={classes!("console-hints-toggle",(self.console_panel==Some(ConsolePanel::Settings)).then_some("active"))}
                    data-testid="console-settings-toggle" aria-pressed={(self.console_panel==Some(ConsolePanel::Settings)).to_string()}
                    onclick={ctx.link().callback(|_|Msg::ToggleSettings)}>{"Settings"}{indicator(self.settings_notification(),"Settings status".into())}</button>
            </nav>
            </div>
            if let Some(command)=&self.export_text{<div class="console-hints-overlay"><div class="console-hints-panel">
                <div class="console-hints-panel-header">{"Export - copy this command"}</div>
                <Editor key="export" value={command.clone()} mode="shell" onchange={ctx.link().callback(Msg::ExportText)}/>
                <div class="console-hints-panel-actions"><button onclick={ctx.link().callback(|_|Msg::CloseExport)}>{"Close"}</button></div>
            </div></div>}
        </div>}
    }
}
