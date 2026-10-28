use anyhow::{Context, Result, bail, ensure};
use axum::{Json, Router, extract::Request, response::IntoResponse};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};
use tokio::{net::TcpStream, time::timeout};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message};

const EXTENSION_ID: &str = "ckicpgdfkgcmgnclbfbcbkillcpaffnb";
const WAIT: Duration = Duration::from_secs(20);

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
    errors: Vec<Value>,
    inspected_page: Option<String>,
}

impl Cdp {
    async fn call(&mut self, method: &str, params: Value, session: Option<&str>) -> Result<Value> {
        self.next += 1;
        let id = self.next;
        let mut request = json!({"id":id,"method":method,"params":params});
        if let Some(session) = session {
            request["sessionId"] = json!(session);
        }
        self.socket
            .send(Message::Text(request.to_string().into()))
            .await?;
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
                    .context("Chrome CDP disconnected")??;
                let Message::Text(text) = message else {
                    continue;
                };
                let reply: Value = serde_json::from_str(&text)?;
                if let Some(id) = reply["id"].as_u64() {
                    self.replies.insert(id, reply);
                } else if reply["method"] == "Runtime.exceptionThrown" {
                    self.errors
                        .push(reply["params"]["exceptionDetails"].clone());
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
                            json!({"source":include_str!("browser-init.js"),"runImmediately":true}),
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
                            json!({"expression":command["params"]["expression"],"returnByValue":true,"silent":true}),
                            Some(&inspected))).await {
                            Ok(result) => result,
                            Err(error) => json!({"exceptionDetails":{"text":format!("Inspected-page fixture evaluation failed: {error:#}")}}),
                        }
                    } else if method == "Test.inspectedCommand" {
                        let inspected = self.inspected_page.clone().context("Missing inspected fixture page")?;
                        let method = command["params"]["method"].as_str().context("Missing inspected command")?;
                        ensure!(matches!(method, "Input.dispatchMouseEvent" | "Input.dispatchKeyEvent" | "Input.insertText" | "Page.captureScreenshot"),
                            "Unexpected inspected-page test command: {method}");
                        Box::pin(self.call(method, command["params"]["params"].clone(), Some(&inspected))).await?
                    } else {
                        Box::pin(self.call(method, command["params"].clone(), Some(&target))).await?
                    };
                    Box::pin(self.call(
                        "Runtime.evaluate",
                        json!({
                            "contextId":reply["params"]["executionContextId"],
                            "expression":format!("__testResolve({}, {})", command["id"], result),
                        }),
                        Some(&target),
                    ))
                    .await?;
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
            errors: vec![],
            inspected_page: None,
        },
    ))
}

async fn installed_reload(
    chrome: &Path,
    home: &Path,
    base: &Path,
    payload: &Path,
    log: &Path,
) -> Result<()> {
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
        .page(&url, include_str!("browser-init.js"))
        .await
        .context("Opening physical installed extension before update")?;
    let before = cdp.evaluate(&page,
        "(async () => { localStorage.setItem('__reload_probe', 'kept'); return { version: chrome.runtime.getManifest().version, probe: await (await fetch(chrome.runtime.getURL('reload-probe.txt'), {cache:'no-store'})).text() }; })()"
    ).await?;
    cdp.evaluate(&page, r##"(async () => {
      await testDriver.wait(() => window.document.querySelector('[data-testid="console-session-name"]'),
        () => `Reload fixture did not open Console: ${window.document.body.innerText}`);
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
    })()"##).await?;
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
            json!(archive)
        ),
    )
    .await?;
    ensure!(cdp.evaluate(&page, r##"(async () => {
      const {wait,click,testid,cm,assert}=testDriver;
      await wait(()=>document.querySelector('[data-testid="console-editor"]')?.CodeMirror
        || document.querySelector('[data-testid="console-editor"]')?.nextElementSibling?.CodeMirror);
      cm('console-editor').setValue('#check update persistence\n');
      await click(testid('console-sessions-toggle'));
      await wait(()=>testid('console-sessions').dataset.busy==='false');
      await click(testid('console-settings-toggle'));
      await wait(()=>!testid('settings-update-status').textContent.includes('Checking'));
      await click(testid('settings-update-check'));
      await wait(()=>document.querySelector('[data-testid="settings-update-install"]'),
        ()=>`Approved update did not become available: ${document.body.innerText}; requests=${JSON.stringify(updateFixture.requests)}; release=${JSON.stringify(updateFixture.release)}`);
      updateFixture.reloads=0;
      chrome.runtime.reload=()=>{updateFixture.reloads++;};
      await click(testid('settings-update-install'));
      await wait(()=>updateFixture.reloads===1,
        ()=>`Client updater failed: ${document.body.innerText}`, 60000);
      const saved=await testDriver.sessionLibrary();
      assert(JSON.stringify(saved.temporary.console.documents).includes('#check update persistence\\n'),
        'Workspace edits must be durably saved before installing and reloading');
      assert(updateFixture.installs.length===1,
        'An update must request exactly one native download and installation');
      await testDriver.clearSessions();
      return true;
    })()"##).await? == true, "Client-controlled native archive update did not complete");
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
    let reloaded = match cdp.page(&url, include_str!("browser-init.js")).await {
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
    Ok(())
}

