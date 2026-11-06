//! The REST console part: checks become HTTP requests, `curl` command lines are imported
//! and exported.

use serde_json::{Value, json};
use services_module_ui::{
    Imported, Sections, ServiceUi, command, curl, imported_body, imported_headers,
};

/// The module's manifest, for consoles that know the module before a host lists it.
pub const MANIFEST: &str = include_str!("../../manifest.json");

pub struct Rest;

impl ServiceUi for Rest {
    fn request(&self, s: &Sections) -> Result<Value, String> {
        let pieces: Vec<_> = s.text("endpoint").split_whitespace().collect();
        if pieces.len() != 2
            || !["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"]
                .contains(&pieces[0].to_ascii_uppercase().as_str())
        {
            return Err("#endpoint must be METHOD URL".into());
        }
        let mut out = json!({"method":pieces[0].to_ascii_uppercase(),"url":pieces[1],"headers":s.pairs("headers")});
        if !s.text("body").is_empty() {
            out["body"] = json!(s.text("body"));
        }
        Ok(out)
    }

    fn import(&self, command: &str) -> Result<Imported, String> {
        let p = command::parse_curl(command)?;
        let endpoint = format!(
            "{} {}",
            p["method"].as_str().unwrap(),
            p["url"].as_str().unwrap()
        );
        Ok(Imported {
            endpoint,
            sections: [imported_headers(&p), imported_body(&p, "body")].concat(),
        })
    }

    fn export(&self, p: &Value) -> Result<String, String> {
        Ok(curl(
            p["method"].as_str().unwrap_or("GET"),
            p["url"].as_str().unwrap_or(""),
            p["headers"].as_object().cloned().unwrap_or_default(),
            p["body"].as_str().unwrap_or(""),
        ))
    }
}

services_module_ui::export!(Rest);
