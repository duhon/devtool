use anyhow::{Context, Result, bail, ensure};
use axum::{Json, Router, extract::Request, response::IntoResponse};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};
use tokio::{net::TcpStream, time::timeout};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message};

const EXTENSION_ID: &str = "ckicpgdfkgcmgnclbfbcbkillcpaffnb";
const WAIT: Duration = Duration::from_secs(20);

/// Test driver and shared Console checks injected into every extension page before it loads.
const INIT: &str = concat!(
    include_str!("browser-driver.js"),
    "\n",
    include_str!("browser-console-checks.js")
);

struct Process(Child);

impl Drop for Process {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

struct Fixture(tokio::task::JoinHandle<()>);

impl Drop for Fixture {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct Cdp {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    next: u64,
    replies: HashMap<u64, Value>,
    /// `__testResolve` evaluations sent without waiting: a page sends its next test command as
    /// soon as one resolves, so waiting for the reply would nest one call per command.
    resolving: HashSet<u64>,
    errors: Vec<Value>,
    /// JavaScript dialogs the pages opened (beforeunload is accepted, anything else dismissed).
    dialogs: Vec<Value>,
    inspected_page: Option<String>,
    targets: HashMap<String, String>,
    /// Files a page may choose in a file input with `Test.chooseFile`, by name.
    files: HashMap<String, PathBuf>,
}

impl Cdp {
    async fn send(&mut self, method: &str, params: Value, session: Option<&str>) -> Result<u64> {
        self.next += 1;
        let id = self.next;
        let mut request = json!({"id":id,"method":method,"params":params});
        if let Some(session) = session {
            request["sessionId"] = json!(session);
        }
        self.socket
            .send(Message::Text(request.to_string().into()))
            .await?;
        Ok(id)
    }

    async fn call(&mut self, method: &str, params: Value, session: Option<&str>) -> Result<Value> {
        let id = self.send(method, params, session).await?;
        timeout(Duration::from_secs(180), async {
            loop {
                if let Some(reply) = self.replies.remove(&id) {
                    ensure!(reply.get("error").is_none(), "{method}: {}", reply["error"]);
                    return Ok(reply["result"].clone());
                }
                let message = self
                    .socket
                    .next()
                    .await
                    .with_context(|| format!("Chrome CDP disconnected during {method} ({session:?})"))?
                    .with_context(|| format!("Chrome CDP transport failed during {method} ({session:?})"))?;
                let Message::Text(text) = message else {
                    continue;
                };
                let reply: Value = serde_json::from_str(&text)?;
                if let Some(id) = reply["id"].as_u64() {
                    if self.resolving.remove(&id) {
                        ensure!(
                            reply.get("error").is_none(),
                            "Resolving a browser test command: {}",
                            reply["error"]
                        );
                    } else {
                        self.replies.insert(id, reply);
                    }
                } else if reply["method"] == "Runtime.exceptionThrown" {
                    self.errors
                        .push(reply["params"]["exceptionDetails"].clone());
                } else if reply["method"] == "Page.javascriptDialogOpening" {
                    // An open dialog blocks navigation and evaluation in its page until it is handled.
                    let target = reply["sessionId"]
                        .as_str()
                        .context("Missing dialog session")?
                        .to_owned();
                    let accept = reply["params"]["type"] == "beforeunload";
                    self.dialogs.push(reply["params"].clone());
                    Box::pin(self.call(
                        "Page.handleJavaScriptDialog",
                        json!({"accept":accept}),
                        Some(&target),
                    ))
                    .await?;
                } else if reply["method"] == "Target.attachedToTarget" {
                    let target = reply["params"]["sessionId"]
                        .as_str()
                        .context("Missing attached frame session")?
                        .to_owned();
                    if reply["params"]["targetInfo"]["type"] == "iframe" {
                        Box::pin(self.call("Page.enable", json!({}), Some(&target))).await?;
                        Box::pin(self.call("Runtime.enable", json!({}), Some(&target))).await?;
                        Box::pin(self.call(
                            "Runtime.addBinding",
                            json!({"name":"__testInput"}),
                            Some(&target),
                        ))
                        .await?;
                        Box::pin(self.call(
                            "Page.addScriptToEvaluateOnNewDocument",
                            json!({"source":INIT,"runImmediately":true}),
                            Some(&target),
                        ))
                        .await?;
                    }
                    Box::pin(self.call(
                        "Runtime.runIfWaitingForDebugger",
                        json!({}),
                        Some(&target),
                    ))
                    .await?;
                } else if reply["method"] == "Runtime.bindingCalled" {
                    let command: Value = serde_json::from_str(
                        reply["params"]["payload"]
                            .as_str()
                            .context("Missing test binding payload")?,
                    )?;
                    let method = command["method"].as_str().context("Missing input method")?;
                    ensure!(
                        matches!(
                            method,
                            "Input.dispatchKeyEvent"
                                | "Input.insertText"
                                | "Input.dispatchMouseEvent"
                                | "Page.captureScreenshot"
                                | "Test.inspectedPage"
                                | "Test.inspectedCommand"
                                | "Test.chooseFile"
                        ),
                        "Unexpected browser test command: {method}"
                    );
                    let target = reply["sessionId"]
                        .as_str()
                        .context("Missing CDP session")?
                        .to_owned();
                    let result = if method == "Test.inspectedPage" {
                        let inspected = self.inspected_page.clone().context("Missing inspected fixture page")?;
                        match Box::pin(self.call("Runtime.evaluate",
                            json!({"expression":command["params"]["expression"],"returnByValue":true,"silent":true,"includeCommandLineAPI":true}),
                            Some(&inspected))).await {
                            Ok(result) => result,
                            Err(error) => json!({"exceptionDetails":{"text":format!("Inspected-page fixture evaluation failed: {error:#}")}}),
                        }
                    } else if method == "Test.inspectedCommand" {
                        let inspected = self.inspected_page.clone().context("Missing inspected fixture page")?;
                        let method = command["params"]["method"].as_str().context("Missing inspected command")?;
                        ensure!(matches!(method, "Input.dispatchMouseEvent" | "Input.dispatchKeyEvent" | "Input.insertText" | "Page.captureScreenshot"
                            | "DOM.getDocument" | "DOM.querySelector" | "DOM.setInspectedNode" | "DOM.disable"),
                            "Unexpected inspected-page test command: {method}");
                        Box::pin(self.call(method, command["params"]["params"].clone(), Some(&inspected))).await?
                    } else if method == "Test.chooseFile" {
                        // As a user's choice: Chrome reads the file and fires the input's change event.
                        let name = command["params"]["file"].as_str().context("Missing test file name")?;
                        let file = self.files.get(name).with_context(|| format!("Unknown test file {name:?}"))?.clone();
                        let document = Box::pin(self.call("DOM.getDocument", json!({"depth":0}), Some(&target))).await?;
                        let node = Box::pin(self.call("DOM.querySelector",
                            json!({"nodeId":document["root"]["nodeId"],"selector":command["params"]["selector"]}),
                            Some(&target))).await?;
                        ensure!(node["nodeId"].as_u64().is_some_and(|node| node > 0),
                            "No file input matches {}", command["params"]["selector"]);
                        Box::pin(self.call("DOM.setFileInputFiles",
                            json!({"files":[file],"nodeId":node["nodeId"]}), Some(&target))).await?
                    } else {
                        Box::pin(self.call(method, command["params"].clone(), Some(&target))).await?
                    };
                    let resolve = self
                        .send(
                            "Runtime.evaluate",
                            json!({
                                "contextId":reply["params"]["executionContextId"],
                                "expression":format!("__testResolve({}, {})", command["id"], result),
                            }),
                            Some(&target),
                        )
                        .await?;
                    self.resolving.insert(resolve);
                }
            }
        })
        .await
        .with_context(|| format!("Chrome CDP command timed out: {method} ({session:?})"))?
    }

    async fn evaluate(&mut self, session: &str, expression: &str) -> Result<Value> {
        let reply = self
            .call(
                "Runtime.evaluate",
                json!({"expression":expression,"awaitPromise":true,"returnByValue":true}),
                Some(session),
            )
            .await?;
        ensure!(
            reply.get("exceptionDetails").is_none(),
            "Browser assertion failed: {}",
            reply["exceptionDetails"]
        );
        Ok(reply["result"]["value"].clone())
    }

