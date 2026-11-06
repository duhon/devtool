//! The MySQL console part: checks become SQL queries (`\G` asks for vertical results).

use serde_json::{Value, json};
use services_module_ui::{Sections, ServiceUi};

/// The module's manifest, for consoles that know the module before a host lists it.
pub const MANIFEST: &str = include_str!("../../manifest.json");

pub struct Mysql;

impl ServiceUi for Mysql {
    fn request(&self, s: &Sections) -> Result<Value, String> {
        let endpoint = s.text("endpoint");
        if !endpoint.to_ascii_lowercase().starts_with("mysql://") {
            return Err(
                "MySQL request needs a #endpoint block (a mysql:// connection string)".into(),
            );
        }
        let mut value = parse_sql(s.text("query"), endpoint)?;
        for (name, option) in s.pairs("options") {
            if name.eq_ignore_ascii_case("Columns-Type") {
                value["columnsType"] = option;
            }
        }
        services_contracts::columns::Options::parse(&value)?;
        Ok(value)
    }
}

/// A query and its connection; a final `\G` (with an optional `;` on either side) asks for
/// vertical results and is not sent.
pub fn parse_sql(source: &str, connection: &str) -> Result<Value, String> {
    let mut sql = source.trim();
    if sql.is_empty() {
        return Err("SQL query is required".into());
    }
    let tail = sql.strip_suffix(';').unwrap_or(sql).trim_end();
    let marker = tail
        .strip_suffix("\\G")
        .or_else(|| tail.strip_suffix("\\g"));
    if let Some(query) = marker {
        let query = query.trim_end();
        sql = query.strip_suffix(';').unwrap_or(query).trim();
    }
    Ok(json!({"sql":sql,"connectionString":connection,"vertical":marker.is_some()}))
}

services_module_ui::export!(Mysql);