async fn acceptance(root: &Path, agent_only: bool) -> Result<()> {
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
    fs::create_dir_all(&home)?;
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
    let unpacked = temporary.path().join("package");
    fs::create_dir(&unpacked)?;
    let status = Command::new("/usr/bin/tar")
        .arg("-xzf")
        .arg(&archive)
        .arg("-C")
        .arg(&unpacked)
        .status()?;
    ensure!(
        status.success(),
        "Extract browser acceptance payload: {status}"
    );
    let extension = unpacked.join("ServicesDevTools/extension");
    let base = home.join("Library/Application Support/ServicesDevTools");
    let data = base.join("data");
    fs::create_dir_all(&data)?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let fixture_url = format!("http://{}", listener.local_addr()?);
    fs::write(data.join(".variable-profiles.json"), json!({
        "order":["default","second","var"],
        "profiles":{"default":{"TOKEN":"fixture","REF":"{{second:TOKEN}}"},"second":{"TOKEN":"resolved-fixture"},
            "var":{
                "sku":"fixture-materialized-sku",
                "magento":format!("localhost:{}",listener.local_addr()?.port()),
                "magentoBase":format!("http://localhost:{}",listener.local_addr()?.port()),
                "magentoToken":format!("http://localhost:{}/rest/default/V1/integration/admin/token", listener.local_addr()?.port())
            }},
    }).to_string())?;
    fs::write(
        data.join(".console-hints.txt"),
        [
            ("mysql", "mysql://mysql.fixture.invalid"),
            ("rest", "https://rest.fixture.invalid"),
            ("graphql", "https://graphql.fixture.invalid"),
            ("grpc", "grpc://grpc.fixture.invalid:9000"),
            ("splunk", "https://splunk.fixture.invalid"),
        ]
        .into_iter()
        .map(|(service, url)| format!("## {service}\n#endpoint\n{url}\n"))
        .collect::<String>(),
    )?;
    let (browser, mut cdp) = launch_browser(
        &chrome,
        &home,
        &profile,
        &extension,
        &temporary.path().join("chrome.stderr"),
    )
    .await?;
    let init = include_str!("browser-init.js");
    let setup = cdp
        .page(
            &format!("chrome-extension://{EXTENSION_ID}/panel.html"),
            init,
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
    if !agent_only {
        installed_reload(
            &chrome,
            &home,
            &base,
            &unpacked.join("ServicesDevTools"),
            &temporary.path().join("chrome-reload.stderr"),
        )
        .await?;
    }
    cdp.evaluate(&setup, "(async () => { await testDriver.click(testDriver.button('Retry connection')); await testDriver.wait(() => document.querySelector('[data-testid=\"native-status\"]').hidden); return true; })()").await?;
    let native = cdp
        .page(
            &format!("chrome-extension://{EXTENSION_ID}/panel.html"),
            init,
        )
        .await?;
    let agent_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count_read = agent_count.clone();
    let stop_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let stop_read = stop_count.clone();
    let router = Router::new()
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
    });
    let _fixture = Fixture(tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("Browser HTTP fixture failed");
    }));
    cdp.evaluate(
        &native,
        &format!(
            "globalThis.fixtureUrl = {}; globalThis.fixtureVersion = {};",
            json!(fixture_url),
            json!(env!("CARGO_PKG_VERSION"))
        ),
    )
    .await?;
    if !agent_only {
        ensure!(
            cdp.evaluate(&native, include_str!("browser-sessions.js"))
                .await
                .context("Native sessions browser acceptance")?
                == true,
            "Native sessions assertions did not complete"
        );
        let result = cdp
            .evaluate(&native, include_str!("browser-native.js"))
            .await
            .context("Native Console browser acceptance")?;
        ensure!(result == true, "Native browser assertions did not complete");
        ensure!(
            cdp.evaluate(&native, include_str!("browser-templates.js"))
                .await
                .context("Native global Templates acceptance")?
                == true,
            "Native Templates assertions did not complete"
        );
        ensure!(
            cdp.evaluate(&native, include_str!("browser-layout.js"))
                .await
                .context("Native check spacing and session layout acceptance")?
                == true,
            "Native layout assertions did not complete"
        );
        ensure!(
            cdp.evaluate(&native, include_str!("browser-updates.js"))
                .await?
                == true,
            "Authenticated update checks and traffic-light assertions did not complete"
        );
        ensure!(
            cdp.evaluate(&native, include_str!("browser-history.js"))
                .await
                .context("Native execution History acceptance")?
                == true,
            "Native History assertions did not complete"
        );
        println!("Checking native typed responses and JSON folding");
        ensure!(
            cdp.evaluate(&native, include_str!("browser-splunk.js"))
                .await
                .context("Native streaming Splunk export acceptance")?
                == true,
            "Native Splunk export assertions did not complete"
        );
        println!("Checking native table and JSON folding performance");
        ensure!(
            cdp.evaluate(&native, include_str!("browser-table-performance.js"))
                .await
                .context("Native large typed table performance acceptance")?
                == true,
            "Native table performance assertions did not complete"
        );
        println!(
            "Native table performance: {}",
            cdp.evaluate(&native, "globalThis.tablePerformanceReceipt")
                .await?
        );
    }
    ensure!(
        cdp.evaluate(&native, include_str!("browser-agent.js"))
            .await
            .context("Native Agent browser acceptance")?
            == true,
        "Native Copilot SDK acceptance did not complete"
    );
    let inspected = cdp
        .page(&format!("{fixture_url}/page-agent-fixture"), "")
        .await?;
    cdp.inspected_page = Some(inspected);
    cdp.call("Page.bringToFront", json!({}), Some(&native))
        .await?;
    ensure!(
        cdp.evaluate(&native, include_str!("browser-page-agent.js"))
            .await
            .with_context(|| format!(
                "Inspected-page Agent acceptance; SDK fixture stderr: {}",
                fs::read_to_string(home.join("copilot-fixture.stderr")).unwrap_or_default()
            ))?
            == true,
        "Inspected-page Agent assertions did not complete"
    );
    cdp.evaluate(&native, r#"(() => {
        const key = 'servicesConsole.agentSession.' + localStorage.getItem('servicesConsole.agentActive');
        const chat = JSON.parse(localStorage.getItem(key));
        chat.messages.push({id:'legacy-empty',role:'assistant',text:' \n '});
        localStorage.setItem(key, JSON.stringify(chat));
    })()"#).await?;
    cdp.call("Page.navigate", json!({"url":"about:blank"}), Some(&native))
        .await?;
    let native_resume = cdp
        .page(
            &format!("chrome-extension://{EXTENSION_ID}/panel.html"),
            init,
        )
        .await?;
    ensure!(
        cdp.evaluate(&native_resume, include_str!("browser-agent-resume.js"))
            .await
            .context("Native Agent resume acceptance")?
            == true,
        "Native Copilot session persistence did not complete"
    );
    cdp.evaluate(
        &native_resume,
        &format!("globalThis.fixtureUrl = {};", json!(fixture_url)),
    )
    .await?;
    ensure!(
        cdp.evaluate(&native_resume, include_str!("browser-stop.js"))
            .await
            .context("Native scoped Stop acceptance")?
            == true,
        "Native scoped Stop and hidden-panel cleanup did not complete"
    );
    let probe = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = probe.local_addr()?.port();
    drop(probe);
    let _web = Process(
        command(base.join("extension/native/bin/services-devtools"), &home)
            .args(["web", "--port", &port.to_string()])
            .env("SERVICES_DATA_DIR", &data)
            .env("SERVICES_UI_DIR", base.join("extension"))
            .env("SERVICES_COPILOT_PATH", &copilot)
            .env("SERVICES_DEVTOOLS_ENV_PROBE", "companion-fixture-only")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(temporary.path().join("web.stderr"))?)
            .spawn()?,
    );
    let url = format!("http://127.0.0.1:{port}/panel.html");
    timeout(WAIT, async {
        loop {
            if TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("Rust HTTP adapter did not start")?;
    let web = cdp.page(&url, init).await?;
    cdp.evaluate(
        &web,
        &format!(
            "globalThis.fixtureUrl = {}; globalThis.fixtureVersion = {}; testDriver.graphqlExplore = {};",
            json!(fixture_url),
            json!(env!("CARGO_PKG_VERSION")),
            include_str!("../../ui/tests/fixtures/graphql-explore.json")
        ),
    )
    .await?;
    if !agent_only {
        ensure!(
            cdp.evaluate(&web, include_str!("browser-sessions.js"))
                .await
                .context("HTTP sessions browser acceptance")?
                == true,
            "HTTP sessions assertions did not complete"
        );
        ensure!(
            cdp.evaluate(&web, include_str!("browser-web.js"))
                .await
                .context("HTTP Console browser acceptance")?
                == true,
            "Web browser assertions did not complete"
        );
        ensure!(
            cdp.evaluate(&web, include_str!("browser-templates.js"))
                .await
                .context("HTTP global Templates acceptance")?
                == true,
            "HTTP Templates assertions did not complete"
        );
        ensure!(
            cdp.evaluate(&web, include_str!("browser-layout.js"))
                .await
                .context("HTTP check spacing and session layout acceptance")?
                == true,
            "HTTP layout assertions did not complete"
        );
        ensure!(
            cdp.evaluate(&web, include_str!("browser-history.js"))
                .await
                .context("HTTP execution History acceptance")?
                == true,
            "HTTP History assertions did not complete"
        );
        println!("Checking HTTP typed responses and JSON folding");
        ensure!(
            cdp.evaluate(&web, include_str!("browser-splunk.js"))
                .await
                .context("HTTP streaming Splunk export acceptance")?
                == true,
            "HTTP Splunk export assertions did not complete"
        );
        println!("Checking HTTP table and JSON folding performance");
        ensure!(
            cdp.evaluate(&web, include_str!("browser-table-performance.js"))
                .await
                .context("HTTP large typed table performance acceptance")?
                == true,
            "HTTP table performance assertions did not complete"
        );
        println!(
            "HTTP table performance: {}",
            cdp.evaluate(&web, "globalThis.tablePerformanceReceipt")
                .await?
        );
    }
    ensure!(
        cdp.evaluate(&web, include_str!("browser-agent.js"))
            .await
            .context("HTTP Agent browser acceptance")?
            == true,
        "HTTP Copilot SDK acceptance did not complete"
    );
    cdp.call("Page.navigate", json!({"url":"about:blank"}), Some(&web))
        .await?;
    let web_resume = cdp.page(&url, init).await?;
    ensure!(
        cdp.evaluate(&web_resume, include_str!("browser-agent-resume.js"))
            .await
            .context("HTTP Agent resume acceptance")?
            == true,
        "HTTP Copilot session persistence did not complete"
    );
    cdp.evaluate(
        &web_resume,
        &format!("globalThis.fixtureUrl = {};", json!(fixture_url)),
    )
    .await?;
    ensure!(
        cdp.evaluate(&web_resume, include_str!("browser-stop.js"))
            .await
            .context("HTTP scoped Stop acceptance")?
            == true,
        "HTTP scoped Stop and hidden-panel cleanup did not complete"
    );
    let mut closing = Vec::new();
    for session in [&native_resume, &web_resume] {
        cdp.call("Page.bringToFront", json!({}), Some(session))
            .await?;
        let pids = cdp
            .evaluate(session, "startClosingStopFixture()")
            .await
            .with_context(|| format!("Starting page-close fixture for {session}"))?;
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
        closing.push(pids);
    }
    for (index, session) in [&native_resume, &web_resume].into_iter().enumerate() {
        cdp.call("Page.navigate", json!({"url":"about:blank"}), Some(session))
            .await?;
        timeout(WAIT, async {
            loop {
                let mut running = false;
                for pid in &closing[index] {
                    running |= Command::new("/bin/ps")
                        .args(["-p", &pid.to_string()])
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status()?
                        .success();
                }
                if !running {
                    return Ok::<(), anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .context("Closing Console left a background Terminal process alive")??;
        if index == 0 {
            for pid in &closing[1] {
                ensure!(
                    Command::new("/bin/ps")
                        .args(["-p", &pid.to_string()])
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status()?
                        .success(),
                    "Closing one Console must not stop another window's work"
                );
            }
        }
    }
    ensure!(
        cdp.errors.is_empty(),
        "Uncaught browser exceptions: {:?}",
        cdp.errors
    );
    cdp.call("Browser.close", json!({}), None).await?;
    drop(cdp);
    drop(browser);
    drop(_web);
    let peers = fs::read_dir(data.join(".agent"))?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("acceptance-peer-")
        })
        .map(|entry| fs::read_to_string(entry.path()).context("Read SDK acceptance PID"))
        .collect::<Result<Vec<_>>>()?;
    ensure!(!peers.is_empty(), "SDK fixture did not start");
    timeout(WAIT, async {
        loop {
            let mut running = false;
            for peer in &peers {
                let pid = peer.parse::<u32>()?;
                ensure!(pid > 1, "Invalid SDK fixture PID");
                running |= Command::new("/bin/ps")
                    .args(["-p", &pid.to_string()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()?
                    .success();
            }
            if !running {
                return Ok::<(), anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("SDK subprocess survived browser/gateway shutdown")??;
    if agent_only {
        println!(
            "PASS: native and HTTP Copilot SDK tools, live edits/Undo, execution approvals/Stop/conflicts, response reads, inspected-page DOM bridge contract/consent, session persistence/resume and subprocess cleanup"
        );
    } else {
        println!(
            "PASS: archived Rust/WASM UI; automatic running-Chrome registration; native and HTTP/Terminal requests; nine service views; isolated Console sessions, empty startup, copies/deletion, drafts, request-local var, global Dictionary/Agent, busy guards and cross-window conflicts; live Copilot SDK tools, Undo, approval/denial/Stop, response reads and chat resume; inspected-page DOM bridge contract/consent; schema hints and clipboard policies; installed extension update/reload preserves imported storage (no Node.js)"
        );
    }
    Ok(())
}

pub fn run(root: &Path, agent_only: bool) -> Result<()> {
    tokio::runtime::Runtime::new()?.block_on(async {
        timeout(Duration::from_secs(600), acceptance(root, agent_only))
            .await
            .context("Browser acceptance exceeded ten minutes")?
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
}
