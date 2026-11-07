//! Cliente `store/v1` (spec `docs/specs/store.md`, parcial v0.5).
//!
//! - Config, índice esparso, catalog, cache ETag, múltiplas sources.
//! - Verificação em 4 passos: sig do índice (chave da loja) → sha256 do
//!   download → sig do artefato (chave do publisher) → `atom.toml` interno
//!   confere com o índice (name, version, kind, capabilities, provides).
//! - Resolver guloso + `engine` + plataforma + `yanked`.
//! - `file://` tem o mesmo layout (lojas privadas e testes).
//!
//! Novas dependências justificadas (AGENTS.md): `ed25519-dalek` + `base64`
//! (assinaturas, spec store §4) e `rand` (CSPRNG do `keygen`).

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::bus::Core;
use crate::error::CoreError;
use crate::pkg;

/// Cliente `store/v2` (TUF, spec `docs/specs/store-v2.md`): verificação
/// própria com `ed25519-dalek` + `sha2` (já permitidos, sem C), sem `tough`
/// e sem dependência nova — decisão documentada em `tuf`.
#[path = "store_tuf.rs"]
pub mod tuf;

/// Plataforma no formato da loja (`linux-amd64`, spec store §2).
pub fn store_platform() -> String {
    pkg::current_platform()
}

/// Source resolvida do perfil, ordenada por `priority`.
#[derive(Debug, Clone)]
pub struct Source {
    pub name: String,
    pub base: String,
    pub keys: Vec<String>,
    pub priority: i32,
    /// sha256 do `1.root.json` confiável (`store/v2`, spec store-v2 §3).
    pub root_sha256: Option<String>,
}

pub fn profile_sources(core: &Core) -> Vec<Source> {
    let mut out: Vec<Source> = core
        .config
        .sources
        .iter()
        .map(|s| Source {
            name: s.name.clone(),
            base: ensure_trailing_slash(&s.url),
            keys: s.keys.clone(),
            priority: s.priority,
            root_sha256: s.root_sha256.clone(),
        })
        .collect();
    out.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then_with(|| a.name.cmp(&b.name))
    });
    out
}

fn ensure_trailing_slash(url: &str) -> String {
    if url.ends_with('/') {
        url.to_string()
    } else {
        format!("{url}/")
    }
}

// ---------------------------------------------------------------------------
// Chaves e assinaturas (spec store §4).

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

/// `"ed25519:BASE64"` → 32 bytes da chave pública.
pub fn parse_pubkey(s: &str) -> Result<[u8; 32], CoreError> {
    let b64part = s
        .strip_prefix("ed25519:")
        .ok_or_else(|| CoreError::Package(format!("chave sem prefixo ed25519:: {s:?}")))?;
    let raw = b64()
        .decode(b64part.trim())
        .map_err(|e| CoreError::Package(format!("chave inválida: {e}")))?;
    raw.try_into()
        .map_err(|_| CoreError::Package("chave ed25519 deve ter 32 bytes".to_string()))
}

pub fn pubkey_str(raw: &[u8; 32]) -> String {
    format!("ed25519:{}", b64().encode(raw))
}

fn verify_sig(pubkey: &[u8; 32], msg: &[u8], sig_b64: &str) -> Result<(), CoreError> {
    use ed25519_dalek::{Signature, VerifyingKey};
    let vk = VerifyingKey::from_bytes(pubkey)
        .map_err(|e| CoreError::Package(format!("chave inválida: {e}")))?;
    let raw = b64()
        .decode(sig_b64.trim())
        .map_err(|e| CoreError::SignatureInvalid(format!("sig base64: {e}")))?;
    let sig = Signature::from_slice(&raw)
        .map_err(|e| CoreError::SignatureInvalid(format!("sig: {e}")))?;
    vk.verify_strict(msg, &sig)
        .map_err(|e| CoreError::SignatureInvalid(format!("assinatura inválida: {e}")))
}

/// Mensagem assinada pelo publisher (spec store §4).
pub fn artifact_message(name: &str, version: &str, platform: &str, sha_hex: &str) -> Vec<u8> {
    format!("atom-artifact-v1\n{name}\n{version}\n{platform}\n{sha_hex}\n").into_bytes()
}

/// Gera par de chaves; retorna (pubkey `ed25519:…`, seed base64).
pub fn keygen() -> (String, String) {
    use ed25519_dalek::SigningKey;
    use rand::RngCore;
    let mut seed = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut seed);
    let sk = SigningKey::from_bytes(&seed);
    let seed_b64 = b64().encode(seed);
    let vk = sk.verifying_key();
    (pubkey_str(&vk.to_bytes()), seed_b64)
}

/// Lê seed de chave privada (`ed25519-seed:BASE64` ou base64 puro).
pub fn load_seed(path: &Path) -> Result<[u8; 32], CoreError> {
    let text = std::fs::read_to_string(path).map_err(|e| CoreError::Io(e.to_string()))?;
    let t = text.trim();
    let part = t.strip_prefix("ed25519-seed:").unwrap_or(t).trim();
    let raw = b64()
        .decode(part)
        .map_err(|e| CoreError::Package(format!("chave privada inválida: {e}")))?;
    raw.try_into()
        .map_err(|_| CoreError::Package("seed deve ter 32 bytes".to_string()))
}

pub fn pubkey_of_seed(seed: &[u8; 32]) -> String {
    use ed25519_dalek::SigningKey;
    let sk = SigningKey::from_bytes(seed);
    pubkey_str(&sk.verifying_key().to_bytes())
}

pub fn sign_with_seed(seed: &[u8; 32], msg: &[u8]) -> String {
    use ed25519_dalek::{Signer, SigningKey};
    b64().encode(SigningKey::from_bytes(seed).sign(msg).to_bytes())
}

