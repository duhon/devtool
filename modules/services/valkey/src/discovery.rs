use crate::resp::Resp;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const LIMIT: usize = 200;
pub const TTL_MS: f64 = 30_000.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    Keys(String),
    Hash(String),
}

impl Scope {
    pub fn first(&self) -> Vec<String> {
        match self {
            Self::Keys(_) => vec!["COMMAND".into()],
            Self::Hash(hash) => vec![
                "HSCAN".into(),
                hash.clone(),
                "0".into(),
                "COUNT".into(),
                "100".into(),
            ],
        }
    }
}

enum Stage {
    Commands,
    Scan,
    Types(usize),
    Hash,
    Done,
}

pub struct Collector {
    scope: Scope,
    stage: Stage,
    started: f64,
    pages: usize,
    seen: BTreeSet<String>,
    snapshot: Value,
}

impl Collector {
    pub fn new(scope: Scope) -> Self {
        let stage = match scope {
            Scope::Keys(_) => Stage::Commands,
            Scope::Hash(_) => Stage::Hash,
        };
        Self {
            scope,
            stage,
            started: services_sdk::now(),
            pages: 0,
            seen: BTreeSet::new(),
            snapshot: json!({"commands": [], "keys": [], "fields": [], "complete": true, "unavailable": {}}),
        }
    }

    pub fn snapshot(&self) -> Value {
        self.snapshot.clone()
    }

    pub fn accept(&mut self, reply: Resp) -> Result<Option<Vec<String>>, String> {
        match self.stage {
            Stage::Commands => {
                match reply {
                    Resp::Error(error) => self.snapshot["unavailable"]["commands"] = json!(error),
                    _ => {
                        let mut commands = BTreeMap::new();
                        metadata(&reply, &mut commands);
                        self.snapshot["commands"] =
                            json!(commands.into_values().collect::<Vec<_>>());
                    }
                }
                self.stage = Stage::Scan;
                Ok(Some(self.scan_command("0")))
            }
            Stage::Scan | Stage::Hash => {
                let hash = matches!(self.stage, Stage::Hash);
                if let Resp::Error(error) = reply {
                    self.snapshot["unavailable"][if hash { "fields" } else { "keys" }] =
                        json!(error);
                    self.snapshot["complete"] = json!(false);
                    self.stage = Stage::Done;
                    return Ok(None);
                }
                let page = reply.array().ok_or("Invalid SCAN response")?;
                let [cursor, values] = page else {
                    return Err("Invalid SCAN tuple".into());
                };
                let cursor = cursor.text().ok_or("Invalid SCAN cursor")?;
                let values = values.array().ok_or("Invalid SCAN values")?;
                let field = if hash { "fields" } else { "keys" };
                for value in values.iter().step_by(if hash { 2 } else { 1 }) {
                    let name = value.json();
                    if !self.seen.insert(name.to_string()) {
                        continue;
                    }
                    let items = self.snapshot[field].as_array_mut().unwrap();
                    if items.len() >= LIMIT {
                        self.snapshot["complete"] = json!(false);
                        break;
                    }
                    items.push(if hash {
                        name
                    } else {
                        json!({"name": name, "type": null})
                    });
                }
                self.pages += 1;
                if cursor != "0" {
                    let limit = self.snapshot[field].as_array().unwrap().len() >= LIMIT
                        || self.pages >= 32
                        || services_sdk::now() - self.started >= 2000.0;
                    if !limit {
                        return Ok(Some(self.scan_command(cursor)));
                    }
                    self.snapshot["complete"] = json!(false);
                }
                if hash {
                    self.stage = Stage::Done;
                    Ok(None)
                } else {
                    Ok(self.next_type(0))
                }
            }
            Stage::Types(index) => {
                match reply {
                    Resp::Error(error) => {
                        self.snapshot["unavailable"]["types"] = json!(error);
                        self.stage = Stage::Done;
                        return Ok(None);
                    }
                    reply => self.snapshot["keys"][index]["type"] = reply.json(),
                }
                Ok(self.next_type(index + 1))
            }
            Stage::Done => Err("Unexpected discovery response".into()),
        }
    }

    fn scan_command(&self, cursor: &str) -> Vec<String> {
        match &self.scope {
            Scope::Keys(prefix) => {
                let pattern: String = prefix
                    .chars()
                    .flat_map(|ch| {
                        if matches!(ch, '*' | '?' | '[' | ']' | '\\') {
                            vec!['\\', ch]
                        } else {
                            vec![ch]
                        }
                    })
                    .collect();
                vec![
                    "SCAN".into(),
                    cursor.into(),
                    "MATCH".into(),
                    format!("{pattern}*"),
                    "COUNT".into(),
                    "100".into(),
                ]
            }
            Scope::Hash(hash) => vec![
                "HSCAN".into(),
                hash.clone(),
                cursor.into(),
                "COUNT".into(),
                "100".into(),
            ],
        }
    }

    fn next_type(&mut self, mut index: usize) -> Option<Vec<String>> {
        let keys = self.snapshot["keys"].as_array().unwrap();
        while let Some(key) = keys.get(index) {
            if let Some(name) = key["name"].as_str() {
                let args = vec!["TYPE".into(), name.into()];
                self.stage = Stage::Types(index);
                return Some(args);
            }
            index += 1;
        }
        self.stage = Stage::Done;
        None
    }
}

fn metadata(reply: &Resp, commands: &mut BTreeMap<String, Value>) {
    let Some(rows) = reply.array() else { return };
    for row in rows {
        let Some(row) = row.array() else { continue };
        if row.len() < 6 {
            continue;
        }
        let Some(name) = row[0].text() else { continue };
        commands.insert(
            name.to_ascii_uppercase(),
            json!({
                "name": name.replace('|', " ").to_ascii_uppercase(),
                "arity": row[1].json(), "flags": row[2].json(),
                "firstKey": row[3].json(), "lastKey": row[4].json(), "keyStep": row[5].json(),
            }),
        );
        if let Some(subcommands) = row.get(9) {
            metadata(subcommands, commands);
        }
    }
}
