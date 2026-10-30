use serde::{Deserialize, Serialize};

pub const FUSION_MODEL_ID: &str = "auto";
pub const FUSION_MODEL_NAME: &str = "Copilot Fusion";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Options {
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    pub id: String,
    pub name: String,
    pub category: Option<String>,
    pub reasoning_efforts: Vec<String>,
    pub default_reasoning_effort: Option<String>,
    pub enabled: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    pub models: Vec<Model>,
    pub current: Options,
}

impl Options {
    pub fn for_new_chat() -> Self {
        Self {
            model: Some(FUSION_MODEL_ID.into()),
            reasoning_effort: None,
        }
    }

    pub fn validate(&self, models: &[Model]) -> Result<(), String> {
        let Some(id) = &self.model else {
            return if self.reasoning_effort.is_some() {
                Err("Choose a model before setting Reasoning".into())
            } else {
                Ok(())
            };
        };
        if id == FUSION_MODEL_ID {
            return if self.reasoning_effort.is_none() {
                Ok(())
            } else {
                Err("Copilot Fusion manages Reasoning automatically".into())
            };
        }
        let model = models
            .iter()
            .find(|model| &model.id == id && model.enabled)
            .ok_or_else(|| format!("Copilot model is unavailable: {id}"))?;
        if let Some(effort) = &self.reasoning_effort
            && !model.reasoning_efforts.contains(effort)
        {
            return Err(format!("Reasoning {effort} is not supported by {id}"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fusion_default_does_not_change_legacy_options_or_require_a_model_catalog() {
        let mut options = Options::for_new_chat();
        assert_eq!(options.model.as_deref(), Some("auto"));
        assert!(options.reasoning_effort.is_none());
        assert!(options.validate(&[]).is_ok());
        options.reasoning_effort = Some("high".into());
        assert!(options.validate(&[]).is_err());
        assert!(Options::default().model.is_none());
        let legacy: Options = serde_json::from_str("{}").unwrap();
        assert_eq!(legacy, Options::default());
        let saved: Options =
            serde_json::from_str(r#"{"model":"saved-model","reasoningEffort":"high"}"#).unwrap();
        assert_eq!(saved.model.as_deref(), Some("saved-model"));
        assert_eq!(saved.reasoning_effort.as_deref(), Some("high"));
    }

    #[test]
    fn validates_model_availability_and_supported_reasoning() {
        let models = vec![Model {
            id: "fixture".into(),
            name: "Fixture".into(),
            category: Some("powerful".into()),
            reasoning_efforts: vec!["low".into(), "high".into()],
            default_reasoning_effort: Some("low".into()),
            enabled: true,
        }];
        assert!(Options::default().validate(&models).is_ok());
        let mut options = Options {
            model: Some("fixture".into()),
            reasoning_effort: Some("high".into()),
        };
        assert!(options.validate(&models).is_ok());
        options.reasoning_effort = Some("max".into());
        assert!(options.validate(&models).is_err());
        options.reasoning_effort = None;
        options.model = Some("missing".into());
        assert!(options.validate(&models).is_err());
        options.model = None;
        options.reasoning_effort = Some("low".into());
        assert!(options.validate(&models).is_err());
        let mut disabled = models;
        disabled[0].enabled = false;
        options.model = Some("fixture".into());
        assert!(options.validate(&disabled).is_err());
    }
}