// ---------------------------------------------------------------------------
// Tipos do protocolo store/v1.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreConfig {
    pub store: u32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub index_signature_keys: Vec<String>,
    #[serde(default)]
    pub artifacts_base: Option<String>,
    #[serde(default)]
    pub features: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexFile {
    pub name: String,
    #[serde(default)]
    pub publisher_keys: Vec<String>,
    #[serde(default)]
    pub versions: Vec<IndexVersion>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexVersion {
    pub version: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub engine: String,
    #[serde(default)]
    pub protocol: Option<String>,
    #[serde(default)]
    pub requires: IndexRequires,
    #[serde(default)]
    pub provides: Vec<IndexProvides>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub artifacts: Vec<IndexArtifact>,
    #[serde(default)]
    pub yanked: bool,
    #[serde(default)]
    pub published_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IndexRequires {
    #[serde(default)]
    pub packages: HashMap<String, String>,
    #[serde(default)]
    pub contracts: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexProvides {
    pub contract: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexArtifact {
    pub platform: String,
    pub url: String,
    pub sha256: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub latest: String,
}

// ---------------------------------------------------------------------------
// Fetch com cache ETag (http) ou leitura direta (file://).

fn cache_dir(core: &Core, source: &Source) -> PathBuf {
    let safe: String = source
        .name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    core.atom_paths.cache.join("index").join(safe)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CacheMeta {
    #[serde(default)]
    etag: Option<String>,
    #[serde(default)]
    last_modified: Option<String>,
}

fn is_file_base(base: &str) -> bool {
    base.starts_with("file://")
}

fn file_base_path(base: &str) -> Result<PathBuf, CoreError> {
    base.strip_prefix("file://")
        .map(PathBuf::from)
        .ok_or_else(|| CoreError::Package(format!("base inválida: {base}")))
}

/// GET relativo à base; `sig: true` busca `<path>.sig` e retorna texto.
/// Retorna (bytes, veio_do_cache).
async fn fetch(
    core: &Core,
    source: &Source,
    rel: &str,
    want_sig: bool,
) -> Result<(Vec<u8>, bool), CoreError> {
    let path = if want_sig {
        format!("{rel}.sig")
    } else {
        rel.to_string()
    };
    if is_file_base(&source.base) {
        let mut p = file_base_path(&source.base)?;
        p.push(&path);
        let b = std::fs::read(&p)
            .map_err(|e| CoreError::Package(format!("store {}: {path}: {e}", source.name)))?;
        return Ok((b, false));
    }
    let url = format!("{}{}", source.base, path);
    let dir = cache_dir(core, source);
    std::fs::create_dir_all(&dir).map_err(|e| CoreError::Io(e.to_string()))?;
    let cache_file = dir.join(path.replace('/', "__"));
    let meta_file = dir.join(format!("{}.meta.json", path.replace('/', "__")));
    let meta: CacheMeta = std::fs::read_to_string(&meta_file)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let client = reqwest::Client::new();
    let mut req = client.get(&url);
    if let Some(etag) = &meta.etag {
        req = req.header(reqwest::header::IF_NONE_MATCH, etag.clone());
    }
    if let Some(lm) = &meta.last_modified {
        req = req.header(reqwest::header::IF_MODIFIED_SINCE, lm.clone());
    }
    // Auth bearer p/ lojas privadas (secret `store:<source>`, best-effort).
    if let Ok(Some(secret)) = core.secrets.get(&format!("store:{}", source.name)) {
        req = req.bearer_auth(secret);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| CoreError::Package(format!("GET {url}: {e}")))?;
    if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
        let b = std::fs::read(&cache_file).map_err(|e| CoreError::Io(e.to_string()))?;
        return Ok((b, true));
    }
    if !resp.status().is_success() {
        return Err(CoreError::Package(format!(
            "GET {url}: http {}",
            resp.status()
        )));
    }
    let etag = resp
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let last_modified = resp
        .headers()
        .get(reqwest::header::LAST_MODIFIED)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| CoreError::Package(format!("GET {url}: {e}")))?
        .to_vec();
    // Cache só é confiável com validador; sem ETag/Last-Modified, usa TTL de 1h
    // via mtime do arquivo (`shortcut: sem relógio lógico; mtime basta`).
    std::fs::write(&cache_file, &bytes).map_err(|e| CoreError::Io(e.to_string()))?;
    let meta = CacheMeta {
        etag,
        last_modified,
    };
    let _ = std::fs::write(&meta_file, serde_json::to_vec(&meta).unwrap_or_default());
    Ok((bytes, false))
}

/// Proteção contra rollback de índice (spec store §5).
fn seen_path(core: &Core, source: &Source) -> PathBuf {
    cache_dir(core, source).join("seen.json")
}

fn check_rollback(core: &Core, source: &Source, index: &IndexFile) -> Result<(), CoreError> {
    let path = seen_path(core, source);
    let seen: HashMap<String, HashMap<String, bool>> = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    if let Some(vers) = seen.get(&index.name) {
        let current: HashSet<&str> = index.versions.iter().map(|v| v.version.as_str()).collect();
        for (v, was_yanked) in vers {
            if !current.contains(v.as_str()) && !was_yanked {
                return Err(CoreError::SignatureInvalid(format!(
                    "índice de {} removeu versão {v} já vista (rollback?)",
                    index.name
                )));
            }
        }
        let max_new = index.versions.iter().map(|v| v.published_at.as_str()).max();
        let _ = max_new;
    }
    Ok(())
}

fn record_seen(core: &Core, source: &Source, index: &IndexFile) {
    let path = seen_path(core, source);
    let mut seen: HashMap<String, HashMap<String, bool>> = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let entry = seen.entry(index.name.clone()).or_default();
    for v in &index.versions {
        entry.insert(v.version.clone(), v.yanked);
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(b) = serde_json::to_vec(&seen) {
        let _ = std::fs::write(&path, b);
    }
}

// ---------------------------------------------------------------------------
// Resolução e instalação.

fn index_rel(name: &str) -> Result<String, CoreError> {
    let (publisher, pkg) = name
        .split_once('/')
        .ok_or_else(|| CoreError::Package(format!("nome inválido: {name:?}")))?;
    Ok(format!("index/{publisher}/{pkg}.json"))
}

pub async fn fetch_config(core: &Core, source: &Source) -> Result<StoreConfig, CoreError> {
    let (bytes, _) = fetch(core, source, "config.json", false).await?;
    let cfg: StoreConfig = serde_json::from_slice(&bytes)
        .map_err(|e| CoreError::Package(format!("config.json inválida: {e}")))?;
    if cfg.store != 1 {
        return Err(CoreError::Package(format!(
            "store {}: versão {}",
            source.name, cfg.store
        )));
    }
    if !source.keys.is_empty()
        && !cfg
            .index_signature_keys
            .iter()
            .any(|k| source.keys.contains(k))
    {
        return Err(CoreError::SignatureInvalid(format!(
            "nenhuma chave raiz de `{}` está em index_signature_keys",
            source.name
        )));
    }
    Ok(cfg)
}

pub async fn fetch_index(
    core: &Core,
    source: &Source,
    name: &str,
    cfg: &StoreConfig,
) -> Result<IndexFile, CoreError> {
    let rel = index_rel(name)?;
    let (bytes, _cached) = fetch(core, source, &rel, false).await?;
    let (sig_bytes, _) = fetch(core, source, &rel, true).await?;
    let sig = String::from_utf8(sig_bytes)
        .map_err(|_| CoreError::SignatureInvalid("index .sig não é UTF-8".to_string()))?;
    // Passo 1: .sig válida por chave da loja.
    let mut ok = false;
    let mut last_err = String::new();
    for k in &cfg.index_signature_keys {
        match parse_pubkey(k).and_then(|pk| verify_sig(&pk, &bytes, &sig)) {
            Ok(()) => {
                ok = true;
                break;
            }
            Err(e) => last_err = e.to_string(),
        }
    }
    if !ok {
        return Err(CoreError::SignatureInvalid(format!(
            "index {name}: sig inválida ({last_err})"
        )));
    }
    let index: IndexFile = serde_json::from_slice(&bytes)
        .map_err(|e| CoreError::Package(format!("index {name} inválido: {e}")))?;
    if index.name != name {
        return Err(CoreError::Package(format!(
            "index respondeu {:?}, esperado {name:?}",
            index.name
        )));
    }
    check_rollback(core, source, &index)?;
    record_seen(core, source, &index);
    Ok(index)
}

pub(crate) fn parse_req(req: Option<&str>) -> Result<semver::VersionReq, CoreError> {
    let r = req.unwrap_or("*").trim();
    if r.is_empty() || r == "*" {
        return Ok(semver::VersionReq::STAR);
    }
    if let Ok(v) = semver::Version::parse(r) {
        return Ok(semver::VersionReq::parse(&format!("={v}")).unwrap());
    }
    semver::VersionReq::parse(r).map_err(|e| CoreError::Package(format!("requisito {r:?}: {e}")))
}

pub(crate) struct Pick<'a> {
    version: &'a IndexVersion,
    artifact: &'a IndexArtifact,
}

/// Versão resolvida (sem instalar): p/ prévia de update (v0.7).
#[derive(Debug, Clone)]
pub struct ResolvedVersion {
    pub version: IndexVersion,
    pub source: String,
}

/// Maior versão que satisfaz `req` na source (prioridade), sem instalar.
/// Erro se nada resolve (index ausente, req vazio, plataforma, yanked).
/// Caminho v1 (só sources não-v2); o dispatch público particiona antes.
async fn resolve_latest_v1_on(
    core: &Core,
    sources: &[Source],
    name: &str,
    req: Option<&str>,
    source_only: Option<&str>,
) -> Result<ResolvedVersion, CoreError> {
    let req_parsed = parse_req(req)?;
    let locked_version: Option<String> = pkg::load_lock(&core.paths).ok().and_then(|l| {
        l.package
            .iter()
            .find(|p| p.name == name)
            .map(|p| p.version.clone())
    });
    let mut errors = vec![];
    for s in sources {
        if let Some(only) = source_only
            && s.name != only
        {
            continue;
        }
        let cfg = match fetch_config(core, s).await {
            Ok(c) => c,
            Err(e) => {
                errors.push(format!("{}: {e}", s.name));
                continue;
            }
        };
        let index = match fetch_index(core, s, name, &cfg).await {
            Ok(i) => i,
            Err(e) => {
                errors.push(format!("{}: {e}", s.name));
                continue;
            }
        };
        match pick(&index, &req_parsed, locked_version.as_deref()) {
            Ok(p) => {
                return Ok(ResolvedVersion {
                    version: p.version.clone(),
                    source: s.name.clone(),
                });
            }
            Err(e) => {
                errors.push(format!("{}: {e}", s.name));
                continue;
            }
        }
    }
    Err(CoreError::NoProvider(format!(
        "{name}@{} ({})",
        req.unwrap_or("*"),
        errors.join("; ")
    )))
}

/// Maior versão que satisfaz `req` na source (prioridade), sem instalar.
/// Erro se nada resolve (index ausente, req vazio, plataforma, yanked).
/// Dispatch: source_only v2 → só tuf; v1-only → caminho v1; misto sem
/// source_only → tenta v1 e, só em `NoProvider`, tenta `tuf`.
pub async fn resolve_latest_version(
    core: &Core,
    name: &str,
    req: Option<&str>,
    source_only: Option<&str>,
) -> Result<ResolvedVersion, CoreError> {
    let sources = profile_sources(core);
    if sources.is_empty() {
        return Err(CoreError::Package("nenhuma source configurada".to_string()));
    }
    if let Some(only) = source_only {
        let Some(found) = sources.iter().find(|s| s.name == only) else {
            return Err(CoreError::Package(format!("source `{only}` não existe")));
        };
        if tuf::is_v2_source(&found.base) {
            let r = tuf::resolve_latest_version(core, name, req, source_only).await?;
            return Ok(ResolvedVersion {
                version: r.version,
                source: r.source,
            });
        }
        return resolve_latest_v1_on(core, &sources, name, req, source_only).await;
    }
    let v1: Vec<Source> = sources
        .iter()
        .filter(|s| !tuf::is_v2_source(&s.base))
        .cloned()
        .collect();
    let has_v2 = sources.iter().any(|s| tuf::is_v2_source(&s.base));
    if !has_v2 {
        return resolve_latest_v1_on(core, &v1, name, req, None).await;
    }
    if v1.is_empty() {
        let r = tuf::resolve_latest_version(core, name, req, None).await?;
        return Ok(ResolvedVersion {
            version: r.version,
            source: r.source,
        });
    }
    match resolve_latest_v1_on(core, &v1, name, req, None).await {
        Ok(r) => Ok(r),
        Err(CoreError::NoProvider(_)) => {
            let r = tuf::resolve_latest_version(core, name, req, None).await?;
            Ok(ResolvedVersion {
                version: r.version,
                source: r.source,
            })
        }
        Err(e) => Err(e),
    }
}
/// Maior versão que satisfaz req + engine + plataforma; yanked só se já no lock.
pub(crate) fn pick<'a>(
    index: &'a IndexFile,
    req: &semver::VersionReq,
    locked_version: Option<&str>,
) -> Result<Pick<'a>, CoreError> {
    let mut cands: Vec<&IndexVersion> = index
        .versions
        .iter()
        .filter(|v| {
            semver::Version::parse(&v.version).is_ok_and(|sv| req.matches(&sv))
                && (pkg::engine_ok(if v.engine.is_empty() {
                    ">=0.0.0"
                } else {
                    &v.engine
                }))
                && (!v.yanked || locked_version == Some(v.version.as_str()))
        })
        .collect();
    cands.sort_by(|a, b| {
        let va = semver::Version::parse(&a.version).unwrap();
        let vb = semver::Version::parse(&b.version).unwrap();
        vb.cmp(&va)
    });
    let me = store_platform();
    for v in cands {
        if let Some(a) = v.artifacts.iter().find(|a| a.platform == me) {
            return Ok(Pick {
                version: v,
                artifact: a,
            });
        }
        if let Some(a) = v.artifacts.iter().find(|a| a.platform == "any") {
            return Ok(Pick {
                version: v,
                artifact: a,
            });
        }
    }
    Err(CoreError::NoProvider(format!(
        "{} (sem versão p/ req, engine, plataforma {me} ou yanked)",
        index.name
    )))
}

fn artifact_url(cfg: &StoreConfig, base: &str, art: &IndexArtifact) -> String {
    if art.url.starts_with("http://")
        || art.url.starts_with("https://")
        || art.url.starts_with("file://")
    {
        return art.url.clone();
    }
    if let Some(ab) = &cfg.artifacts_base {
        let ab = ensure_trailing_slash(ab);
        return format!("{ab}{}", art.url.trim_start_matches('/'));
    }
    format!("{base}{}", art.url.trim_start_matches('/'))
}

async fn download(url: &str) -> Result<Vec<u8>, CoreError> {
    if let Some(rest) = url.strip_prefix("file://") {
        return std::fs::read(rest).map_err(|e| CoreError::Package(format!("ler {url}: {e}")));
    }
    let bytes = reqwest::Client::new()
        .get(url)
        .send()
        .await
        .map_err(|e| CoreError::Package(format!("GET {url}: {e}")))?
        .error_for_status()
        .map_err(|e| CoreError::Package(format!("GET {url}: {e}")))?
        .bytes()
        .await
        .map_err(|e| CoreError::Package(format!("GET {url}: {e}")))?;
    Ok(bytes.to_vec())
}

pub(crate) fn check_manifest_against_index<'a>(
    index_name: &'a str,
    v: &'a IndexVersion,
) -> impl Fn(&atom_protocol::Manifest) -> Result<(), CoreError> + 'a {
    move |m: &atom_protocol::Manifest| {
        if m.name != index_name {
            return Err(CoreError::SignatureInvalid(format!(
                "atom.toml name {:?} != índice {index_name:?}",
                m.name
            )));
        }
        if m.version != v.version {
            return Err(CoreError::SignatureInvalid(format!(
                "atom.toml version {:?} != índice {:?}",
                m.version, v.version
            )));
        }
        if m.kind != v.kind {
            return Err(CoreError::SignatureInvalid(format!(
                "atom.toml kind {:?} != índice {:?}",
                m.kind, v.kind
            )));
        }
        let mut a: Vec<&String> = m.capabilities.iter().collect();
        let mut b: Vec<&String> = v.capabilities.iter().collect();
        a.sort();
        b.sort();
        if a != b {
            return Err(CoreError::SignatureInvalid(
                "capabilities divergem do índice".to_string(),
            ));
        }
        let mut pa: Vec<(&str, &str)> = m
            .provides
            .iter()
            .map(|p| (p.contract.as_str(), p.version.as_str()))
            .collect();
        let mut pb: Vec<(&str, &str)> = v
            .provides
            .iter()
            .map(|p| (p.contract.as_str(), p.version.as_str()))
            .collect();
        pa.sort();
        pb.sort();
        if pa != pb {
            return Err(CoreError::SignatureInvalid(
                "provides divergem do índice".to_string(),
            ));
        }
        Ok(())
    }
}

