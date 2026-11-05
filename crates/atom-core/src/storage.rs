#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::path::PathBuf;

use crate::error::CoreError;
use crate::profile::atomic_write;

/// (v1.1, `[P-KV-1..6]`): cada chave tem `version` opaca; `set/delete`
/// aceitam `if_version`/`if_absent` com `-32041 VersionConflict`; `list` é
/// ordenada com paginação; `atom.storage.watch` por prefixo notifica
/// `storage.changed` (`[P-KV-6]`). Formato novo `{v:2, entries:{...}}`;
/// arquivos v1 (mapa plano) são lidos com `version=None` e migrados no
/// próximo `set`. Conformidade genérica em `conformance` (`[S-BE-1]`).
#[derive(Debug)]
pub struct Kv {
    file: PathBuf,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Entry {
    value: serde_json::Value,
    version: String,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct FileV2 {
    v: u32,
    #[serde(default)]
    entries: std::collections::HashMap<String, Entry>,
}

fn slug(name: &str) -> String {
    name.replace('/', "_")
}

fn new_version() -> String {
    format!("v_{}", ulid::Ulid::new())
}

/// Id de assinatura `atom.storage.watch` (`[P-KV-6]`): `w_<ULID>`.
// spec: [P-KV-6]
pub fn new_subscription_id() -> String {
    format!("w_{}", ulid::Ulid::new())
}

/// Registro em memória de watches por `(ext, prefix)` (`[P-KV-6]`).
/// Chave = `subscription`; valor = `(ext, prefix)`.
#[derive(Debug, Default)]
pub struct WatchRegistry {
    watches: HashMap<String, (String, String)>,
}

impl WatchRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registra `prefix` para `ext`; retorna `subscription` (`w_...`).
    // spec: [P-KV-6]
    pub fn watch(&mut self, ext: &str, prefix: &str) -> String {
        let id = new_subscription_id();
        self.watches
            .insert(id.clone(), (ext.to_string(), prefix.to_string()));
        id
    }

    /// Remove `subscription` somente se pertence a `ext`.
    // spec: [P-KV-6]
    pub fn unwatch(&mut self, ext: &str, subscription: &str) -> bool {
        match self.watches.get(subscription) {
            Some((owner, _)) if owner == ext => {
                self.watches.remove(subscription);
                true
            }
            _ => false,
        }
    }

    /// Subscriptions de `ext` cujo prefixo casa `key`, ordenadas.
    // spec: [P-KV-6]
    pub fn matching(&self, ext: &str, key: &str) -> Vec<String> {
        let mut out: Vec<String> = self
            .watches
            .iter()
            .filter(|(_, (owner, prefix))| owner == ext && key.starts_with(prefix.as_str()))
            .map(|(sub, _)| sub.clone())
            .collect();
        out.sort();
        out
    }

    /// Payload `storage.changed` para mudança (`[P-KV-6]`).
    // spec: [P-KV-6]
    pub fn changed_payload(
        subscription: &str,
        key: &str,
        version: Option<&str>,
        deleted: bool,
    ) -> serde_json::Value {
        if deleted {
            serde_json::json!({"subscription": subscription, "key": key, "deleted": true})
        } else if let Some(v) = version {
            serde_json::json!({"subscription": subscription, "key": key, "version": v})
        } else {
            serde_json::json!({"subscription": subscription, "key": key})
        }
    }

    /// Payload `storage.changed` de estouro (`[P-KV-6]`): assinante relê.
    // spec: [P-KV-6]
    pub fn overflow_payload(subscription: &str) -> serde_json::Value {
        serde_json::json!({"subscription": subscription, "overflow": true})
    }
}

/// Backend genérico para a suíte de conformidade (`[S-BE-1]`).
pub trait Backend {
    fn get_versioned(
        &self,
        key: &str,
    ) -> Result<(Option<serde_json::Value>, Option<String>), CoreError>;
    fn set_conditional(
        &self,
        key: &str,
        value: serde_json::Value,
        if_version: Option<&str>,
        if_absent: bool,
    ) -> Result<String, CoreError>;
    fn delete_conditional(&self, key: &str, if_version: Option<&str>) -> Result<bool, CoreError>;
    fn list_paged(
        &self,
        prefix: &str,
        cursor: Option<&str>,
        limit: Option<usize>,
    ) -> Result<(Vec<String>, Option<String>), CoreError>;
}

impl Backend for Kv {
    fn get_versioned(
        &self,
        key: &str,
    ) -> Result<(Option<serde_json::Value>, Option<String>), CoreError> {
        Kv::get_versioned(self, key)
    }
    fn set_conditional(
        &self,
        key: &str,
        value: serde_json::Value,
        if_version: Option<&str>,
        if_absent: bool,
    ) -> Result<String, CoreError> {
        Kv::set_conditional(self, key, value, if_version, if_absent)
    }
    fn delete_conditional(&self, key: &str, if_version: Option<&str>) -> Result<bool, CoreError> {
        Kv::delete_conditional(self, key, if_version)
    }
    fn list_paged(
        &self,
        prefix: &str,
        cursor: Option<&str>,
        limit: Option<usize>,
    ) -> Result<(Vec<String>, Option<String>), CoreError> {
        Kv::list_paged(self, prefix, cursor, limit)
    }
}

impl Kv {
    pub fn new(kv_dir: &std::path::Path, extension: &str) -> Self {
        Self {
            file: kv_dir.join(format!("{}.json", slug(extension))),
        }
    }

