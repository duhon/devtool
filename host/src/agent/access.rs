//! Permission policy for the Agent access levels. Copilot asks this handler before every built-in or MCP tool
//! runs; the decision depends only on the saved `Access`, the per-chat grants and whether untrusted content
//! (page, web or MCP output) has entered the current turn.
use super::Bridge;
use async_trait::async_trait;
use github_copilot_sdk::{
    PermissionRequestData, RequestId, SessionId,
    handler::{PermissionHandler, PermissionResult},
};
use serde_json::{Value, json};
use services_contracts::agent::{Access, Confirm};
use std::{
    collections::HashSet,
    path::{Component, Path, PathBuf},
    sync::{Arc, atomic::Ordering},
};
use tokio::sync::Mutex;

/// Approvals the user gave in this chat. They never outlive the chat.
#[derive(Default)]
pub struct Grants {
    directories: Vec<PathBuf>,
    keys: HashSet<String>,
}
impl Grants {
    fn covers(&self, directory: &Path) -> bool {
        self.directories.iter().any(|d| directory.starts_with(d))
    }
}

#[derive(Debug, PartialEq)]
pub enum Remember {
    Directory(PathBuf),
    Key(String),
}
#[derive(Debug, PartialEq)]
pub enum Verdict {
    Allow,
    Deny(String),
    Ask { remember: Remember, prompt: Value },
}

pub struct Context<'a> {
    pub access: &'a Access,
    pub tainted: bool,
    pub grants: &'a Grants,
    /// Locations the agent may never read or write, whatever the level (secrets of this application).
    pub protected: &'a [PathBuf],
    /// Skill folders: readable at every level because the skill tool needs them.
    pub readable: &'a [PathBuf],
    /// The session's own folder: readable at every level even inside a protected location.
    pub open: &'a [PathBuf],
}

pub fn normalize(path: &str) -> PathBuf {
    let path = Path::new(path);
    if let Ok(real) = std::fs::canonicalize(path) {
        return real;
    }
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    // The file may not exist yet: resolve its nearest existing parent so symlinks cannot hide a protected path.
    if let (Some(parent), Some(name)) = (out.parent(), out.file_name())
        && let Ok(real) = std::fs::canonicalize(parent)
    {
        return real.join(name);
    }
    out
}

/// A URL that can carry data out: credentials, a query or a fragment.
pub fn url_carries_data(url: &str) -> bool {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    authority.contains('@') || url.contains('?') || url.contains('#')
}

fn confirm(option: Confirm, tainted: bool) -> bool {
    option == Confirm::Ask || tainted
}

pub fn decide(cx: &Context, request: &Value) -> Verdict {
    let access = cx.access;
    let text = |name: &str| request.get(name).and_then(Value::as_str).unwrap_or("");
    let deny = |reason: &str| Verdict::Deny(reason.to_owned());
    let blocked = |path: &Path| {
        cx.protected.iter().any(|p| path.starts_with(p))
            && !cx.open.iter().any(|o| path.starts_with(o))
    };
    match text("kind") {
        "custom-tool" => Verdict::Allow,
        "url" => {
            if !access.has(1) {
                return deny("Web access is off at this access level");
            }
            let url = text("url");
            if url_carries_data(url) && confirm(access.web_data, cx.tainted) {
                let key = format!("url:{url}");
                if cx.grants.keys.contains(&key) {
                    return Verdict::Allow;
                }
                return Verdict::Ask {
                    remember: Remember::Key(key),
                    prompt: json!({"kind":"url","title":"Send data to a website","detail":url}),
                };
            }
            Verdict::Allow
        }
        "mcp" => {
            let server = text("serverName");
            if !access.has(2) || !access.mcp_servers.iter().any(|s| s == server) {
                return deny("This MCP server is not enabled in Agent access");
            }
            let read_only = request
                .get("readOnly")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if cx.tainted && !read_only {
                let key = format!("mcp:{server}:{}", text("toolName"));
                if cx.grants.keys.contains(&key) {
                    return Verdict::Allow;
                }
                return Verdict::Ask {
                    remember: Remember::Key(key),
                    prompt: json!({"kind":"mcp","title":format!("Run MCP tool {server}/{}", text("toolName")),
                        "detail":request.get("args").map(Value::to_string).unwrap_or_default()}),
                };
            }
            Verdict::Allow
        }
        "read" => {
            let path = normalize(text("path"));
            if blocked(&path) {
                return deny("This location holds application secrets");
            }
            if cx.readable.iter().any(|dir| path.starts_with(dir)) {
                return Verdict::Allow;
            }
            if !access.has(3) {
                return deny("Reading files is off at this access level");
            }
            let directory = if path.is_dir() {
                path
            } else {
                path.parent().map(Path::to_path_buf).unwrap_or(path)
            };
            if access.read_directories == Confirm::Allow || cx.grants.covers(&directory) {
                return Verdict::Allow;
            }
            Verdict::Ask {
                prompt: json!({"kind":"read","title":"Read files in a new folder","detail":directory.display().to_string()}),
                remember: Remember::Directory(directory),
            }
        }
        "write" => {
            let name = text("fileName");
            let path = normalize(name);
            if blocked(&path) {
                return deny("This location holds application secrets");
            }
            if !access.has(4) {
                return deny("Writing files is off at this access level");
            }
            let key = format!("file:{}", path.display());
            if !confirm(access.write, cx.tainted) || cx.grants.keys.contains(&key) {
                return Verdict::Allow;
            }
            Verdict::Ask {
                remember: Remember::Key(key),
                prompt: json!({"kind":"write","title":"Write a file","detail":path.display().to_string(),
                    "diff":text("diff")}),
            }
        }
        "shell" => {
            if !access.has(4) {
                return deny("Shell commands are off at this access level");
            }
            let paths = request
                .get("possiblePaths")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str);
            if paths.into_iter().any(|p| blocked(&normalize(p))) {
                return deny("This command touches application secrets");
            }
            let command = text("fullCommandText");
            let key = format!("cmd:{command}");
            if !confirm(access.shell, cx.tainted) || cx.grants.keys.contains(&key) {
                return Verdict::Allow;
            }
            Verdict::Ask {
                remember: Remember::Key(key),
                prompt: json!({"kind":"shell","title":"Run a shell command","detail":command}),
            }
        }
        _ => deny("This kind of action is not available to the Agent"),
    }
}