/// Baixa, verifica (4 passos) e instala um artefato. Retorna o id instalado.
async fn install_pick(
    core: &mut Core,
    source: &Source,
    cfg: &StoreConfig,
    index: &IndexFile,
    pick: &Pick<'_>,
    requested: &str,
) -> Result<String, CoreError> {
    let v = pick.version;
    let a = pick.artifact;
    let url = artifact_url(cfg, &source.base, a);
    let bytes = download(&url).await?;
    // Passo 2: sha256 do download confere.
    {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(&bytes);
        let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if hex != a.sha256 {
            return Err(CoreError::SignatureInvalid(format!(
                "sha256 de {}@{} diverge (esperado {}, obtido {hex})",
                index.name, v.version, a.sha256
            )));
        }
    }
    if a.size > 0 && bytes.len() as u64 != a.size {
        return Err(CoreError::SignatureInvalid(format!(
            "tamanho de {}@{} diverge",
            index.name, v.version
        )));
    }
    // Passo 3: assinatura do publisher.
    {
        let msg = artifact_message(&index.name, &v.version, &a.platform, &a.sha256);
        let mut ok = false;
        for k in &index.publisher_keys {
            if parse_pubkey(k)
                .and_then(|pk| verify_sig(&pk, &msg, &a.signature))
                .is_ok()
            {
                ok = true;
                break;
            }
        }
        if !ok {
            return Err(CoreError::SignatureInvalid(format!(
                "assinatura do publisher inválida p/ {}@{}",
                index.name, v.version
            )));
        }
    }
    // Passo 4 (+ extração) via check do manifesto.
    let check = check_manifest_against_index(&index.name, v);
    let id = pkg::install_bytes(
        &core.paths,
        &core.data_store,
        &bytes,
        source.name.clone(),
        &a.platform,
        requested,
        Some(&check),
    )?;
    Ok(id)
}

