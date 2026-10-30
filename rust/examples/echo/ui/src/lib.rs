//! The Echo console part: turns the sections of a check into request params.

use serde_json::{Value, json};
use services_module_ui::{Sections, ServiceUi};

pub struct Echo;

impl ServiceUi for Echo {
    fn request(&self, s: &Sections) -> Result<Value, String> {
        let endpoint = s.text("endpoint");
        if endpoint.is_empty() {
            return Err("#endpoint is empty".into());
        }
        Ok(json!({"endpoint": endpoint, "text": s.text("text")}))
    }
}

services_module_ui::export!(Echo);
