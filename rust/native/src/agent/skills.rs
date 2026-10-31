//! Team skills: a local folder of `<name>/SKILL.md` skills (a team repository checkout or a
//! synced drive) that every Agent turn hands to the Copilot SDK as a skill directory.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const FILE: &str = "skills.json";

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Settings {
    team_folder: Option<PathBuf>,
}

fn read(root: &Path) -> Result<Settings> {
    match std::fs::read(root.join(FILE)) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| Error::new(format!("Invalid Agent skills settings: {error}"))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(error) => Err(error.into()),
    }
}

fn write(root: &Path, settings: &Settings) -> Result<()> {
    std::fs::create_dir_all(root)?;
    let mut file = tempfile::NamedTempFile::new_in(root)?;
    std::io::Write::write_all(&mut file, &serde_json::to_vec_pretty(settings)?)?;
    file.persist(root.join(FILE))
        .map_err(|error| Error::new(format!("Could not save Agent skills settings: {error}")))?;
    Ok(())
}

/// Skill directories for the next turn, and why a configured folder was left out.
pub(super) fn directories(root: &Path) -> (Vec<PathBuf>, Option<String>) {
    match read(root) {
        Err(error) => (
            Vec::new(),
            Some(format!(
                "This turn ran without team skills: {}",
                error.message
            )),
        ),
        Ok(Settings { team_folder: None }) => (Vec::new(), None),
        Ok(Settings {
            team_folder: Some(folder),
        }) if folder.is_dir() => (vec![folder], None),
        Ok(Settings {
            team_folder: Some(folder),
        }) => (
            Vec::new(),
            Some(format!(
                "This turn ran without team skills: the team skills folder is unavailable: {}",
                folder.display()
            )),
        ),
    }
}

/// `get` describes the configured folder and its skills; `save` sets or clears the folder.
pub(super) fn dispatch(root: &Path, message: &Value) -> Result<Value> {
    match message["op"].as_str() {
        Some("get") => Ok(describe(read(root)?.team_folder.as_deref())),
        Some("save") => {
            let folder = match &message["teamFolder"] {
                Value::Null => None,
                Value::String(text) if text.trim().is_empty() => None,
                Value::String(text) => Some(expand(text.trim())?),
                _ => return Err(Error::new("Team skills folder must be a path")),
            };
            if let Some(folder) = &folder
                && !folder.is_dir()
            {
                return Err(Error::new(format!(
                    "Team skills folder does not exist or is not a folder: {}",
                    folder.display()
                )));
            }
            write(
                root,
                &Settings {
                    team_folder: folder.clone(),
                },
            )?;
            Ok(describe(folder.as_deref()))
        }
        _ => Err(Error::new("Agent skills support only get and save")),
    }
}

fn expand(text: &str) -> Result<PathBuf> {
    let path = match text.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join(rest))
            .ok_or_else(|| Error::new("HOME is not set; enter the full folder path"))?,
        None => PathBuf::from(text),
    };
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(Error::new(
            "Team skills folder must be an absolute path (or start with ~/)",
        ))
    }
}

fn describe(folder: Option<&Path>) -> Value {
    let Some(folder) = folder else {
        return json!({"teamFolder":null,"skills":[],"problems":[]});
    };
    let mut skills = Vec::new();
    let mut problems = Vec::new();
    match std::fs::read_dir(folder) {
        Err(error) => problems.push(format!("Cannot read {}: {error}", folder.display())),
        Ok(entries) => {
            let mut directories: Vec<_> = entries
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| path.is_dir())
                .collect();
            directories.sort();
            for directory in directories {
                let name = directory
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let file = directory.join("SKILL.md");
                if name.starts_with('.') || !file.is_file() {
                    continue;
                }
                match std::fs::read_to_string(&file)
                    .map_err(|error| format!("cannot read SKILL.md: {error}"))
                    .and_then(|text| frontmatter(&text))
                {
                    Ok((skill, description)) => {
                        skills.push(json!({"name":skill,"description":description,"folder":name}))
                    }
                    Err(problem) => problems.push(format!("{name}: {problem}")),
                }
            }
            if folder.join("SKILL.md").is_file() {
                problems.push(
                    "This folder is a single skill; choose the folder that contains skill folders"
                        .into(),
                );
            } else if skills.is_empty() && problems.is_empty() {
                problems.push("No skill folders with a SKILL.md were found".into());
            }
        }
    }
    let mut result = json!({"teamFolder":folder,"skills":skills,"problems":problems});
    if let Some(first) = problems.first() {
        result["warning"] = json!(if problems.len() == 1 {
            first.clone()
        } else {
            format!("{first} (and {} more)", problems.len() - 1)
        });
    }
    result
}