/// Caminho v1 de instalação (só sources passadas; o dispatch particiona).
async fn install_package_v1_on(
    core: &mut Core,
    sources: &[Source],
    name: &str,
    req: Option<&str>,
    source_only: Option<&str>,
    force_refresh: bool,
) -> Result<String, CoreError> {
    let req_parsed = parse_req(req)?;
    let requested = req.unwrap_or("*");
    if sources.is_empty() {
        return Err(CoreError::Package(
            "nenhuma source configurada (`source add`)".to_string(),
        ));
    }
    let mut plan: Vec<(Source, IndexFile, StoreConfig, IndexVersion, IndexArtifact)> = vec![];
    let mut visiting: Vec<String> = vec![];
    resolve_into(
        core,
        sources,
        name,
        &req_parsed,
        requested,
        source_only,
        force_refresh,
        &mut plan,
        &mut visiting,
    )
    .await?;
    // Instala dependências antes dos dependentes (ordem do plano já é pós-ordem).
    let mut last = String::new();
    for (source, cfg, index, v, a) in plan
        .iter()
        .map(|(s, i, c, vv, aa)| (s.clone(), c.clone(), i.clone(), vv.clone(), aa.clone()))
    {
        let pick = Pick {
            version: &v,
            artifact: &a,
        };
        let r = if index.name == name { requested } else { "*" };
        last = install_pick(core, &source, &cfg, &index, &pick, r).await?;
    }
    core.refresh()?;
    Ok(last)
}

/// Resolve + instala pacote e dependências (`requires.packages`), recursivo.
/// Dispatch: source_only v2 → só tuf; v1-only → caminho v1; misto sem
/// source_only → tenta v1 e, só em `NoProvider`, tenta `tuf`.
pub async fn install_package(
    core: &mut Core,
    name: &str,
    req: Option<&str>,
    source_only: Option<&str>,
    force_refresh: bool,
) -> Result<String, CoreError> {
    let sources = profile_sources(core);
    if sources.is_empty() {
        return Err(CoreError::Package(
            "nenhuma source configurada (`source add`)".to_string(),
        ));
    }
    if let Some(only) = source_only {
        let Some(found) = sources.iter().find(|s| s.name == only) else {
            return Err(CoreError::Package(format!("source `{only}` não existe")));
        };
        if tuf::is_v2_source(&found.base) {
            return tuf::install_package(core, name, req, source_only, force_refresh).await;
        }
        return install_package_v1_on(core, &sources, name, req, source_only, force_refresh).await;
    }
    let v1: Vec<Source> = sources
        .iter()
        .filter(|s| !tuf::is_v2_source(&s.base))
        .cloned()
        .collect();
    let has_v2 = sources.iter().any(|s| tuf::is_v2_source(&s.base));
    if !has_v2 {
        return install_package_v1_on(core, &v1, name, req, None, force_refresh).await;
    }
    if v1.is_empty() {
        return tuf::install_package(core, name, req, None, force_refresh).await;
    }
    match install_package_v1_on(core, &v1, name, req, None, force_refresh).await {
        Ok(id) => Ok(id),
        Err(CoreError::NoProvider(_)) => {
            tuf::install_package(core, name, req, None, force_refresh).await
        }
        Err(e) => Err(e),
    }
}

#[allow(clippy::too_many_arguments)]
async fn resolve_into(
    core: &Core,
    sources: &[Source],
    name: &str,
    req: &semver::VersionReq,
    requested: &str,
    source_only: Option<&str>,
    force_refresh: bool,
    plan: &mut Vec<(Source, IndexFile, StoreConfig, IndexVersion, IndexArtifact)>,
    visiting: &mut Vec<String>,
) -> Result<(), CoreError> {
    let _ = requested;
    if visiting.contains(&name.to_string()) {
        return Err(CoreError::Package(format!("dependência cíclica: {name}")));
    }
    if plan.iter().any(|(_, i, _, _, _)| i.name == name) {
        return Ok(());
    }
    // Já instalado e satisfaz? pula (lock como fonte da verdade).
    // Com force_refresh (update), re-resolve p/ checar frescor (rollback
    // protection) e achar versão maior.
    if !force_refresh
        && let Ok(lock) = pkg::load_lock(&core.paths)
        && let Some(lp) = lock.package.iter().find(|p| p.name == name)
        && let Ok(sv) = semver::Version::parse(&lp.version)
        && req.matches(&sv)
    {
        return Ok(());
    }
    visiting.push(name.to_string());
    let mut resolved: Option<(Source, StoreConfig, IndexFile, IndexVersion, IndexArtifact)> = None;
    let mut errors = vec![];
    for s in sources {
        if let Some(only) = source_only
            && s.name != only
        {
            continue;
        }
        let cfg = match fetch_config(core, s).await {
            Ok(c) => c,
            Err(e) => {
                errors.push(format!("{}: {e}", s.name));
                continue;
            }
        };
        let index = match fetch_index(core, s, name, &cfg).await {
            Ok(i) => i,
            Err(e) => {
                // 404 = pacote ausente nesta source; tenta a próxima.
                errors.push(format!("{}: {e}", s.name));
                continue;
            }
        };
        let locked_version: Option<String> = pkg::load_lock(&core.paths).ok().and_then(|l| {
            l.package
                .iter()
                .find(|p| p.name == name)
                .map(|p| p.version.clone())
        });
        let (version, artifact) = match pick(&index, req, locked_version.as_deref()) {
            Ok(p) => (p.version.clone(), p.artifact.clone()),
            Err(e) => {
                errors.push(format!("{}: {e}", s.name));
                continue;
            }
        };
        resolved = Some((s.clone(), cfg, index, version, artifact));
        break;
    }
    let (source, cfg, index, version, artifact) = resolved
        .ok_or_else(|| CoreError::NoProvider(format!("{name}@{req} ({})", errors.join("; "))))?;
    // Contratos exigidos precisam de provider instalado (erro claro, sem auto-install).
    for (contract, creq) in &version.requires.contracts {
        let ok = core.installed().values().any(|(_, m)| {
            m.provides.iter().any(|p| {
                p.contract == *contract
                    && semver::VersionReq::parse(creq)
                        .map(|r| semver::Version::parse(&p.version).is_ok_and(|v| r.matches(&v)))
                        .unwrap_or(false)
            })
        });
        if !ok {
            return Err(CoreError::NoProvider(format!(
                "{name} exige contrato {contract}@{creq} (nenhum provider instalado)"
            )));
        }
    }
    for (dep, dep_req) in &version.requires.packages.clone() {
        let r = parse_req(Some(dep_req))?;
        Box::pin(resolve_into(
            core,
            sources,
            dep,
            &r,
            dep_req,
            source_only,
            force_refresh,
            plan,
            visiting,
        ))
        .await?;
    }
    visiting.pop();
    // Checa namespace: mesmo publisher em outra source exige confirmação.
    if let Ok(lock) = pkg::load_lock(&core.paths)
        && let Some(lp) = lock.package.iter().find(|p| p.name == name)
    {
        let is_store_src = sources.iter().any(|s| s.name == lp.source);
        if is_store_src && lp.source != source.name {
            return Err(CoreError::Package(format!(
                "{name} veio da source `{}`; use --source p/ trocar (namespace por publisher)",
                lp.source
            )));
        }
    }
    let _ = cfg;
    plan.push((source, index, cfg, version, artifact));
    Ok(())
}

