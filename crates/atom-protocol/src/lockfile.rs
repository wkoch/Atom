#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

/// Lockfile por perfil (spec profiles §4). Fonte da verdade do que roda.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lockfile {
    pub lock: u32,
    pub core: String,
    #[serde(default)]
    pub package: Vec<LockPackage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockPackage {
    pub name: String,
    pub version: String,
    pub source: String,
    pub platform: String,
    pub sha256: String,
    #[serde(default)]
    pub requested: String,
    #[serde(default)]
    pub dependencies: Vec<String>,
}

impl Lockfile {
    pub fn empty(core: &str) -> Self {
        Self {
            lock: 1,
            core: core.to_string(),
            package: Vec::new(),
        }
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }

    pub fn render(&self) -> Result<String, String> {
        toml::to_string_pretty(self).map_err(|e| e.to_string())
    }

    pub fn find(&self, name: &str) -> Option<&LockPackage> {
        self.package.iter().find(|p| p.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let mut l = Lockfile::empty("0.1.0");
        l.package.push(LockPackage {
            name: "test/echo".to_string(),
            version: "0.1.0".to_string(),
            source: "dev:testdata/echo".to_string(),
            platform: "any".to_string(),
            sha256: "abc".to_string(),
            requested: String::new(),
            dependencies: vec![],
        });
        let text = l.render().unwrap();
        let back = Lockfile::parse(&text).unwrap();
        assert_eq!(back.package.len(), 1);
    }
}
