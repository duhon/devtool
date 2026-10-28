use super::error::{Error, Result};
use serde_json::{Value, json};
use services_contracts::templates::{self, Library};
use std::{
    fs::{self, OpenOptions},
    io::{ErrorKind, Write},
    path::PathBuf,
};

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
    pub fn get(&self) -> Result<Value> {
        Ok(json!({"templates":self.read()?}))
    }
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
        if !templates::sections(service).contains(&section) {
            return Err(Error::new("Unknown template service or section"));
        }
        templates::parse(text).map_err(Error::new)?;
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
        let mut library = self.read()?;
        let current = library
            .get(service)
            .and_then(|sections| sections.get(section))
            .map(String::as_str)
            .unwrap_or("");
        if current != base && current != text {
            return Err(Error::new(
                "CONFLICT: templates changed in another window. Your draft is preserved; discard it and reload to use the saved version.",
            ));
        }
        library
            .entry(service.into())
            .or_default()
            .insert(section.into(), text.into());
        let bytes = serde_json::to_vec_pretty(&library)?;
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
        Ok(json!({"templates":library}))
    }
}