    fn load(&self) -> Result<HashMap<String, Entry>, CoreError> {
        if !self.file.exists() {
            return Ok(HashMap::new());
        }
        let text = std::fs::read_to_string(&self.file).map_err(|e| CoreError::Io(e.to_string()))?;
        if let Ok(f2) = serde_json::from_str::<FileV2>(&text)
            && f2.v == 2
        {
            return Ok(f2.entries);
        }
        // Arquivo v1: mapa plano chave -> valor.
        let old: HashMap<String, serde_json::Value> =
            serde_json::from_str(&text).map_err(|e| CoreError::Storage(e.to_string()))?;
        Ok(old
            .into_iter()
            .map(|(k, value)| {
                let version = new_version();
                (k, Entry { value, version })
            })
            .collect())
    }

    fn save(&self, map: &HashMap<String, Entry>) -> Result<(), CoreError> {
        if map.is_empty() {
            let _ = std::fs::remove_file(&self.file);
            return Ok(());
        }
        let f2 = FileV2 {
            v: 2,
            entries: map.clone(),
        };
        let text =
            serde_json::to_string_pretty(&f2).map_err(|e| CoreError::Storage(e.to_string()))?;
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
        Ok(self.load()?.remove(key).map(|e| e.value))
    }

    /// `get` com `version` (`[P-KV-1/3]`). Chave ausente = `(None, None)`.
    // spec: [P-KV-1]
    // spec: [P-KV-3]
    pub fn get_versioned(
        &self,
        key: &str,
    ) -> Result<(Option<serde_json::Value>, Option<String>), CoreError> {
        Self::check(key, None)?;
        Ok(self
            .load()?
            .remove(key)
            .map(|e| (Some(e.value), Some(e.version)))
            .unwrap_or((None, None)))
    }

    pub fn set(&self, key: &str, value: serde_json::Value) -> Result<(), CoreError> {
        Self::check(key, Some(&value))?;
        let mut map = self.load()?;
        map.insert(
            key.to_string(),
            Entry {
                value,
                version: new_version(),
            },
        );
        self.save(&map)
    }

    /// `set` condicional (`[P-KV-4]`): `if_version` exige versão atual igual;
    /// `if_absent` exige chave inexistente. Retorna a nova `version`.
    // spec: [P-KV-4]
    pub fn set_conditional(
        &self,
        key: &str,
        value: serde_json::Value,
        if_version: Option<&str>,
        if_absent: bool,
    ) -> Result<String, CoreError> {
        Self::check(key, Some(&value))?;
        let mut map = self.load()?;
        match (map.get(key), if_version, if_absent) {
            (Some(cur), _, true) => Err(CoreError::VersionConflict(format!(
                "chave {key:?} já existe (version {})",
                cur.version
            ))),
            (Some(cur), Some(want), _) if cur.version != want => Err(CoreError::VersionConflict(
                format!("chave {key:?}: versão atual {} != {want}", cur.version),
            )),
            (None, Some(_), _) => Err(CoreError::VersionConflict(format!(
                "chave {key:?} inexistente para if_version"
            ))),
            _ => {
                let v = new_version();
                map.insert(
                    key.to_string(),
                    Entry {
                        value,
                        version: v.clone(),
                    },
                );
                self.save(&map)?;
                Ok(v)
            }
        }
    }

