#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const MANIFEST_VERSION: u32 = 1;

/// Regex documentadas na spec (manifest §1); validação manual sem dependência C.
pub const NAME_RE: &str = r"^[a-z0-9][a-z0-9-]{0,38}/[a-z0-9][a-z0-9-]{0,63}$";
pub const PROFILE_RE: &str = r"^[a-z0-9][a-z0-9-]{0,31}$";

fn valid_name(name: &str) -> bool {
    let mut parts = name.split('/');
    let (Some(pub_), Some(item)) = (parts.next(), parts.next()) else {
        return false;
    };
    if parts.next().is_some() {
        return false;
    }
    valid_name_part(pub_, 39) && valid_name_part(item, 64)
}

fn valid_name_part(s: &str, max: usize) -> bool {
    if s.is_empty() || s.len() > max {
        return false;
    }
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

pub fn valid_profile_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 32 {
        return false;
    }
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Manifesto `atom.toml` (spec manifest §3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub manifest: u32,
    pub name: String,
    pub version: String,
    pub kind: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub homepage: String,
    pub engine: String,
    #[serde(default)]
    pub protocol: Option<String>,
    #[serde(default)]
    pub entrypoint: Option<String>,
    #[serde(default)]
    pub runtime: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub activation: Vec<String>,
    #[serde(default)]
    pub requires: Requires,
    #[serde(default)]
    pub provides: Vec<ProvidesDecl>,
    #[serde(default)]
    pub handles: Vec<String>,
    #[serde(default)]
    pub intercepts: Vec<InterceptDecl>,
    #[serde(default)]
    pub subscribes: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub config_schema: Option<String>,
    #[serde(default)]
    pub contributes: HashMap<String, toml::Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Requires {
    #[serde(default)]
    pub packages: HashMap<String, String>,
    #[serde(default)]
    pub contracts: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvidesDecl {
    pub contract: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterceptDecl {
    #[serde(rename = "match")]
    pub match_: String,
    pub phase: String,
    #[serde(default = "default_priority")]
    pub priority: i32,
}

fn default_priority() -> i32 {
    100
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("manifest {0} maior que o suportado ({1})")]
    FutureVersion(u32, u32),
    #[error("campo obrigatório ausente ou inválido: {0}")]
    Invalid(String),
    #[error("toml: {0}")]
    Toml(String),
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Self, ManifestError> {
        let m: Manifest = toml::from_str(text).map_err(|e| ManifestError::Toml(e.to_string()))?;
        m.validate()?;
        Ok(m)
    }

    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.manifest > MANIFEST_VERSION {
            return Err(ManifestError::FutureVersion(
                self.manifest,
                MANIFEST_VERSION,
            ));
        }
        if !valid_name(&self.name) {
            return Err(ManifestError::Invalid(format!("name {:?}", self.name)));
        }
        if semver::Version::parse(&self.version).is_err() {
            return Err(ManifestError::Invalid(format!(
                "version {:?}",
                self.version
            )));
        }
        if self.kind.is_empty() {
            return Err(ManifestError::Invalid("kind vazio".to_string()));
        }
        if self.engine.is_empty() {
            return Err(ManifestError::Invalid("engine vazio".to_string()));
        }
        let executable = self.entrypoint.is_some();
        if executable && self.protocol.as_deref() != Some("atom/1") {
            return Err(ManifestError::Invalid(
                "protocol deve ser \"atom/1\" para pacote executável".to_string(),
            ));
        }
        if !executable
            && (!self.provides.is_empty()
                || !self.handles.is_empty()
                || !self.intercepts.is_empty()
                || !self.subscribes.is_empty())
        {
            return Err(ManifestError::Invalid(
                "pacote declarativo não pode ter provides/handles/intercepts/subscribes"
                    .to_string(),
            ));
        }
        for p in &self.provides {
            if !p.contract.contains('@') {
                return Err(ManifestError::Invalid(format!(
                    "contrato {:?} sem @major",
                    p.contract
                )));
            }
        }
        Ok(())
    }

    pub fn id(&self) -> String {
        format!("{}@{}", self.name, self.version)
    }

    pub fn is_executable(&self) -> bool {
        self.entrypoint.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: &str = r#"
manifest = 1
name = "test/echo"
version = "0.1.0"
kind = "extension"
engine = ">=0.1.0"
protocol = "atom/1"
entrypoint = "bin/echo"

[[provides]]
contract = "test.echo@0"
version = "0.1.0"
"#;

    #[test]
    fn parses_minimal_executable() {
        let m = Manifest::parse(MIN).unwrap();
        assert_eq!(m.id(), "test/echo@0.1.0");
        assert!(m.is_executable());
    }

    #[test]
    fn rejects_declarative_with_provides() {
        let bad = MIN.replace("entrypoint = \"bin/echo\"", "# sem entrypoint");
        assert!(Manifest::parse(&bad).is_err());
    }

    #[test]
    fn rejects_bad_name() {
        let bad = MIN.replace("test/echo", "BAD/name!");
        assert!(Manifest::parse(&bad).is_err());
    }
}
