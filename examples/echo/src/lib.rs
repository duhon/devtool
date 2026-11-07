//! Echo: the smallest module. It answers each run of a check with the check's text, without
//! calling any driver.

use serde_json::json;
use services_sdk::{Cmd, Event, Module, Rendered, Subscriptions, cmd};

#[derive(Debug, Default)]
pub struct Echo;

impl Module for Echo {
    type Msg = ();

    fn update(&mut self, event: Event<()>) -> Vec<Cmd> {
        match event {
            Event::Run { tag, check } => {
                let endpoint = check.text_of("endpoint").trim();
                if endpoint.is_empty() {
                    return vec![cmd::fail(&tag, "#endpoint is required", &json!({}))];
                }
                let rendered = Rendered {
                    text: check.text_of("text").to_owned(),
                    language: "text".into(),
                    outcome: "OK".into(),
                    columns: Vec::new(),
                };
                vec![cmd::result(&tag, &rendered, &json!({"endpoint": endpoint}))]
            }
            _ => Vec::new(),
        }
    }

    fn subscriptions(&self) -> Subscriptions {
        Subscriptions {
            run: true,
            ..Subscriptions::default()
        }
    }
}

services_sdk::export!(Echo);
