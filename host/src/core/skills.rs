//! Agent skills on disk: folders of `<name>/SKILL.md` skills with YAML frontmatter. Module
//! packages and sources bring skills; the application renames them so that skills from
//! different places never share a name.
use std::path::Path;

/// The skill file of a skill folder.
pub const FILE: &str = "SKILL.md";

/// Reads `name` and `description` from the YAML frontmatter.
pub use services_contracts::package::skill_frontmatter as frontmatter;

/// The skill text with frontmatter `name` set to `name`; the rest stays as written.
pub fn rename(text: &str, name: &str) -> Result<String, String> {
    frontmatter(text)?;
    let body = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut out = String::with_capacity(body.len() + name.len());
    let mut lines = body.split_inclusive('\n');
    out.push_str(lines.next().unwrap_or_default());
    let mut skipping = false;
    let mut header = true;
    for line in lines {
        if header {
            if line.trim_end() == "---" {
                header = false;
            } else if skipping && line.starts_with([' ', '\t']) {
                continue;
            } else {
                skipping = false;
                if line.split_once(':').is_some_and(|(key, _)| key == "name") {
                    out.push_str(&format!("name: {name}\n"));
                    skipping = true;
                    continue;
                }
            }
        }
        out.push_str(line);
    }
    Ok(out)
}

/// Renames the skill in `folder` (its `SKILL.md` frontmatter) to `name`.
pub fn rename_file(folder: &Path, name: &str) -> Result<(), String> {
    let file = folder.join(FILE);
    let text = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    let renamed = rename(&text, name).map_err(|e| format!("{}: {e}", file.display()))?;
    if renamed != text {
        std::fs::write(&file, renamed).map_err(|e| format!("{}: {e}", file.display()))?;
    }
    Ok(())
}

/// Checks the skills folder of an unpacked module package: only the module's own skill,
/// renamed to it.
pub fn prepare_package(package: &Path, id: &str) -> Result<(), String> {
    let dir = package.join(services_contracts::package::SKILLS_DIR);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(());
    };
    let own = services_contracts::package::skill_name(id);
    for entry in entries {
        let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name != own {
            return Err(format!(
                "Invalid package file: skills/{name}; a module package holds only its own skill, skills/{own}/"
            ));
        }
        if !entry.path().join(FILE).is_file() {
            return Err(format!("Invalid package file: skills/{own}/ has no {FILE}"));
        }
        rename_file(&entry.path(), &own).map_err(|e| format!("Invalid package file: {e}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renaming_changes_only_the_frontmatter_name() {
        assert_eq!(
            rename(
                "---\nname: >\n  rest\n  api\ndescription: d\n---\nname: body\n",
                "team-rest"
            )
            .unwrap(),
            "---\nname: team-rest\ndescription: d\n---\nname: body\n"
        );
        assert_eq!(
            rename(
                "\u{feff}---\r\nname: \"x\"\r\ndescription: d\r\n---\r\n",
                "y"
            )
            .unwrap(),
            "---\r\nname: y\ndescription: d\r\n---\r\n"
        );
        assert!(rename("no header", "y").is_err());
    }

    #[test]
    fn a_package_holds_only_its_own_skill() {
        let package = tempfile::tempdir().unwrap();
        prepare_package(package.path(), "my_rest").unwrap();
        let own = package.path().join("skills/module-my-rest");
        std::fs::create_dir_all(&own).unwrap();
        assert!(
            prepare_package(package.path(), "my_rest")
                .unwrap_err()
                .contains("has no SKILL.md")
        );
        std::fs::write(own.join(FILE), "---\nname: rest\ndescription: d\n---\n").unwrap();
        prepare_package(package.path(), "my_rest").unwrap();
        assert_eq!(
            frontmatter(&std::fs::read_to_string(own.join(FILE)).unwrap())
                .unwrap()
                .0,
            "module-my-rest"
        );
        std::fs::create_dir_all(package.path().join("skills/other")).unwrap();
        assert!(
            prepare_package(package.path(), "my_rest")
                .unwrap_err()
                .contains("skills/other")
        );
    }
}
