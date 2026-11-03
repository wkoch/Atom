#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::CoreError;

/// Diretórios base (spec profiles §1). `ATOM_HOME` sobrepõe tudo.
#[derive(Debug, Clone)]
pub struct AtomPaths {
    pub config: PathBuf,
    pub data: PathBuf,
    pub cache: PathBuf,
    pub runtime: PathBuf,
}

impl AtomPaths {
    pub fn resolve() -> Self {
        Self::resolve_with(None)
    }

    /// Como `resolve`, mas `--home` (CLI) tem prioridade sobre `ATOM_HOME`.
    /// Evita `set_var` (unsafe neste toolchain + crate `forbid(unsafe_code)`).
    pub fn resolve_with(home: Option<&Path>) -> Self {
        let from_flag = home.map(PathBuf::from);
        let from_env = std::env::var("ATOM_HOME").ok().map(PathBuf::from);
        if let Some(h) = from_flag.or(from_env) {
            return Self {
                config: h.join("config"),
                data: h.join("data"),
                cache: h.join("cache"),
                runtime: h.join("runtime"),
            };
        }
        let base = directories::BaseDirs::new();
        let (mut cfg, mut data, mut cache, mut run) = (
            PathBuf::from("~/.config/atom"),
            PathBuf::from("~/.local/share/atom"),
            PathBuf::from("~/.cache/atom"),
            std::env::temp_dir().join("atom"),
        );
        if let Some(b) = base {
            cfg = b.config_dir().join("atom");
            data = b.data_dir().join("atom");
            cache = b.cache_dir().join("atom");
            run = b
                .runtime_dir()
                .map(|r| r.join("atom"))
                .unwrap_or_else(|| std::env::temp_dir().join("atom"));
            #[cfg(target_os = "macos")]
            {
                // directories já aponta para ~/Library/…; mantém.
            }
        }
        // Expande ~ caso fallback.
        for p in [&mut cfg, &mut data, &mut cache] {
            if let Ok(s) = p.clone().into_os_string().into_string()
                && let Some(rest) = s.strip_prefix("~/")
                && let Ok(h) = std::env::var("HOME")
            {
                *p = PathBuf::from(h).join(rest);
            }
        }
        Self {
            config: cfg,
            data,
            cache,
            runtime: run,
        }
    }

    pub fn ensure(&self) -> Result<(), CoreError> {
        for d in [&self.config, &self.data, &self.cache, &self.runtime] {
            std::fs::create_dir_all(d).map_err(|e| CoreError::Io(e.to_string()))?;
        }
        Ok(())
    }
}

/// Caminhos de um perfil (spec profiles §2).
#[derive(Debug, Clone)]
pub struct ProfilePaths {
    pub name: String,
    pub dir: PathBuf,
    pub profile_toml: PathBuf,
    pub lock: PathBuf,
    pub lock_prev: PathBuf,
    pub grants: PathBuf,
    pub data_dir: PathBuf,
    pub kv_dir: PathBuf,
    pub socket: PathBuf,
    pub pid_file: PathBuf,
}

impl ProfilePaths {
    pub fn new(atom: &AtomPaths, name: &str) -> Result<Self, CoreError> {
        if !atom_protocol::valid_profile_name(name) {
            return Err(CoreError::Profile(format!(
                "nome de perfil inválido: {name:?}"
            )));
        }
        Ok(Self {
            name: name.to_string(),
            dir: atom.config.join("profiles").join(name),
            profile_toml: atom.config.join("profiles").join(name).join("profile.toml"),
            lock: atom.config.join("profiles").join(name).join("atom.lock"),
            lock_prev: atom
                .config
                .join("profiles")
                .join(name)
                .join("atom.lock.prev"),
            grants: atom.config.join("profiles").join(name).join("grants.toml"),
            data_dir: atom.data.join("profiles").join(name),
            kv_dir: atom.data.join("profiles").join(name).join("kv"),
            socket: socket_path(&atom.runtime, name),
            pid_file: atom.runtime.join(format!("atom-{name}.pid")),
        })
    }

