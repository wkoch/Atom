#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::path::PathBuf;

use crate::error::CoreError;
use crate::profile::atomic_write;

/// KV por extensão+perfil (`atom.storage.*`). Arquivo JSON por extensão (v0.x).
#[derive(Debug)]
pub struct Kv {
    file: PathBuf,
}

fn slug(name: &str) -> String {
    name.replace('/', "_")
}

impl Kv {
    pub fn new(kv_dir: &std::path::Path, extension: &str) -> Self {
        Self {
            file: kv_dir.join(format!("{}.json", slug(extension))),
        }
    }

    fn load(&self) -> Result<HashMap<String, serde_json::Value>, CoreError> {
        if !self.file.exists() {
            return Ok(HashMap::new());
        }
        let text = std::fs::read_to_string(&self.file).map_err(|e| CoreError::Io(e.to_string()))?;
        serde_json::from_str(&text).map_err(|e| CoreError::Storage(e.to_string()))
    }

    fn save(&self, map: &HashMap<String, serde_json::Value>) -> Result<(), CoreError> {
        if map.is_empty() {
            let _ = std::fs::remove_file(&self.file);
            return Ok(());
        }
        let text =
            serde_json::to_string_pretty(map).map_err(|e| CoreError::Storage(e.to_string()))?;
        atomic_write(&self.file, text.as_bytes())
    }

    fn check(key: &str, value: Option<&serde_json::Value>) -> Result<(), CoreError> {
        if key.len() > 512 {
            return Err(CoreError::Storage("chave > 512 bytes".to_string()));
        }
        if let Some(v) = value {
            let s = serde_json::to_string(v).map_err(|e| CoreError::Storage(e.to_string()))?;
            if s.len() > 1024 * 1024 {
                return Err(CoreError::Storage("valor > 1 MiB".to_string()));
            }
        }
        Ok(())
    }

    pub fn get(&self, key: &str) -> Result<Option<serde_json::Value>, CoreError> {
        Self::check(key, None)?;
        Ok(self.load()?.remove(key))
    }

    pub fn set(&self, key: &str, value: serde_json::Value) -> Result<(), CoreError> {
        Self::check(key, Some(&value))?;
        let mut map = self.load()?;
        map.insert(key.to_string(), value);
        self.save(&map)
    }

    pub fn delete(&self, key: &str) -> Result<bool, CoreError> {
        Self::check(key, None)?;
        let mut map = self.load()?;
        let had = map.remove(key).is_some();
        self.save(&map)?;
        Ok(had)
    }

    pub fn list(&self, prefix: &str) -> Result<Vec<String>, CoreError> {
        let map = self.load()?;
        let mut keys: Vec<String> = map
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect();
        keys.sort();
        Ok(keys)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn kv_roundtrip() {
        let t = tempfile::tempdir().unwrap();
        let kv = Kv::new(t.path(), "test/echo");
        kv.set("a", json!({"x": 1})).unwrap();
        assert_eq!(kv.get("a").unwrap(), Some(json!({"x": 1})));
        assert_eq!(kv.list("").unwrap(), vec!["a".to_string()]);
        assert!(kv.delete("a").unwrap());
        assert_eq!(kv.get("a").unwrap(), None);
    }
}