/// Reads `name` and `description` from the YAML frontmatter, including `>`/`|` block text.
fn frontmatter(text: &str) -> std::result::Result<(String, String), String> {
    let mut lines = text.strip_prefix('\u{feff}').unwrap_or(text).lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return Err("SKILL.md must start with --- frontmatter".into());
    }
    let (mut name, mut description) = (None::<String>, None::<String>);
    let mut block: Option<&str> = None;
    let mut closed = false;
    for line in lines {
        if line.trim_end() == "---" {
            closed = true;
            break;
        }
        if let Some(key) = block
            && line.starts_with([' ', '\t'])
        {
            let target = if key == "name" {
                &mut name
            } else {
                &mut description
            };
            let text = target.get_or_insert_with(String::new);
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(line.trim());
            continue;
        }
        block = None;
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = match key {
            "name" => "name",
            "description" => "description",
            _ => continue,
        };
        let value = value.trim();
        if matches!(value, "" | ">" | "|" | ">-" | "|-" | ">+" | "|+") {
            block = Some(key);
            continue;
        }
        let value = value
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .or_else(|| {
                value
                    .strip_prefix('\'')
                    .and_then(|value| value.strip_suffix('\''))
            })
            .unwrap_or(value)
            .to_owned();
        *(if key == "name" {
            &mut name
        } else {
            &mut description
        }) = Some(value);
    }
    if !closed {
        return Err("SKILL.md frontmatter is not closed with ---".into());
    }
    let name = name
        .filter(|name| !name.trim().is_empty())
        .ok_or("SKILL.md frontmatter has no name")?;
    let description = description
        .filter(|description| !description.trim().is_empty())
        .ok_or("SKILL.md frontmatter has no description")?;
    Ok((name, description))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(folder: &Path, name: &str, text: &str) {
        std::fs::create_dir_all(folder.join(name)).unwrap();
        std::fs::write(folder.join(name).join("SKILL.md"), text).unwrap();
    }

    #[test]
    fn team_folder_lists_valid_skills_and_explains_broken_ones() {
        let root = tempfile::tempdir().unwrap();
        let team = tempfile::tempdir().unwrap();
        skill(
            team.path(),
            "deploy",
            "---\nname: deploy\ndescription: >\n  Deploys the shop\n  to staging.\n---\nSteps\n",
        );
        skill(
            team.path(),
            "orders",
            "\u{feff}---\nname: \"orders\"\ndescription: 'Checks order flows'\n---\n",
        );
        skill(team.path(), "broken", "---\nname: broken\n---\n");
        skill(team.path(), ".git", "---\nname: hidden\n---\n");
        std::fs::create_dir_all(team.path().join("notes")).unwrap();
        assert_eq!(
            dispatch(root.path(), &json!({"op":"get"})).unwrap(),
            json!({"teamFolder":null,"skills":[],"problems":[]})
        );
        let saved = dispatch(
            root.path(),
            &json!({"op":"save","teamFolder":format!(" {} ",team.path().display())}),
        )
        .unwrap();
        assert_eq!(saved["teamFolder"], json!(team.path()));
        assert_eq!(
            saved["skills"],
            json!([
                {"name":"deploy","description":"Deploys the shop to staging.","folder":"deploy"},
                {"name":"orders","description":"Checks order flows","folder":"orders"}
            ])
        );
        assert_eq!(
            saved["problems"],
            json!(["broken: SKILL.md frontmatter has no description"])
        );
        assert_eq!(saved["warning"], saved["problems"][0]);
        assert_eq!(dispatch(root.path(), &json!({"op":"get"})).unwrap(), saved);
        assert_eq!(
            directories(root.path()),
            (vec![team.path().to_path_buf()], None)
        );
    }

    #[test]
    fn invalid_folders_are_rejected_and_a_vanished_folder_only_warns() {
        let root = tempfile::tempdir().unwrap();
        let team = tempfile::tempdir().unwrap();
        for folder in [json!("relative/skills"), json!(7)] {
            assert!(dispatch(root.path(), &json!({"op":"save","teamFolder":folder})).is_err());
        }
        let missing = team.path().join("missing");
        let error = dispatch(root.path(), &json!({"op":"save","teamFolder":missing})).unwrap_err();
        assert!(
            error.message.contains("does not exist"),
            "{}",
            error.message
        );
        assert!(
            !root.path().join(FILE).exists(),
            "a rejected folder is not saved"
        );
        let empty = dispatch(root.path(), &json!({"op":"save","teamFolder":team.path()})).unwrap();
        assert_eq!(empty["skills"], json!([]));
        assert!(
            empty["warning"]
                .as_str()
                .unwrap()
                .contains("No skill folders")
        );
        std::fs::write(
            team.path().join("SKILL.md"),
            "---\nname: single\ndescription: One skill\n---\n",
        )
        .unwrap();
        assert!(
            dispatch(root.path(), &json!({"op":"get"})).unwrap()["warning"]
                .as_str()
                .unwrap()
                .contains("single skill")
        );
        std::fs::remove_dir_all(team.path()).unwrap();
        let (found, warning) = directories(root.path());
        assert!(found.is_empty() && warning.unwrap().contains("unavailable"));
        assert_eq!(
            dispatch(root.path(), &json!({"op":"save","teamFolder":""})).unwrap(),
            json!({"teamFolder":null,"skills":[],"problems":[]})
        );
        assert_eq!(directories(root.path()), (Vec::new(), None));
        assert!(dispatch(root.path(), &json!({"op":"delete"})).is_err());
    }

    #[test]
    fn frontmatter_requires_a_closed_header_with_name_and_description() {
        assert!(frontmatter("name: x\n").is_err());
        assert!(frontmatter("---\nname: x\ndescription: y\n").is_err());
        assert_eq!(
            frontmatter(
                "---\nname: x\ndescription: |\n  line one\n  line two\nlicense: MIT\n---\n"
            ),
            Ok(("x".into(), "line one line two".into()))
        );
    }
}
