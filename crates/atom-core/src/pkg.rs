#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use atom_protocol::{LockPackage, Lockfile, Manifest};
use sha2::{Digest, Sha256};

use crate::error::CoreError;
use crate::profile::{self, ProfilePaths};

/// Lê e valida `atom.toml` de uma pasta.
pub fn read_manifest(dir: &Path) -> Result<Manifest, CoreError> {
    let text = std::fs::read_to_string(dir.join("atom.toml"))
        .map_err(|e| CoreError::Package(format!("atom.toml: {e}")))?;
    Manifest::parse(&text).map_err(|e| CoreError::Package(e.to_string()))
}

pub fn load_lock(paths: &ProfilePaths) -> Result<Lockfile, CoreError> {
    if !paths.lock.exists() {
        return Ok(Lockfile::empty(crate::CORE_VERSION));
    }
    let text =
        std::fs::read_to_string(&paths.lock).map_err(|e| CoreError::Package(e.to_string()))?;
    Lockfile::parse(&text).map_err(CoreError::Package)
}

/// Grava lock com backup em `atom.lock.prev/<timestamp>.lock` + escrita atômica.
pub fn save_lock(paths: &ProfilePaths, lock: &Lockfile) -> Result<(), CoreError> {
    if paths.lock.exists() {
        let prev = paths.lock_prev.join(format!("{}.lock", stamp()));
        let cur = std::fs::read(&paths.lock).map_err(|e| CoreError::Io(e.to_string()))?;
        std::fs::create_dir_all(&paths.lock_prev).map_err(|e| CoreError::Io(e.to_string()))?;
        std::fs::write(&prev, cur).map_err(|e| CoreError::Io(e.to_string()))?;
        prune_prev(paths)?;
    }
    let text = lock.render().map_err(CoreError::Package)?;
    profile::atomic_write(&paths.lock, text.as_bytes())
}

fn stamp() -> String {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => format!("{}", d.as_secs()),
        Err(_) => "0".to_string(),
    }
}

