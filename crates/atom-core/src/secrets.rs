#![forbid(unsafe_code)]
//! Secrets por perfil (`atom.secrets.*`, CLI `secret`).
//!
//! Backend: keyring do OS (serviço `atom`, conta `<perfil>/<nome>`), com
//! fallback em arquivo 0600 (`secrets.json`) quando o keyring falha.
//! `shortcut: fallback em claro 0600 (só leitura pelo dono); criptografar (age/sodium) quando houver demanda.`

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::CoreError;

/// Backend de secrets com escopo de perfil.
#[derive(Debug, Clone)]
pub struct Secrets {
    profile: String,
    file: PathBuf,
    index: PathBuf,
    use_keyring: bool,
}

fn rpc_err(e: impl ToString) -> atom_protocol::RpcError {
    atom_protocol::RpcError::new(atom_protocol::ErrorCode::STORAGE_ERROR, e.to_string())
}

impl Secrets {
    /// Sonda o keyring uma vez; cai para arquivo se indisponível.
    pub fn new(profile: &str, data_dir: &Path) -> Self {
        let s = Self::file_only(profile, data_dir);
        let use_keyring = probe_keyring();
        if !use_keyring {
            tracing::debug!("keyring indisponível; secrets em arquivo 0600");
        }
        Self { use_keyring, ..s }
    }

    /// Só arquivo (determinístico; testes).
    pub fn file_only(profile: &str, data_dir: &Path) -> Self {
        Self {
            profile: profile.to_string(),
            file: data_dir.join("secrets.json"),
            index: data_dir.join("secrets.index.json"),
            use_keyring: false,
        }
    }

    fn account(&self, name: &str) -> String {
        format!("{}/{}", self.profile, name)
    }

    fn read_file(&self) -> HashMap<String, String> {
        if !self.file.exists() {
            return HashMap::new();
        }
        let t = std::fs::read_to_string(&self.file).unwrap_or_default();
        serde_json::from_str(&t).unwrap_or_default()
    }

    fn write_file(&self, map: &HashMap<String, String>) -> Result<(), CoreError> {
        let t = serde_json::to_string_pretty(map).map_err(|e| CoreError::Storage(e.to_string()))?;
        // 0600 desde a criação (revisão v1.0): sem janela legível entre escrita e chmod.
        crate::profile::atomic_write_private(&self.file, t.as_bytes())
    }

    fn index_add(&self, name: &str) {
        let mut names: Vec<String> = if self.index.exists() {
            std::fs::read_to_string(&self.index)
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or_default()
        } else {
            vec![]
        };
        if !names.iter().any(|n| n == name) {
            names.push(name.to_string());
            names.sort();
            let _ = std::fs::write(
                &self.index,
                serde_json::to_string(&names).unwrap_or_default(),
            );
        }
    }

    fn entry(&self, name: &str) -> Result<keyring::Entry, CoreError> {
        keyring::Entry::new("atom", &self.account(name))
            .map_err(|e| CoreError::Storage(format!("keyring: {e}")))
    }

    fn check_name(name: &str) -> Result<(), CoreError> {
        if name.is_empty() || name.len() > 256 {
            return Err(CoreError::Storage("nome de secret inválido".to_string()));
        }
        Ok(())
    }

    pub fn get(&self, name: &str) -> Result<Option<String>, CoreError> {
        Self::check_name(name)?;
        if self.use_keyring {
            match self.entry(name)?.get_password() {
                Ok(v) => return Ok(Some(v)),
                Err(keyring::Error::NoEntry) => return Ok(None),
                Err(e) => return Err(CoreError::Storage(format!("keyring: {e}"))),
            }
        }
        Ok(self.read_file().remove(name))
    }

    pub fn set(&self, name: &str, value: &str) -> Result<(), CoreError> {
        Self::check_name(name)?;
        if value.len() > 64 * 1024 {
            return Err(CoreError::Storage("secret > 64 KiB".to_string()));
        }
        if self.use_keyring {
            self.entry(name)?
                .set_password(value)
                .map_err(|e| CoreError::Storage(format!("keyring: {e}")))?;
        } else {
            let mut map = self.read_file();
            map.insert(name.to_string(), value.to_string());
            self.write_file(&map)?;
        }
        self.index_add(name);
        Ok(())
    }

    pub fn delete(&self, name: &str) -> Result<bool, CoreError> {
        Self::check_name(name)?;
        let mut had = false;
        if self.use_keyring {
            match self.entry(name)?.delete_credential() {
                Ok(()) => had = true,
                Err(keyring::Error::NoEntry) => {}
                Err(e) => return Err(CoreError::Storage(format!("keyring: {e}"))),
            }
        }
        let mut map = self.read_file();
        if map.remove(name).is_some() {
            had = true;
            self.write_file(&map)?;
        }
        Ok(had)
    }

    pub fn list(&self) -> Result<Vec<String>, CoreError> {
        if !self.index.exists() {
            return Ok(vec![]);
        }
        let t = std::fs::read_to_string(&self.index).map_err(|e| CoreError::Io(e.to_string()))?;
        serde_json::from_str(&t).map_err(|e| CoreError::Storage(e.to_string()))
    }

    pub fn rpc_get(&self, name: &str) -> Result<serde_json::Value, atom_protocol::RpcError> {
        let v = self.get(name).map_err(rpc_err)?;
        Ok(v.map(serde_json::Value::String)
            .unwrap_or(serde_json::Value::Null))
    }

    pub fn rpc_set(
        &self,
        name: &str,
        value: &str,
    ) -> Result<serde_json::Value, atom_protocol::RpcError> {
        self.set(name, value).map_err(rpc_err)?;
        Ok(serde_json::json!({"stored": true}))
    }
}

/// Testa se o keyring funciona (set+delete de item descartável).
fn probe_keyring() -> bool {
    let name = format!("__probe_{}__", atom_protocol::new_trace_id());
    let Ok(e) = keyring::Entry::new("atom", &name) else {
        return false;
    };
    if e.set_password("x").is_err() {
        return false;
    }
    let _ = e.delete_credential();
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_roundtrip() {
        let t = tempfile::tempdir().unwrap();
        let s = Secrets::file_only("dev", t.path());
        assert_eq!(s.get("k").unwrap(), None);
        s.set("k", "v").unwrap();
        assert_eq!(s.get("k").unwrap().as_deref(), Some("v"));
        assert_eq!(s.list().unwrap(), vec!["k".to_string()]);
        assert!(s.delete("k").unwrap());
        assert_eq!(s.get("k").unwrap(), None);
    }

    #[test]
    fn secrets_read_your_writes_filesystem() {
        // spec: [S-BE-1]
        // spec: [P-SEC-1]
        let t = tempfile::tempdir().unwrap();
        let s = Secrets::file_only("dev", t.path());
        // read-your-writes: `get` lê o que `set` escreveu.
        assert_eq!(s.get("tok").unwrap(), None);
        s.set("tok", "s3cr3t").unwrap();
        assert_eq!(s.get("tok").unwrap().as_deref(), Some("s3cr3t"));
        // `set` durável antes de responder: reabre e relê do arquivo.
        let s2 = Secrets::file_only("dev", t.path());
        assert_eq!(s2.get("tok").unwrap().as_deref(), Some("s3cr3t"));
        assert!(s2.delete("tok").unwrap());
        let s3 = Secrets::file_only("dev", t.path());
        assert_eq!(s3.get("tok").unwrap(), None);
    }
}
