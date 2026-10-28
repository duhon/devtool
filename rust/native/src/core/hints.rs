use super::error::Result;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
const HTTP: &str = "#endpoint\nhttp://\nhttps://\n\n#headers\nAccept: */*\nAccept-Encoding: gzip, deflate, br\nAuthorization: ****** application/json\nCookie:\nHost:\nUser-Agent:\nX-Api-Key:\nX-Forwarded-For:\nX-Requested-With: XMLHttpRequest\n";
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
    fn sections(&self) -> Vec<(String, String)> {
        let text = std::fs::read_to_string(&self.path).unwrap_or_default();
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
    pub fn get(&self, service: &str) -> Value {
        let value = self
            .sections()
            .into_iter()
            .find(|(s, _)| s == service)
            .map(|(_, v)| {
                if v.is_empty() {
                    v
                } else {
                    format!("{}\n", v.trim_end_matches('\n'))
                }
            })
            .unwrap_or_else(|| match service {
                "mysql" => "#endpoint\nmysql://\n".into(),
                "rest" | "grpc" | "graphql" | "splunk" => HTTP.into(),
                _ => String::new(),
            });
        json!({"text":value})
    }
    pub fn save(&self, service: &str, text: &str) -> Result<Value> {
        let _lock = self.lock.lock().unwrap();
        let mut sections = self.sections();
        if let Some((_, v)) = sections.iter_mut().find(|(s, _)| s == service) {
            *v = text.into();
        } else {
            sections.push((service.into(), text.into()));
        }
        let mut output = String::new();
        for (name, text) in sections {
            output.push_str(&format!("## {name}\n{text}"));
            if !text.is_empty() && !text.ends_with('\n') {
                output.push('\n');
            }
        }
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.path, output)?;
        Ok(json!({"text":text}))
    }
}
