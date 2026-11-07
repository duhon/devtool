use super::{
    error::{Error, Result},
    sources,
};
use serde_json::{Value, json};
use services_contracts::templates::{self, Library};
use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
};

/// Template sections per module. A section is its local text, else the text of the
/// highest-priority applied source that has it. The Host does not know which sections a
/// module has: the App offers them, the Host only checks their names.
#[derive(Clone)]
pub struct Templates {
    path: PathBuf,
}
impl Templates {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
    fn read(&self) -> Result<Library> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Library::new()),
            Err(error) => return Err(error.into()),
        };
        let library: Library = serde_json::from_slice(&bytes)?;
        templates::validate(&library).map_err(Error::new)?;
        Ok(library)
    }
    /// The sections of the applied sources and the source each comes from.
    fn base(&self) -> Result<(Library, Library)> {
        let mut library = Library::new();
        let mut origins = Library::new();
        let data = self.path.parent().unwrap_or(Path::new("."));
        for (layer, value) in sources::layer_files(data, sources::TEMPLATES)? {
            let layer_library: Library = serde_json::from_value(value).map_err(|e| {
                Error::new(format!(
                    "{}: {e}",
                    layer.dir.join(sources::TEMPLATES).display()
                ))
            })?;
            for (service, sections) in layer_library {
                for (section, text) in sections {
                    origins
                        .entry(service.clone())
                        .or_default()
                        .insert(section.clone(), layer.name.clone());
                    library
                        .entry(service.clone())
                        .or_default()
                        .insert(section, text);
                }
            }
        }
        Ok((library, origins))
    }
    /// `{templates, origins}`: every section and where it comes from,
    /// `{origin: "local"|"source", source?}` with the source under a local section.
    pub fn get(&self) -> Result<Value> {
        let (mut library, sources) = self.base()?;
        let mut origins = serde_json::Map::new();
        for (service, sections) in &sources {
            for (section, source) in sections {
                origins.entry(service.clone()).or_insert_with(|| json!({}))[section] =
                    json!({"origin": "source", "source": source});
            }
        }
        for (service, sections) in self.read()? {
            for (section, text) in sections {
                let origin = origins.entry(service.clone()).or_insert_with(|| json!({}));
                origin[&section]["origin"] = json!("local");
                library
                    .entry(service.clone())
                    .or_default()
                    .insert(section, text);
            }
        }
        Ok(json!({"templates": library, "origins": origins}))
    }
    /// The sections with local text.
    pub fn local(&self) -> Result<BTreeSet<(String, String)>> {
        Ok(self
            .read()?
            .into_iter()
            .flat_map(|(service, sections)| {
                sections
                    .into_keys()
                    .map(move |section| (service.clone(), section))
            })
            .collect())
    }
    /// Saves a section locally when `base` is still its text; text equal to the source's
    /// drops the local section instead.
    pub fn save(&self, message: &Value) -> Result<Value> {
        let required = |key| {
            message[key]
                .as_str()
                .ok_or_else(|| Error::new(format!("Templates {key} must be a string")))
        };
        let service = required("serviceName")?;
        let section = required("section")?;
        let text = required("text")?;
        let base = required("base")?;
        let name = |name: &str| {
            !name.is_empty()
                && name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
        };
        if !name(service) || !name(section) {
            return Err(Error::new("Invalid template service or section"));
        }
        templates::parse(text).map_err(Error::new)?;
        let _lock = self.lock()?;
        let mut library = self.read()?;
        let (sources, _) = self.base()?;
        let source = sources
            .get(service)
            .and_then(|sections| sections.get(section))
            .map(String::as_str);
        let current = library
            .get(service)
            .and_then(|sections| sections.get(section))
            .map(String::as_str)
            .or(source)
            .unwrap_or("");
        if current != base && current != text {
            return Err(Error::new(
                "CONFLICT: templates changed in another window. Your draft is preserved; discard it and reload to use the saved version.",
            ));
        }
        if source.unwrap_or("") == text {
            Self::remove(&mut library, service, section);
        } else {
            library
                .entry(service.into())
                .or_default()
                .insert(section.into(), text.into());
        }
        self.write(&library)?;
        drop(_lock);
        self.get()
    }
    /// Drops the local text of a section; it gets the text of its source again.
    pub fn reset(&self, message: &Value) -> Result<Value> {
        let service = super::error::text(message, "serviceName");
        let section = super::error::text(message, "section");
        if self.path.exists() {
            let _lock = self.lock()?;
            let mut library = self.read()?;
            if Self::remove(&mut library, &service, &section) {
                self.write(&library)?;
            }
        }
        self.get()
    }
    fn remove(library: &mut Library, service: &str, section: &str) -> bool {
        let Some(sections) = library.get_mut(service) else {
            return false;
        };
        let removed = sections.remove(section).is_some();
        if sections.is_empty() {
            library.remove(service);
        }
        removed
    }
    /// Deletes the local templates of `service`.
    pub fn forget(&self, service: &str) -> Result<()> {
        if !self.path.exists() {
            return Ok(());
        }
        let _lock = self.lock()?;
        let mut library = self.read()?;
        if library.remove(service).is_some() {
            self.write(&library)?;
        }
        Ok(())
    }
    /// Serializes changes of the library file across host processes.
    fn lock(&self) -> Result<fs::File> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options.open(self.path.with_extension("lock"))?;
        lock.lock()?;
        Ok(lock)
    }
    fn write(&self, library: &Library) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(library)?;
        if bytes.len() > 512 * 1024 {
            return Err(Error::new("The global template library exceeds 512 KiB."));
        }
        let pending = self
            .path
            .with_extension(format!("{}.pending", std::process::id()));
        let mut output = OpenOptions::new();
        output.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            output.mode(0o600);
        }
        let mut file = output.open(&pending)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(pending, &self.path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_sections_cover_source_sections_until_reset() {
        let data = tempfile::tempdir().unwrap();
        let team = tempfile::tempdir().unwrap();
        fs::write(
            team.path().join(sources::TEMPLATES),
            r#"{"rest":{"check":"@team\nGET /\n"}}"#,
        )
        .unwrap();
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
        let templates = Templates::new(data.path().join(".console-templates.json"));
        let team_text = "@team\nGET /\n";
        let got = templates.get().unwrap();
        assert_eq!(got["templates"]["rest"]["check"], team_text);
        assert_eq!(
            got["origins"]["rest"]["check"],
            json!({"origin": "source", "source": "Team"})
        );
        let save = |text: &str, base: &str| {
            templates.save(
                &json!({"serviceName": "rest", "section": "check", "text": text, "base": base}),
            )
        };
        let got = save("@mine\nPOST /\n", team_text).unwrap();
        assert_eq!(got["templates"]["rest"]["check"], "@mine\nPOST /\n");
        assert_eq!(
            got["origins"]["rest"]["check"],
            json!({"origin": "local", "source": "Team"})
        );
        assert!(save("@other\nPUT /\n", team_text).is_err());
        let got = templates
            .reset(&json!({"serviceName": "rest", "section": "check"}))
            .unwrap();
        assert_eq!(got["templates"]["rest"]["check"], team_text);
        save("@mine\nPOST /\n", team_text).unwrap();
        save(team_text, "@mine\nPOST /\n").unwrap();
        assert!(templates.local().unwrap().is_empty());
    }
}