    pub fn delete(&self, key: &str) -> Result<bool, CoreError> {
        Self::check(key, None)?;
        let mut map = self.load()?;
        let had = map.remove(key).is_some();
        self.save(&map)?;
        Ok(had)
    }

    /// `delete` condicional (`[P-KV-4]`).
    // spec: [P-KV-4]
    pub fn delete_conditional(
        &self,
        key: &str,
        if_version: Option<&str>,
    ) -> Result<bool, CoreError> {
        Self::check(key, None)?;
        let mut map = self.load()?;
        match (map.get(key), if_version) {
            (None, _) => Ok(false),
            (Some(cur), Some(want)) if cur.version != want => Err(CoreError::VersionConflict(
                format!("chave {key:?}: versão atual {} != {want}", cur.version),
            )),
            _ => {
                map.remove(key);
                self.save(&map)?;
                Ok(true)
            }
        }
    }

    pub fn list(&self, prefix: &str) -> Result<Vec<String>, CoreError> {
        Ok(self.list_paged(prefix, None, None)?.0)
    }

    /// `list` ordenada com paginação (`[P-KV-5]`): `cursor` = última chave
    /// vista; `limit` <= 1000. Retorna `(chaves, próximo_cursor)`.
    // spec: [P-KV-5]
    pub fn list_paged(
        &self,
        prefix: &str,
        cursor: Option<&str>,
        limit: Option<usize>,
    ) -> Result<(Vec<String>, Option<String>), CoreError> {
        if let Some(n) = limit
            && (n == 0 || n > 1000)
        {
            return Err(CoreError::Storage("limit deve ser 1..=1000".to_string()));
        }
        let map = self.load()?;
        let mut keys: Vec<String> = map
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect();
        keys.sort();
        let start = match cursor {
            None => 0,
            Some(c) => keys
                .iter()
                .position(|k| k.as_str() > c)
                .unwrap_or(keys.len()),
        };
        let slice = &keys[start..];
        match limit {
            None => Ok((slice.to_vec(), None)),
            Some(n) if slice.len() <= n => Ok((slice.to_vec(), None)),
            Some(n) => {
                let page = slice[..n].to_vec();
                let next = page.last().cloned();
                Ok((page, next))
            }
        }
    }
}

/// Suíte genérica de conformidade do backend KV (`[S-BE-1]`).
/// Todo backend (filesystem hoje; Postgres/S3/Vault depois) passa nela.
// spec: [S-BE-1]
pub mod conformance {
    use super::{Backend, WatchRegistry};
    use crate::error::CoreError;

    /// CAS: `if_version`/`if_absent` → `-32041` (`[P-KV-4]`).
    // spec: [P-KV-4]
    // spec: [S-BE-1]
    pub fn check_cas<B: Backend>(backend: &B) -> Result<(), String> {
        let v1 = backend
            .set_conditional("k", serde_json::json!(1), None, false)
            .map_err(|e| format!("set inicial: {e}"))?;
        let e = backend
            .set_conditional("k", serde_json::json!(2), None, true)
            .unwrap_err();
        if !matches!(e, CoreError::VersionConflict(_)) {
            return Err(format!(
                "if_absent com chave existente deve dar VersionConflict, deu {e}"
            ));
        }
        if e.code() != atom_protocol::ErrorCode::VERSION_CONFLICT {
            return Err("if_absent deve mapear para -32041".to_string());
        }
        let bad = backend
            .set_conditional("k", serde_json::json!(2), Some("v_nope"), false)
            .unwrap_err();
        if bad.code() != atom_protocol::ErrorCode::VERSION_CONFLICT {
            return Err("if_version errado deve mapear para -32041".to_string());
        }
        let v2 = backend
            .set_conditional("k", serde_json::json!(2), Some(&v1), false)
            .map_err(|e| format!("if_version certo: {e}"))?;
        if v1 == v2 {
            return Err("versão deve mudar a cada escrita".to_string());
        }
        let del_err = backend.delete_conditional("k", Some("v_nope")).unwrap_err();
        if del_err.code() != atom_protocol::ErrorCode::VERSION_CONFLICT {
            return Err("delete com if_version errado deve mapear para -32041".to_string());
        }
        if !backend
            .delete_conditional("k", Some(&v2))
            .map_err(|e| format!("delete condicional certo: {e}"))?
        {
            return Err("delete condicional certo deve remover".to_string());
        }
        Ok(())
    }

