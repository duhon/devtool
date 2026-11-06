//! The skills handed to every Agent turn: the skill of each installed module and the
//! skills of the applied sources. The root `services-console` skill is compiled in and
//! served by the `console_skill` tool, so nothing here can replace it.
use crate::core::{skills, sources};
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

/// Why skills in the folders of the next turn won't load: `<origin>: <skill>: <problem>`.
pub(super) fn problems(root: &Path) -> Vec<String> {
    let data = data(root);
    let origins = module_dirs(data)
        .into_iter()
        .map(|(module, dir)| (format!("module {module}"), dir))
        .chain(
            sources::skill_dirs(data)
                .into_iter()
                .map(|(source, dir)| (format!("source {source}"), dir)),
        );
    let mut problems = Vec::new();
    for (origin, dir) in origins {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut folders: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.is_dir())
            .collect();
        folders.sort();
        for folder in folders {
            let name = folder.file_name().unwrap_or_default().to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            if let Err(problem) = std::fs::read_to_string(folder.join(skills::FILE))
                .map_err(|e| format!("cannot read {}: {e}", skills::FILE))
                .and_then(|text| skills::frontmatter(&text).map(drop))
            {
                problems.push(format!("{origin}: {name}: {problem}"));
            }
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
        let broken = data.path().join("modules/grpc/skills/module-grpc");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(broken.join("SKILL.md"), "---\nname: module-grpc\n---\n").unwrap();
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
                data.path().join("modules/grpc/skills"),
                data.path().join("modules/rest/skills"),
                data.path().join("sources/team/skills")
            ]
        );
        assert_eq!(
            problems(&root),
            ["module grpc: module-grpc: SKILL.md frontmatter has no description"]
        );
    }
}
