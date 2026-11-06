//! The static `manifest.json` of a module: only what the Kernel needs before loading its wasm.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Service,
    Panel,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub id: String,
    pub kind: Kind,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    pub abi: u32,
    /// The App versions the module runs in, e.g. `>=4.0`.
    #[serde(default)]
    pub app: String,
    /// Host drivers the module calls and their major versions.
    #[serde(default)]
    pub drivers: BTreeMap<String, String>,
    /// Tab or button position and the order of request hooks.
    #[serde(default)]
    pub order: Option<u32>,
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Self, String> {
        let manifest: Self =
            serde_json::from_str(text).map_err(|error| format!("Invalid manifest: {error}"))?;
        let id_ok = !manifest.id.is_empty()
            && manifest
                .id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if !id_ok {
            return Err(format!("Invalid module id \"{}\"", manifest.id));
        }
        Ok(manifest)
    }

    /// Whether this App, with the drivers its Host offers, can run the module.
    pub fn check(&self, app: &str, drivers: &BTreeMap<String, String>) -> Result<(), String> {
        if self.abi != crate::ABI {
            return Err(format!(
                "{} was built for module ABI {}, the App runs ABI {}",
                self.name,
                self.abi,
                crate::ABI
            ));
        }
        if !satisfies(app, &self.app)? {
            return Err(format!(
                "{} needs App {}, this is {app}",
                self.name, self.app
            ));
        }
        for (driver, wanted) in &self.drivers {
            match drivers.get(driver) {
                Some(offered) if major(offered) == major(wanted) => {}
                Some(offered) => {
                    return Err(format!(
                        "{} needs the {driver} driver {wanted}, the Host has {offered}",
                        self.name
                    ));
                }
                None => {
                    return Err(format!(
                        "{} needs the {driver} driver, the Host has none",
                        self.name
                    ));
                }
            }
        }
        Ok(())
    }
}

fn major(version: &str) -> &str {
    version.split('.').next().unwrap_or("")
}

fn numbers(version: &str) -> Result<Vec<u64>, String> {
    version
        .trim()
        .split('.')
        .map(|part| {
            part.parse()
                .map_err(|_| format!("Invalid version \"{version}\""))
        })
        .collect()
}

/// `requirement` is empty or `>=x.y[.z]`.
fn satisfies(version: &str, requirement: &str) -> Result<bool, String> {
    let requirement = requirement.trim();
    if requirement.is_empty() {
        return Ok(true);
    }
    let minimum = requirement
        .strip_prefix(">=")
        .ok_or_else(|| format!("Unsupported App requirement \"{requirement}\""))?;
    Ok(numbers(version)? >= numbers(minimum)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(extra: &str) -> Result<Manifest, String> {
        Manifest::parse(&format!(
            r#"{{"id":"rest","kind":"service","name":"REST","version":"1.0.0","abi":{}{extra}}}"#,
            crate::ABI
        ))
    }

    #[test]
    fn checks_abi_app_and_drivers() {
        let drivers = BTreeMap::from([("http".to_owned(), "1".to_owned())]);
        let ok = manifest(r#","app":">=3.7","drivers":{"http":"1"}"#).unwrap();
        assert_eq!(ok.check("3.7.0", &drivers), Ok(()));
        assert!(
            ok.check("3.6.9", &drivers)
                .unwrap_err()
                .contains("needs App >=3.7")
        );
        let grpc = manifest(r#","drivers":{"grpc":"1"}"#).unwrap();
        assert!(
            grpc.check("3.7.0", &drivers)
                .unwrap_err()
                .contains("has none")
        );
        let newer = manifest(r#","drivers":{"http":"2"}"#).unwrap();
        assert!(
            newer
                .check("3.7.0", &drivers)
                .unwrap_err()
                .contains("Host has 1")
        );
        let mut old = ok.clone();
        old.abi += 1;
        assert!(
            old.check("3.7.0", &drivers)
                .unwrap_err()
                .contains("module ABI")
        );
    }

    #[test]
    fn rejects_unknown_fields_and_bad_ids() {
        assert!(
            manifest(r#","params":{}"#)
                .unwrap_err()
                .contains("unknown field")
        );
        assert!(
            Manifest::parse(r#"{"id":"A b","kind":"panel","name":"x","version":"1","abi":1}"#)
                .unwrap_err()
                .contains("Invalid module id")
        );
    }
}
