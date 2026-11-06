//! The Terminal console part: checks become shell commands.

use serde_json::{Value, json};
use services_module_ui::{Sections, ServiceUi};

/// The module's manifest, for consoles that know the module before a host lists it.
pub const MANIFEST: &str = include_str!("../../manifest.json");

pub struct Terminal;

impl ServiceUi for Terminal {
    fn request(&self, s: &Sections) -> Result<Value, String> {
        let command = if s.has("command") {
            s.text("command")
        } else {
            s.text("leftover")
        };
        Ok(json!({"endpoint":s.text("endpoint"),"env":s.pairs("env"),"command":command}))
    }
}

services_module_ui::export!(Terminal);
