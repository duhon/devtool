use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use services_contracts::{HOST_NAME, PROTOCOL_VERSION};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    sync::mpsc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

mod browser_profiles;
pub mod quarantine;
pub mod updates;

const APPLICATION: &str = "ServicesDevTools";
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub fn unique_id() -> String {
    format!(
        "{}-{}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

pub fn extension_id(key: &str) -> Result<String> {
    let bytes = STANDARD
        .decode(key)
        .context("Invalid public extension key")?;
    ensure!(!bytes.is_empty(), "Missing public extension key");
    spki::SubjectPublicKeyInfoRef::try_from(bytes.as_slice())
        .map_err(|error| anyhow::anyhow!("Invalid public extension key: {error}"))?;
    Ok(Sha256::digest(bytes)[..16]
        .iter()
        .flat_map(|byte| {
            [
                char::from(b'a' + (byte >> 4)),
                char::from(b'a' + (byte & 15)),
            ]
        })
        .collect())
}

pub fn copy_tree(source: &Path, target: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    ensure!(
        !metadata.file_type().is_symlink(),
        "Unexpected payload symlink: {}",
        source.display()
    );
    if metadata.is_dir() {
        fs::create_dir_all(target)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_tree(&entry.path(), &target.join(entry.file_name()))?;
        }
    } else {
        fs::copy(source, target)?;
        fs::set_permissions(target, metadata.permissions())?;
    }
    quarantine::copy(source, target)?;
    Ok(())
}

#[derive(Clone, Debug)]
pub struct Locations {
    pub home: PathBuf,
    pub base: PathBuf,
    pub data: PathBuf,
    pub releases: PathBuf,
    pub current: PathBuf,
    pub state: PathBuf,
    pub registration: PathBuf,
}

pub fn locations(home: &Path) -> Locations {
    let base = home.join("Library/Application Support").join(APPLICATION);
    Locations {
        home: home.to_owned(),
        data: base.join("data"),
        releases: base.join("releases"),
        current: base.join("current"),
        state: base.join("install-state.json"),
        registration: home
            .join("Library/Application Support/Google/Chrome/NativeMessagingHosts")
            .join(format!("{HOST_NAME}.json")),
        base,
    }
}

fn read_json(path: &Path) -> Result<Value> {
    serde_json::from_slice(&fs::read(path).with_context(|| format!("Read {}", path.display()))?)
        .with_context(|| format!("Invalid JSON in {}", path.display()))
}

fn remove_file_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn atomic_write(file: &Path, content: &[u8], mode: u32) -> Result<()> {
    let temporary = file.with_extension(format!("{}.tmp", unique_id()));
    let result = (|| {
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(mode)
            .open(&temporary)?;
        output.set_permissions(fs::Permissions::from_mode(mode))?;
        output.write_all(content)?;
        output.sync_all()?;
        fs::rename(&temporary, file)?;
        Ok(())
    })();
    let cleanup = remove_file_if_present(&temporary);
    result.and(cleanup)
}

#[derive(Clone, Copy, PartialEq)]
enum ExtensionPath {
    Missing,
    LegacyLink,
    Directory,
}

fn installed_extension(paths: &Locations, state: Option<&State>) -> Result<ExtensionPath> {
    let extension = paths.base.join("extension");
    let info = match fs::symlink_metadata(&extension) {
        Ok(info) => info,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ExtensionPath::Missing);
        }
        Err(error) => return Err(error.into()),
    };
    let state = state.with_context(|| {
        format!(
            "Refusing to overwrite an unmanaged path: {}",
            extension.display()
        )
    })?;
    if info.is_symlink() {
        ensure!(
            fs::read_link(&extension)? == Path::new("current/extension"),
            "Refusing to replace an unmanaged extension symlink: {}",
            extension.display()
        );
        return Ok(ExtensionPath::LegacyLink);
    }
    ensure!(
        info.is_dir(),
        "Unmanaged extension path: {}",
        extension.display()
    );
    let manifest = read_json(&extension.join("manifest.json"))?;
    let id = extension_id(
        manifest["key"]
            .as_str()
            .context("Missing installed extension key")?,
    )?;
    ensure!(
        id == state.extension_id,
        "Installed extension identity differs"
    );
    Ok(ExtensionPath::Directory)
}