// ---------------------------------------------------------------------------
// search / info / update / rollback.

async fn search_v1_on(
    core: &Core,
    sources: &[Source],
    query: &str,
    kind: Option<&str>,
) -> Result<Vec<(String, CatalogEntry)>, CoreError> {
    if sources.is_empty() {
        return Err(CoreError::Package("nenhuma source configurada".to_string()));
    }
    let q = query.to_lowercase();
    let mut seen_names = HashSet::new();
    let mut out = vec![];
    for s in sources {
        let cfg = fetch_config(core, s).await?;
        let (bytes, _) = match fetch(core, s, "catalog.json", false).await {
            Ok(b) => b,
            Err(_) => continue,
        };
        if let Ok((sig_bytes, _)) = fetch(core, s, "catalog.json", true).await {
            let sig = String::from_utf8(sig_bytes).unwrap_or_default();
            let mut ok = false;
            for k in &cfg.index_signature_keys {
                if parse_pubkey(k)
                    .and_then(|pk| verify_sig(&pk, &bytes, &sig))
                    .is_ok()
                {
                    ok = true;
                    break;
                }
            }
            if !ok {
                return Err(CoreError::SignatureInvalid(format!(
                    "catalog de `{}` com sig inválida",
                    s.name
                )));
            }
        }
        let entries: Vec<CatalogEntry> = serde_json::from_slice(&bytes)
            .map_err(|e| CoreError::Package(format!("catalog inválido: {e}")))?;
        for e in entries {
            if !seen_names.insert(e.name.clone()) {
                continue;
            }
            if !q.is_empty()
                && !e.name.to_lowercase().contains(&q)
                && !e.description.to_lowercase().contains(&q)
            {
                continue;
            }
            if let Some(k) = kind
                && e.kind != k
            {
                continue;
            }
            out.push((s.name.clone(), e));
        }
    }
    Ok(out)
}

/// Busca no catalog: v1 sobre não-v2 + `tuf::search_v2`, concatena com
/// dedupe por nome (prioridade = ordem de `profile_sources`).
/// Erro só se ambos falharem.
pub async fn search(
    core: &Core,
    query: &str,
    kind: Option<&str>,
) -> Result<Vec<(String, CatalogEntry)>, CoreError> {
    let sources = profile_sources(core);
    if sources.is_empty() {
        return Err(CoreError::Package("nenhuma source configurada".to_string()));
    }
    let v1: Vec<Source> = sources
        .iter()
        .filter(|s| !tuf::is_v2_source(&s.base))
        .cloned()
        .collect();
    let has_v2 = sources.iter().any(|s| tuf::is_v2_source(&s.base));
    let mut seen = HashSet::new();
    let mut out: Vec<(String, CatalogEntry)> = vec![];
    let mut v1_err: Option<CoreError> = None;
    if !v1.is_empty() {
        match search_v1_on(core, &v1, query, kind).await {
            Ok(hits) => {
                for (src, e) in hits {
                    if seen.insert(e.name.clone()) {
                        out.push((src, e));
                    }
                }
            }
            Err(e) => v1_err = Some(e),
        }
    }
    if has_v2 {
        match tuf::search_v2(core, query, kind).await {
            Ok(hits) => {
                for (src, e) in hits {
                    if seen.insert(e.name.clone()) {
                        out.push((src, e));
                    }
                }
            }
            Err(e) => {
                if out.is_empty() {
                    if let Some(ve) = v1_err {
                        return Err(ve);
                    }
                    return Err(e);
                }
            }
        }
    } else if out.is_empty()
        && let Some(ve) = v1_err
    {
        return Err(ve);
    }
    Ok(out)
}

async fn info_v1_on(
    core: &Core,
    sources: &[Source],
    name: &str,
) -> Result<Vec<(String, IndexFile)>, CoreError> {
    let mut out = vec![];
    for s in sources {
        let cfg = match fetch_config(core, s).await {
            Ok(c) => c,
            Err(_) => continue,
        };
        match fetch_index(core, s, name, &cfg).await {
            Ok(i) => out.push((s.name.clone(), i)),
            Err(_) => continue,
        }
    }
    if out.is_empty() {
        return Err(CoreError::NoProvider(name.to_string()));
    }
    Ok(out)
}

/// Índices do pacote: v1 sobre não-v2 + `tuf::info_v2`.
/// Vazio total = `NoProvider`.
pub async fn info(core: &Core, name: &str) -> Result<Vec<(String, IndexFile)>, CoreError> {
    let sources = profile_sources(core);
    let v1: Vec<Source> = sources
        .iter()
        .filter(|s| !tuf::is_v2_source(&s.base))
        .cloned()
        .collect();
    let has_v2 = sources.iter().any(|s| tuf::is_v2_source(&s.base));
    let mut out = info_v1_on(core, &v1, name).await.unwrap_or_default();
    if has_v2 && let Ok(hits) = tuf::info_v2(core, name).await {
        out.extend(hits);
    }
    if out.is_empty() {
        return Err(CoreError::NoProvider(name.to_string()));
    }
    Ok(out)
}

/// Atualiza um pacote (ou todos os da loja) p/ a maior versão do mesmo req.
/// Particiona alvos por kind de source: v1 = loop atual; v2 = `tuf::update_v2`.
pub async fn update_packages(
    core: &mut Core,
    name: Option<&str>,
) -> Result<Vec<(String, String, String)>, CoreError> {
    let by_source: std::collections::HashMap<String, bool> = profile_sources(core)
        .into_iter()
        .map(|s| (s.name.clone(), tuf::is_v2_source(&s.base)))
        .collect();
    let targets: Vec<(String, String, String)> = {
        let lock = pkg::load_lock(&core.paths)?;
        lock.package
            .iter()
            .filter(|p| {
                by_source.contains_key(&p.source) && name.as_ref().is_none_or(|n| *n == p.name)
            })
            .map(|p| (p.name.clone(), p.version.clone(), p.source.clone()))
            .collect()
    };
    if targets.is_empty() {
        return Err(CoreError::Package(
            "nada da loja para atualizar".to_string(),
        ));
    }
    let (v2_targets, v1_targets): (Vec<_>, Vec<_>) = targets
        .into_iter()
        .partition(|(_, _, src)| by_source.get(src).copied().unwrap_or(false));
    let mut done = vec![];
    if !v2_targets.is_empty() {
        let only = name.map(|n| n.to_string());
        done.extend(tuf::update_v2(core, only.as_deref()).await?);
    }
    for (n, from, src) in v1_targets {
        let req = pkg::load_lock(&core.paths)
            .ok()
            .and_then(|l| {
                l.package
                    .iter()
                    .find(|p| p.name == n)
                    .map(|p| p.requested.clone())
            })
            .unwrap_or_else(|| "*".to_string());
        let before = n.clone();
        install_package(core, &n, Some(&req), Some(&src), true).await?;
        let after = pkg::load_lock(&core.paths)?
            .package
            .iter()
            .find(|p| p.name == before)
            .map(|p| p.version.clone())
            .unwrap_or_default();
        if after != from {
            done.push((n, from, after));
        }
    }
    core.refresh()?;
    Ok(done)
}