pub struct Policy {
    pub access: Access,
    pub grants: Arc<Mutex<Grants>>,
    pub bridge: Arc<Mutex<Option<Bridge>>>,
    pub protected: Vec<PathBuf>,
    pub readable: Vec<PathBuf>,
    pub open: Vec<PathBuf>,
}

#[async_trait]
impl PermissionHandler for Policy {
    async fn handle(
        &self,
        _session: SessionId,
        request_id: RequestId,
        data: PermissionRequestData,
    ) -> PermissionResult {
        let Some(bridge) = self.bridge.lock().await.clone() else {
            return PermissionResult::reject(Some(
                "The Console turn is no longer connected".into(),
            ));
        };
        let request = data
            .extra
            .get("permissionRequest")
            .cloned()
            .unwrap_or_else(|| data.extra.clone());
        let verdict = {
            let grants = self.grants.lock().await;
            decide(
                &Context {
                    access: &self.access,
                    tainted: bridge.tainted.load(Ordering::SeqCst),
                    grants: &grants,
                    protected: &self.protected,
                    readable: &self.readable,
                    open: &self.open,
                },
                &request,
            )
        };
        let kind = request["kind"].as_str().unwrap_or("");
        let allowed = match verdict {
            Verdict::Deny(reason) => return PermissionResult::reject(Some(reason)),
            Verdict::Allow => true,
            Verdict::Ask {
                remember,
                mut prompt,
            } => {
                prompt["tainted"] = json!(bridge.tainted.load(Ordering::SeqCst));
                let id = format!("permission-{}", request_id);
                let answer = bridge.call(&id, "permission_request", prompt).await;
                if !answer
                    .as_ref()
                    .is_ok_and(|v| v["allow"].as_bool() == Some(true))
                {
                    return PermissionResult::reject(Some("The user declined this action".into()));
                }
                let mut grants = self.grants.lock().await;
                match remember {
                    Remember::Directory(path) => grants.directories.push(path),
                    Remember::Key(key) => {
                        grants.keys.insert(key);
                    }
                }
                true
            }
        };
        // Web and MCP output is untrusted: from here on risky actions need a confirmation in this turn.
        if allowed && matches!(kind, "url" | "mcp") {
            bridge.tainted.store(true, Ordering::SeqCst);
        }
        PermissionResult::approve_once()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(level: u8) -> Access {
        Access {
            level,
            mcp_servers: vec!["playwright".into()],
            ..Access::default()
        }
    }
    fn run(access: &Access, tainted: bool, request: Value) -> Verdict {
        let protected = [PathBuf::from("/data")];
        decide(
            &Context {
                access,
                tainted,
                grants: &Grants::default(),
                protected: &protected,
                readable: &[],
                open: &[],
            },
            &request,
        )
    }

    #[test]
    fn levels_gate_each_kind() {
        let url = json!({"kind":"url","url":"https://example.com"});
        assert!(matches!(run(&at(0), false, url.clone()), Verdict::Deny(_)));
        assert_eq!(run(&at(1), false, url), Verdict::Allow);
        let mcp = json!({"kind":"mcp","serverName":"playwright","toolName":"x","readOnly":true});
        assert!(matches!(run(&at(1), false, mcp.clone()), Verdict::Deny(_)));
        assert_eq!(run(&at(2), false, mcp), Verdict::Allow);
        let other = json!({"kind":"mcp","serverName":"scout","toolName":"x"});
        assert!(matches!(run(&at(4), false, other), Verdict::Deny(_)));
        let shell = json!({"kind":"shell","fullCommandText":"ls"});
        assert!(matches!(run(&at(3), false, shell), Verdict::Deny(_)));
        assert!(matches!(
            run(&at(4), false, json!({"kind":"memory"})),
            Verdict::Deny(_)
        ));
    }

    #[test]
    fn urls_with_data_follow_the_option() {
        let url = json!({"kind":"url","url":"https://example.com/?q=secret"});
        assert!(matches!(
            run(&at(1), false, url.clone()),
            Verdict::Ask { .. }
        ));
        let mut open = at(1);
        open.web_data = Confirm::Allow;
        assert_eq!(run(&open, false, url.clone()), Verdict::Allow);
        assert!(matches!(run(&open, true, url), Verdict::Ask { .. }));
        assert!(url_carries_data("https://user:pw@example.com/"));
        assert!(!url_carries_data("https://example.com/a/b"));
    }

    #[test]
    fn untrusted_content_forces_confirmation_for_risky_actions() {
        let mut access = at(4);
        access.shell = Confirm::Allow;
        access.write = Confirm::Allow;
        let shell = json!({"kind":"shell","fullCommandText":"ls"});
        assert_eq!(run(&access, false, shell.clone()), Verdict::Allow);
        assert!(matches!(run(&access, true, shell), Verdict::Ask { .. }));
        let write = json!({"kind":"write","fileName":"/tmp/agent-access-test.txt"});
        assert_eq!(run(&access, false, write.clone()), Verdict::Allow);
        assert!(matches!(run(&access, true, write), Verdict::Ask { .. }));
        let mcp = json!({"kind":"mcp","serverName":"playwright","toolName":"x","readOnly":false});
        assert!(matches!(run(&access, true, mcp), Verdict::Ask { .. }));
    }

    #[test]
    fn directories_are_asked_once_and_cover_subdirectories() {
        let access = at(3);
        let request = json!({"kind":"read","path":"/nonexistent-agent/project/a.txt"});
        let Verdict::Ask {
            remember: Remember::Directory(directory),
            ..
        } = run(&access, false, request.clone())
        else {
            panic!("expected a question")
        };
        let mut grants = Grants::default();
        grants.directories.push(directory);
        let protected = [PathBuf::from("/data")];
        let cx = Context {
            access: &access,
            tainted: false,
            grants: &grants,
            protected: &protected,
            readable: &[],
            open: &[],
        };
        assert_eq!(decide(&cx, &request), Verdict::Allow);
        let deeper = json!({"kind":"read","path":"/nonexistent-agent/project/src/b.txt"});
        assert_eq!(decide(&cx, &deeper), Verdict::Allow);
        let elsewhere = json!({"kind":"read","path":"/nonexistent-agent/other/c.txt"});
        assert!(matches!(decide(&cx, &elsewhere), Verdict::Ask { .. }));
    }

    #[test]
    fn application_secrets_are_never_readable() {
        let mut access = at(5);
        access.read_directories = Confirm::Allow;
        let read = json!({"kind":"read","path":"/data/.agent/config.json"});
        assert!(matches!(run(&access, false, read), Verdict::Deny(_)));
        let write = json!({"kind":"write","fileName":"/data/x"});
        assert!(matches!(run(&access, false, write), Verdict::Deny(_)));
    }

    #[test]
    fn session_folder_is_readable_at_every_level_inside_protected_data() {
        let access = at(0);
        let grants = Grants::default();
        let protected = [PathBuf::from("/data")];
        let open = [PathBuf::from("/data/.agent/sessions/s/tool-output")];
        let cx = Context {
            access: &access,
            tainted: false,
            grants: &grants,
            protected: &protected,
            readable: &open,
            open: &open,
        };
        let inside = json!({"kind":"read","path":"/data/.agent/sessions/s/tool-output/a.txt"});
        assert_eq!(decide(&cx, &inside), Verdict::Allow);
        let sibling = json!({"kind":"read","path":"/data/.agent/sessions/s/events.jsonl"});
        assert!(matches!(decide(&cx, &sibling), Verdict::Deny(_)));
    }
}
