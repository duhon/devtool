use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Text,
    Number,
    Boolean,
    Datetime,
    Json,
}

impl Kind {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value.to_ascii_lowercase().as_str() {
            "text" | "string" => Self::Text,
            "number" | "numeric" | "integer" | "int" | "float" | "decimal" => Self::Number,
            "boolean" | "bool" => Self::Boolean,
            "datetime" | "date" | "time" | "timestamp" => Self::Datetime,
            "json" => Self::Json,
            _ => return None,
        })
    }
}

#[derive(Default)]
pub struct Options {
    pub types: BTreeMap<String, Kind>,
    pub sizes: BTreeMap<String, usize>,
}

fn assignments(params: &Value, key: &str) -> Result<Vec<(String, String)>, String> {
    let Some(value) = params.get(key).filter(|v| !v.is_null()) else {
        return Ok(vec![]);
    };
    let source = value
        .as_str()
        .ok_or_else(|| format!("{key} must contain column=value assignments"))?;
    let mut result = BTreeMap::new();
    for pair in source.split_whitespace() {
        let (name, value) = pair
            .split_once('=')
            .filter(|(k, v)| !k.is_empty() && !v.is_empty())
            .ok_or_else(|| format!("Invalid {key} assignment {pair:?}; expected column=value"))?;
        if result.insert(name.to_owned(), value.to_owned()).is_some() {
            return Err(format!("Duplicate {key} assignment for {name}"));
        }
    }
    Ok(result.into_iter().collect())
}

impl Options {
    pub fn parse(params: &Value) -> Result<Self, String> {
        let mut options = Self::default();
        for (name, value) in assignments(params, "columnsType")? {
            let kind = Kind::parse(&value)
                .ok_or_else(|| format!("Unsupported column type {value:?} for {name}"))?;
            options.types.insert(name, kind);
        }
        for (name, value) in assignments(params, "columnsSize")? {
            let size = value
                .parse::<usize>()
                .ok()
                .filter(|n| *n > 0)
                .ok_or_else(|| {
                    format!("Column size for {name} must be a positive number of characters")
                })?;
            options.sizes.insert(name, size);
        }
        Ok(options)
    }

    pub fn apply(&self, result: &mut Value) -> Result<(), String> {
        if result["cancelled"] == true {
            return Ok(());
        }
        for name in self.types.keys().chain(self.sizes.keys()) {
            if !result["columns"]
                .as_array()
                .is_some_and(|columns| columns.iter().any(|c| c.as_str() == Some(name)))
            {
                return Err(format!(
                    "Column options refer to an unknown result column: {name}"
                ));
            }
        }
        if !self.types.is_empty() {
            if !result["columnTypes"].is_object() {
                result["columnTypes"] = json!({});
            }
            for (name, kind) in &self.types {
                result["columnTypes"][name] = json!(kind);
            }
        }
        if !self.sizes.is_empty() {
            result["columnSizes"] = json!(self.sizes);
        }
        Ok(())
    }
}

pub fn kind(result: &Value, name: &str) -> Kind {
    result["columnTypes"][name]
        .as_str()
        .and_then(Kind::parse)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_names_aliases_and_overrides_preserve_raw_values() {
        let options=Options::parse(&json!({"columnsType":"message.timestamp=timestamp message.message=JSON amount=decimal enabled=bool","columnsSize":"message.message=100"})).unwrap();
        let mut result = json!({"columns":["message.timestamp","message.message","amount","enabled"],
            "columnTypes":{"amount":"text"},"rows":[{"amount":"123456789012345678.123456789"}]});
        let rows = result["rows"].clone();
        options.apply(&mut result).unwrap();
        assert_eq!(kind(&result, "message.timestamp"), Kind::Datetime);
        assert_eq!(kind(&result, "message.message"), Kind::Json);
        assert_eq!(kind(&result, "amount"), Kind::Number);
        assert_eq!(kind(&result, "enabled"), Kind::Boolean);
        assert_eq!(result["columnSizes"]["message.message"], 100);
        assert_eq!(result["rows"], rows);
    }

    #[test]
    fn invalid_options_and_unknown_columns_fail_explicitly() {
        for value in [
            json!({"columnsType":"missing-equals"}),
            json!({"columnsType":"=json"}),
            json!({"columnsType":"a=xml"}),
            json!({"columnsType":"a=json a=text"}),
            json!({"columnsSize":"a=0"}),
            json!({"columnsSize":"a=-1"}),
            json!({"columnsSize":"a=1.5"}),
            json!({"columnsSize":"a=10px"}),
            json!({"columnsSize":"a=2 a=3"}),
            json!({"columnsType":3}),
        ] {
            assert!(Options::parse(&value).is_err(), "{value}");
        }
        let options = Options::parse(&json!({"columnsType":"missing=json"})).unwrap();
        assert!(
            options
                .apply(&mut json!({"columns":["present"]}))
                .unwrap_err()
                .contains("missing")
        );
        options.apply(&mut json!({"cancelled":true})).unwrap();
    }
}