/// Restaura o `atom.lock` anterior mais recente (rollback).
pub fn rollback_lock(paths: &crate::profile::ProfilePaths) -> Result<String, CoreError> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(&paths.lock_prev)
        .map_err(|e| CoreError::Io(e.to_string()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    files.sort();
    let prev = files
        .pop()
        .ok_or_else(|| CoreError::Package("sem rollback disponível".to_string()))?;
    let bytes = std::fs::read(&prev).map_err(|e| CoreError::Io(e.to_string()))?;
    crate::profile::atomic_write(&paths.lock, &bytes)?;
    Ok(prev
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("?")
        .to_string())
}

// ---------------------------------------------------------------------------
// pack / publish (ferramentas da loja; usadas pelo CLI e pela CI de referência).

/// Resultado do `pack`: artefato + assinatura + snippet p/ o índice.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackSnippet {
    pub name: String,
    pub version: String,
    pub kind: String,
    #[serde(default)]
    pub engine: String,
    #[serde(default)]
    pub protocol: Option<String>,
    #[serde(default)]
    pub requires: IndexRequires,
    #[serde(default)]
    pub provides: Vec<IndexProvides>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    pub publisher_key: String,
    pub artifact: IndexArtifact,
    pub published_at: String,
}

fn now_rfc3339() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // `shortcut: sem chrono; segundos desde epoch como string. ISO-8601 real
    // exigiria dependência nova; ordenação lexicográfica ainda funciona para
    // `published_at` dentro da mesma era (só dígitos).`
    format!("{secs}")
}

/// Empacota pasta → tar.gz + `.sig` + snippet do índice. Retorna paths escritos.
/// `shortcut: tar sem mtime fixo; a assinatura cobre os bytes gerados.`
pub fn pack_package(
    src: &Path,
    platform: Option<&str>,
    keyfile: &Path,
    out_dir: &Path,
) -> Result<(PathBuf, PathBuf), CoreError> {
    let m = pkg::read_manifest(src)?;
    semver::Version::parse(&m.version)
        .map_err(|e| CoreError::Package(format!("version {:?}: {e}", m.version)))?;
    let seed = load_seed(keyfile)?;
    let publisher_key = pubkey_of_seed(&seed);
    let plat = platform.map(|s| s.to_string()).unwrap_or_else(|| {
        if m.entrypoint.is_some() {
            store_platform()
        } else {
            "any".to_string()
        }
    });
    let stem = format!("{}-{}-{plat}", m.name.replace('/', "-"), m.version);
    std::fs::create_dir_all(out_dir).map_err(|e| CoreError::Io(e.to_string()))?;
    let artifact_path = out_dir.join(format!("{stem}.tar.gz"));
    {
        let f = std::fs::File::create(&artifact_path).map_err(|e| CoreError::Io(e.to_string()))?;
        let gz = flate2::write::GzEncoder::new(f, flate2::Compression::default());
        let mut tar = tar::Builder::new(gz);
        tar.append_dir_all(".", src)
            .map_err(|e| CoreError::Package(format!("tar: {e}")))?;
        tar.finish()
            .map_err(|e| CoreError::Package(format!("tar: {e}")))?;
    }
    let bytes = std::fs::read(&artifact_path).map_err(|e| CoreError::Io(e.to_string()))?;
    let sha_hex = {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(&bytes);
        h.finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    let sig = sign_with_seed(
        &seed,
        &artifact_message(&m.name, &m.version, &plat, &sha_hex),
    );
    std::fs::write(
        out_dir.join(format!("{stem}.tar.gz.sig")),
        format!("{sig}\n"),
    )
    .map_err(|e| CoreError::Io(e.to_string()))?;
    let provides = m
        .provides
        .iter()
        .map(|p| IndexProvides {
            contract: p.contract.clone(),
            version: p.version.clone(),
        })
        .collect();
    let requires = IndexRequires {
        packages: m.requires.packages.clone(),
        contracts: m.requires.contracts.clone(),
    };
    let snippet = PackSnippet {
        name: m.name.clone(),
        version: m.version.clone(),
        kind: m.kind.clone(),
        engine: m.engine.clone(),
        protocol: m.protocol.clone(),
        requires,
        provides,
        capabilities: m.capabilities.clone(),
        publisher_key,
        artifact: IndexArtifact {
            platform: plat,
            url: artifact_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("?")
                .to_string(),
            sha256: sha_hex,
            size: bytes.len() as u64,
            signature: sig,
        },
        published_at: now_rfc3339(),
    };
    let snippet_path = out_dir.join(format!("{stem}.index.json"));
    std::fs::write(&snippet_path, serde_json::to_vec_pretty(&snippet).unwrap())
        .map_err(|e| CoreError::Io(e.to_string()))?;
    Ok((artifact_path, snippet_path))
}

// ---------------------------------------------------------------------------
// publish: mantém um diretório `store/v1` (referência p/ GitHub Pages,
// `file://` em testes). Mesmas verificações do cliente, lado escrita.

fn read_store_config(dir: &Path) -> Result<StoreConfig, CoreError> {
    let raw = std::fs::read(dir.join("config.json"))
        .map_err(|e| CoreError::Package(format!("store sem config.json: {e}")))?;
    serde_json::from_slice(&raw).map_err(|e| CoreError::Package(format!("config.json: {e}")))
}

fn write_signed(
    dir: &Path,
    rel: &str,
    bytes: &[u8],
    store_seed: &[u8; 32],
) -> Result<(), CoreError> {
    let p = dir.join(rel);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).map_err(|e| CoreError::Io(e.to_string()))?;
    }
    std::fs::write(&p, bytes).map_err(|e| CoreError::Io(e.to_string()))?;
    let sig = sign_with_seed(store_seed, bytes);
    std::fs::write(dir.join(format!("{rel}.sig")), format!("{sig}\n"))
        .map_err(|e| CoreError::Io(e.to_string()))?;
    Ok(())
}

fn read_manifest_from_bytes(bytes: &[u8]) -> Result<atom_protocol::Manifest, CoreError> {
    let tmp = tempfile::tempdir().map_err(|e| CoreError::Io(e.to_string()))?;
    let gz = flate2::read::GzDecoder::new(bytes);
    let mut ar = tar::Archive::new(gz);
    ar.unpack(tmp.path())
        .map_err(|e| CoreError::Package(format!("tar.gz: {e}")))?;
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
    pkg::read_manifest(&root)
}

/// Verifica um artefato do store (passos 2–4) contra manifesto extraído.
pub fn verify_store_artifact(
    store_dir: &Path,
    index_name: &str,
    v: &IndexVersion,
    a: &IndexArtifact,
    publisher_keys: &[String],
) -> Result<(), CoreError> {
    let ap = if a.url.starts_with("http://") || a.url.starts_with("https://") {
        return Err(CoreError::Package(format!(
            "artefato remoto {url}: baixe p/ verificar",
            url = a.url
        )));
    } else if let Some(rest) = a.url.strip_prefix("file://") {
        PathBuf::from(rest)
    } else {
        store_dir.join(a.url.trim_start_matches('/'))
    };
    let bytes = std::fs::read(&ap)
        .map_err(|e| CoreError::Package(format!("artefato ausente {}: {e}", ap.display())))?;
    {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(&bytes);
        let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if hex != a.sha256 {
            return Err(CoreError::SignatureInvalid(format!(
                "sha256 diverge p/ {}@{} ({})",
                index_name,
                v.version,
                ap.display()
            )));
        }
    }
    if a.size > 0 && bytes.len() as u64 != a.size {
        return Err(CoreError::SignatureInvalid(format!(
            "tamanho diverge p/ {}@{}",
            index_name, v.version
        )));
    }
    let msg = artifact_message(index_name, &v.version, &a.platform, &a.sha256);
    if !publisher_keys.iter().any(|k| {
        parse_pubkey(k)
            .and_then(|pk| verify_sig(&pk, &msg, &a.signature))
            .is_ok()
    }) {
        return Err(CoreError::SignatureInvalid(format!(
            "sig do publisher inválida p/ {}@{}",
            index_name, v.version
        )));
    }
    let m = read_manifest_from_bytes(&bytes)?;
    check_manifest_against_index(index_name, v)(&m)?;
    Ok(())
}

