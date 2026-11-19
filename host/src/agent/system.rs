//! What the user's own Copilot installation already has: skills, custom agents (also those of
//! installed plugins) and the custom instructions. The Agent runs with its own isolated Copilot
//! home, so none of it is seen unless Agent access level 2 hands it over.
use super::mcp::copilot_home;
use crate::core::skills::frontmatter;
use github_copilot_sdk::CustomAgentConfig;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const INSTRUCTIONS_FILE: &str = "copilot-instructions.md";
/// Instructions beyond this many bytes are cut: they all go into the system message.
const INSTRUCTIONS_LIMIT: usize = 32 * 1024;

pub struct Skill {
    pub name: String,
    pub description: String,
    /// The folder that holds the skill folders; this is what a session loads.
    pub root: PathBuf,
}

pub struct AgentFile {
    pub name: String,
    pub description: String,
    pub prompt: String,
}

fn subdirectories(dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.is_dir()
                && !path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with('.'))
        })
        .collect();
    found.sort();
    found
}

/// `<home>/installed-plugins/<marketplace>/<plugin>`.
fn plugins(home: &Path) -> Vec<PathBuf> {
    subdirectories(&home.join("installed-plugins"))
        .iter()
        .flat_map(|marketplace| subdirectories(marketplace))
        .collect()
}

fn skill_roots(home: &Path) -> Vec<PathBuf> {
    let mut roots = vec![home.join("skills")];
    if let Some(user) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(user).join(".agents").join("skills"));
    }
    roots.extend(
        plugins(home)
            .into_iter()
            .map(|plugin| plugin.join("skills")),
    );
    roots
}

fn agent_roots(home: &Path) -> Vec<PathBuf> {
    let mut roots = vec![home.join("agents")];
    roots.extend(
        plugins(home)
            .into_iter()
            .map(|plugin| plugin.join("agents")),
    );
    roots
}

/// The skills of the Copilot configuration; a name found twice is listed once.
pub fn skills() -> Vec<Skill> {
    let Some(home) = copilot_home() else {
        return Vec::new();
    };
    let mut found: Vec<Skill> = Vec::new();
    for root in skill_roots(&home) {
        for folder in subdirectories(&root) {
            let Ok(text) = std::fs::read_to_string(folder.join("SKILL.md")) else {
                continue;
            };
            let Ok((name, description)) = frontmatter(&text) else {
                continue;
            };
            if found.iter().all(|skill| skill.name != name) {
                found.push(Skill {
                    name,
                    description,
                    root: root.clone(),
                });
            }
        }
    }
    found
}

/// The custom agents of the Copilot configuration; a name found twice is listed once.
pub fn agents() -> Vec<AgentFile> {
    let Some(home) = copilot_home() else {
        return Vec::new();
    };
    let mut found: Vec<AgentFile> = Vec::new();
    for root in agent_roots(&home) {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&root)
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.to_string_lossy().ends_with(".agent.md"))
            .collect();
        files.sort();
        for file in files {
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            let stem = file
                .file_name()
                .map(|name| {
                    name.to_string_lossy()
                        .trim_end_matches(".agent.md")
                        .to_owned()
                })
                .unwrap_or_default();
            let (name, description) =
                frontmatter(&text).unwrap_or_else(|_| (stem.clone(), String::new()));
            let prompt = body(&text).to_owned();
            if !prompt.trim().is_empty() && found.iter().all(|agent| agent.name != name) {
                found.push(AgentFile {
                    name,
                    description,
                    prompt,
                });
            }
        }
    }
    found
}

/// The text after the frontmatter.
fn body(text: &str) -> &str {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Some(rest) = text.strip_prefix("---") else {
        return text;
    };
    rest.split_once("\n---")
        .and_then(|(_, after)| after.split_once('\n'))
        .map_or("", |(_, body)| body)
}

pub fn instructions() -> Option<String> {
    let text = std::fs::read_to_string(copilot_home()?.join(INSTRUCTIONS_FILE)).ok()?;
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let mut end = text.len().min(INSTRUCTIONS_LIMIT);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    Some(text[..end].to_owned())
}

/// The folders to load skills from, and the names of the skills left out.
pub fn skill_setup(off: &[String]) -> (Vec<PathBuf>, Vec<String>) {
    let all = skills();
    let mut roots: Vec<PathBuf> = Vec::new();
    for skill in all.iter().filter(|skill| !off.contains(&skill.name)) {
        if !roots.contains(&skill.root) {
            roots.push(skill.root.clone());
        }
    }
    let disabled = all
        .into_iter()
        .map(|skill| skill.name)
        .filter(|name| off.contains(name))
        .collect();
    (roots, disabled)
}

pub fn custom_agents(off: &[String]) -> Vec<CustomAgentConfig> {
    agents()
        .into_iter()
        .filter(|agent| !off.contains(&agent.name))
        .map(|agent| {
            CustomAgentConfig::new(agent.name, agent.prompt).with_description(agent.description)
        })
        .collect()
}

/// What Settings lists: names and descriptions, never file contents.
pub fn list() -> Value {
    json!({
        "skills": skills().into_iter().map(|skill| json!({"name":skill.name,"description":skill.description})).collect::<Vec<_>>(),
        "agents": agents().into_iter().map(|agent| json!({"name":agent.name,"description":agent.description})).collect::<Vec<_>>(),
        "instructions": instructions().is_some(),
    })
}

#[cfg(test)]
mod tests {
    use super::body;

    #[test]
    fn the_body_follows_the_frontmatter() {
        assert_eq!(body("---\nname: a\n---\nHello\nthere\n"), "Hello\nthere\n");
        assert_eq!(body("no frontmatter"), "no frontmatter");
        assert_eq!(body("---\nname: a\n"), "");
    }
}
