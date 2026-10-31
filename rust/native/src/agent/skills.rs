//! The skills handed to every Agent turn: the skill of each installed module and the
//! skills of the applied sources. The root `services-console` skill is compiled in and
//! served by the `console_skill` tool, so nothing here can replace it.
use crate::{
    Error, Result,
    core::{skills, sources},
};
use serde_json::{Value, json};
use services_contracts::package::SKILLS_DIR;
use std::path::{Path, PathBuf};

fn data(root: &Path) -> &Path {
    root.parent().unwrap_or(root)
}

/// `(module folder, skills folder)` of the installed modules that have skills.
fn module_dirs(data: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(data.join("modules")) else {
        return Vec::new();
    };
    let mut dirs: Vec<(String, PathBuf)> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let dir = entry.path().join(SKILLS_DIR);
            (!name.starts_with('.') && dir.is_dir()).then_some((name, dir))
        })
        .collect();
    dirs.sort();
    dirs
}

/// The skills folders for the next turn: installed modules, then applied sources.
pub(super) fn directories(root: &Path) -> Vec<PathBuf> {
    let data = data(root);
    module_dirs(data)
        .into_iter()
        .chain(sources::skill_dirs(data))
        .map(|(_, dir)| dir)
        .collect()
}

/// `get`: `{skills: [{name, description, folder, origin}], problems}` where `origin` is
/// `{kind: "module", module}` or `{kind: "source", source}`.
pub(super) fn dispatch(root: &Path, message: &Value) -> Result<Value> {
    if !matches!(message["op"].as_str(), None | Some("get")) {
        return Err(Error::new("Agent skills support only get"));
    }
    let data = data(root);
    let (mut list, mut problems) = (Vec::new(), Vec::new());
    for (module, dir) in module_dirs(data) {
        let origin = json!({"kind": "module", "module": module});
        skills::list(&dir, &origin, &mut list, &mut problems);
    }
    for (source, dir) in sources::skill_dirs(data) {
        let origin = json!({"kind": "source", "source": source});
        skills::list(&dir, &origin, &mut list, &mut problems);
    }
    Ok(json!({"skills": list, "problems": problems}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(folder: &Path, name: &str, description: &str) {
        std::fs::create_dir_all(folder.join(name)).unwrap();
        std::fs::write(
            folder.join(name).join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\nBody.\n"),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn modules_and_applied_sources_supply_the_skills() {
        let data = tempfile::tempdir().unwrap();
        let root = data.path().join(".agent");
        skill(
            &data.path().join("modules/rest/skills"),
            "module-rest",
            "REST",
        );
        skill(
            &data.path().join("modules/.removed-1/skills"),
            "module-old",
            "Old",
        );
        let team = tempfile::tempdir().unwrap();
        skill(&team.path().join("skills"), "deploy", "Deploys");
        let sources = sources::Sources::new(data.path());
        sources
            .save(&json!({"sources": [{"name": "Team", "kind": "folder", "path": team.path()}]}))
            .unwrap();
        sources.apply("team").await.unwrap();

        assert_eq!(
            directories(&root),
            [
                data.path().join("modules/rest/skills"),
                data.path().join("sources/team/skills")
            ]
        );
        let got = dispatch(&root, &json!({"op": "get"})).unwrap();
        assert_eq!(
            got["skills"],
            json!([
                {"name": "module-rest", "description": "REST", "folder": "module-rest",
                 "origin": {"kind": "module", "module": "rest"}},
                {"name": "team-deploy", "description": "Deploys", "folder": "team-deploy",
                 "origin": {"kind": "source", "source": "Team"}},
            ])
        );
        assert!(dispatch(&root, &json!({"op": "save"})).is_err());
    }
}