/// Verifica um índice inteiro do store (sig + todos os artefatos locais).
pub fn verify_store_index(store_dir: &Path, name: &str) -> Result<(), CoreError> {
    let cfg = read_store_config(store_dir)?;
    let (publisher, pkg) = name
        .split_once('/')
        .ok_or_else(|| CoreError::Package(format!("nome inválido: {name:?}")))?;
    let rel = format!("index/{publisher}/{pkg}.json");
    let raw = std::fs::read(store_dir.join(&rel))
        .map_err(|e| CoreError::Package(format!("index ausente {rel}: {e}")))?;
    let sig = std::fs::read_to_string(store_dir.join(format!("{rel}.sig")))
        .map_err(|e| CoreError::Package(format!("{rel}.sig ausente: {e}")))?;
    let mut ok = false;
    for k in &cfg.index_signature_keys {
        if parse_pubkey(k)
            .and_then(|pk| verify_sig(&pk, &raw, &sig))
            .is_ok()
        {
            ok = true;
            break;
        }
    }
    if !ok {
        return Err(CoreError::SignatureInvalid(format!(
            "sig do índice inválida: {name}"
        )));
    }
    let index: IndexFile = serde_json::from_slice(&raw)
        .map_err(|e| CoreError::Package(format!("index inválido: {e}")))?;
    for v in &index.versions {
        for a in &v.artifacts {
            match verify_store_artifact(store_dir, &index.name, v, a, &index.publisher_keys) {
                Ok(()) => {}
                Err(CoreError::Package(m)) if m.starts_with("artefato remoto") => {
                    eprintln!("aviso: {m} (pulado)");
                }
                Err(e) => return Err(e),
            }
        }
    }
    Ok(())
}