fn prune_prev(paths: &ProfilePaths) -> Result<(), CoreError> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(&paths.lock_prev)
        .map_err(|e| CoreError::Io(e.to_string()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    files.sort();
    while files.len() > 10 {
        let old = files.remove(0);
        let _ = std::fs::remove_file(old);
    }
    Ok(())
}

fn sha16(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex_of(&h.finalize())[..16].to_string()
}

fn hex_of(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn store_dir(data: &Path, m: &Manifest, sha: &str) -> PathBuf {
    let (publisher, name) = m.name.split_once('/').unwrap_or(("", &m.name as &str));
    data.join("store")
        .join(publisher)
        .join(name)
        .join(format!("{}-{sha}", m.version))
}

fn check_engine(m: &Manifest) -> Result<(), CoreError> {
    let req = m
        .engine
        .parse::<semver::VersionReq>()
        .map_err(|e| CoreError::Package(format!("engine {:?}: {e}", m.engine)))?;
    let core = semver::Version::parse(crate::CORE_VERSION)
        .map_err(|e| CoreError::Internal(e.to_string()))?;
    if !req.matches(&core) {
        return Err(CoreError::Package(format!(
            "{} exige core {}, atual é {}",
            m.id(),
            m.engine,
            crate::CORE_VERSION
        )));
    }
    Ok(())
}

fn copy_dir(src: &Path, dst: &Path) -> Result<(), CoreError> {
    std::fs::create_dir_all(dst).map_err(|e| CoreError::Io(e.to_string()))?;
    for e in std::fs::read_dir(src).map_err(|e| CoreError::Io(e.to_string()))? {
        let e = e.map_err(|e| CoreError::Io(e.to_string()))?;
        let to = dst.join(e.file_name());
        let ft = e.file_type().map_err(|e| CoreError::Io(e.to_string()))?;
        if ft.is_dir() {
            copy_dir(&e.path(), &to)?;
        } else if ft.is_file() {
            std::fs::copy(e.path(), &to).map_err(|e| CoreError::Io(e.to_string()))?;
        }
    }
    Ok(())
}

/// Instala de uma pasta local (`--dev`). Retorna o id instalado.
pub fn install_dev(
    paths: &ProfilePaths,
    data_store: &Path,
    src: &Path,
) -> Result<String, CoreError> {
    let m = read_manifest(src)?;
    check_engine(&m)?;
    let manifest_bytes =
        std::fs::read(src.join("atom.toml")).map_err(|e| CoreError::Io(e.to_string()))?;
    let sha = sha16(&manifest_bytes);
    let dst = store_dir(data_store, &m, &sha);
    if dst.exists() {
        let _ = std::fs::remove_dir_all(&dst);
    }
    copy_dir(src, &dst)?;
    upsert_lock(
        paths,
        &m,
        format!("dev:{}", src.display()),
        "any",
        &sha,
        &m.version,
    )?;
    Ok(m.id())
}

/// Instala bytes de um `.tar.gz` já baixado/verificado. Retorna o id.
/// `requested` é o requisito que originou a instalação (lock, v0.5).
/// `check` opcional valida o manifesto extraído (ex.: confere com o índice).
/// `shortcut: extração + cópia não são atômicas entre si; lock sim.`
pub type ManifestCheck<'a> = Option<&'a dyn Fn(&Manifest) -> Result<(), CoreError>>;
pub fn install_bytes(
    paths: &ProfilePaths,
    data_store: &Path,
    bytes: &[u8],
    source: String,
    platform: &str,
    requested: &str,
    check: ManifestCheck<'_>,
) -> Result<String, CoreError> {
    let sha_full = {
        let mut h = Sha256::new();
        h.update(bytes);
        hex_of(&h.finalize())
    };
    let sha = sha_full[..16].to_string();
    let tmp = tempfile::tempdir().map_err(|e| CoreError::Io(e.to_string()))?;
    let gz = flate2::read::GzDecoder::new(bytes);
    let mut ar = tar::Archive::new(gz);
    ar.unpack(tmp.path())
        .map_err(|e| CoreError::Package(format!("tar.gz: {e}")))?;
    // Aceita atom.toml na raiz ou um nível abaixo.
    let root = if tmp.path().join("atom.toml").exists() {
        tmp.path().to_path_buf()
    } else {
        let mut found = None;
        for e in std::fs::read_dir(tmp.path()).map_err(|e| CoreError::Io(e.to_string()))? {
            let e = e.map_err(|e| CoreError::Io(e.to_string()))?;
            if e.path().join("atom.toml").exists() {
                found = Some(e.path());
                break;
            }
        }
        found.ok_or_else(|| CoreError::Package("atom.toml não encontrado".to_string()))?
    };
    let m = read_manifest(&root)?;
    check_engine(&m)?;
    if let Some(check) = check {
        check(&m)?;
    }
    let dst = store_dir(data_store, &m, &sha);
    if dst.exists() {
        let _ = std::fs::remove_dir_all(&dst);
    }
    copy_dir(&root, &dst)?;
    upsert_lock(paths, &m, source, platform, &sha_full, requested)?;
    Ok(m.id())
}

/// Instala de um `.tar.gz` local. Retorna o id instalado.
pub fn install_tarball(
    paths: &ProfilePaths,
    data_store: &Path,
    file: &Path,
) -> Result<String, CoreError> {
    let bytes = std::fs::read(file).map_err(|e| CoreError::Io(e.to_string()))?;
    let sha_full = {
        let mut h = Sha256::new();
        h.update(&bytes);
        hex_of(&h.finalize())
    };
    let sha = sha_full[..16].to_string();
    let tmp = tempfile::tempdir().map_err(|e| CoreError::Io(e.to_string()))?;
    let gz = flate2::read::GzDecoder::new(&bytes[..]);
    let mut ar = tar::Archive::new(gz);
    ar.unpack(tmp.path())
        .map_err(|e| CoreError::Package(format!("tar.gz: {e}")))?;
    // Aceita atom.toml na raiz ou um nível abaixo.
    let root = if tmp.path().join("atom.toml").exists() {
        tmp.path().to_path_buf()
    } else {
        let mut found = None;
        for e in std::fs::read_dir(tmp.path()).map_err(|e| CoreError::Io(e.to_string()))? {
            let e = e.map_err(|e| CoreError::Io(e.to_string()))?;
            if e.path().join("atom.toml").exists() {
                found = Some(e.path());
                break;
            }
        }
        found.ok_or_else(|| CoreError::Package("atom.toml não encontrado".to_string()))?
    };
    let m = read_manifest(&root)?;
    check_engine(&m)?;
    let dst = store_dir(data_store, &m, &sha);
    if dst.exists() {
        let _ = std::fs::remove_dir_all(&dst);
    }
    copy_dir(&root, &dst)?;
    upsert_lock(
        paths,
        &m,
        format!("file:{}", file.display()),
        platform_tag(),
        &sha_full,
        &m.version,
    )?;
    Ok(m.id())
}

fn upsert_lock(
    paths: &ProfilePaths,
    m: &Manifest,
    source: String,
    platform: &str,
    sha256: &str,
    requested: &str,
) -> Result<(), CoreError> {
    let mut lock = load_lock(paths)?;
    lock.core = crate::CORE_VERSION.to_string();
    lock.package.retain(|p| p.name != m.name);
    let deps: Vec<String> = m
        .requires
        .packages
        .iter()
        .map(|(k, v)| format!("{k}@{v}"))
        .collect();
    lock.package.push(LockPackage {
        name: m.name.clone(),
        version: m.version.clone(),
        source,
        platform: platform.to_string(),
        sha256: sha256.to_string(),
        requested: requested.to_string(),
        dependencies: deps,
    });
    save_lock(paths, &lock)
}

/// Remove pacote do lock (não apaga a store; `store gc` futuro limpa).
pub fn remove(paths: &ProfilePaths, name: &str) -> Result<bool, CoreError> {
    let mut lock = load_lock(paths)?;
    let before = lock.package.len();
    lock.package.retain(|p| p.name != name);
    if lock.package.len() == before {
        return Ok(false);
    }
    save_lock(paths, &lock)?;
    Ok(true)
}

/// Diretório extraído na store para um pacote do lock.
pub fn package_dir(data_store: &Path, pkg: &LockPackage) -> Result<PathBuf, CoreError> {
    let (publisher, name) = pkg
        .name
        .split_once('/')
        .ok_or_else(|| CoreError::Package(format!("nome {:?}", pkg.name)))?;
    // sha guardado pode ser completo ou 16; o dir usa os 16 primeiros.
    let short = pkg.sha256.get(..16.min(pkg.sha256.len())).unwrap_or("");
    let dir = data_store
        .join("store")
        .join(publisher)
        .join(name)
        .join(format!("{}-{short}", pkg.version));
    if dir.join("atom.toml").exists() {
        return Ok(dir);
    }
    // Compat: instalação --dev antiga usava sha do manifesto.
    let base = data_store.join("store").join(publisher).join(name);
    if let Ok(rd) = std::fs::read_dir(&base) {
        for e in rd.filter_map(|e| e.ok()) {
            if e.path().join("atom.toml").exists()
                && let Some(n) = e.file_name().to_str()
                && n.starts_with(&format!("{}-", pkg.version))
            {
                return Ok(e.path());
            }
        }
    }
    Err(CoreError::Package(format!(
        "pacote {} extraído não encontrado",
        pkg.name
    )))
}

/// Manifestos de todos os pacotes instalados no perfil.
pub fn installed_manifests(
    paths: &ProfilePaths,
    data_store: &Path,
) -> Result<HashMap<String, (LockPackage, Manifest)>, CoreError> {
    let lock = load_lock(paths)?;
    let mut out = HashMap::new();
    for p in lock.package {
        let dir = package_dir(data_store, &p)?;
        let m = read_manifest(&dir)?;
        out.insert(p.name.clone(), (p, m));
    }
    Ok(out)
}

/// Plataforma no formato da loja (`os-arch`, spec store §2).
pub fn current_platform() -> String {
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    };
    format!("{}-{arch}", std::env::consts::OS)
}

fn platform_tag() -> &'static str {
    std::env::consts::OS
}

