use super::{error::Result, sources};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

/// Console hints per module. A module's hints are its local section of the hints file, else
/// the hints of the highest-priority applied source, else its manifest `console.hints`.
#[derive(Clone)]
pub struct Hints {
    path: PathBuf,
    lock: Arc<Mutex<()>>,
}
impl Hints {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Arc::new(Mutex::new(())),
        }
    }
    fn data(&self) -> &Path {
        self.path.parent().unwrap_or(Path::new("."))
    }
    fn sections(&self) -> Vec<(String, String)> {
        let text = fs::read_to_string(&self.path).unwrap_or_default();
        let mut sections: Vec<(String, String)> = vec![];
        for line in text.lines() {
            let trimmed = line.trim();
            if let Some(name) = trimmed.strip_prefix("##").map(str::trim).filter(|s| {
                !s.is_empty()
                    && s.chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            }) {
                sections.push((name.to_lowercase(), String::new()));
            } else if let Some((_, body)) = sections.last_mut() {
                body.push_str(line);
                body.push('\n');
            }
        }
        sections
    }
    /// The hints of `service` under the local layer, and the source they come from.
    fn base(&self, service: &str, default: &str) -> (String, Option<String>) {
        match sources::hint(self.data(), service) {
            Some((source, text)) => (text, Some(source)),
            None => (default.to_owned(), None),
        }
    }
    /// `{text, origin: "local"|"source"|"module", source?}`: the hints of `service`, where
    /// they come from and the source under local hints; `default` is the manifest's hints.
    pub fn get(&self, service: &str, default: &str) -> Value {
        if let Some((_, text)) = self.sections().into_iter().find(|(s, _)| s == service) {
            let text = if text.is_empty() {
                text
            } else {
                format!("{}\n", text.trim_end_matches('\n'))
            };
            let mut value = json!({"text": text, "origin": "local"});
            if let (_, Some(source)) = self.base(service, default) {
                value["source"] = json!(source);
            }
            return value;
        }
        match self.base(service, default) {
            (text, Some(source)) => json!({"text": text, "origin": "source", "source": source}),
            (text, None) => json!({"text": text, "origin": "module"}),
        }
    }
    /// Saves the hints of `service` locally; hints equal to the ones under the local layer
    /// drop the local section instead.
    pub fn save(&self, service: &str, text: &str, default: &str) -> Result<Value> {
        let (base, _) = self.base(service, default);
        if text.trim_end() == base.trim_end() {
            self.forget(service)?;
        } else {
            self.change(|sections| {
                if let Some((_, v)) = sections.iter_mut().find(|(s, _)| s == service) {
                    *v = text.into();
                } else {
                    sections.push((service.into(), text.into()));
                }
            })?;
        }
        Ok(self.get(service, default))
    }
    /// Deletes the local hints of `service`; it gets the hints of its source or manifest.
    pub fn forget(&self, service: &str) -> Result<()> {
        self.change(|sections| sections.retain(|(s, _)| s != service))
    }
    /// The modules with local hints.
    pub fn local(&self) -> BTreeSet<String> {
        self.sections().into_iter().map(|(name, _)| name).collect()
    }
    fn change(&self, edit: impl FnOnce(&mut Vec<(String, String)>)) -> Result<()> {
        let _lock = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let parent = self.data();
        fs::create_dir_all(parent)?;
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.path.with_extension("lock"))?;
        file.lock()?;
        let mut sections = self.sections();
        let before = sections.clone();
        edit(&mut sections);
        if sections == before {
            return Ok(());
        }
        let mut output = String::new();
        for (name, text) in sections {
            output.push_str(&format!("## {name}\n{text}"));
            if !text.is_empty() && !text.ends_with('\n') {
                output.push('\n');
            }
        }
        sources::write_atomic(&self.path, output.as_bytes(), false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_hints_cover_source_hints_and_saving_the_source_text_drops_them() {
        let data = tempfile::tempdir().unwrap();
        let team = tempfile::tempdir().unwrap();
        fs::create_dir_all(team.path().join("hints")).unwrap();
        fs::write(team.path().join("hints/rest.md"), "team rest\n").unwrap();
        let sources = sources::Sources::new(data.path());
        sources
            .save(&json!({"sources": [{"name": "Team", "kind": "folder", "path": team.path()}]}))
            .unwrap();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(sources.apply("team"))
            .unwrap();
        let hints = Hints::new(data.path().join(".console-hints.txt"));
        assert_eq!(
            hints.get("rest", "default"),
            json!({"text": "team rest\n", "origin": "source", "source": "Team"})
        );
        assert_eq!(hints.get("mysql", "default")["origin"], "module");
        hints.save("rest", "mine", "default").unwrap();
        assert_eq!(
            hints.get("rest", "default"),
            json!({"text": "mine\n", "origin": "local", "source": "Team"})
        );
        assert_eq!(hints.local(), BTreeSet::from(["rest".to_owned()]));
        hints.save("rest", "team rest", "default").unwrap();
        assert_eq!(hints.get("rest", "default")["origin"], "source");
        assert!(hints.local().is_empty());
    }
}