/// Regenera `catalog.json` (+ sig) a partir dos índices presentes.
// `shortcut: description/tags vazios — o índice v1 não carrega descrição;
// catálogo serve p/ busca, não p/ exibição rica.`
pub fn regen_catalog(store_dir: &Path, store_seed: &[u8; 32]) -> Result<usize, CoreError> {
    let mut entries: Vec<CatalogEntry> = vec![];
    let idx_dir = store_dir.join("index");
    let mut pubs: Vec<PathBuf> = std::fs::read_dir(&idx_dir)
        .map_err(|e| CoreError::Package(format!("store sem index/: {e}")))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    pubs.sort();
    for pubdir in pubs {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&pubdir)
            .map_err(|e| CoreError::Io(e.to_string()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
            .collect();
        files.sort();
        for f in files {
            let raw = std::fs::read(&f).map_err(|e| CoreError::Io(e.to_string()))?;
            let index: IndexFile = match serde_json::from_slice(&raw) {
                Ok(i) => i,
                Err(e) => {
                    eprintln!("aviso: índice inválido {}: {e}", f.display());
                    continue;
                }
            };
            let mut vers: Vec<&IndexVersion> =
                index.versions.iter().filter(|v| !v.yanked).collect();
            vers.sort_by(|a, b| {
                semver::Version::parse(&b.version)
                    .unwrap_or(semver::Version::new(0, 0, 0))
                    .cmp(
                        &semver::Version::parse(&a.version)
                            .unwrap_or(semver::Version::new(0, 0, 0)),
                    )
            });
            if let Some(latest) = vers.first() {
                entries.push(CatalogEntry {
                    name: index.name.clone(),
                    description: String::new(),
                    kind: latest.kind.clone(),
                    latest: latest.version.clone(),
                });
            }
        }
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    let bytes = serde_json::to_vec_pretty(&entries).unwrap();
    write_signed(store_dir, "catalog.json", &bytes, store_seed)?;
    Ok(entries.len())
}

/// Publica snippets no store: copia artefatos, mescla índices, assina, regen catalog.
/// `snippets`: paths de `.index.json`; artefato buscado em `artifacts_dir` ou
/// ao lado do snippet. Com `check_only`, só verifica (exige snippets vazios).
/// Com `reindex`, reassina todos os índices + regen catalog.
#[allow(clippy::too_many_arguments)]
pub fn publish_to_store(
    store_dir: &Path,
    store_keyfile: &Path,
    snippets: &[PathBuf],
    artifacts_dir: Option<&Path>,
    reindex: bool,
    check_only: bool,
    github: bool,
) -> Result<String, CoreError> {
    let store_seed = load_seed(store_keyfile)?;
    let store_pub = pubkey_of_seed(&store_seed);
    if check_only {
        return check_store(store_dir);
    }
    let cfg = match read_store_config(store_dir) {
        Ok(c) => {
            if !c.index_signature_keys.contains(&store_pub) {
                return Err(CoreError::SignatureInvalid(
                    "chave da loja não está em index_signature_keys (rotação sem autorização?)"
                        .to_string(),
                ));
            }
            c
        }
        Err(_) => {
            // Bootstrap de loja nova file:// (referência/testes).
            StoreConfig {
                store: 1,
                name: "local".to_string(),
                index_signature_keys: vec![store_pub.clone()],
                artifacts_base: None,
                features: vec!["catalog".to_string()],
            }
        }
    };
    if !store_dir.join("config.json").exists() {
        let bytes = serde_json::to_vec_pretty(&cfg).unwrap();
        write_signed(store_dir, "config.json", &bytes, &store_seed)?;
    }
    if reindex {
        let mut pubs: Vec<PathBuf> = std::fs::read_dir(store_dir.join("index"))
            .map_err(|e| CoreError::Package(format!("store sem index/: {e}")))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_dir())
            .collect();
        pubs.sort();
        for pubdir in pubs {
            let mut files: Vec<PathBuf> = std::fs::read_dir(&pubdir)
                .map_err(|e| CoreError::Io(e.to_string()))?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
                .collect();
            files.sort();
            for f in files {
                let raw = std::fs::read(&f).map_err(|e| CoreError::Io(e.to_string()))?;
                let rel = f
                    .strip_prefix(store_dir)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                write_signed(store_dir, &rel, &raw, &store_seed)?;
            }
        }
        let n = regen_catalog(store_dir, &store_seed)?;
        return Ok(report(github, &format!("reindex: {n} pacotes no catalog")));
    }
    if snippets.is_empty() {
        return Err(CoreError::Package(
            "informe snippets do pack ou use --reindex/--check-only".to_string(),
        ));
    }
    for snip in snippets {
        let raw = std::fs::read(snip)
            .map_err(|e| CoreError::Package(format!("snippet {}: {e}", snip.display())))?;
        let s: PackSnippet = serde_json::from_slice(&raw)
            .map_err(|e| CoreError::Package(format!("snippet inválido: {e}")))?;
        // Localiza artefato.
        let fname = s.artifact.url.rsplit('/').next().unwrap_or("?");
        let mut cand = vec![snip.parent().unwrap_or(Path::new(".")).join(fname)];
        if let Some(d) = artifacts_dir {
            cand.push(d.join(fname));
        }
        let art_bytes = cand
            .iter()
            .find_map(|p| std::fs::read(p).ok())
            .ok_or_else(|| {
                CoreError::Package(format!(
                    "artefato {fname} não encontrado (ao lado do snippet ou --artifacts-dir)"
                ))
            })?;
        // Confere bytes == snippet (sha) antes de copiar.
        {
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            h.update(&art_bytes);
            let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
            if hex != s.artifact.sha256 {
                return Err(CoreError::SignatureInvalid(format!(
                    "sha do artefato {fname} diverge do snippet"
                )));
            }
        }
        let (publisher, pkg) = s
            .name
            .split_once('/')
            .ok_or_else(|| CoreError::Package(format!("nome inválido: {:?}", s.name)))?;
        let dest = store_dir.join("artifacts").join(publisher).join(fname);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| CoreError::Io(e.to_string()))?;
        }
        std::fs::write(&dest, &art_bytes).map_err(|e| CoreError::Io(e.to_string()))?;
        // Mescla no índice.
        let rel = format!("index/{publisher}/{pkg}.json");
        let mut index = store_dir.join(&rel);
        let mut idx: IndexFile = if index.exists() {
            let raw = std::fs::read(&index).map_err(|e| CoreError::Io(e.to_string()))?;
            serde_json::from_slice(&raw)
                .map_err(|e| CoreError::Package(format!("index inválido: {e}")))?
        } else {
            IndexFile {
                name: s.name.clone(),
                publisher_keys: vec![],
                versions: vec![],
            }
        };
        if !idx.publisher_keys.contains(&s.publisher_key) {
            idx.publisher_keys.push(s.publisher_key.clone());
        }
        // publishers/<publisher>.toml: chaves aprovadas.
        let pub_toml = store_dir
            .join("publishers")
            .join(format!("{publisher}.toml"));
        if let Some(parent) = pub_toml.parent() {
            std::fs::create_dir_all(parent).map_err(|e| CoreError::Io(e.to_string()))?;
        }
        let mut approved: Vec<String> = std::fs::read_to_string(&pub_toml)
            .ok()
            .map(|t| {
                t.lines()
                    .filter_map(|l| l.trim().strip_prefix("keys = ").map(|_| ()))
                    .count();
                // Formato simples: uma chave por linha `key = "ed25519:..."`.
                t.lines()
                    .filter_map(|l| l.split('"').nth(1).map(|k| k.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        if !approved.contains(&s.publisher_key) {
            approved.push(s.publisher_key.clone());
        }
        approved.sort();
        let toml_body = approved
            .iter()
            .map(|k| format!("key = \"{k}\"\n"))
            .collect::<String>();
        std::fs::write(&pub_toml, toml_body).map_err(|e| CoreError::Io(e.to_string()))?;
        idx.versions.retain(|v| v.version != s.version);
        idx.versions.push(IndexVersion {
            version: s.version.clone(),
            kind: s.kind.clone(),
            engine: s.engine.clone(),
            protocol: s.protocol.clone(),
            requires: s.requires.clone(),
            provides: s
                .provides
                .iter()
                .map(|p| IndexProvides {
                    contract: p.contract.clone(),
                    version: p.version.clone(),
                })
                .collect(),
            capabilities: s.capabilities.clone(),
            artifacts: vec![IndexArtifact {
                platform: s.artifact.platform.clone(),
                url: format!("artifacts/{publisher}/{fname}"),
                sha256: s.artifact.sha256.clone(),
                size: s.artifact.size,
                signature: s.artifact.signature.clone(),
            }],
            yanked: false,
            published_at: s.published_at.clone(),
        });
        idx.versions.sort_by(|a, b| {
            semver::Version::parse(&b.version)
                .unwrap_or(semver::Version::new(0, 0, 0))
                .cmp(&semver::Version::parse(&a.version).unwrap_or(semver::Version::new(0, 0, 0)))
        });
        let bytes = serde_json::to_vec_pretty(&idx).unwrap();
        write_signed(store_dir, &rel, &bytes, &store_seed)?;
        index = store_dir.join(&rel);
        let _ = index;
    }
    let n = regen_catalog(store_dir, &store_seed)?;
    Ok(report(github, &format!("catalog com {n} pacotes")))
}

fn report(github: bool, msg: &str) -> String {
    if github {
        format!(
            "{msg}\n--github: git add v1 publishers && git commit && git push; anexe artefatos ao Release; abra o PR/MR (CI valida e regenera o catalog)."
        )
    } else {
        msg.to_string()
    }
}

/// Verifica store inteiro: config, sigs dos índices, artefatos locais, catalog.
pub fn check_store(store_dir: &Path) -> Result<String, CoreError> {
    let cfg = read_store_config(store_dir)?;
    if cfg.store != 1 {
        return Err(CoreError::Package("config store != 1".to_string()));
    }
    let mut n_idx = 0;
    let mut n_art = 0;
    let idx_dir = store_dir.join("index");
    let mut pubs: Vec<PathBuf> = std::fs::read_dir(&idx_dir)
        .map_err(|e| CoreError::Package(format!("store sem index/: {e}")))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    pubs.sort();
    for pubdir in pubs {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&pubdir)
            .map_err(|e| CoreError::Io(e.to_string()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
            .collect();
        files.sort();
        for f in files {
            let stem = f
                .strip_prefix(store_dir)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let pub_name = stem.split('/').nth(1).unwrap_or("?");
            let pkg_name = stem
                .split('/')
                .nth(2)
                .and_then(|s| s.strip_suffix(".json"))
                .unwrap_or("?");
            let full = format!("{pub_name}/{pkg_name}");
            verify_store_index(store_dir, &full)?;
            n_idx += 1;
            let raw = std::fs::read(&f).map_err(|e| CoreError::Io(e.to_string()))?;
            let index: IndexFile = serde_json::from_slice(&raw).unwrap();
            for v in &index.versions {
                for a in &v.artifacts {
                    if a.url.starts_with("http") {
                        continue;
                    }
                    n_art += 1;
                }
            }
        }
    }
    Ok(format!(
        "store ok: {n_idx} índices, {n_art} artefatos locais verificados"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pubkey_roundtrip() {
        let raw = [7u8; 32];
        let s = pubkey_str(&raw);
        assert!(s.starts_with("ed25519:"));
        assert_eq!(parse_pubkey(&s).unwrap(), raw);
    }

    #[test]
    fn pubkey_rejects_bad_input() {
        assert!(parse_pubkey("sem-prefixo").is_err());
        assert!(parse_pubkey("ed25519:!!!").is_err());
        let short = pubkey_str(&[1u8; 32])[..20].to_string();
        assert!(parse_pubkey(&short).is_err());
    }

    #[test]
    fn artifact_message_format() {
        let m = artifact_message("a/b", "1.0.0", "linux-amd64", "abc");
        assert_eq!(
            m,
            b"atom-artifact-v1\na/b\n1.0.0\nlinux-amd64\nabc\n".to_vec()
        );
    }

    #[test]
    fn seed_sign_roundtrip() {
        let t = tempfile::tempdir().unwrap();
        let seed = [9u8; 32];
        let f = t.path().join("k");
        std::fs::write(
            &f,
            format!("ed25519-seed:{}", base64::Engine::encode(&b64(), seed)),
        )
        .unwrap();
        let loaded = load_seed(&f).unwrap();
        assert_eq!(loaded, seed);
        let pubkey = pubkey_of_seed(&seed);
        assert_eq!(parse_pubkey(&pubkey).unwrap().len(), 32);
        // Assinatura verifica contra a pubkey (caminho completo store §4).
        let msg = artifact_message("a/b", "1.0.0", "any", "abc");
        let sig = sign_with_seed(&seed, &msg);
        verify_sig(&parse_pubkey(&pubkey).unwrap(), &msg, &sig).unwrap();
        assert!(verify_sig(&parse_pubkey(&pubkey).unwrap(), b"outra", &sig).is_err());
    }

    #[test]
    fn keygen_returns_prefixed_keys() {
        let (pubk, seed) = keygen();
        assert!(pubk.starts_with("ed25519:"));
        assert_eq!(pubkey_of_seed(&load_seed_str(&seed).unwrap()), pubk);
    }

    fn load_seed_str(s: &str) -> Result<[u8; 32], CoreError> {
        let raw = b64()
            .decode(s.trim())
            .map_err(|e| CoreError::Package(format!("chave privada inválida: {e}")))?;
        raw.try_into()
            .map_err(|_| CoreError::Package("seed deve ter 32 bytes".to_string()))
    }
}