    /// Ordem de `list` + paginação (`[P-KV-5]`).
    // spec: [P-KV-5]
    // spec: [S-BE-1]
    pub fn check_order_pagination<B: Backend>(backend: &B) -> Result<(), String> {
        for k in ["ord/c", "ord/a", "ord/b"] {
            backend
                .set_conditional(k, serde_json::json!(1), None, false)
                .map_err(|e| format!("set {k}: {e}"))?;
        }
        let (all, end) = backend
            .list_paged("ord/", None, None)
            .map_err(|e| format!("list: {e}"))?;
        if all
            != vec![
                "ord/a".to_string(),
                "ord/b".to_string(),
                "ord/c".to_string(),
            ]
        {
            return Err(format!("list deve ser ordenada: {all:?}"));
        }
        if end.is_some() {
            return Err("list sem limit não tem cursor".to_string());
        }
        let (page, cursor) = backend
            .list_paged("ord/", None, Some(2))
            .map_err(|e| format!("página 1: {e}"))?;
        if page != vec!["ord/a".to_string(), "ord/b".to_string()] {
            return Err(format!("página 1 errada: {page:?}"));
        }
        let cursor = cursor.ok_or("página 1 deve ter cursor")?;
        let (rest, end2) = backend
            .list_paged("ord/", Some(&cursor), Some(2))
            .map_err(|e| format!("página 2: {e}"))?;
        if rest != vec!["ord/c".to_string()] || end2.is_some() {
            return Err(format!("página 2 errada: {rest:?}/{end2:?}"));
        }
        Ok(())
    }

    /// Read-your-writes (`[P-KV-1]`): `get` lê o que `set` escreveu.
    // spec: [P-KV-1]
    // spec: [S-BE-1]
    pub fn check_read_your_writes<B: Backend>(backend: &B) -> Result<(), String> {
        let v = backend
            .set_conditional("ryw", serde_json::json!({"x": 1}), None, false)
            .map_err(|e| format!("set: {e}"))?;
        let (got, ver) = backend
            .get_versioned("ryw")
            .map_err(|e| format!("get: {e}"))?;
        if got != Some(serde_json::json!({"x": 1})) {
            return Err(format!("read-your-writes falhou: {got:?}"));
        }
        if ver.as_deref() != Some(v.as_str()) {
            return Err("versão do get deve igualar a do set".to_string());
        }
        Ok(())
    }

    /// Prefixos de watch casam por `starts_with`, em ordem (`[P-KV-6]`).
    // spec: [P-KV-6]
    // spec: [S-BE-1]
    pub fn check_watch_matching() -> Result<(), String> {
        let mut reg = WatchRegistry::new();
        let a = reg.watch("e/x", "a/");
        let all = reg.watch("e/x", "");
        let other = reg.watch("e/y", "a/");
        let m = reg.matching("e/x", "a/k");
        let mut want = vec![a.clone(), all.clone()];
        want.sort();
        if m != want {
            return Err(format!("matching errado: {m:?}"));
        }
        if reg.matching("e/x", "b/k") != vec![all.clone()] {
            return Err("prefixo vazio casa tudo".to_string());
        }
        if reg.matching("e/y", "a/k") != vec![other.clone()] {
            return Err("namespace é por extensão".to_string());
        }
        if !reg.unwatch("e/x", &a) || !reg.matching("e/x", "a/k").iter().all(|s| s != &a) {
            return Err("unwatch deve remover".to_string());
        }
        if reg.unwatch("e/x", &other) {
            return Err("unwatch não remove subscription de outra ext".to_string());
        }
        if !WatchRegistry::changed_payload(&all, "a/k", Some("v_1"), false)["version"].is_string() {
            return Err("changed carrega version".to_string());
        }
        if !WatchRegistry::overflow_payload(&all)["overflow"]
            .as_bool()
            .unwrap_or(false)
        {
            return Err("overflow carrega flag".to_string());
        }
        Ok(())
    }