fn remove_extension_path(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(info) if info.is_symlink() => fs::remove_file(path)?,
        Ok(info) if info.is_dir() => fs::remove_dir_all(path)?,
        Ok(_) => bail!("Unexpected extension path: {}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn legacy_paths(paths: &Locations, state: Option<&State>) -> Result<Vec<PathBuf>> {
    let mut obsolete = Vec::new();
    for name in ["native", "current", "releases"] {
        let path = paths.base.join(name);
        let info = match fs::symlink_metadata(&path) {
            Ok(info) => info,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        state.context("Refusing to remove an unmanaged legacy installation")?;
        if name == "releases" {
            ensure!(
                info.is_dir(),
                "Unmanaged releases directory: {}",
                path.display()
            );
        } else {
            ensure!(
                info.is_symlink(),
                "Unmanaged legacy path: {}",
                path.display()
            );
            let target = fs::read_link(&path)?;
            if name == "native" {
                ensure!(
                    target == Path::new("current/native"),
                    "Unmanaged native symlink"
                );
            } else {
                let build = target.file_name().and_then(|name| name.to_str());
                ensure!(
                    target.parent() == Some(Path::new("releases"))
                        && build.is_some_and(valid_build_id),
                    "Unmanaged current symlink"
                );
            }
        }
        obsolete.push(path);
    }
    Ok(obsolete)
}

struct Lock(PathBuf);
impl Lock {
    fn acquire(base: &Path) -> Result<Self> {
        let file = base.join("install.lock");
        fs::create_dir(&file).with_context(|| {
            format!(
                "Another installation may be running. Lock: {}",
                file.display()
            )
        })?;
        Ok(Self(file))
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir(&self.0) {
            eprintln!(
                "Could not remove installation lock {}: {error}",
                self.0.display()
            );
        }
    }
}

fn valid_build_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

pub fn validate_payload(payload: &Path) -> Result<Value> {
    let metadata = read_json(&payload.join("distribution.json"))?;
    ensure!(
        metadata["application"] == APPLICATION
            && metadata["formatVersion"] == 1
            && metadata["protocolVersion"] == PROTOCOL_VERSION
            && metadata["platform"] == "darwin"
            && metadata["arch"] == "arm64"
            && metadata["runtime"] == "rust"
            && metadata["buildId"].as_str().is_some_and(valid_build_id),
        "Unsupported distribution metadata"
    );
    let manifest = read_json(&payload.join("extension/manifest.json"))?;
    let id = extension_id(
        manifest["key"]
            .as_str()
            .context("Missing public extension key")?,
    )?;
    ensure!(
        metadata["extensionId"] == id
            && metadata["version"] == manifest["version"]
            && manifest["manifest_version"] == 3
            && manifest["permissions"]
                .as_array()
                .is_some_and(|values| values.contains(&json!("nativeMessaging"))),
        "Extension identity or version does not match the distribution"
    );
    for name in ["services-devtools", "services-install"] {
        let file = payload.join("native/bin").join(name);
        ensure!(
            fs::metadata(&file)?.permissions().mode() & 0o111 != 0,
            "Payload executable is not executable: {}",
            file.display()
        );
    }
    Ok(metadata)
}

pub fn check_host(binary: &Path, data: &Path) -> Result<()> {
    let body = serde_json::to_vec(&json!({"id":1,"type":"health"}))?;
    let mut child = Command::new(binary)
        .env("SERVICES_DATA_DIR", data)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut input = child.stdin.take().context("Open host stdin")?;
    input.write_all(&(body.len() as u32).to_le_bytes())?;
    input.write_all(&body)?;
    let mut stdout = child.stdout.take().context("Open host stdout")?;
    let (sender, receiver) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let reply: Result<Vec<u8>> = (|| {
            let mut header = [0; 4];
            stdout.read_exact(&mut header)?;
            let size = u32::from_le_bytes(header) as usize;
            ensure!(size <= 65536, "Invalid native health response size");
            let mut body = vec![0; size];
            stdout.read_exact(&mut body)?;
            Ok(body)
        })();
        if sender.send(reply).is_err() {
            eprintln!("Native health response receiver was closed");
        }
    });
    let received = receiver.recv_timeout(Duration::from_secs(20));
    drop(input);
    if received.is_err() && child.try_wait()?.is_none() {
        child.kill()?;
    }
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() > Duration::from_secs(20) {
            child.kill()?;
            child.wait()?;
            bail!("Native host self-check timed out");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    reader
        .join()
        .map_err(|_| anyhow::anyhow!("Native health reader panicked"))?;
    let mut error = String::new();
    child
        .stderr
        .take()
        .context("Open host stderr")?
        .read_to_string(&mut error)?;
    ensure!(
        !matches!(received, Err(mpsc::RecvTimeoutError::Timeout)),
        "Native host self-check received no health reply within 20 seconds: {status}: {error}"
    );
    ensure!(
        status.success(),
        "Native host self-check failed: {status}: {error}"
    );
    let output = received.context("Native host self-check timed out")??;
    let reply: Value = serde_json::from_slice(&output)?;
    ensure!(
        reply["id"] == 1
            && reply["ok"] == true
            && reply["result"]["protocolVersion"] == PROTOCOL_VERSION,
        "Incompatible native host protocol"
    );
    Ok(())
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Registration {
    path: PathBuf,
    previous_registration: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct State {
    application: String,
    extension_id: String,
    build_id: String,
    previous_registration: Option<String>,
    #[serde(default)]
    registrations: Vec<Registration>,
}

impl State {
    fn registrations(&self, paths: &Locations) -> Vec<Registration> {
        if self.registrations.is_empty() {
            vec![Registration {
                path: paths.registration.clone(),
                previous_registration: self.previous_registration.clone(),
            }]
        } else {
            self.registrations.clone()
        }
    }
}

fn registration_content(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(content) => Ok(Some(content)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("Read registration {}", path.display())),
    }
}

fn registration_manifest(paths: &Locations, id: &str) -> Value {
    json!({
        "name":HOST_NAME,"description":"Services DevTools native-messaging host",
        "path":paths.base.join("extension/native/host"),"type":"stdio",
        "allowed_origins":[format!("chrome-extension://{id}/")],
    })
}

fn owned_registration(content: &str, paths: &Locations, id: &str) -> bool {
    serde_json::from_str::<Value>(content).is_ok_and(|value| {
        let expected = registration_manifest(paths, id);
        ["name", "type", "allowed_origins"]
            .iter()
            .all(|key| value[key] == expected[key])
            && (value["path"] == expected["path"]
                || value["path"] == json!(paths.base.join("native/host")))
    })
}

fn registration_root(path: &Path) -> Result<&Path> {
    let parent = path.parent().context("Missing registration directory")?;
    ensure!(
        path.file_name()
            .is_some_and(|name| name == format!("{HOST_NAME}.json").as_str())
            && parent
                .file_name()
                .is_some_and(|name| name == "NativeMessagingHosts"),
        "Invalid managed registration path: {}",
        path.display()
    );
    parent
        .parent()
        .context("Missing browser user-data directory")
}

fn normalized_registration(path: &Path) -> Result<PathBuf> {
    let root = registration_root(path)?;
    let root = match fs::canonicalize(root) {
        Ok(root) => root,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => std::path::absolute(root)?,
        Err(error) => return Err(error.into()),
    };
    Ok(root
        .join("NativeMessagingHosts")
        .join(format!("{HOST_NAME}.json")))
}

fn existing_chrome_registrations(home: &Path, paths: &Locations, id: &str) -> Result<Vec<PathBuf>> {
    let support = home.join("Library/Application Support");
    let mut roots = vec![support.join("Chromium")];
    match fs::read_dir(support.join("Google")) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                if entry.file_type()?.is_dir()
                    && entry.file_name().to_string_lossy().starts_with("Chrome")
                {
                    roots.push(entry.path());
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let home = fs::canonicalize(home)?;
    let mut registrations = Vec::new();
    for root in roots {
        let file = root
            .join("NativeMessagingHosts")
            .join(format!("{HOST_NAME}.json"));
        match registration_content(&file) {
            Ok(Some(content)) if owned_registration(&content, paths, id) => {
                let file = normalized_registration(&file)?;
                if file.starts_with(&home) {
                    registrations.push(file);
                }
            }
            Ok(_) => {}
            Err(error) => eprintln!(
                "Could not inspect existing native registration {}: {error:#}",
                file.display()
            ),
        }
    }
    Ok(registrations)
}

fn with_registration_rollback<T>(
    entries: &[Registration],
    action: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let paths = entries
        .iter()
        .map(|entry| {
            registration_root(&entry.path)?;
            Ok(entry.path.clone())
        })
        .collect::<Result<Vec<_>>>()?;
    with_file_rollback(&paths, action)
}

fn with_file_rollback<T>(paths: &[PathBuf], action: impl FnOnce() -> Result<T>) -> Result<T> {
    let snapshots = paths
        .iter()
        .map(|path| {
            let content = match fs::symlink_metadata(path) {
                Ok(metadata) => {
                    ensure!(
                        metadata.is_file(),
                        "Refusing to replace non-file: {}",
                        path.display()
                    );
                    Some((fs::read(path)?, metadata.permissions().mode()))
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.into()),
            };
            Ok((path, content))
        })
        .collect::<Result<Vec<_>>>()?;
    match action() {
        Ok(value) => Ok(value),
        Err(error) => {
            let mut failures = Vec::new();
            for (path, content) in snapshots.into_iter().rev() {
                let result = if let Some((bytes, mode)) = content {
                    atomic_write(path, &bytes, mode)
                } else {
                    remove_file_if_present(path)
                };
                if let Err(error) = result {
                    failures.push(format!("{}: {error:#}", path.display()));
                }
            }
            if failures.is_empty() {
                Err(error)
            } else {
                Err(error.context(format!("File rollback failed: {}", failures.join("; "))))
            }
        }
    }
}

fn shell_quote(value: &Path) -> String {
    format!("'{}'", value.to_string_lossy().replace('\'', "'\\''"))
}

pub fn install(home: &Path, payload: &Path) -> Result<Locations> {
    install_payload(home, payload, false)
}

fn install_payload(home: &Path, payload: &Path, automatic: bool) -> Result<Locations> {
    let metadata = validate_payload(payload)?;
    ensure!(
        cfg!(all(target_os = "macos", target_arch = "aarch64")),
        "This package only supports macOS Apple Silicon"
    );
    let paths = locations(home);
    fs::create_dir_all(&paths.base)?;
    fs::set_permissions(&paths.base, fs::Permissions::from_mode(0o700))?;
    let _lock = Lock::acquire(&paths.base)?;
    if automatic {
        updates::validate_upgrade(&paths, payload, &metadata)?;
    }
    let previous_state: Option<State> = if paths.state.exists() {
        Some(serde_json::from_slice(&fs::read(&paths.state)?)?)
    } else {
        None
    };
    if let Some(state) = &previous_state {
        ensure!(
            state.application == APPLICATION && metadata["extensionId"] == state.extension_id,
            "Installed application identity differs; refusing to replace it"
        );
    }
    let previous_extension = installed_extension(&paths, previous_state.as_ref())?;
    let obsolete = legacy_paths(&paths, previous_state.as_ref())?;
    fs::create_dir_all(&paths.data)?;
    fs::set_permissions(&paths.data, fs::Permissions::from_mode(0o700))?;
    fs::create_dir_all(paths.registration.parent().unwrap())?;
    let id = metadata["extensionId"].as_str().unwrap();
    let default_registration = normalized_registration(&paths.registration)?;
    let mut registrations = if let Some(state) = &previous_state {
        state.registrations(&paths)
    } else {
        let previous = registration_content(&paths.registration)?
            .filter(|content| !owned_registration(content, &paths, id));
        vec![Registration {
            path: default_registration.clone(),
            previous_registration: previous,
        }]
    };
    for entry in &mut registrations {
        entry.path = normalized_registration(&entry.path)?;
    }
    for path in existing_chrome_registrations(home, &paths, id)? {
        if !registrations.iter().any(|entry| entry.path == path) {
            registrations.push(Registration {
                path,
                previous_registration: None,
            });
        }
    }
    match browser_profiles::discover(home) {
        Ok(profiles) => {
            for profile in profiles {
                let path = profile
                    .join("NativeMessagingHosts")
                    .join(format!("{HOST_NAME}.json"));
                if registrations.iter().any(|entry| entry.path == path) {
                    continue;
                }
                if registration_content(&path)?
                    .is_some_and(|content| !owned_registration(&content, &paths, id))
                {
                    eprintln!(
                        "Custom Chrome registration belongs to another installation; leaving it untouched: {}",
                        path.display()
                    );
                    continue;
                }
                registrations.push(Registration {
                    path,
                    previous_registration: None,
                });
            }
        }
        Err(error) => eprintln!(
            "Custom Chrome profiles could not be detected; standard registration remains available: {error:#}"
        ),
    }
    let build_id = metadata["buildId"].as_str().unwrap();
    let extension = paths.base.join("extension");
    let staged_extension = paths
        .base
        .join(format!(".extension-install-{}", unique_id()));
    let extension_backup = paths
        .base
        .join(format!(".extension-backup-{}", unique_id()));
    let mut extension_backed_up = false;
    let mut extension_activated = false;
    let managed_files = [
        paths.base.join("uninstall"),
        paths.base.join("dev"),
        paths.state.clone(),
    ];
    let mut result: Result<()> = with_file_rollback(&managed_files, || {
        with_registration_rollback(&registrations, || {
            copy_tree(&payload.join("extension"), &staged_extension)?;
            copy_tree(&payload.join("native"), &staged_extension.join("native"))?;
            atomic_write(
                &staged_extension.join("native/distribution.json"),
                &serde_json::to_vec_pretty(&metadata)?,
                0o600,
            )?;
            let launcher = format!(
                "#!/bin/sh\nset -eu\numask 077\nHERE=$(CDPATH= cd -- \"$(dirname -- \"$0\")\" && pwd -P)\nexport SERVICES_DATA_DIR={}\nexport PATH=\"/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin\"\ncd {}\nexec \"$HERE/bin/services-devtools\" native-host \"$@\"\n",
                shell_quote(&paths.data),
                shell_quote(home)
            );
            atomic_write(
                &staged_extension.join("native/host"),
                launcher.as_bytes(),
                0o755,
            )?;
            check_host(
                &staged_extension.join("native/bin/services-devtools"),
                &paths.data,
            )?;
            if previous_extension != ExtensionPath::Missing {
                fs::rename(&extension, &extension_backup)?;
                extension_backed_up = true;
            }
            fs::rename(&staged_extension, &extension)?;
            extension_activated = true;
            let registration = serde_json::to_vec_pretty(&registration_manifest(&paths, id))?;
            for entry in &registrations {
                if !registration_root(&entry.path)?.try_exists()? {
                    eprintln!(
                        "Chrome directory no longer exists; registration not recreated: {}",
                        entry.path.display()
                    );
                    continue;
                }
                let first_default = previous_state.is_none() && entry.path == default_registration;
                if !first_default
                    && registration_content(&entry.path)?
                        .is_some_and(|content| !owned_registration(&content, &paths, id))
                {
                    eprintln!(
                        "Native registration changed; leaving it untouched: {}",
                        entry.path.display()
                    );
                    continue;
                }
                fs::create_dir_all(entry.path.parent().unwrap())?;
                atomic_write(&entry.path, &registration, 0o600)?;
            }
            let uninstall = format!(
                "#!/bin/sh\nset -eu\nexec {} uninstall\n",
                shell_quote(&paths.base.join("extension/native/bin/services-install"))
            );
            atomic_write(&paths.base.join("uninstall"), uninstall.as_bytes(), 0o755)?;
            let cli = format!(
                "#!/bin/sh\nset -eu\nexport SERVICES_DATA_DIR={}\nexec {} cli \"$@\"\n",
                shell_quote(&paths.data),
                shell_quote(&paths.base.join("extension/native/bin/services-devtools"))
            );
            atomic_write(&paths.base.join("dev"), cli.as_bytes(), 0o755)?;
            let state = State {
                application: APPLICATION.into(),
                extension_id: metadata["extensionId"].as_str().unwrap().into(),
                build_id: build_id.into(),
                previous_registration: None,
                registrations: registrations.clone(),
            };
            atomic_write(&paths.state, &serde_json::to_vec_pretty(&state)?, 0o600)?;
            Ok(())
        })
    });
    if let Err(error) = result {
        let mut failures = Vec::new();
        if extension_activated && let Err(error) = remove_extension_path(&extension) {
            failures.push(format!("Remove new extension: {error:#}"));
        }
        if extension_backed_up && let Err(error) = fs::rename(&extension_backup, &extension) {
            failures.push(format!("Restore previous extension: {error:#}"));
        }
        result = Err(if failures.is_empty() {
            error
        } else {
            error.context(format!(
                "Installation rollback failed: {}",
                failures.join("; ")
            ))
        });
    } else {
        remove_extension_path(&extension_backup)?;
    }
    if let Err(cleanup) = remove_extension_path(&staged_extension) {
        return Err(match result {
            Ok(()) => cleanup,
            Err(error) => error.context(format!("Staged installation cleanup failed: {cleanup:#}")),
        });
    }
    result?;
    for path in obsolete {
        remove_extension_path(&path)?;
    }
    println!(
        "Services DevTools {version}: installation completed.\n\
         chrome://extensions -> Developer mode -> Load unpacked\n\
         Cmd+Shift+G, paste:\n\
         {extension}\n\
         Enter -> Select",
        version = metadata["version"].as_str().unwrap(),
        extension = paths.base.join("extension").display(),
    );
    Ok(paths)
}

pub fn uninstall(home: &Path) -> Result<()> {
    let paths = locations(home);
    let state: State = serde_json::from_slice(&fs::read(&paths.state)?)?;
    ensure!(
        state.application == APPLICATION,
        "Installation ownership is not recognized"
    );
    let _lock = Lock::acquire(&paths.base)?;
    installed_extension(&paths, Some(&state))?;
    let obsolete = legacy_paths(&paths, Some(&state))?;
    let registrations = state.registrations(&paths);
    with_registration_rollback(&registrations, || {
        for entry in &registrations {
            if let Some(content) = registration_content(&entry.path)? {
                if owned_registration(&content, &paths, &state.extension_id) {
                    if let Some(previous) = &entry.previous_registration {
                        atomic_write(&entry.path, previous.as_bytes(), 0o600)?;
                    } else {
                        fs::remove_file(&entry.path)?;
                    }
                } else {
                    eprintln!(
                        "Native registration changed; leaving it untouched: {}",
                        entry.path.display()
                    );
                }
            }
        }
        Ok(())
    })?;
    for name in ["uninstall", "dev", "install-state.json"] {
        remove_file_if_present(&paths.base.join(name))?;
    }
    remove_extension_path(&paths.base.join("extension"))?;
    for path in obsolete {
        remove_extension_path(&path)?;
    }
    println!(
        "Uninstalled. User data retained at {}.\nRemove the extension manually in chrome://extensions.",
        paths.data.display()
    );
    Ok(())
}