/// `true` se o core atual satisfaz o requisito `engine` (sem instalar nada).
pub fn engine_ok(engine: &str) -> bool {
    let (Ok(req), Ok(core)) = (
        engine.parse::<semver::VersionReq>(),
        semver::Version::parse(crate::CORE_VERSION),
    ) else {
        return false;
    };
    req.matches(&core)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths_in(t: &tempfile::TempDir) -> (ProfilePaths, PathBuf) {
        let cfg = t.path().join("config");
        let data = t.path().join("data");
        let p = ProfilePaths {
            name: "dev".to_string(),
            dir: cfg.join("profiles/dev"),
            profile_toml: cfg.join("profiles/dev/profile.toml"),
            lock: cfg.join("profiles/dev/atom.lock"),
            lock_prev: cfg.join("profiles/dev/atom.lock.prev"),
            grants: cfg.join("profiles/dev/grants.toml"),
            data_dir: data.join("profiles/dev"),
            kv_dir: data.join("profiles/dev/kv"),
            socket: t.path().join("atom-dev.sock"),
            pid_file: t.path().join("atom-dev.pid"),
            serve_token: data.join("profiles/dev/serve.token"),
        };
        p.ensure().unwrap();
        (p, data)
    }

    fn fixture_pkg(t: &tempfile::TempDir, name: &str) -> PathBuf {
        let src = t.path().join(name);
        std::fs::create_dir_all(src.join("bin")).unwrap();
        std::fs::write(
            src.join("atom.toml"),
            "manifest = 1\nname = \"test/echo\"\nversion = \"0.1.0\"\n\
             kind = \"extension\"\nengine = \">=0.1.0\"\n\
             protocol = \"atom/1\"\nentrypoint = \"bin/echo\"\n",
        )
        .unwrap();
        src
    }

    #[test]
    fn dev_install_writes_lock_and_prev() {
        let t = tempfile::tempdir().unwrap();
        let (p, data) = paths_in(&t);
        let src = fixture_pkg(&t, "src");
        install_dev(&p, &data, &src).unwrap();
        install_dev(&p, &data, &src).unwrap();
        let lock = load_lock(&p).unwrap();
        assert_eq!(lock.package.len(), 1);
        let n = std::fs::read_dir(&p.lock_prev).unwrap().count();
        assert!(n >= 1);
    }

    #[test]
    fn platform_and_engine() {
        let p = current_platform();
        assert!(p.contains('-')); // ex.: "linux-amd64"
        assert!(engine_ok(">=0.1.0"));
        assert!(!engine_ok(">=99.0.0"));
        assert!(!engine_ok("requisito-inválido"));
    }
}