    /// Roda a suíte inteira contra `backend`.
    // spec: [S-BE-1]
    pub fn run_suite<B: Backend>(backend: &B) -> Result<(), String> {
        check_read_your_writes(backend)?;
        check_cas(backend)?;
        check_order_pagination(backend)?;
        check_watch_matching()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn kv_roundtrip() {
        // spec: [P-KV-1]
        // spec: [P-KV-3]
        let t = tempfile::tempdir().unwrap();
        let kv = Kv::new(t.path(), "test/echo");
        kv.set("a", json!({"x": 1})).unwrap();
        assert_eq!(kv.get("a").unwrap(), Some(json!({"x": 1})));
        assert_eq!(kv.list("").unwrap(), vec!["a".to_string()]);
        assert!(kv.delete("a").unwrap());
        assert_eq!(kv.get("a").unwrap(), None);
    }

    #[test]
    fn kv_conformance_filesystem() {
        // spec: [S-BE-1]
        // spec: [P-KV-4]
        // spec: [P-KV-5]
        // spec: [P-KV-6]
        // spec: [P-KV-1]
        let t = tempfile::tempdir().unwrap();
        let kv = Kv::new(t.path(), "test/conformance");
        conformance::run_suite(&kv).expect("filesystem passa na suíte [S-BE-1]");
    }

    #[test]
    fn kv_durability_reopen() {
        // spec: [S-BE-1]
        // spec: [P-KV-2]
        let t = tempfile::tempdir().unwrap();
        let v = Kv::new(t.path(), "test/echo")
            .set_conditional("k", json!({"x": 1}), None, false)
            .unwrap();
        // Reabre o arquivo: `set` já era durável antes de responder.
        let kv2 = Kv::new(t.path(), "test/echo");
        let (got, ver) = kv2.get_versioned("k").unwrap();
        assert_eq!(got, Some(json!({"x": 1})));
        assert_eq!(ver.as_deref(), Some(v.as_str()));
        // `delete` também é durável.
        assert!(kv2.delete("k").unwrap());
        let kv3 = Kv::new(t.path(), "test/echo");
        assert_eq!(kv3.get("k").unwrap(), None);
    }

    #[test]
    fn kv_watch_registry_unit() {
        // spec: [P-KV-6]
        conformance::check_watch_matching().expect("watch matching [P-KV-6]");
        assert!(new_subscription_id().starts_with("w_"));
    }

    #[test]
    fn kv_version_conflict_and_pagination() {
        // spec: [P-KV-4]
        // spec: [P-KV-5]
        let t = tempfile::tempdir().unwrap();
        let kv = Kv::new(t.path(), "test/echo");
        let v1 = kv.set_conditional("k", json!(1), None, false).unwrap();
        // `if_absent` com chave existente -> -32041.
        let e = kv.set_conditional("k", json!(2), None, true).unwrap_err();
        assert!(matches!(e, CoreError::VersionConflict(_)));
        assert_eq!(e.code(), atom_protocol::ErrorCode::VERSION_CONFLICT);
        // `if_version` errado -> conflito; certo -> nova versão.
        let e2 = kv
            .set_conditional("k", json!(2), Some("v_nope"), false)
            .unwrap_err();
        assert_eq!(e2.code(), atom_protocol::ErrorCode::VERSION_CONFLICT);
        let v2 = kv.set_conditional("k", json!(2), Some(&v1), false).unwrap();
        assert_ne!(v1, v2);
        // `delete` condicional.
        let e3 = kv.delete_conditional("k", Some("v_nope")).unwrap_err();
        assert_eq!(e3.code(), atom_protocol::ErrorCode::VERSION_CONFLICT);
        assert!(kv.delete_conditional("k", Some(&v2)).unwrap());
        // Paginação ordenada.
        for k in ["b", "a", "c"] {
            kv.set(k, json!(1)).unwrap();
        }
        let (page, cursor) = kv.list_paged("", None, Some(2)).unwrap();
        assert_eq!(page, vec!["a".to_string(), "b".to_string()]);
        let cursor = cursor.unwrap();
        let (rest, end) = kv.list_paged("", Some(&cursor), Some(2)).unwrap();
        assert_eq!(rest, vec!["c".to_string()]);
        assert!(end.is_none());
    }
}