    async fn page(&mut self, url: &str, init: &str) -> Result<String> {
        let page = self
            .call("Target.createTarget", json!({"url":"about:blank"}), None)
            .await?;
        let attached = self
            .call(
                "Target.attachToTarget",
                json!({"targetId":page["targetId"],"flatten":true}),
                None,
            )
            .await?;
        let session = attached["sessionId"]
            .as_str()
            .context("Missing page session")?
            .to_owned();
        self.targets.insert(
            session.clone(),
            page["targetId"]
                .as_str()
                .context("Missing page target")?
                .to_owned(),
        );
        self.call("Page.enable", json!({}), Some(&session)).await?;
        self.call("Runtime.enable", json!({}), Some(&session))
            .await?;
        self.call(
            "Target.setAutoAttach",
            json!({"autoAttach":true,"waitForDebuggerOnStart":true,"flatten":true}),
            Some(&session),
        )
        .await?;
        self.call(
            "Runtime.addBinding",
            json!({"name":"__testInput"}),
            Some(&session),
        )
        .await?;
        self.call(
            "Emulation.setDeviceMetricsOverride",
            json!({
                "width":1197,"height":960,"deviceScaleFactor":1,"mobile":false,
            }),
            Some(&session),
        )
        .await?;
        self.call(
            "Page.addScriptToEvaluateOnNewDocument",
            json!({"source":init}),
            Some(&session),
        )
        .await?;
        let navigation = self
            .call("Page.navigate", json!({"url":url}), Some(&session))
            .await?;
        ensure!(
            navigation.get("errorText").is_none(),
            "Browser navigation failed for {url}: {navigation}"
        );
        let navigation = timeout(WAIT, async {
            loop {
                let ready = self
                    .evaluate(
                        &session,
                        &format!(
                            "location.href === {} && document.readyState === 'complete'",
                            json!(url)
                        ),
                    )
                    .await?;
                if ready == true {
                    return Ok::<_, anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;
        match navigation {
            Ok(result) => result?,
            Err(error) => {
                let location = self
                    .evaluate(&session, "({href:location.href,title:document.title})")
                    .await?;
                bail!("Browser navigation timed out for {url}: {location}: {error}");
            }
        }
        Ok(session)
    }

    async fn close(&mut self, session: &str) -> Result<()> {
        if let Some(target) = self.targets.remove(session) {
            self.call("Target.closeTarget", json!({"targetId":target}), None)
                .await?;
        }
        Ok(())
    }
}

fn chrome() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("CHROME_BIN") {
        let path = PathBuf::from(path);
        ensure!(
            path.is_file(),
            "CHROME_BIN must point to a Chrome for Testing or Chromium executable"
        );
        return Ok(path);
    }
    for path in [
        "/Applications/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
    ] {
        if Path::new(path).is_file() {
            return Ok(path.into());
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let cache = PathBuf::from(home).join("Library/Caches/ms-playwright");
        if cache.is_dir() {
            let mut entries = fs::read_dir(cache)?.collect::<std::io::Result<Vec<_>>>()?;
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries.into_iter().rev() {
                if !entry.file_name().to_string_lossy().starts_with("chromium-") {
                    continue;
                }
                for directory in ["chrome-mac-arm64", "chrome-mac"] {
                    let path = entry.path().join(directory).join(
                        "Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
                    );
                    if path.is_file() {
                        return Ok(path);
                    }
                    let path = entry
                        .path()
                        .join(directory)
                        .join("Chromium.app/Contents/MacOS/Chromium");
                    if path.is_file() {
                        return Ok(path);
                    }
                }
            }
        }
    }
    bail!(
        "Install Chrome for Testing/Chromium and set CHROME_BIN to its executable; ordinary Google Chrome does not support loading unpacked extensions via command-line flags"
    )
}

fn command(program: impl AsRef<std::ffi::OsStr>, home: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .env_clear()
        .env("HOME", home)
        .env("ZDOTDIR", home)
        .env("SHELL", "/bin/zsh")
        .env("PATH", "/usr/bin:/bin")
        .env("TMPDIR", std::env::temp_dir())
        .env("LANG", "en_US.UTF-8");
    command
}

fn browser_command(chrome: &Path, home: &Path, profile: &Path, extension: &Path) -> Command {
    let mut browser = command(chrome, home);
    browser
        .args([
            "--headless=new",
            "--no-first-run",
            "--no-default-browser-check",
            "--remote-debugging-port=0",
            "--password-store=basic",
            "--use-mock-keychain",
        ])
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg(format!(
            "--disable-extensions-except={}",
            extension.display()
        ))
        .arg(format!("--load-extension={}", extension.display()));
    browser
}

async fn launch_browser(
    chrome: &Path,
    home: &Path,
    profile: &Path,
    extension: &Path,
    log: &Path,
) -> Result<(Process, Cdp)> {
    let mut browser = Process(
        browser_command(chrome, home, profile, extension)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(log)?)
            .spawn()?,
    );
    let endpoint = timeout(WAIT, async {
        loop {
            if let Some(status) = browser.0.try_wait()? {
                bail!("Chrome exited {status}: {}", fs::read_to_string(log)?);
            }
            match fs::read_to_string(profile.join("DevToolsActivePort")) {
                Ok(text) => {
                    let mut lines = text.lines();
                    if let (Some(port), Some(path)) = (lines.next(), lines.next()) {
                        return Ok(format!("ws://127.0.0.1:{port}{path}"));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("Chrome failed to become responsive")??;
    let (socket, _) = tokio_tungstenite::connect_async(endpoint).await?;
    Ok((
        browser,
        Cdp {
            socket,
            next: 0,
            replies: HashMap::new(),
            resolving: HashSet::new(),
            errors: vec![],
            dialogs: vec![],
            inspected_page: None,
            targets: HashMap::new(),
            files: HashMap::new(),
        },
    ))
}

/// Installs into `home`, updates through the client updater and reloads the installed extension.
/// Returns the uncaught page exceptions of the separate update browser.
async fn installed_reload(
    chrome: &Path,
    home: &Path,
    base: &Path,
    payload: &Path,
    log: &Path,
) -> Result<Vec<Value>> {
    let extension = base.join("extension");
    ensure!(
        fs::symlink_metadata(&extension)?.is_dir(),
        "Installed extension must be a physical directory"
    );
    let permanent = fs::canonicalize(&extension)?;
    ensure!(
        permanent == fs::canonicalize(base)?.join("extension"),
        "Chrome must load a permanent extension path"
    );
    let installed_metadata = extension.join("native/distribution.json");
    let mut source: Value = serde_json::from_slice(&fs::read(&installed_metadata)?)?;
    if source["repository"].as_str().is_none_or(str::is_empty) {
        source["repository"] = json!("fixture/services-devtools");
        source["branch"] = json!("devtool");
        fs::write(&installed_metadata, serde_json::to_vec_pretty(&source)?)?;
        fs::write(
            payload.join("distribution.json"),
            serde_json::to_vec_pretty(&source)?,
        )?;
    }
    let (_browser, mut cdp) = launch_browser(
        chrome,
        home,
        &home.join("Chrome reload profile"),
        &extension,
        log,
    )
    .await?;
    services_installer::install(home, payload)
        .context("Register native messaging for the newly opened reload profile")?;
    fs::write(extension.join("reload-probe.txt"), "before-update")?;
    let settings = cdp.page("chrome://extensions/", "").await?;
    let developer_mode = cdp.evaluate(&settings,
        "(async () => { await chrome.developerPrivate.updateProfileConfiguration({inDeveloperMode: true}); return (await chrome.developerPrivate.getProfileConfiguration()).inDeveloperMode; })()"
    ).await?;
    ensure!(
        developer_mode == true,
        "Reload fixture must enable developer mode"
    );
    let url = format!("chrome-extension://{EXTENSION_ID}/panel.html");
    let page = cdp
        .page(&url, INIT)
        .await
        .context("Opening physical installed extension before update")?;
    let before = cdp.evaluate(&page,
        "(async () => { localStorage.setItem('__reload_probe', 'kept'); return { version: chrome.runtime.getManifest().version, probe: await (await fetch(chrome.runtime.getURL('reload-probe.txt'), {cache:'no-store'})).text() }; })()"
    ).await?;
    cdp.evaluate(&page, r##"testDriver.wait(() => window.document.querySelector('[data-testid="console-session-name"]'),
        () => `Reload fixture did not open Console: ${window.document.body.innerText}`).then(() => true)"##).await?;
    ensure!(
        before["probe"] == "before-update",
        "Initial installed resource must load"
    );
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(payload.join("extension/manifest.json"))?)?;
    manifest["version"] = json!("99.0.1");
    fs::write(
        payload.join("extension/manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    fs::write(payload.join("extension/reload-probe.txt"), "after-update")?;
    let mut distribution: Value =
        serde_json::from_slice(&fs::read(payload.join("distribution.json"))?)?;
    distribution["version"] = json!("99.0.1");
    distribution["buildId"] = json!("browser-reload-upgrade");
    fs::write(
        payload.join("distribution.json"),
        serde_json::to_vec_pretty(&distribution)?,
    )?;
    let archive = home.join("Downloads/services-macos-arm64.tar.gz");
    fs::create_dir_all(archive.parent().unwrap())?;
    let output = Command::new("/usr/bin/tar")
        .env("COPYFILE_DISABLE", "1")
        .arg("-czf")
        .arg(&archive)
        .arg("-C")
        .arg(payload.parent().unwrap())
        .arg("ServicesDevTools")
        .output()?;
    ensure!(
        output.status.success(),
        "Archive update fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    use sha2::{Digest, Sha256};
    let sha = format!("{:x}", Sha256::digest(fs::read(&archive)?));
    let repository = distribution["repository"]
        .as_str()
        .context("Fixture update repository")?;
    let asset_base = format!("https://github.com/{repository}/releases/download/devtool-v99.0.1");
    let release = json!({
        "schemaVersion":1,"application":"ServicesDevTools","version":"99.0.1","tag":"devtool-v99.0.1",
        "commit":"a".repeat(40),"repository":repository,"branch":distribution["branch"],"platform":"darwin",
        "arch":"arm64","minimumMacOS":"13.5","archive":"services-macos-arm64.tar.gz","sha256":sha,
        "archiveUrl":format!("{asset_base}/services-macos-arm64.tar.gz"),
        "checksumsUrl":format!("{asset_base}/checksums.txt"),"installerUrl":format!("{asset_base}/install.sh"),
    });
    cdp.evaluate(
        &page,
        &format!(
            "updateFixture.release = {}; updateFixture.archive = {}; true;",
            release,
            json!(archive),
        ),
    )
    .await?;
    ensure!(cdp.evaluate(&page, r##"(async () => {
      const {wait,click,testid,cm,assert}=testDriver;
      // Temporary session 0 is never persisted, so the durable edit happens in imported session 1.
      await testDriver.activateSession(1);
      await wait(()=>document.querySelector('[data-testid="console-editor"]')?.CodeMirror
        || document.querySelector('[data-testid="console-editor"]')?.nextElementSibling?.CodeMirror);
      cm('console-editor').setValue('#check update persistence\n');
      if(testid('console-sessions-toggle').getAttribute('aria-pressed')!=='true') await click(testid('console-sessions-toggle'));
      await testDriver.sessionsIdle();
      await click(testid('console-settings-toggle'));
      await wait(()=>!testid('settings-update-status').textContent.includes('Checking'));
      await click(testid('settings-update-check'));
      await wait(()=>document.querySelector('[data-testid="settings-update-install"]'),
        ()=>`Approved update did not become available: ${document.body.innerText}; requests=${JSON.stringify(updateFixture.requests)}; release=${JSON.stringify(updateFixture.release)}`);
      updateFixture.reloads=0;
      chrome.runtime.reload=()=>{updateFixture.reloads++;};
      // Install refuses while any request is in flight; opening Settings loads the Agent model catalog.
      await wait(()=>!testid('agent-model-input').disabled, 'Agent model catalog to load before Install', 60000);
      await wait(()=>testid('console-session-name').dataset.storageBusy==='false', 'Session save to finish before Install');
      await click(testid('settings-update-install'));
      await wait(()=>updateFixture.reloads===1,
        ()=>`Client updater failed: ${document.body.innerText}`, 60000);
      const saved=(await testDriver.sessionLibrary())?.sessions.find(session=>session.id===1);
      assert(JSON.stringify(saved?.snapshot.console.documents).includes('#check update persistence\\n'),
        'Workspace edits must be durably saved before installing and reloading');
      assert(updateFixture.installs.length===1,
        'An update must request exactly one native download and installation');
      return true;
    })()"##).await? == true, "Client-controlled native archive update did not complete");
    // Legacy single-workspace storage without a session library must migrate after the reload.
    ensure!(
        cdp.evaluate(&page, r##"(async () => {
      const services = ['rest', 'mysql', 'graphql', 'grpc', 'splunk', 'terminal'];
      const document = '#request legacy\n#endpoint\nbash -c\n#command\necho "#request inline"\n\n#responce OK\n#request literal output\n#check literal output\n\n#request second\n#command\necho next';
      const lines = document.split('\n');
      const regions = [{ request: 0, header: lines.indexOf('#responce OK'), end: lines.indexOf('#request second') }];
      localStorage.setItem('servicesConsole.state', JSON.stringify({
        version: 1, service: 'terminal',
        documents: Object.fromEntries(services.map(service => [service, document])),
        responses: Object.fromEntries(services.map(service => [service, regions])),
      }));
      localStorage.setItem('servicesWorkbench.activeService', 'console');
      localStorage.removeItem('servicesConsole.sessions.v1');
      await testDriver.clearSessions();
      localStorage.setItem('__check_migration_expected', JSON.stringify({
        document: document.replace('#request legacy', '#check legacy').replace('#request second', '#check second'),
        regions,
      }));
      return true;
    })()"##).await? == true,
        "Seeding legacy workspace storage did not complete"
    );
    ensure!(
        fs::canonicalize(&extension)? == permanent,
        "Update changed Chrome's registered real path"
    );
    for name in ["releases", "current", "native"] {
        ensure!(
            matches!(fs::symlink_metadata(base.join(name)), Err(error) if error.kind() == std::io::ErrorKind::NotFound),
            "Update must not create a legacy {name} path"
        );
    }
    ensure!(
        cdp.evaluate(&page, "chrome.runtime.getManifest().version")
            .await?
            == before["version"],
        "Manifest must remain the loaded version until extension reload"
    );
    cdp.evaluate(
        &settings,
        &format!("chrome.developerPrivate.reload('{EXTENSION_ID}', {{failQuietly: false}}).then(() => true)"),
    )
    .await?;
    let reloaded = match cdp.page(&url, INIT).await {
        Ok(page) => page,
        Err(error) => {
            let info = cdp.evaluate(&settings,
                "chrome.developerPrivate.getExtensionsInfo({includeDisabled:true,includeTerminated:true}).then(items => items.filter(item => item.id === 'ckicpgdfkgcmgnclbfbcbkillcpaffnb').map(({state,version,path,disableReasons,manifestErrors,runtimeErrors}) => ({state,version,path,disableReasons,manifestErrors,runtimeErrors})))"
            ).await?;
            return Err(error.context(format!(
                "Installed extension reload failed; Chrome extension state: {info}"
            )));
        }
    };
    let after = cdp.evaluate(&reloaded, r##"(async () => {
      await testDriver.wait(() => document.querySelector('[data-testid="console-session-name"]'));
      testDriver.assert(testDriver.testid('console-session-name').dataset.sessionId === '0',
        'An extension update must start in empty temporary session 0');
      await testDriver.activateSession(1);
      const editor = await testDriver.wait(() => document.querySelector('.console-editor-host .CodeMirror')?.CodeMirror);
      const expected = JSON.parse(localStorage.getItem('__check_migration_expected'));
      const saved = (await testDriver.sessionLibrary()).sessions[0].snapshot.console;
      const services = ['rest', 'mysql', 'graphql', 'grpc', 'splunk', 'terminal'];
      return {
        version: chrome.runtime.getManifest().version,
        stored: localStorage.getItem('__reload_probe'),
        probe: await (await fetch(chrome.runtime.getURL('reload-probe.txt'), {cache:'no-store'})).text(),
        migrated: services.every(service => saved.documents[service] === expected.document
          && JSON.stringify(saved.responses[service]) === JSON.stringify(expected.regions)),
        displayed: editor.getValue() === expected.document,
      };
    })()"##).await?;
    ensure!(
        after["version"] == "99.0.1"
            && after["probe"] == "after-update"
            && after["stored"] == "kept"
            && after["migrated"] == true
            && after["displayed"] == true,
        "Extension reload must load updated manifest/resources and retain storage: {after}"
    );
    ensure!(
        cdp.evaluate(&reloaded, r##"(async () => {
          for (const service of ['rest', 'mysql', 'graphql', 'grpc', 'splunk', 'terminal']) {
            await testDriver.click(testDriver.testid(`console-service-${service}`));
            const editor = testDriver.cm('console-editor');
            const expected = JSON.parse(localStorage.getItem('__check_migration_expected'));
            testDriver.assert(editor.getValue() === expected.document, 'Reload folding must use the migrated document');
            await testDriver.checkConsoleFolding(editor);
          }
          return true;
        })()"##).await? == true,
        "Reloaded Console checks must fold through protected responses and literal output markers"
    );
    cdp.call("Browser.close", json!({}), None).await?;
    Ok(std::mem::take(&mut cdp.errors))
}

/// Coarse areas used by the legacy `test-agent` and `test-settings` commands.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Group {
    Core,
    Agent,
    Settings,
}

#[derive(Clone, Copy)]
enum Setup {
    /// A fresh panel page on cleared storage.
    Fresh,
    /// Also opens the inspected fixture page that page tools act on.
    InspectedPage,
    /// Runs this script on a fresh page, then reopens the panel on the storage it left behind.
    Reopen(&'static str),
}

#[derive(Clone, Copy)]
enum Kind {
    Script {
        setup: Setup,
        script: &'static str,
        receipt: Option<&'static str>,
    },
    /// Separate home and Chrome profile: installed extension update and reload.
    InstalledUpdate,
    /// Two panel windows; closing one must stop only its own processes.
    CloseWindow,
}

#[derive(Clone, Copy)]
struct Scenario {
    name: &'static str,
    group: Group,
    timeout: Duration,
    summary: &'static str,
    kind: Kind,
    /// Measures wall-clock budgets or is CPU-bound against a fixed timeout, so it runs alone after the parallel phase.
    exclusive: bool,
    /// Sends Agent prompts, so the Copilot SDK fixture must have started by the end of the run.
    sdk: bool,
}

impl Scenario {
    fn label(&self) -> String {
        self.name.to_string()
    }

    fn exclusive(self) -> Self {
        Self {
            exclusive: true,
            ..self
        }
    }

    fn starts_sdk(self) -> Self {
        Self { sdk: true, ..self }
    }
}

fn scenario(
    name: &'static str,
    group: Group,
    seconds: u64,
    summary: &'static str,
    kind: Kind,
) -> Scenario {
    Scenario {
        name,
        group,
        timeout: Duration::from_secs(seconds),
        summary,
        kind,
        exclusive: false,
        sdk: false,
    }
}

fn script(script: &'static str) -> Kind {
    Kind::Script {
        setup: Setup::Fresh,
        script,
        receipt: None,
    }
}

/// Every browser scenario in run order. Each one starts on a fresh page with reset storage and data files.
fn scenarios() -> Vec<Scenario> {
    use Group::*;
    vec![
        scenario(
            "installed-update",
            Core,
            120,
            "Separate profile: install, client updater download/install, extension reload keeps storage and migrates legacy checks",
            Kind::InstalledUpdate,
        ),
        scenario(
            "sessions",
            Core,
            90,
            "Named and temporary Console sessions: copies, deletion, drafts, global Dictionary/Agent, cross-window conflicts",
            script(include_str!("browser-sessions.js")),
        ),
        scenario(
            "console",
            Core,
            240,
            "Console editor over native messaging: services, discovery, completion, execution, folding, scrollbars",
            script(include_str!("browser-native.js")),
        ),
        scenario(
            "templates",
            Core,
            60,
            "Global Templates: save, insert, rename, delete and persistence",
            script(include_str!("browser-templates.js")),
        ),
        scenario(
            "layout",
            Core,
            60,
            "Check spacing, session layout and Settings version",
            script(include_str!("browser-layout.js")),
        )
        .exclusive(),
        scenario(
            "updates",
            Settings,
            60,
            "Settings design, bottom status, live Debug, update checks, guards, workspace preparation and install errors",
            script(include_str!("browser-updates.js")),
        ),
        scenario(
            "modules",
            Settings,
            90,
            "Settings modules: list, cancelled and confirmed removal, invalid file, install and reinstall from a package file, kept consoles, the example module installed, run and removed",
            script(include_str!("browser-modules.js")),
        ),
        scenario(
            "history",
            Core,
            60,
            "Execution History recording, large payloads and auto-loading",
            script(include_str!("browser-history.js")),
        ),
        scenario(
            "splunk",
            Core,
            90,
            "Streaming Splunk export: preview/final rows, typed cells and JSON folding",
            script(include_str!("browser-splunk.js")),
        ),
        scenario(
            "table-performance",
            Core,
            60,
            "Large typed table and JSON folding performance (prints a receipt)",
            Kind::Script {
                setup: Setup::Fresh,
                script: include_str!("browser-table-performance.js"),
                receipt: Some("globalThis.tablePerformanceReceipt"),
            },
        )
        .exclusive(),
        scenario(
            "perf-baseline",
            Core,
            120,
            "3.0 performance baseline: panel open, Console REST round trip, large response (prints a receipt)",
            Kind::Script {
                setup: Setup::Fresh,
                script: include_str!("browser-perf-baseline.js"),
                receipt: Some("globalThis.perfBaselineReceipt"),
            },
        )
        .exclusive(),
        scenario(
            "agent",
            Agent,
            60,
            "Copilot SDK chat: tools, live edits/Undo, approvals/denial, conflicts, tab names/drafts persistence",
            script(include_str!("browser-agent.js")),
        )
        .starts_sdk(),
        scenario(
            "agent-settings",
            Agent,
            60,
            "Agent model and reasoning settings, team skills folder",
            script(include_str!("browser-agent-settings.js")),
        ),
        scenario(
            "page-tools",
            Agent,
            120,
            "Inspected page tools: complete paginated reads, Unicode budget, shadow/frame controls, diagnostics (prints a receipt)",
            Kind::Script {
                setup: Setup::InspectedPage,
                script: include_str!("browser-page-tools.js"),
                receipt: Some("pageToolsReceipt"),
            },
        )
        // About 300 sequential tool calls over a 6,000-node page: its time grows with CPU contention.
        .exclusive(),
        scenario(
            "page-agent",
            Agent,
            90,
            "Inspected-page Agent: consent, extended tools, asynchronous waits, Stop and guide-only restrictions",
            Kind::Script {
                setup: Setup::InspectedPage,
                script: include_str!("browser-page-agent.js"),
                receipt: None,
            },
        )
        .starts_sdk(),
        scenario(
            "agent-resume",
            Agent,
            60,
            "A new window restores a saved chat (name, draft, closed tabs) and resumes its SDK session",
            Kind::Script {
                setup: Setup::Reopen(include_str!("browser-agent-resume-setup.js")),
                script: include_str!("browser-agent-resume.js"),
                receipt: None,
            },
        )
        .starts_sdk(),
        scenario(
            "stop",
            Agent,
            90,
            "Scoped Stop for requests, Terminal and Agent runs, and cleanup of hidden-panel work",
            script(include_str!("browser-stop.js")),
        )
        .starts_sdk(),
        scenario(
            "close-window",
            Agent,
            90,
            "Closing one Console window stops its own Terminal processes and leaves another window's running",
            Kind::CloseWindow,
        )
        // Asserts terminals exit within 20s of window close: browser teardown time grows with CPU contention.
        .exclusive(),
    ]
}

pub const USAGE: &str =
    "cargo run -p xtask -- test browser [--scenario NAME]... [--jobs N] [--list] [--fail-fast]";
/// Global safety cap; each scenario also has its own timeout.
const SAFETY_CAP: Duration = Duration::from_secs(45 * 60);
/// Upper bound for the default parallelism: each job is a separate Chrome, host and fixture set.
const MAX_DEFAULT_JOBS: usize = 4;

#[derive(Default)]
pub struct Selection {
    names: Vec<String>,
    group: Option<Group>,
    jobs: Option<usize>,
    fail_fast: bool,
    list: bool,
}

impl Selection {
    pub fn all() -> Self {
        Self::default()
    }

    pub fn group(group: Group) -> Self {
        Self {
            group: Some(group),
            ..Self::default()
        }
    }

    /// Parses the arguments after `test browser`.
    pub fn parse(args: &[String]) -> Result<Self> {
        let mut selection = Self::default();
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--scenario" => selection.names.push(
                    args.next()
                        .with_context(|| format!("--scenario needs a name\nUsage: {USAGE}"))?
                        .clone(),
                ),
                "--list" => selection.list = true,
                "--fail-fast" => selection.fail_fast = true,
                "--jobs" => {
                    let jobs = args
                        .next()
                        .with_context(|| format!("--jobs needs a number\nUsage: {USAGE}"))?;
                    selection.jobs = Some(
                        jobs.parse()
                            .ok()
                            .filter(|jobs| *jobs > 0)
                            .with_context(|| {
                                format!("--jobs must be a positive number, got {jobs:?}")
                            })?,
                    );
                }
                _ => bail!("Unknown browser test option {arg:?}\nUsage: {USAGE}"),
            }
        }
        Ok(selection)
    }

    /// Parallel jobs for `scenarios` scenarios: `--jobs`, else half the cores (at most four).
    fn jobs(&self, scenarios: usize) -> usize {
        self.jobs
            .unwrap_or_else(|| {
                std::thread::available_parallelism()
                    .map_or(1, |cores| cores.get() / 2)
                    .clamp(1, MAX_DEFAULT_JOBS)
            })
            .min(scenarios.max(1))
    }

    fn select(&self) -> Result<Vec<Scenario>> {
        let all = scenarios();
        for name in &self.names {
            ensure!(
                all.iter().any(|scenario| scenario.name == name),
                "Unknown browser scenario {name:?}; run with --list to see: {}",
                all.iter()
                    .map(|scenario| scenario.name)
                    .fold(Vec::new(), |mut names, name| {
                        if !names.contains(&name) {
                            names.push(name);
                        }
                        names
                    })
                    .join(", ")
            );
        }
        let chosen: Vec<_> = all
            .into_iter()
            .filter(|scenario| {
                (self.names.is_empty() || self.names.iter().any(|name| name == scenario.name))
                    && self.group.is_none_or(|group| group == scenario.group)
            })
            .collect();
        ensure!(
            !chosen.is_empty(),
            "No browser scenario matches the selection"
        );
        Ok(chosen)
    }
}

fn prepare_home(home: &Path) -> Result<PathBuf> {
    fs::create_dir_all(home)?;
    let copilot = home.join("copilot-acceptance");
    let executable = std::env::current_exe()?;
    let quote = |value: &str| format!("'{}'", value.replace('\'', "'\"'\"'"));
    fs::write(
        &copilot,
        format!(
            "#!/bin/sh\nexec {} copilot-fixture \"$@\" 2>>{}\n",
            quote(&executable.to_string_lossy()),
            quote(&home.join("copilot-fixture.stderr").to_string_lossy())
        ),
    )?;
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&copilot, fs::Permissions::from_mode(0o700))?;
    }
    fs::write(
        home.join(".zprofile"),
        format!(
            "export SERVICES_DEVTOOLS_ENV_PROBE=companion-fixture-only\nexport SERVICES_COPILOT_PATH={}\nprintf 'fixture startup banner\\n'\n",
            quote(&copilot.to_string_lossy())
        ),
    )?;
    fs::write(
        home.join(".zshrc"),
        "export SERVICES_DEVTOOLS_INTERACTIVE_PROBE=interactive-fixture-only\n",
    )?;
    Ok(copilot)
}

/// Extracts the distribution archive into a new `directory` and returns its `ServicesDevTools` payload.
fn extract(archive: &Path, directory: &Path) -> Result<PathBuf> {
    fs::create_dir_all(directory)?;
    let status = Command::new("/usr/bin/tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(directory)
        .status()?;
    ensure!(
        status.success(),
        "Extract browser acceptance payload: {status}"
    );
    Ok(directory.join("ServicesDevTools"))
}

/// The example module's package file (`rust/examples/echo`), built once per run: a module
/// the distribution does not ship.
static EXAMPLE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Package files a scenario may choose (`Test.chooseFile`): a shipped module repacked, the
/// example module and an invalid file. A payload without packages (an older release in an
/// A/B run) offers no shipped one.
fn module_files(payload: &Path, directory: &Path) -> Result<HashMap<String, PathBuf>> {
    use services_contracts::package::{Package, archive, archive_name};
    fs::create_dir_all(directory)?;
    let broken = directory.join("broken.module.tgz");
    fs::write(&broken, "not a package")?;
    let mut files = HashMap::from([("broken".to_owned(), broken)]);
    let graphql = payload.join(services_installer::MODULES).join("graphql");
    if graphql.exists() {
        let package = Package::read(&graphql).map_err(anyhow::Error::msg)?;
        let file = directory.join(archive_name(&package));
        fs::write(&file, archive::pack(&graphql).map_err(anyhow::Error::msg)?)?;
        files.insert("graphql".to_owned(), file);
    }
    if let Some(example) = EXAMPLE.get() {
        files.insert("echo".to_owned(), example.clone());
    }
    Ok(files)
}

/// Writes the legacy native variables and hints every scenario starts from.
fn seed_data(data: &Path, fixture_port: u16) -> Result<Vec<(PathBuf, String)>> {
    let seeds = vec![
        (
            data.join(".variable-profiles.json"),
            json!({
                "order":["default","second","var"],
                "profiles":{"default":{"TOKEN":"fixture","REF":"{{second:TOKEN}}"},"second":{"TOKEN":"resolved-fixture"},
                    "var":{
                        "sku":"fixture-materialized-sku",
                        "magento":format!("localhost:{fixture_port}"),
                        "magentoBase":format!("http://localhost:{fixture_port}"),
                        "magentoToken":format!("http://localhost:{fixture_port}/rest/default/V1/integration/admin/token")
                    }},
            })
            .to_string(),
        ),
        (
            data.join(".console-hints.txt"),
            [
                ("mysql", "mysql://mysql.fixture.invalid"),
                ("rest", "https://rest.fixture.invalid"),
                ("graphql", "https://graphql.fixture.invalid"),
                ("grpc", "grpc://grpc.fixture.invalid:9000"),
                ("splunk", "https://splunk.fixture.invalid"),
                ("artemis", "http://artemis.fixture.invalid:8161/orders"),
            ]
            .into_iter()
            .map(|(service, url)| format!("## {service}\n#endpoint\n{url}\n"))
            .collect::<String>(),
        ),
    ];
    fs::create_dir_all(data)?;
    for (path, contents) in &seeds {
        fs::write(path, contents)?;
    }
    Ok(seeds)
}

/// Native-host PIDs in `ps -o pid=,command=` output whose executable lies in the harness directory `marker`.
/// Chrome passes the extension origin to every host, whatever subcommand it re-executes as.
fn host_pids(ps: &str, marker: &str) -> Vec<u64> {
    let origin = format!(" chrome-extension://{EXTENSION_ID}/");
    ps.lines()
        .filter(|line| {
            line.contains(marker)
                && line.contains("/extension/native/bin/services-devtools ")
                && line.contains(&origin)
        })
        .filter_map(|line| line.split_whitespace().next()?.parse().ok())
        .collect()
}

fn alive(pid: u64) -> Result<bool> {
    Ok(Command::new("/bin/ps")
        .args(["-p", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?
        .success())
}

/// Process state (status, wait channel, CPU, elapsed, command) for pids that would not exit,
/// so a stuck-host failure explains itself instead of only naming the pids.
fn diagnose(pids: &[u64]) -> String {
    let mut args = vec![
        "-o".to_owned(),
        "pid=,stat=,wchan=,%cpu=,etime=,command=".to_owned(),
    ];
    for pid in pids {
        args.push("-p".to_owned());
        args.push(pid.to_string());
    }
    match Command::new("/bin/ps").args(&args).output() {
        Ok(output) => {
            let text = String::from_utf8_lossy(&output.stdout);
            let text = text.trim();
            if text.is_empty() {
                "no matching processes (already gone)".to_owned()
            } else {
                format!("process state:\n{text}")
            }
        }
        Err(error) => format!("could not inspect processes: {error}"),
    }
}

async fn exited(pids: &[u64]) -> Result<()> {
    timeout(WAIT, async {
        loop {
            let mut running = false;
            for pid in pids {
                running |= alive(*pid)?;
            }
            if !running {
                return Ok::<(), anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .with_context(|| {
        format!(
            "Processes still running after {}s: {pids:?}",
            WAIT.as_secs()
        )
    })?
}

/// Copilot SDK fixture processes started under this data directory.
fn sdk_peers(data: &Path) -> Result<Vec<u64>> {
    let directory = data.join(".agent");
    if !directory.is_dir() {
        return Ok(vec![]);
    }
    let mut peers = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with("acceptance-peer-")
        {
            let pid: u64 = fs::read_to_string(entry.path())
                .context("Read SDK acceptance PID")?
                .trim()
                .parse()?;
            ensure!(pid > 1, "Invalid SDK fixture PID");
            peers.push(pid);
        }
    }
    Ok(peers)
}

fn fixture_router() -> Router {
    let agent_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count_read = agent_count.clone();
    let stop_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let stop_read = stop_count.clone();
    Router::new()
        .route("/page-agent-fixture", axum::routing::get(|| async {
            axum::response::Html(include_str!("browser-page-fixture.html"))
        }))
        .route("/agent-count",axum::routing::get(move || {
            let count=count_read.clone();
            async move {([("Access-Control-Allow-Origin","*")],Json(json!({"count":count.load(std::sync::atomic::Ordering::SeqCst)})))}
        }))
        .route("/agent-run",axum::routing::get(move || {
            let count=agent_count.clone();
            async move {
                count.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
                Json(json!({"marker":"agent-approved-response","path":"/agent-run"}))
            }
        }))
        .route("/stop-count", axum::routing::get(move || {
            let count = stop_read.clone();
            async move {([("Access-Control-Allow-Origin","*")],Json(json!({"count":count.load(std::sync::atomic::Ordering::SeqCst)})))}
        }))
        .route("/stop-slow", axum::routing::get(move || {
            let count = stop_count.clone();
            async move {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            tokio::time::sleep(Duration::from_secs(60)).await;
            Json(json!({"marker":"stop-fixture-finished"}))
            }
        }))
        .route("/native-large-response", axum::routing::get(|| async {
            format!("native-large-start:{}:native-large-end", "🦀\"\\\n".repeat(180_000))
        }))
        .route("/history-large-fixture", axum::routing::get(|| async {
            Json(json!({
                "rows":(0..3000).map(|id|json!({"id":id,"text":"chunk-é-🦀 ".repeat(10)})).collect::<Vec<_>>(),
                "tail":"history-auto-loaded-end"
            }))
        }))
        .route("/servicesNS/-/search/search/v2/jobs/export", axum::routing::post(
            |axum::Form(form): axum::Form<HashMap<String,String>>| async move {
                if !matches!(form.get("preview").map(String::as_str), Some("false" | "true"))
                    || form.get("search").map(String::as_str) != Some("search index=fixture | sort - _time")
                    || form.get("earliest_time").map(String::as_str) != Some("-1h")
                    || form.get("latest_time").map(String::as_str) != Some("now")
                    || form.contains_key("count") || form.contains_key("offset") || form.contains_key("exec_mode")
                {
                    return (axum::http::StatusCode::BAD_REQUEST,"Incorrect Splunk export form").into_response();
                }
                let body = concat!(
                    "{\"preview\":true,\"offset\":0,\"result\":{\"host\":\"splunk-preview-short\"},\"lastrow\":true}\n",
                    "{\"preview\":true,\"offset\":0,\"result\":{\"host\":\"splunk-preview-🦀-1\"}}\n",
                    "{\"preview\":true,\"offset\":1,\"result\":{\"host\":\"splunk-preview-🦀-2\"}}\n",
                    "{\"preview\":true,\"offset\":2,\"result\":{\"host\":\"splunk-preview-🦀-3\"},\"lastrow\":true}\n",
                    "{\"preview\":false,\"offset\":0,\"result\":{\"host\":\"splunk-final-🦀-1\"}}\n",
                    "{\"preview\":false,\"offset\":1,\"result\":{\"host\":\"splunk-final-🦀-2\"}}\n",
                    "{\"preview\":false,\"offset\":2,\"result\":{\"host\":\"splunk-final-🦀-3\"},\"lastrow\":true}\n"
                );
                let body=body.lines().map(|line|{
                    let mut record:Value=serde_json::from_str(line).expect("Splunk fixture JSON");
                    record["result"]["message.message"]=json!(r#"{"text":"long wrapped value | for copying","n":42,"ok":true}"#);
                    record["result"]["message.timestamp"]=json!("2026-10-27T09:00:00Z");
                    record["result"]["score"]=json!(123);
                    record["result"]["enabled"]=json!(true);
                    record["result"]["plain"]=json!(r#"{"not":"typed"}"#);
                    format!("{record}\n")
                }).collect::<String>();
                let pieces = body.as_bytes().chunks(7).map(|piece| Ok::<_,std::io::Error>(piece.to_vec())).collect::<Vec<_>>();
                let stream = futures_util::stream::iter(pieces)
                    .chain(futures_util::stream::pending::<std::result::Result<Vec<u8>,std::io::Error>>());
                axum::body::Body::from_stream(stream).into_response()
            }
        ))
        .fallback(|request: Request| async move {
        let path = request.uri().path().to_owned();
        if path == "/history-fixture" {
            static SEQUENCE:std::sync::atomic::AtomicU64=std::sync::atomic::AtomicU64::new(0);
            let sequence=SEQUENCE.fetch_add(1,std::sync::atomic::Ordering::SeqCst)+1;
            let query=request.uri().query().unwrap_or("").to_owned();
            let body=axum::body::to_bytes(request.into_body(),1024*1024).await.expect("History fixture body");
            return Json(json!({"sequence":sequence,"query":query,"body":String::from_utf8(body.to_vec()).expect("History fixture UTF-8")}));
        }
        if let Some(site) = ["/site1", "/site2"]
            .into_iter()
            .find(|site| path == format!("{site}/swagger.json"))
        {
            let name = site.trim_start_matches('/');
            return Json(json!({"openapi":"3.0.0","paths":{
                "/rest/v1/product":{"post":{"requestBody":{"content":{"application/json":{"schema":{
                    "type":"object","required":[name],"properties":{name:{"type":"string"}}
                }}}}}}
            }}));
        }
        if path == "/rest/default/swagger.json" {
            return Json(json!({
                "swagger":"2.0",
                "info":{"title":"Magento-shaped local fixture","version":"1"},
                "basePath":"/rest/default",
                "paths":{"/V1/integration/admin/token":{"post":{
                    "parameters":[{"name":"body","in":"body","required":true,
                        "schema":{"$ref":"#/definitions/AdminTokenRequest"}}]
                }},"/V1/categories/{categoryId}/move":{"put":{}},"/V1/enum-test":{"post":{
                    "parameters":[{"name":"body","in":"body","schema":{"$ref":"#/definitions/EnumRequest"}}]
                }}},
                "definitions":{"AdminTokenRequest":{"type":"object",
                    "required":["username","password","generated"],
                    "properties":{
                        "username":{"type":"string"},"password":{"type":"string"},
                        "scope":{"type":"string"},"generated":{"type":"string","readOnly":true}
                    }
                },"EnumRequest":{"type":"object","required":["status"],"properties":{
                    "status":{"type":"string","enum":["pending","active"]},
                    "priority":{"type":"integer","enum":[1,2,3]},
                    "enabled":{"type":"boolean","enum":[true,false]},
                    "tags":{"type":"array","items":{"type":"string","enum":["red","blue"]}},
                    "nullable":{"enum":["active",null]},
                    "escaped":{"type":"string","enum":["quote\"slash\\ки😀\""]},
                    "constantArray":{"type":"array","enum":[[],["locked"]]},
                    "constantObject":{"type":"object","enum":[{"mode":"fast"},{}]}
                }}}
            }));
        }
        if path == "/spec.json" {
            return Json(json!({"openapi":"3.0.1","paths":{
                "/api/v1/Books":{"get":{},"post":{"requestBody":{"content":{
                    "application/json; v=1.0":{"schema":{"$ref":"#/components/schemas/Book"}}
                }}}}
            },"components":{"schemas":{"Book":{"type":"object","properties":{
                "id":{"type":"integer","format":"int32"},
                "title":{"type":"string","nullable":true},
                "description":{"type":"string","nullable":true},
                "pageCount":{"type":"integer","format":"int32"},
                "excerpt":{"type":"string","nullable":true},
                "publishDate":{"type":"string","format":"date-time"}
            },"additionalProperties":false}}}}));
        }
        if path == "/graphql-native-discovery" {
            if request
                .headers()
                .get("x-token")
                .and_then(|v| v.to_str().ok())
                != Some("resolved-fixture")
            {
                return Json(json!({"errors":[{"message":"GraphQL discovery requires X-Token"}]}));
            }
            return Json(json!({"data":{"__schema":{
                "queryType":{"name":"CatalogQuery"},
                "types":[
                    {"name":"CatalogQuery","kind":"OBJECT","fields":[
                        {"name":"products","type":{"kind":"OBJECT","name":"Product"}}]},
                    {"name":"Product","kind":"OBJECT","fields":[
                        {"name":"sku","type":{"kind":"SCALAR","name":"String"}},
                        {"name":"images","type":{"kind":"OBJECT","name":"Image"}}]},
                    {"name":"Image","kind":"OBJECT","fields":[
                        {"name":"url","type":{"kind":"SCALAR","name":"String"}}]},
                    {"name":"String","kind":"SCALAR"}
                ]
            }}}));
        }
        if path == "/graphql-materialization" {
            let body = axum::body::to_bytes(request.into_body(), 1024 * 1024)
                .await
                .expect("Reading GraphQL materialization fixture body");
            let body: Value =
                serde_json::from_slice(&body).expect("GraphQL fixture must receive valid JSON");
            Json(json!({"data":{"received":body}}))
        } else {
            Json(json!({"marker":"rust-ui-fixture","path":request.uri().to_string()}))
        }
    })
}

/// Shared per-run state: one Chrome with the unpacked extension, the installed native host
/// and the HTTP fixture server.
struct Harness {
    cdp: Cdp,
    browser: Option<Process>,
    _fixture: Fixture,
    chrome: PathBuf,
    archive: PathBuf,
    home: PathBuf,
    data: PathBuf,
    /// The extracted distribution the browser's host was installed from.
    payload: PathBuf,
    seeds: Vec<(PathBuf, String)>,
    fixture_url: String,
    fixture_port: u16,
    pages: Vec<String>,
    control: String,
    /// Report lines produced while the current scenario ran (receipts).
    notes: Vec<String>,
    temporary: tempfile::TempDir,
}

impl Harness {
    async fn start(root: &Path) -> Result<Self> {
        let chrome = chrome()?;
        let archive = root.join("dist/distribution/services-macos-arm64.tar.gz");
        ensure!(
            archive.is_file(),
            "Build the distribution first: cargo run -p xtask -- build"
        );
        let temporary = tempfile::Builder::new()
            .prefix("services-browser-'test-")
            .tempdir()?;
        let home = temporary.path().join("home");
        let profile = home.join("Chrome's custom profile");
        prepare_home(&home)?;
        let payload = extract(&archive, &temporary.path().join("package"))?;
        let base = home.join("Library/Application Support/ServicesDevTools");
        let data = base.join("data");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let fixture_port = listener.local_addr()?.port();
        let fixture_url = format!("http://{}", listener.local_addr()?);
        let seeds = seed_data(&data, fixture_port)?;
        let (browser, mut cdp) = launch_browser(
            &chrome,
            &home,
            &profile,
            &payload.join("extension"),
            &temporary.path().join("chrome.stderr"),
        )
        .await?;
        cdp.files = module_files(&payload, &temporary.path().join("files"))?;
        let setup = cdp
            .page(
                &format!("chrome-extension://{EXTENSION_ID}/panel.html"),
                INIT,
            )
            .await?;
        let missing = cdp.evaluate(&setup, "testNativeHealth()").await?;
        ensure!(
            missing["ok"] == false
                && missing["error"].as_str().is_some_and(|error| error
                    .to_lowercase()
                    .contains("specified native messaging host not found")),
            "Expected unregistered custom-profile failure, got {missing}"
        );
        ensure!(
            cdp.evaluate(&setup, r##"(async () => {
              await testDriver.wait(() => document.querySelector('[data-testid="native-status"]')?.hidden === false);
              const status = testDriver.testid('native-status');
              testDriver.assert(status.textContent === 'Retry connection' && !status.classList.contains('banner-error'),
                'Disconnected transport must retain Retry without a duplicate top error banner');
              return true;
            })()"##).await? == true,
            "Compact native reconnection control did not pass"
        );
        let mut installer = tokio::process::Command::from(command("/bin/bash", &home));
        let output = timeout(
            Duration::from_secs(60),
            installer
                .arg(root.join("dist/distribution/install.sh"))
                .kill_on_drop(true)
                .output(),
        )
        .await
        .context("Installer timed out")??;
        ensure!(
            output.status.success(),
            "Fixture installation failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        ensure!(
            profile
                .join("NativeMessagingHosts/com.chrome_extentions.host.json")
                .is_file(),
            "Installer did not discover running custom Chrome"
        );
        ensure!(
            cdp.evaluate(&setup, "testNativeHealth()").await?["ok"] == true,
            "Installed host must connect without restarting Chrome"
        );
        cdp.evaluate(&setup, "(async () => { await testDriver.click(testDriver.button('Retry connection')); await testDriver.wait(() => document.querySelector('[data-testid=\"native-status\"]').hidden); return true; })()").await?;
        cdp.close(&setup).await?;
        let control = cdp.page("about:blank", "").await?;
        let fixture = Fixture(tokio::spawn(async move {
            axum::serve(listener, fixture_router())
                .await
                .expect("Browser HTTP fixture failed");
        }));
        ensure!(
            cdp.errors.is_empty(),
            "Uncaught browser exceptions during shared setup: {:?}",
            cdp.errors
        );
        Ok(Self {
            cdp,
            browser: Some(browser),
            _fixture: fixture,
            chrome,
            archive,
            home,
            data,
            payload,
            seeds,
            fixture_url,
            fixture_port,
            pages: vec![],
            control,
            notes: vec![],
            temporary,
        })
    }

    fn panel_url(&self) -> String {
        format!("chrome-extension://{EXTENSION_ID}/panel.html")
    }

    /// Restores the seeded data files and clears the panel page storage so no scenario sees another's state.
    async fn reset(&mut self) -> Result<()> {
        // Hosts of the previous scenario's closed pages may still be persisting its state.
        let hosts = self.native_hosts()?;
        let waited = std::time::Instant::now();
        if let Err(error) = exited(&hosts).await {
            return Err(error.context(format!(
                "A native host of the previous scenario did not exit; {}",
                diagnose(&hosts)
            )));
        }
        if waited.elapsed() >= Duration::from_secs(2) {
            self.notes.push(format!(
                "  native host(s) {hosts:?} of the previous scenario took {:.1}s to exit",
                waited.elapsed().as_secs_f64()
            ));
        }
        for entry in fs::read_dir(&self.data)? {
            let entry = entry?;
            if entry.file_name() == ".profile-stores.lock" {
                continue;
            }
            // A still-exiting native host may remove its pending file meanwhile.
            let removed = entry.file_type().and_then(|kind| {
                if kind.is_file() {
                    fs::remove_file(entry.path())
                } else {
                    Ok(())
                }
            });
            if let Err(error) = removed
                && error.kind() != std::io::ErrorKind::NotFound
            {
                return Err(error).with_context(|| format!("Removing {}", entry.path().display()));
            }
        }
        for (path, contents) in &self.seeds {
            fs::write(path, contents)?;
        }
        match fs::remove_file(self.data.join(".agent/skills.json")) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err(error).context("Removing the Agent team skills setting");
            }
            _ => {}
        }
        // A failed `modules` scenario may leave a shipped module removed or another installed.
        // A payload without packages (an older release in an A/B run) has nothing to restore.
        let shipped = self.payload.join(services_installer::MODULES);
        let modules = self.data.join(services_installer::MODULES);
        if !shipped.exists() {
            return self.clear_storage().await;
        }
        fs::create_dir_all(&modules)?;
        for entry in fs::read_dir(&modules)? {
            let entry = entry?;
            if !shipped.join(entry.file_name()).exists() {
                fs::remove_dir_all(entry.path())
                    .with_context(|| format!("Removing {}", entry.path().display()))?;
            }
        }
        for entry in fs::read_dir(&shipped)? {
            let id = entry?.file_name();
            if fs::symlink_metadata(modules.join(&id)).is_err() {
                services_installer::copy_tree(&shipped.join(&id), &modules.join(&id))?;
            }
        }
        self.clear_storage().await
    }

    async fn clear_storage(&mut self) -> Result<()> {
        let origin = format!("chrome-extension://{EXTENSION_ID}");
        // The browser target rejects storage clearing; a page target may clear any storage key.
        self.cdp
            .call(
                "Storage.clearDataForStorageKey",
                json!({"storageKey":format!("{origin}/"),"storageTypes":"local_storage,indexeddb"}),
                Some(&self.control),
            )
            .await
            .context("Clearing page storage")?;
        Ok(())
    }

    async fn panel(&mut self) -> Result<String> {
        let url = self.panel_url();
        let page = self.cdp.page(&url, INIT).await?;
        self.pages.push(page.clone());
        let skills = self.temporary.path().join("team skills");
        if !skills.exists() {
            fs::create_dir_all(skills.join("shop-deploy"))?;
            fs::write(
                skills.join("shop-deploy/SKILL.md"),
                "---\nname: shop-deploy\ndescription: Deploys the fixture shop\n---\nCheck the shop.\n",
            )?;
            fs::create_dir_all(skills.join("broken"))?;
            fs::write(skills.join("broken/SKILL.md"), "---\nname: broken\n---\n")?;
        }
        let globals = format!(
            "globalThis.fixtureUrl = {}; globalThis.fixtureVersion = {}; globalThis.fixtureSkills = {};",
            json!(self.fixture_url),
            json!(env!("CARGO_PKG_VERSION")),
            json!(skills)
        );
        self.cdp.evaluate(&page, &globals).await?;
        Ok(page)
    }

    /// A fresh panel whose origin storage must not contain anything an earlier scenario left behind.
    async fn fresh_panel(&mut self) -> Result<String> {
        let page = self.panel().await?;
        let leaked = self
            .cdp
            .evaluate(
                &page,
                "(() => { const leaked = localStorage.getItem('__scenarioIsolation'); localStorage.setItem('__scenarioIsolation', 'used'); return leaked; })()",
            )
            .await?;
        ensure!(
            leaked.is_null(),
            "Page storage from an earlier scenario leaked into this one"
        );
        Ok(page)
    }

    /// Native hosts Chrome started for this harness; each exits once its panel pages close.
    fn native_hosts(&self) -> Result<Vec<u64>> {
        let marker = self
            .temporary
            .path()
            .file_name()
            .context("Harness directory name")?
            .to_string_lossy()
            .into_owned();
        let output = Command::new("/bin/ps")
            .args(["-axo", "pid=,command="])
            .output()?;
        ensure!(
            output.status.success(),
            "ps failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(host_pids(&String::from_utf8_lossy(&output.stdout), &marker))
    }

    async fn close_pages(&mut self) -> Result<()> {
        self.cdp.inspected_page = None;
        for page in std::mem::take(&mut self.pages) {
            self.cdp.close(&page).await?;
        }
        Ok(())
    }

    async fn run(&mut self, scenario: &Scenario) -> Result<()> {
        let (setup, script, receipt) = match scenario.kind {
            Kind::InstalledUpdate => return self.installed_update().await,
            Kind::CloseWindow => {
                self.reset().await?;
                return self.close_window().await;
            }
            Kind::Script {
                setup,
                script,
                receipt,
            } => (setup, script, receipt),
        };
        self.reset().await?;
        let mut page = self.fresh_panel().await?;
        match setup {
            Setup::Fresh => {}
            Setup::InspectedPage => {
                let inspected = self
                    .cdp
                    .page(&format!("{}/page-agent-fixture", self.fixture_url), "")
                    .await?;
                self.pages.push(inspected.clone());
                self.cdp.inspected_page = Some(inspected);
                self.cdp
                    .call("Page.bringToFront", json!({}), Some(&page))
                    .await?;
                self.cdp
                    .evaluate(
                        &page,
                        &format!(
                            "globalThis.pageAgentScript = {}; true",
                            serde_json::to_string(include_str!("../../ui/assets/page-agent.js"))?
                        ),
                    )
                    .await?;
            }
            Setup::Reopen(prepare) => {
                ensure!(
                    self.cdp
                        .evaluate(&page, prepare)
                        .await
                        .context("Scenario setup")?
                        == true,
                    "Scenario setup did not complete"
                );
                self.cdp
                    .call("Page.navigate", json!({"url":"about:blank"}), Some(&page))
                    .await?;
                self.cdp.close(&page).await?;
                self.pages.retain(|open| open != &page);
                page = self.panel().await?;
            }
        }
        let result = self.cdp.evaluate(&page, script).await?;
        ensure!(result == true, "Scenario script returned {result}");
        if let Some(receipt) = receipt {
            let receipt = self.cdp.evaluate(&page, receipt).await?;
            self.notes
                .push(format!("  {} receipt: {receipt}", scenario.label()));
        }
        Ok(())
    }

    async fn installed_update(&mut self) -> Result<()> {
        let root = self.temporary.path().join("update");
        let home = root.join("home");
        prepare_home(&home)?;
        let payload = extract(&self.archive, &root.join("package"))?;
        let base = home.join("Library/Application Support/ServicesDevTools");
        let data = base.join("data");
        seed_data(&data, self.fixture_port)?;
        services_installer::install(&home, &payload)
            .context("Install into the separate update home")?;
        let result = installed_reload(
            &self.chrome,
            &home,
            &base,
            &payload,
            &root.join("chrome.stderr"),
        )
        .await;
        let peers = sdk_peers(&data)?;
        exited(&peers)
            .await
            .context("SDK subprocess survived the update browser shutdown")?;
        self.cdp.errors.extend(result?);
        Ok(())
    }

    async fn close_window(&mut self) -> Result<()> {
        let first = self.fresh_panel().await?;
        let second = self.panel().await?;
        // The second window stays in its own temporary session so both windows never edit one saved session.
        self.cdp
            .evaluate(&second, "testDriver.session = 0; true")
            .await?;
        let mut owned = Vec::new();
        for page in [&first, &second] {
            self.cdp
                .call("Page.bringToFront", json!({}), Some(page))
                .await?;
            let pids = self
                .cdp
                .evaluate(page, include_str!("browser-close-window.js"))
                .await
                .context("Starting page-close fixture")?;
            let pids = pids
                .as_array()
                .context("Closing fixture did not return process IDs")?
                .iter()
                .map(|pid| {
                    pid.as_u64()
                        .filter(|pid| *pid > 1)
                        .context("Invalid closing fixture process ID")
                })
                .collect::<Result<Vec<_>>>()?;
            ensure!(
                pids.len() == 2,
                "Closing must own two concurrently running Terminal checks"
            );
            owned.push(pids);
        }
        for (index, page) in [&first, &second].into_iter().enumerate() {
            self.cdp
                .call("Page.navigate", json!({"url":"about:blank"}), Some(page))
                .await
                .with_context(|| format!("Closing Console fixture window {index}"))?;
            if let Err(error) = exited(&owned[index]).await {
                return Err(error.context(format!(
                    "Closing Console left a background Terminal process alive; {}",
                    diagnose(&owned[index])
                )));
            }
            if index == 0 {
                for pid in &owned[1] {
                    ensure!(
                        alive(*pid)?,
                        "Closing one Console must not stop another window's work"
                    );
                }
            }
        }
        Ok(())
    }

    /// Bytes the Copilot SDK fixture wrote to stderr so far.
    fn sdk_log_len(&self) -> u64 {
        fs::metadata(self.home.join("copilot-fixture.stderr")).map_or(0, |meta| meta.len())
    }

    fn sdk_log_since(&self, offset: u64) -> String {
        fs::read(self.home.join("copilot-fixture.stderr"))
            .ok()
            .and_then(|log| log.get(offset as usize..).map(<[u8]>::to_vec))
            .map(|log| String::from_utf8_lossy(&log).trim().to_owned())
            .unwrap_or_default()
    }

    /// Closes Chrome, then checks no Copilot SDK subprocess outlives it.
    /// Returns whether the SDK fixture was started by this harness.
    async fn finish(&mut self) -> Result<bool> {
        let closed = self.cdp.call("Browser.close", json!({}), None).await;
        drop(self.browser.take());
        closed?;
        let peers = sdk_peers(&self.data)?;
        exited(&peers)
            .await
            .context("SDK subprocess survived browser shutdown")?;
        Ok(!peers.is_empty())
    }
}

fn describe_exception(details: &Value) -> String {
    let text = details["exception"]["description"]
        .as_str()
        .or_else(|| details["text"].as_str())
        .unwrap_or("Uncaught exception");
    let location = details["url"]
        .as_str()
        .filter(|url| !url.is_empty())
        .map(|url| format!(" ({url}:{})", details["lineNumber"]))
        .unwrap_or_default();
    format!(
        "{}{location}",
        text.lines().take(6).collect::<Vec<_>>().join("\n        ")
    )
}

/// One scenario's result; its report is printed as one block so parallel jobs never interleave lines.
struct Outcome {
    scenario: Scenario,
    passed: bool,
    report: String,
}

/// Runs one scenario on `harness` and collects everything it reported.
async fn attempt(harness: &mut Harness, scenario: &Scenario, job: usize) -> Outcome {
    let label = scenario.label();
    let sdk_log = harness.sdk_log_len();
    let begin = std::time::Instant::now();
    let result = match timeout(scenario.timeout, harness.run(scenario)).await {
        Ok(result) => result,
        Err(_) => Err(anyhow::anyhow!(
            "Timed out after {}s",
            scenario.timeout.as_secs()
        )),
    };
    let closed = harness.close_pages().await;
    let errors = std::mem::take(&mut harness.cdp.errors);
    let result = result
        .and(closed.context("Closing scenario pages"))
        .and_then(|()| {
            ensure!(
                errors.is_empty(),
                "{} uncaught browser exception(s)",
                errors.len()
            );
            Ok(())
        });
    let seconds = begin.elapsed().as_secs_f64();
    let mut report = format!(
        "{} {:<18} {seconds:>6.1}s  [job {job}]\n",
        if result.is_ok() { "PASS" } else { "FAIL" },
        scenario.name
    );
    let mut line = |text: String| {
        report.push_str(&text);
        report.push('\n');
    };
    for note in std::mem::take(&mut harness.notes) {
        line(note);
    }
    for dialog in std::mem::take(&mut harness.cdp.dialogs) {
        line(format!(
            "  {label} page dialog handled: {} {}",
            dialog["type"].as_str().unwrap_or("?"),
            dialog["url"].as_str().unwrap_or("")
        ));
    }
    if let Err(error) = &result {
        line(format!("  {label} error: {error:#}"));
        if errors.is_empty() {
            line(format!("  {label} console errors: none"));
        } else {
            line(format!("  {label} console errors ({}):", errors.len()));
            for details in &errors {
                line(format!("      {}", describe_exception(details)));
            }
        }
        let sdk = harness.sdk_log_since(sdk_log);
        if !sdk.is_empty() {
            line(format!("  {label} SDK fixture stderr:\n{sdk}"));
        }
    }
    Outcome {
        scenario: *scenario,
        passed: result.is_ok(),
        report,
    }
}

/// Scenarios not yet started, shared by all parallel jobs.
struct Queue {
    pending: std::sync::Mutex<std::collections::VecDeque<Scenario>>,
    stop: std::sync::atomic::AtomicBool,
    fail_fast: bool,
}

impl Queue {
    fn next(&self) -> Option<Scenario> {
        if self.stop.load(std::sync::atomic::Ordering::SeqCst) {
            return None;
        }
        self.pending.lock().unwrap().pop_front()
    }

    fn record(&self, outcome: &Outcome) {
        print!("{}", outcome.report);
        if !outcome.passed && self.fail_fast {
            self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
            println!("Stopping after the first failure (--fail-fast)");
        }
    }
}

#[derive(Default)]
struct JobReport {
    outcomes: Vec<Outcome>,
    /// Setup or teardown failures that belong to no scenario.
    failures: Vec<String>,
    sdk_started: bool,
    /// Handed back instead of closed when the load-sensitive phase reuses it.
    harness: Option<Harness>,
}

impl JobReport {
    async fn start(&mut self, root: &Path, job: usize) -> Option<Harness> {
        let begin = std::time::Instant::now();
        match Harness::start(root).await {
            Ok(harness) => {
                println!(
                    "Job {job}: Chrome, install.sh, fixtures ready in {:.1}s",
                    begin.elapsed().as_secs_f64()
                );
                Some(harness)
            }
            Err(error) => {
                println!("FAIL job {job} setup: {error:#}");
                self.failures.push(format!("job {job} setup"));
                None
            }
        }
    }

    async fn run(&mut self, harness: &mut Harness, queue: &Queue, job: usize) {
        while let Some(scenario) = queue.next() {
            println!("RUN  {} [job {job}]", scenario.label());
            let outcome = attempt(harness, &scenario, job).await;
            queue.record(&outcome);
            self.outcomes.push(outcome);
        }
    }

    async fn finish(&mut self, mut harness: Harness, job: usize) {
        match harness.finish().await {
            Ok(started) => self.sdk_started |= started,
            Err(error) => {
                println!("FAIL job {job} teardown: {error:#}");
                self.failures.push(format!("job {job} teardown"));
            }
        }
    }
}

/// One parallel job: its own temporary HOME, Chrome profile, installation, fixture server and HTTP adapter.
async fn job(root: PathBuf, job: usize, queue: std::sync::Arc<Queue>, keep: bool) -> JobReport {
    let mut report = JobReport::default();
    let Some(mut harness) = report.start(&root, job).await else {
        return report;
    };
    report.run(&mut harness, &queue, job).await;
    if keep {
        report.harness = Some(harness);
    } else {
        report.finish(harness, job).await;
    }
    report
}

async fn acceptance(root: &Path, selection: &Selection, scenarios: &[Scenario]) -> Result<()> {
    let started = std::time::Instant::now();
    let (exclusive, mut parallel): (Vec<_>, Vec<_>) = scenarios
        .iter()
        .copied()
        .partition(|scenario| scenario.exclusive);
    // Longest budgets first, so a slow scenario never starts last and stretches the run.
    parallel.sort_by_key(|scenario| std::cmp::Reverse(scenario.timeout));
    let jobs = if parallel.is_empty() {
        0
    } else {
        selection.jobs(parallel.len())
    };
    println!(
        "Browser acceptance: {} scenario(s) on {jobs} parallel job(s){}",
        scenarios.len(),
        if exclusive.is_empty() {
            String::new()
        } else {
            format!(", then {} load-sensitive one(s) alone", exclusive.len())
        }
    );
    let queue = std::sync::Arc::new(Queue {
        pending: std::sync::Mutex::new(parallel.into()),
        stop: Default::default(),
        fail_fast: selection.fail_fast,
    });
    let handles: Vec<_> = (1..=jobs)
        .map(|index| {
            tokio::spawn(job(
                root.to_owned(),
                index,
                queue.clone(),
                index == 1 && !exclusive.is_empty(),
            ))
        })
        .collect();
    let mut reports = Vec::new();
    for handle in handles {
        reports.push(handle.await.context("Browser job panicked")?);
    }
    let kept = reports.iter_mut().find_map(|report| report.harness.take());
    if !exclusive.is_empty() {
        // Runs after every parallel job has finished, so wall-clock budgets are measured on an idle machine.
        let job = jobs.max(1);
        let alone = Queue {
            pending: std::sync::Mutex::new(exclusive.clone().into()),
            stop: std::sync::atomic::AtomicBool::new(
                queue.stop.load(std::sync::atomic::Ordering::SeqCst),
            ),
            fail_fast: selection.fail_fast,
        };
        let mut report = JobReport::default();
        let harness = match kept {
            Some(harness) => Some(harness),
            None => report.start(root, job).await,
        };
        if let Some(mut harness) = harness {
            report.run(&mut harness, &alone, job).await;
            report.finish(harness, job).await;
        }
        reports.push(report);
    }
    let outcomes: Vec<_> = reports.iter().flat_map(|report| &report.outcomes).collect();
    let mut failed: Vec<String> = outcomes
        .iter()
        .filter(|outcome| !outcome.passed)
        .map(|outcome| outcome.scenario.label())
        .collect();
    let failed_scenarios = failed.len();
    failed.extend(reports.iter().flat_map(|report| report.failures.clone()));
    let agent_ran = outcomes.iter().any(|outcome| outcome.scenario.sdk);
    if agent_ran && !reports.iter().any(|report| report.sdk_started) {
        println!("FAIL teardown: SDK fixture did not start");
        failed.push("SDK fixture".into());
    }
    println!(
        "Browser acceptance: {} passed, {failed_scenarios} failed, {} not run in {:.1}s",
        outcomes.len() - failed_scenarios,
        scenarios.len() - outcomes.len(),
        started.elapsed().as_secs_f64()
    );
    ensure!(
        failed.is_empty(),
        "Browser scenarios failed: {}",
        failed.join(", ")
    );
    Ok(())
}

pub fn run(root: &Path, selection: Selection) -> Result<()> {
    let scenarios = selection.select()?;
    if selection.list {
        for scenario in &scenarios {
            println!(
                "{:<18} {:>4}s  {}",
                scenario.name,
                scenario.timeout.as_secs(),
                scenario.summary
            );
        }
        return Ok(());
    }
    let example = root.join("rust/examples/echo").display().to_string();
    let packages = crate::target(root).join("browser-packages");
    let [file] = &crate::package_files(
        root,
        &[example, "--out".into(), packages.display().to_string()],
    )?[..] else {
        bail!("The example module must build one package file");
    };
    EXAMPLE.get_or_init(|| file.clone());
    // Jobs run on worker threads; the harness futures are large in debug builds, beyond tokio's 2 MiB default stack.
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(32 * 1024 * 1024)
        .build()?
        .block_on(async {
            timeout(SAFETY_CAP, acceptance(root, &selection, &scenarios))
                .await
                .with_context(|| {
                    format!(
                        "Browser acceptance exceeded the {}-minute safety cap",
                        SAFETY_CAP.as_secs() / 60
                    )
                })?
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn browser_command_isolates_keychain_and_profile_without_launching_chrome() {
        let chrome = Path::new("/fixture/chrome");
        let home = Path::new("/fixture/home");
        let profile = home.join("Chrome test profile");
        let extension = home.join("extension");
        let browser = browser_command(chrome, home, &profile, &extension);
        let args: Vec<_> = browser.get_args().collect();
        assert_eq!(browser.get_program(), chrome.as_os_str());
        for flag in [
            "--use-mock-keychain",
            "--password-store=basic",
            "--headless=new",
        ] {
            assert!(
                args.contains(&OsStr::new(flag)),
                "Missing isolation flag: {flag}"
            );
        }
        assert!(args.contains(&OsStr::new(
            "--user-data-dir=/fixture/home/Chrome test profile"
        )));
        assert!(args.contains(&OsStr::new("--load-extension=/fixture/home/extension")));
        assert!(
            browser.get_envs().any(|(key, value)| {
                key == OsStr::new("HOME") && value == Some(home.as_os_str())
            })
        );
    }

    fn selected(args: &[&str]) -> Vec<String> {
        let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        Selection::parse(&args)
            .unwrap()
            .select()
            .unwrap()
            .iter()
            .map(Scenario::label)
            .collect()
    }

    #[test]
    fn scenarios_are_unique_and_filterable() {
        let all = selected(&[]);
        let mut unique = all.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), all.len(), "Duplicate scenario labels");
        assert_eq!(
            selected(&["--scenario", "agent-resume", "--scenario", "stop"]),
            ["agent-resume", "stop"]
        );
        assert_eq!(selected(&["--scenario", "history"]), ["history"]);
        let args = |args: &[&str]| args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
        assert!(
            Selection::parse(&args(&["--scenario", "missing"]))
                .unwrap()
                .select()
                .is_err()
        );
        assert!(Selection::parse(&args(&["--scenario"])).is_err());
    }

    #[test]
    fn jobs_are_bounded_and_timing_scenarios_run_alone() {
        let parse = |args: &[&str]| {
            Selection::parse(&args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>())
        };
        assert_eq!(parse(&["--jobs", "3"]).unwrap().jobs(10), 3);
        assert_eq!(
            parse(&["--jobs", "8"]).unwrap().jobs(2),
            2,
            "never more jobs than scenarios"
        );
        let default = parse(&[]).unwrap().jobs(100);
        assert!((1..=MAX_DEFAULT_JOBS).contains(&default));
        for invalid in [&["--jobs"][..], &["--jobs", "0"], &["--jobs", "many"]] {
            assert!(parse(invalid).is_err(), "{invalid:?} must be rejected");
        }
        let mut exclusive: Vec<_> = scenarios()
            .into_iter()
            .filter(|scenario| scenario.exclusive)
            .map(|scenario| scenario.label())
            .collect();
        exclusive.sort();
        assert_eq!(
            exclusive,
            [
                "close-window",
                "layout",
                "page-tools",
                "perf-baseline",
                "table-performance"
            ]
        );
        let sdk: Vec<_> = scenarios()
            .into_iter()
            .filter(|scenario| scenario.sdk)
            .map(|scenario| scenario.label())
            .collect();
        assert_eq!(
            sdk,
            ["agent", "page-agent", "agent-resume", "stop"],
            "only scenarios that send Agent prompts require the SDK fixture"
        );
    }

    #[test]
    fn native_hosts_are_found_only_in_their_own_harness() {
        let ps = format!(
            "87792 /private/var/folders/T/services-browser-'test-NqVNYT/home/Library/Application Support/ServicesDevTools/extension/native/bin/services-devtools __native-host-shell chrome-extension://{EXTENSION_ID}/\n\
             87811 /Applications/Chrome.app/Contents/MacOS/Chrome --user-data-dir=/private/var/folders/T/services-browser-'test-NqVNYT/home/profile\n\
             98894 /Users/me/Library/Application Support/ServicesDevTools/extension/native/bin/services-devtools __native-host-shell chrome-extension://{EXTENSION_ID}/\n\
             98895 /private/var/folders/T/services-browser-'test-Other1/home/Library/Application Support/ServicesDevTools/extension/native/bin/services-devtools native-host chrome-extension://{EXTENSION_ID}/"
        );
        assert_eq!(host_pids(&ps, "services-browser-'test-NqVNYT"), [87792]);
        assert_eq!(host_pids(&ps, "services-browser-'test-Other1"), [98895]);
    }

    #[test]
    fn legacy_commands_map_to_groups() {
        let labels = |selection: Selection| -> Vec<String> {
            selection
                .select()
                .unwrap()
                .iter()
                .map(Scenario::label)
                .collect()
        };
        assert_eq!(
            labels(Selection::group(Group::Settings)),
            ["updates", "modules"]
        );
        let agent = labels(Selection::group(Group::Agent));
        for expected in [
            "agent",
            "page-agent",
            "agent-resume",
            "stop",
            "close-window",
        ] {
            assert!(agent.iter().any(|label| label == expected), "{expected}");
        }
        assert!(!agent.iter().any(|label| label.ends_with("/sessions")));
        assert_eq!(labels(Selection::all()).len(), scenarios().len());
    }
}