    pub fn ensure(&self) -> Result<(), CoreError> {
        for d in [&self.dir, &self.data_dir, &self.kv_dir, &self.lock_prev] {
            std::fs::create_dir_all(d).map_err(|e| CoreError::Io(e.to_string()))?;
        }
        Ok(())
    }
}

#[cfg(unix)]
fn socket_path(runtime: &Path, profile: &str) -> PathBuf {
    runtime.join(format!("atom-{profile}.sock"))
}

#[cfg(not(unix))]
fn socket_path(runtime: &Path, profile: &str) -> PathBuf {
    runtime.join(format!("atom-{profile}.sock"))
}

/// Conteúdo de `profile.toml` (spec profiles §3, parcial v0.1).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProfileConfig {
    #[serde(default = "default_profile_ver")]
    pub profile: u32,
    #[serde(default)]
    pub bindings: HashMap<String, String>,
    #[serde(default)]
    pub config: HashMap<String, toml::Value>,
}

fn default_profile_ver() -> u32 {
    1
}

pub fn load_or_create(paths: &ProfilePaths) -> Result<ProfileConfig, CoreError> {
    paths.ensure()?;
    if !paths.profile_toml.exists() {
        let cfg = ProfileConfig {
            profile: 1,
            bindings: HashMap::new(),
            config: HashMap::new(),
        };
        save(paths, &cfg)?;
        return Ok(cfg);
    }
    let text =
        std::fs::read_to_string(&paths.profile_toml).map_err(|e| CoreError::Io(e.to_string()))?;
    toml::from_str(&text).map_err(|e| CoreError::Profile(format!("profile.toml: {e}")))
}

pub fn save(paths: &ProfilePaths, cfg: &ProfileConfig) -> Result<(), CoreError> {
    paths.ensure()?;
    let text = toml::to_string_pretty(cfg).map_err(|e| CoreError::Profile(e.to_string()))?;
    atomic_write(&paths.profile_toml, text.as_bytes())
}

/// Escrita atômica: temp + fsync + rename.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), CoreError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| CoreError::Io(e.to_string()))?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).map_err(|e| CoreError::Io(e.to_string()))?;
    let f = std::fs::File::open(&tmp).map_err(|e| CoreError::Io(e.to_string()))?;
    let _ = f.sync_all();
    std::fs::rename(&tmp, path).map_err(|e| CoreError::Io(e.to_string()))?;
    Ok(())
}

/// Lista perfis existentes.
pub fn list_profiles(atom: &AtomPaths) -> Result<Vec<String>, CoreError> {
    let dir = atom.config.join("profiles");
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut out = vec![];
    for e in std::fs::read_dir(&dir).map_err(|e| CoreError::Io(e.to_string()))? {
        let e = e.map_err(|e| CoreError::Io(e.to_string()))?;
        if e.file_type()
            .map_err(|e| CoreError::Io(e.to_string()))?
            .is_dir()
            && let Some(n) = e.file_name().to_str()
        {
            out.push(n.to_string());
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_profile_name() {
        let a = AtomPaths {
            config: PathBuf::from("/tmp/x"),
            data: PathBuf::from("/tmp/x"),
            cache: PathBuf::from("/tmp/x"),
            runtime: PathBuf::from("/tmp/x"),
        };
        assert!(ProfilePaths::new(&a, "BAD").is_err());
        assert!(ProfilePaths::new(&a, "ok-1").is_ok());
    }

    #[test]
    fn create_and_load() {
        let t = tempfile::tempdir().unwrap();
        let a = AtomPaths {
            config: t.path().join("c"),
            data: t.path().join("d"),
            cache: t.path().join("ca"),
            runtime: t.path().join("r"),
        };
        let p = ProfilePaths::new(&a, "dev").unwrap();
        let c = load_or_create(&p).unwrap();
        assert_eq!(c.profile, 1);
    }
}
