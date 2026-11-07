//! Cliente `store/v2` (TUF, spec `docs/specs/store-v2.md`, v1.1, ADR-024).
//!
//! Verificação própria com `ed25519-dalek` + `sha2` (já permitidos pelo
//! AGENTS.md, sem dependência C no build padrão): `tough` foi avaliado e
//! rejeitado para o cliente (árvore de dependências com risco de C; a
//! verificação cabe em poucas linhas sobre os envelopes canônicos). A
//! publicação usa o mesmo assinador (CI de referência). `store/v1`
//! continua intocado (fallback, spec store-v2 §6).

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::bus::Core;
use crate::error::CoreError;
use crate::pkg;
use crate::profile;
use crate::store::{
    CatalogEntry, IndexArtifact, IndexFile, IndexProvides, IndexRequires, IndexVersion, Source,
    check_manifest_against_index, parse_req, pick,
};

/// `true` se a base é `store/v2` (convenção: URL termina com `/v2/`).
pub fn is_v2_source(url: &str) -> bool {
    url.trim_end().trim_end_matches('/').ends_with("/v2")
}

/// `<cache>/tuf/<source>/` (nome sanitizado; mesma regra de `store::cache_dir`).
pub fn tuf_cache_dir(core: &Core, source_name: &str) -> PathBuf {
    tuf_cache_dir_at(&core.atom_paths.cache, source_name)
}

fn tuf_cache_dir_at(cache: &Path, source_name: &str) -> PathBuf {
    let safe: String = source_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    cache.join("tuf").join(safe)
}

/// Confere o sha256 hex do `1.root.json` contra o esperado e grava o trust
/// (`trusted-root.json` + `1.root.json`) com escrita atômica.
/// Erro = `Package` (TOFU proibido, spec store-v2 §3).
pub async fn bootstrap_root(
    core: &Core,
    source: &Source,
    root_bytes: &[u8],
    expected_sha256: &str,
) -> Result<(), CoreError> {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(root_bytes);
    let actual: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if !actual.eq_ignore_ascii_case(expected_sha256.trim()) {
        return Err(CoreError::Package(format!(
            "sha256 do 1.root.json diverge (esperado {expected_sha256}, obtido {actual})"
        )));
    }
    // Raiz precisa ser TUF válida antes de confiar.
    let root: Root = serde_json::from_slice(root_bytes)
        .map_err(|e| CoreError::Package(format!("1.root.json inválido: {e}")))?;
    root.check_self_signed()?;
    let dir = tuf_cache_dir(core, &source.name);
    std::fs::create_dir_all(&dir).map_err(|e| CoreError::Io(e.to_string()))?;
    profile::atomic_write(&dir.join("trusted-root.json"), root_bytes)?;
    profile::atomic_write(&dir.join("1.root.json"), root_bytes)?;
    let versions = serde_json::json!({"root": root.signed.version});
    profile::atomic_write(
        &dir.join("versions.json"),
        &serde_json::to_vec(&versions).unwrap_or_default(),
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Envelopes TUF (assinatura sobre bytes canônicos do `signed`).

/// Envelope `{"signatures": [{"keyid", "sig"}], "signed": T}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope<T> {
    pub signatures: Vec<Signature>,
    pub signed: T,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signature {
    pub keyid: String,
    pub sig: String,
}

/// Chave TUF (`ed25519`, `public` = hex de 32 bytes).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TufKey {
    #[serde(default)]
    pub keytype: String,
    pub scheme: String,
    pub keyval: KeyVal,
    #[serde(default)]
    pub expires: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyVal {
    pub public: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Role {
    pub keyids: Vec<String>,
    pub threshold: u32,
}

/// Bytes canônicos do `signed`: JSON com chaves ordenadas, sem espaços.
fn canonical(v: &serde_json::Value) -> Result<Vec<u8>, CoreError> {
    fn write(v: &serde_json::Value, out: &mut Vec<u8>) -> Result<(), CoreError> {
        match v {
            serde_json::Value::Null => out.extend_from_slice(b"null"),
            serde_json::Value::Bool(b) => {
                out.extend_from_slice(if *b { b"true" } else { b"false" })
            }
            serde_json::Value::Number(n) => out.extend_from_slice(n.to_string().as_bytes()),
            serde_json::Value::String(s) => {
                let e = serde_json::to_string(s)
                    .map_err(|e| CoreError::Package(format!("canônico: {e}")))?;
                out.extend_from_slice(e.as_bytes());
            }
            serde_json::Value::Array(a) => {
                out.push(b'[');
                for (i, e) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(b',');
                    }
                    write(e, out)?;
                }
                out.push(b']');
            }
            serde_json::Value::Object(m) => {
                out.push(b'{');
                let mut keys: Vec<&String> = m.keys().collect();
                keys.sort();
                for (i, k) in keys.iter().enumerate() {
                    if i > 0 {
                        out.push(b',');
                    }
                    let e = serde_json::to_string(k)
                        .map_err(|e| CoreError::Package(format!("canônico: {e}")))?;
                    out.extend_from_slice(e.as_bytes());
                    out.push(b':');
                    write(&m[*k], out)?;
                }
                out.push(b'}');
            }
        }
        Ok(())
    }
    let v = serde_json::to_value(v).map_err(|e| CoreError::Package(format!("canônico: {e}")))?;
    let mut out = vec![];
    write(&v, &mut out)?;
    Ok(out)
}

fn keyid_of(pub_hex: &str) -> Result<String, CoreError> {
    use sha2::{Digest, Sha256};
    let raw = hex_decode(pub_hex)?;
    if raw.len() != 32 {
        return Err(CoreError::Package(
            "chave TUF deve ter 32 bytes".to_string(),
        ));
    }
    let mut h = Sha256::new();
    h.update(&raw);
    Ok(h.finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()[..8]
        .to_string())
}

fn hex_decode(s: &str) -> Result<Vec<u8>, CoreError> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) {
        return Err(CoreError::Package("hex ímpar".to_string()));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16)
                .map_err(|_| CoreError::Package("hex inválido".to_string()))
        })
        .collect()
}

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

fn verify_role(
    keys: &HashMap<String, TufKey>,
    role: &Role,
    signed_value: &serde_json::Value,
    signatures: &[Signature],
    what: &str,
) -> Result<(), CoreError> {
    let msg = canonical(signed_value)?;
    let mut valid: HashSet<String> = HashSet::new();
    for sig in signatures {
        let Some(k) = keys.get(&sig.keyid) else {
            continue;
        };
        if k.scheme != "ed25519" {
            continue;
        }
        let raw = hex_decode(&k.keyval.public)?;
        let arr: [u8; 32] = raw
            .try_into()
            .map_err(|_| CoreError::Package(format!("chave inválida em {what}")))?;
        let vk = ed25519_dalek::VerifyingKey::from_bytes(&arr)
            .map_err(|e| CoreError::Package(format!("chave inválida em {what}: {e}")))?;
        let sig_raw = b64().decode(sig.sig.trim()).map_err(|e| {
            CoreError::SignatureInvalid(format!("tuf threshold: {what}: sig base64: {e}"))
        })?;
        let s = ed25519_dalek::Signature::from_slice(&sig_raw)
            .map_err(|e| CoreError::SignatureInvalid(format!("tuf threshold: {what}: sig: {e}")))?;
        use ed25519_dalek::Verifier;
        if vk.verify(&msg, &s).is_ok() {
            valid.insert(sig.keyid.clone());
        }
    }
    // Só contam chaves autorizadas no papel.
    let authorized: usize = valid.iter().filter(|id| role.keyids.contains(id)).count();
    if authorized < role.threshold.max(1) as usize {
        return Err(CoreError::SignatureInvalid(format!(
            "tuf threshold: {what}: {authorized} de {} (limiar {})",
            role.keyids.len(),
            role.threshold.max(1),
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Papéis (spec store-v2 §2).

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RootSigned {
    #[serde(rename = "_type")]
    pub typ: String,
    pub version: u32,
    pub expires: String,
    pub keys: HashMap<String, TufKey>,
    pub roles: HashMap<String, Role>,
    #[serde(default)]
    pub consistent_snapshot: bool,
}

pub type Root = Envelope<RootSigned>;

impl RootSigned {
    fn role(&self, name: &str) -> Result<&Role, CoreError> {
        self.roles
            .get(name)
            .ok_or_else(|| CoreError::Package(format!("root sem papel `{name}`")))
    }

    fn keyid_check(&self) -> Result<(), CoreError> {
        for (id, k) in &self.keys {
            if k.scheme != "ed25519" {
                return Err(CoreError::Package(format!(
                    "chave {id}: scheme {:?} (só ed25519)",
                    k.scheme
                )));
            }
            if *id != keyid_of(&k.keyval.public)? {
                return Err(CoreError::Package(format!(
                    "keyid {id} não confere com a chave"
                )));
            }
        }
        Ok(())
    }
}

impl Root {
    fn check_self_signed(&self) -> Result<(), CoreError> {
        if self.signed.typ != "root" {
            return Err(CoreError::Package("1.root.json não é root".to_string()));
        }
        self.signed.keyid_check()?;
        check_expiry(&self.signed.expires, "root")?;
        let role = self.signed.role("root")?.clone();
        let v = serde_json::to_value(&self.signed)
            .map_err(|e| CoreError::Package(format!("root: {e}")))?;
        verify_role(&self.signed.keys, &role, &v, &self.signatures, "root")?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimestampSigned {
    #[serde(rename = "_type")]
    pub typ: String,
    pub version: u32,
    pub expires: String,
    pub meta: HashMap<String, MetaFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaFile {
    pub version: u32,
    #[serde(default)]
    pub length: Option<u64>,
    #[serde(default)]
    pub hashes: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotSigned {
    #[serde(rename = "_type")]
    pub typ: String,
    pub version: u32,
    pub expires: String,
    pub meta: HashMap<String, MetaFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetsSigned {
    #[serde(rename = "_type")]
    pub typ: String,
    pub version: u32,
    pub expires: String,
    pub targets: HashMap<String, TargetFile>,
    #[serde(default)]
    pub delegations: Option<Delegations>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetFile {
    pub length: u64,
    pub hashes: HashMap<String, String>,
    #[serde(default)]
    pub custom: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Delegations {
    pub keys: HashMap<String, TufKey>,
    pub roles: Vec<DelegatedRole>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegatedRole {
    pub name: String,
    pub keyids: Vec<String>,
    pub threshold: u32,
    pub paths: Vec<String>,
    #[serde(default)]
    pub terminating: bool,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Dias desde a era civil (1970-01-01) — Howard Hinnant, sem chrono.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Epoch de `AAAA-MM-DDTHH:MM:SSZ` estrito; `None` se ilegível.
fn rfc3339_to_epoch(s: &str) -> Option<i64> {
    // spec: [ST2-2] formato exato com `Z`, sem fração/offset.
    let b = s.as_bytes();
    if b.len() != 20 {
        return None;
    }
    if b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'Z'
    {
        return None;
    }
    let num = |lo: usize, hi: usize| -> Option<i64> {
        let t = &s[lo..hi];
        if !t.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        t.parse::<i64>().ok()
    };
    let y = num(0, 4)?;
    let mo = num(5, 7)?;
    let d = num(8, 10)?;
    let h = num(11, 13)?;
    let mi = num(14, 16)?;
    let se = num(17, 19)?;
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    // Valida o dia contra o mês (bissexto incluído).
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let dim = match mo {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        _ => return None,
    };
    if d > dim {
        return None;
    }
    Some(days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + se)
}

fn check_expiry(expires: &str, what: &str) -> Result<(), CoreError> {
    // spec: [ST2-2] comparação real com o relógio. Sentinela `"9999999999"`
    // (só p/ testes locais) = válida; epoch numérico em segundos (só
    // dígitos); RFC 3339 estrito `AAAA-MM-DDTHH:MM:SSZ` via days-from-civil
    // (sem chrono); ilegível = expirado (fail-safe).
    if expires == "9999999999" {
        return Ok(());
    }
    let now = now_secs() as i64;
    let expired = if !expires.is_empty() && expires.bytes().all(|c| c.is_ascii_digit()) {
        expires.parse::<i64>().map(|e| e < now).unwrap_or(true)
    } else if let Some(e) = rfc3339_to_epoch(expires) {
        e < now
    } else {
        true
    };
    if expired {
        return Err(CoreError::SignatureInvalid(format!(
            "tuf expired: {what} expirou em {expires}"
        )));
    }
    Ok(())
}

fn check_version_monotonic(role: &str, next: u32, last: u32) -> Result<(), CoreError> {
    if next < last {
        return Err(CoreError::SignatureInvalid(format!(
            "tuf rollback: {role} versão {next} < {last} vista"
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Cache de metadados verificados (`<cache>/tuf/<source>/`, spec §5).

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SeenVersions {
    #[serde(default)]
    root: u32,
    #[serde(default)]
    timestamp: u32,
    #[serde(default)]
    snapshot: u32,
    #[serde(default)]
    targets: u32,
    #[serde(default)]
    delegated: HashMap<String, u32>,
}

fn seen_path(dir: &Path) -> PathBuf {
    dir.join("versions.json")
}

fn load_seen(dir: &Path) -> SeenVersions {
    // `trusted-root.json` carrega a versão inicial do root (bootstrap).
    let mut seen: SeenVersions = std::fs::read(seen_path(dir))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    if seen.root == 0
        && let Ok(b) = std::fs::read(dir.join("trusted-root.json"))
        && let Ok(r) = serde_json::from_slice::<Root>(&b)
    {
        seen.root = r.signed.version;
    }
    seen
}

fn save_seen(dir: &Path, seen: &SeenVersions) {
    if let Some(p) = seen_path(dir).parent() {
        let _ = std::fs::create_dir_all(p);
    }
    if let Ok(b) = serde_json::to_vec(seen) {
        let _ = profile::atomic_write(&seen_path(dir), &b);
    }
}

// ---------------------------------------------------------------------------
// Fetch (`file://` direto; http com cache ETag simples).

fn is_file_base(base: &str) -> bool {
    base.starts_with("file://")
}

async fn fetch_rel(core: &Core, source: &Source, rel: &str) -> Result<Vec<u8>, CoreError> {
    if is_file_base(&source.base) {
        let base = source
            .base
            .strip_prefix("file://")
            .ok_or_else(|| CoreError::Package(format!("base inválida: {}", source.base)))?;
        let p = PathBuf::from(base).join(rel);
        return std::fs::read(&p)
            .map_err(|e| CoreError::Package(format!("store {}: {rel}: {e}", source.name)));
    }
    let url = format!("{}{}", source.base, rel);
    let dir = tuf_cache_dir(core, &source.name).join("http");
    std::fs::create_dir_all(&dir).map_err(|e| CoreError::Io(e.to_string()))?;
    let cache_file = dir.join(rel.replace('/', "__"));
    let meta_file = dir.join(format!("{}.etag", rel.replace('/', "__")));
    let etag: Option<String> = std::fs::read_to_string(&meta_file)
        .ok()
        .map(|s| s.trim().to_string());
    let client = reqwest::Client::new();
    let mut req = client.get(&url);
    if let Some(e) = &etag {
        req = req.header(reqwest::header::IF_NONE_MATCH, e.clone());
    }
    if let Ok(Some(secret)) = core.secrets.get(&format!("store:{}", source.name)) {
        req = req.bearer_auth(secret);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| CoreError::Package(format!("GET {url}: {e}")))?;
    if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
        return std::fs::read(&cache_file).map_err(|e| CoreError::Io(e.to_string()));
    }
    if !resp.status().is_success() {
        return Err(CoreError::Package(format!(
            "GET {url}: http {}",
            resp.status()
        )));
    }
    let etag_new = resp
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| CoreError::Package(format!("GET {url}: {e}")))?
        .to_vec();
    let _ = std::fs::write(&cache_file, &bytes);
    if let Some(e) = etag_new {
        let _ = std::fs::write(&meta_file, e);
    }
    Ok(bytes)
}

// ---------------------------------------------------------------------------
// Workflow de atualização (`[ST2-1]`): root → timestamp → snapshot → targets.

/// Metadados verificados e prontos para resolver pacotes.
/// `targets` de topo carrega as delegações (`spec store-v2 §2`); o índice
/// servível vem dos papéis delegados (`delegated`).
#[allow(dead_code)]
pub struct VerifiedRepo {
    source: Source,
    targets: Envelope<TargetsSigned>,
    delegated: HashMap<String, Envelope<TargetsSigned>>,
}

fn load_trusted_root(dir: &Path) -> Result<Root, CoreError> {
    // TOFU proibido: sem `trusted-root.json` (via `bootstrap_root`), recusa.
    let b = std::fs::read(dir.join("trusted-root.json")).map_err(|_| {
        CoreError::Package(
            "source v2 sem root confiável (`atom source add/migrate --root`)".to_string(),
        )
    })?;
    let root: Root = serde_json::from_slice(&b)
        .map_err(|e| CoreError::Package(format!("trusted root inválido: {e}")))?;
    root.check_self_signed()?;
    Ok(root)
}

fn verify_envelope<T>(
    keys: &HashMap<String, TufKey>,
    role: &Role,
    raw: &[u8],
    what: &str,
) -> Result<Envelope<T>, CoreError>
where
    T: serde::de::DeserializeOwned + Serialize,
{
    let env: Envelope<T> = serde_json::from_slice(raw)
        .map_err(|e| CoreError::Package(format!("{what} inválido: {e}")))?;
    let v = serde_json::to_value(&env.signed)
        .map_err(|e| CoreError::Package(format!("{what}: {e}")))?;
    let typ = v.get("_type").and_then(|t| t.as_str()).unwrap_or("");
    if typ != what && !(what == "delegated" && typ == "targets") {
        return Err(CoreError::Package(format!("{what}: _type {typ:?}")));
    }
    let expires = v.get("expires").and_then(|e| e.as_str()).unwrap_or("");
    check_expiry(expires, what)?;
    verify_role(keys, role, &v, &env.signatures, what)?;
    Ok(env)
}

fn verify_meta_against(meta: &MetaFile, bytes: &[u8], rel: &str) -> Result<(), CoreError> {
    if let Some(len) = meta.length
        && bytes.len() as u64 != len
    {
        return Err(CoreError::SignatureInvalid(format!(
            "tamanho de {rel} diverge"
        )));
    }
    if let Some(h) = &meta.hashes
        && let Some(expected) = h.get("sha256")
    {
        use sha2::{Digest, Sha256};
        let mut sh = Sha256::new();
        sh.update(bytes);
        let actual: String = sh.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if actual != *expected {
            return Err(CoreError::SignatureInvalid(format!(
                "sha256 de {rel} diverge"
            )));
        }
    }
    Ok(())
}

async fn refresh_repo(core: &Core, source: &Source) -> Result<VerifiedRepo, CoreError> {
    let dir = tuf_cache_dir(core, &source.name);
    let trusted = load_trusted_root(&dir)?;
    let mut seen = load_seen(&dir);

    // --- Root: rotação pela cadeia (`N.root.json`), versão ≥ última vista.
    let mut root = trusted;
    root.signed.keyid_check()?;
    let mut n = root.signed.version + 1;
    loop {
        let rel = format!("metadata/{n}.root.json");
        let raw = match fetch_rel(core, source, &rel).await {
            Ok(b) => b,
            Err(_) => break,
        };
        let next: Root = serde_json::from_slice(&raw)
            .map_err(|e| CoreError::Package(format!("{rel} inválido: {e}")))?;
        next.check_self_signed()?;
        // Novo root precisa ser assinado pelo root anterior (cadeia).
        {
            let v = serde_json::to_value(&next.signed)
                .map_err(|e| CoreError::Package(format!("{rel}: {e}")))?;
            let prev_role = root.signed.role("root")?.clone();
            verify_role(&root.signed.keys, &prev_role, &v, &next.signatures, "root")?;
        }
        check_version_monotonic(
            "root",
            next.signed.version,
            seen.root.max(root.signed.version),
        )?;
        root = next;
        n = root.signed.version + 1;
    }
    if root.signed.version < seen.root {
        return Err(CoreError::SignatureInvalid(format!(
            "tuf rollback: root versão {} < {} vista",
            root.signed.version, seen.root
        )));
    }
    check_expiry(&root.signed.expires, "root")?;
    seen.root = seen.root.max(root.signed.version);
    let keys = root.signed.keys.clone();

    // --- Timestamp (assinado pelo papel `timestamp`).
    let ts_role = root.signed.role("timestamp")?.clone();
    let ts_raw = fetch_rel(core, source, "metadata/timestamp.json").await?;
    let ts: Envelope<TimestampSigned> = verify_envelope(&keys, &ts_role, &ts_raw, "timestamp")?;
    check_version_monotonic("timestamp", ts.signed.version, seen.timestamp)?;
    seen.timestamp = seen.timestamp.max(ts.signed.version);
    let snap_meta = ts
        .signed
        .meta
        .get("snapshot.json")
        .ok_or_else(|| CoreError::Package("timestamp sem snapshot.json".to_string()))?;

    // --- Snapshot (assinado pelo papel `snapshot`), hash/tamanho vs timestamp.
    let snap_role = root.signed.role("snapshot")?.clone();
    // `consistent_snapshot`: `<N>.snapshot.json`; senão `snapshot.json`.
    let snap_name = if root.signed.consistent_snapshot {
        format!("{}.snapshot.json", snap_meta.version)
    } else {
        "snapshot.json".to_string()
    };
    let snap_raw = fetch_rel(core, source, &format!("metadata/{snap_name}")).await?;
    verify_meta_against(snap_meta, &snap_raw, &snap_name)?;
    let snap: Envelope<SnapshotSigned> = verify_envelope(&keys, &snap_role, &snap_raw, "snapshot")?;
    check_version_monotonic("snapshot", snap.signed.version, seen.snapshot)?;
    if snap.signed.version != snap_meta.version {
        return Err(CoreError::SignatureInvalid(format!(
            "tuf rollback: snapshot versão {} != {} do timestamp",
            snap.signed.version, snap_meta.version
        )));
    }
    seen.snapshot = seen.snapshot.max(snap.signed.version);

    // --- Targets (assinado pelo papel `targets`), versão vs snapshot.
    let tg_role = root.signed.role("targets")?.clone();
    let tg_meta = snap
        .signed
        .meta
        .get("targets.json")
        .ok_or_else(|| CoreError::Package("snapshot sem targets.json".to_string()))?;
    let tg_name = if root.signed.consistent_snapshot {
        format!("{}.targets.json", tg_meta.version)
    } else {
        "targets.json".to_string()
    };
    let tg_raw = fetch_rel(core, source, &format!("metadata/{tg_name}")).await?;
    verify_meta_against(tg_meta, &tg_raw, &tg_name)?;
    let targets: Envelope<TargetsSigned> = verify_envelope(&keys, &tg_role, &tg_raw, "targets")?;
    check_version_monotonic("targets", targets.signed.version, seen.targets)?;
    if targets.signed.version != tg_meta.version {
        return Err(CoreError::SignatureInvalid(format!(
            "tuf rollback: targets versão {} != {} do snapshot",
            targets.signed.version, tg_meta.version
        )));
    }
    seen.targets = seen.targets.max(targets.signed.version);

    // --- Delegados por publisher (papel `<publisher>`, `[ST2-5]` prefixo).
    let mut delegated = HashMap::new();
    if let Some(dels) = &targets.signed.delegations {
        for role in &dels.roles {
            // Só delegações `paths: ["<publisher>/*", ...]` simples.
            let Some(publisher) = role.name.strip_prefix("publisher:") else {
                continue;
            };
            let _ = publisher;
            let meta_key = format!("{}.json", role_file(&role.name));
            let Some(meta) = snap.signed.meta.get(&meta_key) else {
                continue;
            };
            let rel_name = if root.signed.consistent_snapshot {
                format!("{}.{}.json", meta.version, role_file(&role.name))
            } else {
                format!("{}.json", role_file(&role.name))
            };
            let raw = fetch_rel(core, source, &format!("metadata/{rel_name}")).await?;
            verify_meta_against(meta, &raw, &rel_name)?;
            let role_keys: HashMap<String, TufKey> = dels
                .keys
                .iter()
                .filter(|(id, _)| role.keyids.contains(id))
                .map(|(id, k)| (id.clone(), k.clone()))
                .collect();
            let drole = Role {
                keyids: role.keyids.clone(),
                threshold: role.threshold,
            };
            let env: Envelope<TargetsSigned> =
                verify_envelope(&role_keys, &drole, &raw, "delegated")?;
            // `_type` de delegado é `targets`; aceita ambos os rótulos.
            let last = seen.delegated.get(&role.name).copied().unwrap_or(0);
            check_version_monotonic(&role.name, env.signed.version, last)?;
            if env.signed.version != meta.version {
                return Err(CoreError::SignatureInvalid(format!(
                    "tuf rollback: {} versão {} != {} do snapshot",
                    role.name, env.signed.version, meta.version
                )));
            }
            // `[ST2-5]`: papel só pode assinar sob o próprio prefixo.
            for path in env.signed.targets.keys() {
                if !role_allows(&role.paths, path) {
                    return Err(CoreError::SignatureInvalid(format!(
                        "tuf threshold: {} assinou fora do prefixo permitido ({path})",
                        role.name
                    )));
                }
            }
            seen.delegated.insert(
                role.name.clone(),
                seen.delegated
                    .get(&role.name)
                    .copied()
                    .unwrap_or(0)
                    .max(env.signed.version),
            );
            delegated.insert(role.name.clone(), env);
        }
    }

    // Persiste versões vistas + metadados verificados (spec §5).
    save_seen(&dir, &seen);
    let _ = std::fs::create_dir_all(dir.join("verified"));
    let _ = profile::atomic_write(&dir.join("verified/timestamp.json"), &ts_raw);
    let _ = profile::atomic_write(&dir.join("verified/snapshot.json"), &snap_raw);
    let _ = profile::atomic_write(&dir.join("verified/targets.json"), &tg_raw);

    Ok(VerifiedRepo {
        source: source.clone(),
        targets,
        delegated,
    })
}

fn role_allows(patterns: &[String], path: &str) -> bool {
    // Suporta `prefixo/*` (prefixo literal) e caminho exato.
    for p in patterns {
        if let Some(prefix) = p.strip_suffix("/*") {
            if path == prefix || path.starts_with(&format!("{prefix}/")) {
                return true;
            }
        } else if path == *p {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Resolução e instalação sobre o repo verificado.

/// Índice `index/<publisher>/<nome>.json` reconstruído do TUF:
/// `N.<publisher>.json` (delegado) carrega o índice do publisher.
fn tuf_index_for(
    repo: &VerifiedRepo,
    name: &str,
) -> Result<(IndexFile, IndexVersion, Vec<IndexArtifactMeta>), CoreError> {
    let (publisher, _pkg) = name
        .split_once('/')
        .ok_or_else(|| CoreError::Package(format!("nome inválido: {name:?}")))?;
    let role_name = format!("publisher:{publisher}");
    let env = repo
        .delegated
        .get(&role_name)
        .ok_or_else(|| CoreError::NoProvider(format!("{name} (sem delegação p/ {publisher})")))?;
    // Alvo `index/<publisher>/<nome>.json` dentro do papel delegado.
    let target_path = format!("index/{name}.json");
    let tf = env.signed.targets.get(&target_path).ok_or_else(|| {
        CoreError::NoProvider(format!("{name} (alvo ausente no papel {role_name})"))
    })?;
    let custom = tf.custom.clone().unwrap_or(serde_json::Value::Null);
    let index: IndexFile = serde_json::from_value(custom)
        .map_err(|e| CoreError::Package(format!("índice TUF de {name} inválido: {e}")))?;
    if index.name != name {
        return Err(CoreError::Package(format!(
            "índice respondeu {:?}, esperado {name:?}",
            index.name
        )));
    }
    // Artefatos do índice: hash/tamanho vêm do papel delegado (`[ST2-3]`);
    // `custom.targets` do delegado carrega os hashes por caminho relativo.
    let mut metas = vec![];
    for v in &index.versions {
        for a in &v.artifacts {
            let rel = artifact_target_path(name, &v.version, a);
            let Some(t) = env.signed.targets.get(&rel) else {
                return Err(CoreError::SignatureInvalid(format!(
                    "artefato {rel} ausente no papel {role_name}"
                )));
            };
            metas.push(IndexArtifactMeta {
                version: v.version.clone(),
                platform: a.platform.clone(),
                url: a.url.clone(),
                sha256: t.hashes.get("sha256").cloned().unwrap_or_default(),
                size: t.length,
            });
        }
    }
    // Versão do índice: usa o próprio IndexFile (completo).
    let latest = index
        .versions
        .first()
        .cloned()
        .ok_or_else(|| CoreError::NoProvider(format!("{name} (índice vazio)")))?;
    Ok((index, latest, metas))
}

#[derive(Debug, Clone)]
struct IndexArtifactMeta {
    version: String,
    platform: String,
    url: String,
    sha256: String,
    size: u64,
}

fn artifact_target_path(name: &str, version: &str, a: &IndexArtifact) -> String {
    // Layout v2: `<publisher>/<nome>/<versão>/<arquivo>`; URL relativa do
    // índice resolve contra esse caminho (externa em `custom.url` não entra).
    let file = a.url.rsplit('/').next().unwrap_or(&a.url).to_string();
    format!("{name}/{version}/{file}")
}

/// Baixa, verifica (`[ST2-3]` hash+tamanho vs papel delegado, `[ST2-4]`
/// manifesto) e instala. Retorna o id instalado.
async fn install_pick_v2(
    core: &mut Core,
    repo: &VerifiedRepo,
    index: &IndexFile,
    v: &IndexVersion,
    meta: &IndexArtifactMeta,
    requested: &str,
) -> Result<String, CoreError> {
    let url = artifact_url_v2(&repo.source.base, meta);
    let bytes = download_v2(&url).await?;
    // `[ST2-3]`: tamanho e sha256 conferem com o papel delegado.
    if bytes.len() as u64 != meta.size {
        return Err(CoreError::SignatureInvalid(format!(
            "tamanho de {}@{} diverge",
            index.name, v.version
        )));
    }
    {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(&bytes);
        let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if hex != meta.sha256 {
            return Err(CoreError::SignatureInvalid(format!(
                "sha256 de {}@{} diverge",
                index.name, v.version
            )));
        }
    }
    // `[ST2-4]`: `atom.toml` interno confere com o índice.
    let check = check_manifest_against_index(&index.name, v);
    let id = pkg::install_bytes(
        &core.paths,
        &core.data_store,
        &bytes,
        repo.source.name.clone(),
        &meta.platform,
        requested,
        Some(&check),
    )?;
    Ok(id)
}

fn artifact_url_v2(base: &str, meta: &IndexArtifactMeta) -> String {
    if meta.url.starts_with("http://")
        || meta.url.starts_with("https://")
        || meta.url.starts_with("file://")
    {
        return meta.url.clone();
    }
    format!("{}targets/{}", base, meta.url.trim_start_matches('/'))
}

async fn download_v2(url: &str) -> Result<Vec<u8>, CoreError> {
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

/// Resolve + instala pacote e dependências (`requires.packages`) via v2.
/// Erros de verificação → `-32031` com `details.reason` (`[ST2-2]`).
pub async fn install_package(
    core: &mut Core,
    name: &str,
    req: Option<&str>,
    source_only: Option<&str>,
    force_refresh: bool,
) -> Result<String, CoreError> {
    let req_parsed = parse_req(req)?;
    let requested = req.unwrap_or("*");
    let sources: Vec<Source> = crate::store::profile_sources(core)
        .into_iter()
        .filter(|s| is_v2_source(&s.base))
        .filter(|s| source_only.is_none_or(|o| s.name == o))
        .collect();
    if sources.is_empty() {
        return Err(CoreError::NoProvider(format!(
            "{name}@{req_parsed} (nenhuma source v2)"
        )));
    }
    // yanked honrado no `pick` (mesma regra de v1); lock existente permite
    // reinstalar a versão travada mesmo yanked.
    let locked_version: Option<String> = pkg::load_lock(&core.paths).ok().and_then(|l| {
        l.package
            .iter()
            .find(|p| p.name == name)
            .map(|p| p.version.clone())
    });
    let mut errors = vec![];
    // Plano pós-ordem (dependências antes dos dependentes), como v1.
    let mut plan: Vec<(Source, IndexFile, IndexVersion, IndexArtifactMeta)> = vec![];
    let mut visiting: Vec<String> = vec![];
    resolve_into_v2(
        core,
        &sources,
        name,
        &req_parsed,
        &locked_version,
        force_refresh,
        &mut plan,
        &mut visiting,
        &mut errors,
    )
    .await?;
    let mut last = String::new();
    for (source, index, v, meta) in plan {
        let repo = refresh_repo(core, &source).await?;
        let r = if index.name == name { requested } else { "*" };
        last = install_pick_v2(core, &repo, &index, &v, &meta, r).await?;
    }
    core.refresh()?;
    Ok(last)
}

#[allow(clippy::too_many_arguments)]
fn resolve_into_v2<'a>(
    core: &'a Core,
    sources: &'a [Source],
    name: &'a str,
    req: &'a semver::VersionReq,
    locked_version: &'a Option<String>,
    force_refresh: bool,
    plan: &'a mut Vec<(Source, IndexFile, IndexVersion, IndexArtifactMeta)>,
    visiting: &'a mut Vec<String>,
    errors: &'a mut Vec<String>,
) -> std::pin::Pin<Box<dyn Future<Output = Result<(), CoreError>> + Send + 'a>> {
    Box::pin(async move {
        if visiting.contains(&name.to_string()) {
            return Err(CoreError::Package(format!("dependência cíclica: {name}")));
        }
        if plan.iter().any(|(_, i, _, _)| i.name == name) {
            return Ok(());
        }
        if !force_refresh
            && let Ok(lock) = pkg::load_lock(&core.paths)
            && let Some(lp) = lock.package.iter().find(|p| p.name == name)
            && let Ok(sv) = semver::Version::parse(&lp.version)
            && req.matches(&sv)
        {
            return Ok(());
        }
        visiting.push(name.to_string());
        let mut resolved: Option<(Source, IndexFile, IndexVersion, IndexArtifactMeta)> = None;
        for s in sources {
            let repo = match refresh_repo(core, s).await {
                Ok(r) => r,
                Err(e) => {
                    // Falha de verificação TUF (`-32031` + `details.reason`)
                    // propaga direto (`[ST2-2]`); só "ausente aqui"
                    // (`NoProvider`) tenta a próxima source.
                    if matches!(e, CoreError::SignatureInvalid(_)) {
                        return Err(e);
                    }
                    errors.push(format!("{}: {e}", s.name));
                    continue;
                }
            };
            let (index, _, metas) = match tuf_index_for(&repo, name) {
                Ok(t) => t,
                Err(e) => {
                    if matches!(e, CoreError::SignatureInvalid(_)) {
                        return Err(e);
                    }
                    errors.push(format!("{}: {e}", s.name));
                    continue;
                }
            };
            // Reconstrói `IndexArtifact` a partir dos metas do delegado para
            // reaproveitar o `pick` de v1 (engine/plataforma/yanked).
            let mut idx = index.clone();
            for v in &mut idx.versions {
                for a in &mut v.artifacts {
                    if let Some(m) = metas
                        .iter()
                        .find(|m| m.version == v.version && m.platform == a.platform)
                    {
                        a.sha256 = m.sha256.clone();
                        a.size = m.size;
                    }
                }
            }
            // Contratos exigidos precisam de provider instalado (como v1).
            let (version, artifact) = match pick(&idx, req, locked_version.as_deref()) {
                Ok(p) => (p.version.clone(), p.artifact.clone()),
                Err(e) => {
                    errors.push(format!("{}: {e}", s.name));
                    continue;
                }
            };
            let meta = metas
                .iter()
                .find(|m| m.version == version.version && m.platform == artifact.platform)
                .cloned()
                .ok_or_else(|| CoreError::Package(format!("artefato de {name} sem meta TUF")))?;
            resolved = Some((s.clone(), idx, version, meta));
            break;
        }
        let (source, index, version, meta) = resolved.ok_or_else(|| {
            CoreError::NoProvider(format!("{name}@{req} ({})", errors.join("; ")))
        })?;
        for (contract, creq) in &version.requires.contracts {
            let ok = core.installed().values().any(|(_, m)| {
                m.provides.iter().any(|p| {
                    p.contract == *contract
                        && semver::VersionReq::parse(creq)
                            .map(|r| {
                                semver::Version::parse(&p.version).is_ok_and(|v| r.matches(&v))
                            })
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
            resolve_into_v2(
                core,
                sources,
                dep,
                &r,
                &None,
                force_refresh,
                plan,
                visiting,
                errors,
            )
            .await?;
        }
        visiting.pop();
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
        plan.push((source, index, version, meta));
        Ok(())
    })
}

/// Versão resolvida (sem instalar), como `store::resolve_latest_version`.
#[derive(Debug, Clone)]
pub struct ResolvedVersion {
    pub version: IndexVersion,
    pub source: String,
}

pub async fn resolve_latest_version(
    core: &Core,
    name: &str,
    req: Option<&str>,
    source_only: Option<&str>,
) -> Result<ResolvedVersion, CoreError> {
    let req_parsed = parse_req(req)?;
    let sources: Vec<Source> = crate::store::profile_sources(core)
        .into_iter()
        .filter(|s| is_v2_source(&s.base))
        .filter(|s| source_only.is_none_or(|o| s.name == o))
        .collect();
    let locked_version: Option<String> = pkg::load_lock(&core.paths).ok().and_then(|l| {
        l.package
            .iter()
            .find(|p| p.name == name)
            .map(|p| p.version.clone())
    });
    let mut errors = vec![];
    for s in &sources {
        let repo = match refresh_repo(core, s).await {
            Ok(r) => r,
            Err(e) => {
                errors.push(format!("{}: {e}", s.name));
                continue;
            }
        };
        let (index, _, metas) = match tuf_index_for(&repo, name) {
            Ok(t) => t,
            Err(e) => {
                errors.push(format!("{}: {e}", s.name));
                continue;
            }
        };
        let mut idx = index.clone();
        for v in &mut idx.versions {
            for a in &mut v.artifacts {
                if let Some(m) = metas
                    .iter()
                    .find(|m| m.version == v.version && m.platform == a.platform)
                {
                    a.sha256 = m.sha256.clone();
                    a.size = m.size;
                }
            }
        }
        match pick(&idx, &req_parsed, locked_version.as_deref()) {
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

// ---------------------------------------------------------------------------
// Ferramenta de publicação (CI de referência): gera `metadata/` TUF sobre um
// diretório `v2/` a partir dos snippets `pack_package` (mesmo formato v1).

/// Chaves de um papel (para `publish_repo`; 1 chave = limiar 1).
pub struct RoleKeys {
    pub seeds: Vec<[u8; 32]>,
}

impl RoleKeys {
    pub fn single(seed: [u8; 32]) -> Self {
        Self { seeds: vec![seed] }
    }
}

/// Publica snippets no layout `v2/` (spec §1): artefatos em `targets/` +
/// metadados TUF em `metadata/` (root/timestamp/snapshot/targets/delegados).
/// `publisher_seeds`: publisher → seed (delegação `publisher:<p>`, limiar 1).
pub fn publish_repo(
    v2_dir: &Path,
    root_seed: &[u8; 32],
    snapshot_seed: &[u8; 32],
    timestamp_seed: &[u8; 32],
    publisher_seeds: &HashMap<String, [u8; 32]>,
    snippets: &[PathBuf],
    artifacts_dir: Option<&Path>,
) -> Result<String, CoreError> {
    use ed25519_dalek::SigningKey;
    use sha2::{Digest, Sha256};

    let meta_dir = v2_dir.join("metadata");
    let targets_dir = v2_dir.join("targets");
    std::fs::create_dir_all(&meta_dir)
        .map_err(|e| CoreError::Io(format!("mkdir {}: {e}", meta_dir.display())))?;
    std::fs::create_dir_all(&targets_dir)
        .map_err(|e| CoreError::Io(format!("mkdir {}: {e}", targets_dir.display())))?;

    // Chaves TUF (keyid = 8 hex do sha256 da pubkey).
    let tuf_key_of = |seed: &[u8; 32]| -> Result<(String, TufKey), CoreError> {
        let sk = SigningKey::from_bytes(seed);
        let raw = sk.verifying_key().to_bytes();
        let hex: String = raw.iter().map(|b| format!("{b:02x}")).collect();
        let id = keyid_of(&hex)?;
        Ok((
            id,
            TufKey {
                keytype: "ed25519".to_string(),
                scheme: "ed25519".to_string(),
                keyval: KeyVal { public: hex },
                expires: None,
            },
        ))
    };
    let (root_id, root_key) = tuf_key_of(root_seed)?;
    let (snap_id, _snap_key) = tuf_key_of(snapshot_seed)?;
    let (ts_id, _ts_key) = tuf_key_of(timestamp_seed)?;

    // Agrupa snippets por publisher.
    let mut by_pub: HashMap<String, Vec<crate::store::PackSnippet>> = HashMap::new();
    for snip in snippets {
        let raw = std::fs::read(snip)
            .map_err(|e| CoreError::Package(format!("snippet {}: {e}", snip.display())))?;
        let s: crate::store::PackSnippet = serde_json::from_slice(&raw)
            .map_err(|e| CoreError::Package(format!("snippet {}: {e}", snip.display())))?;
        let (publisher, _) = s
            .name
            .split_once('/')
            .ok_or_else(|| CoreError::Package(format!("nome inválido no snippet: {:?}", s.name)))?;
        // Copia artefato p/ `targets/<publisher>/<nome>/<versão>/`.
        let file = s
            .artifact
            .url
            .rsplit('/')
            .next()
            .unwrap_or(&s.artifact.url)
            .to_string();
        let rel = format!(
            "{}/{}/{}/{}",
            publisher,
            s.name.split('/').nth(1).unwrap_or("?"),
            s.version,
            file
        );
        let dst = targets_dir.join(&rel);
        if let Some(p) = dst.parent() {
            std::fs::create_dir_all(p).map_err(|e| CoreError::Io(e.to_string()))?;
        }
        let src_cands = [
            snip.parent().map(|p| p.join(&file)).unwrap_or_default(),
            artifacts_dir.map(|d| d.join(&file)).unwrap_or_default(),
        ];
        let mut copied = false;
        for c in &src_cands {
            if c.is_file() {
                std::fs::copy(c, &dst).map_err(|e| {
                    CoreError::Io(format!("copy {} -> {}: {e}", c.display(), dst.display()))
                })?;
                copied = true;
                break;
            }
        }
        if !copied {
            return Err(CoreError::Package(format!(
                "artefato {file} não encontrado"
            )));
        }
        by_pub.entry(publisher.to_string()).or_default().push(s);
    }

    // Delegados por publisher: `targets` do papel = índice + artefatos.
    let mut delegated_envs: HashMap<String, Envelope<TargetsSigned>> = HashMap::new();
    let mut delegation_keys: HashMap<String, TufKey> = HashMap::new();
    let mut delegation_roles: Vec<DelegatedRole> = vec![];
    let mut pubs: Vec<String> = by_pub.keys().cloned().collect();
    pubs.sort();
    for publisher in &pubs {
        let seed = publisher_seeds
            .get(publisher)
            .ok_or_else(|| CoreError::Package(format!("sem seed p/ publisher {publisher}")))?;
        let (keyid, key) = tuf_key_of(seed)?;
        let snippets = &by_pub[publisher];
        // Índice `index/<publisher>/<nome>.json` (mesmo schema v1, sem sig).
        let mut per_name: HashMap<String, Vec<&crate::store::PackSnippet>> = HashMap::new();
        for s in snippets {
            per_name.entry(s.name.clone()).or_default().push(s);
        }
        let mut names: Vec<String> = per_name.keys().cloned().collect();
        names.sort();
        let mut role_targets: HashMap<String, TargetFile> = HashMap::new();
        for name in &names {
            let pkg_short = name.split('/').nth(1).unwrap_or(name);
            let versions: Vec<IndexVersion> = per_name[name]
                .iter()
                .map(|s| IndexVersion {
                    version: s.version.clone(),
                    kind: s.kind.clone(),
                    engine: s.engine.clone(),
                    protocol: s.protocol.clone(),
                    requires: IndexRequires {
                        packages: s.requires.packages.clone(),
                        contracts: s.requires.contracts.clone(),
                    },
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
                        url: format!(
                            "{}/{}/{}/{}",
                            publisher,
                            pkg_short,
                            s.version,
                            s.artifact.url.rsplit('/').next().unwrap_or(&s.artifact.url)
                        ),
                        sha256: s.artifact.sha256.clone(),
                        size: s.artifact.size,
                        signature: String::new(),
                    }],
                    yanked: false,
                    published_at: s.published_at.clone(),
                })
                .collect();
            let index = IndexFile {
                name: name.clone(),
                publisher_keys: vec![crate::store::pubkey_str(
                    &SigningKey::from_bytes(seed).verifying_key().to_bytes(),
                )],
                versions,
            };
            let index_rel = format!("index/{name}.json");
            let index_bytes =
                serde_json::to_vec(&index).map_err(|e| CoreError::Internal(e.to_string()))?;
            role_targets.insert(index_rel, target_file_of(&index_bytes));
            for s in per_name[name].iter() {
                let file = s.artifact.url.rsplit('/').next().unwrap_or(&s.artifact.url);
                let rel = format!("{}/{}/{}/{}", publisher, pkg_short, s.version, file);
                let bytes = std::fs::read(targets_dir.join(&rel))
                    .map_err(|e| CoreError::Io(e.to_string()))?;
                // Hash real do artefato publicado (não o do snippet).
                let mut h = Sha256::new();
                h.update(&bytes);
                let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
                role_targets.insert(
                    rel,
                    TargetFile {
                        length: bytes.len() as u64,
                        hashes: HashMap::from([("sha256".to_string(), hex)]),
                        custom: None,
                    },
                );
                // `custom` do alvo-índice carrega o índice completo (offline).
                if let Some(t) = role_targets.get_mut(&format!("index/{name}.json")) {
                    t.custom = Some(serde_json::to_value(&index).unwrap_or_default());
                }
            }
        }
        let role_name = format!("publisher:{publisher}");
        let signed = TargetsSigned {
            typ: "targets".to_string(),
            version: 1,
            expires: far_future(),
            targets: role_targets,
            delegations: None,
        };
        let env = sign_envelope(signed, &[*seed], std::slice::from_ref(&keyid))?;
        delegation_keys.insert(keyid.clone(), key);
        delegation_roles.push(DelegatedRole {
            name: role_name.clone(),
            keyids: vec![keyid],
            threshold: 1,
            paths: vec![format!("{publisher}/*"), format!("index/{publisher}/*")],
            terminating: true,
        });
        let bytes =
            serde_json::to_vec_pretty(&env).map_err(|e| CoreError::Internal(e.to_string()))?;
        profile::atomic_write(
            &meta_dir.join(format!("1.{}.json", role_file(&role_name))),
            &bytes,
        )?;
        profile::atomic_write(
            &meta_dir.join(format!("{}.json", role_file(&role_name))),
            &bytes,
        )?;
        delegated_envs.insert(role_name, env);
    }

    // Targets de topo: delegações + `catalog.json` como alvo.
    let catalog_targets = catalog_from_delegated(&delegated_envs);
    let catalog_bytes =
        serde_json::to_vec(&catalog_targets).map_err(|e| CoreError::Internal(e.to_string()))?;
    let mut top_targets_map: HashMap<String, TargetFile> = HashMap::new();
    top_targets_map.insert("catalog.json".to_string(), target_file_of(&catalog_bytes));
    let top_signed = TargetsSigned {
        typ: "targets".to_string(),
        version: 1,
        expires: far_future(),
        targets: top_targets_map,
        delegations: Some(Delegations {
            keys: delegation_keys,
            roles: delegation_roles,
        }),
    };
    let mut top_keys = HashMap::new();
    top_keys.insert(root_id.clone(), root_key.clone());
    let top_env = sign_envelope(top_signed, &[*root_seed], std::slice::from_ref(&root_id))?;
    let top_bytes =
        serde_json::to_vec_pretty(&top_env).map_err(|e| CoreError::Internal(e.to_string()))?;
    profile::atomic_write(&meta_dir.join("1.targets.json"), &top_bytes)?;
    profile::atomic_write(&meta_dir.join("targets.json"), &top_bytes)?;
    // `targets/catalog.json` servível (spec store-v2 §1).
    let _ = profile::atomic_write(&targets_dir.join("catalog.json"), &catalog_bytes);

    // Snapshot: versões de `targets.json` + delegados.
    let mut snap_meta: HashMap<String, MetaFile> = HashMap::new();
    snap_meta.insert(
        "targets.json".to_string(),
        MetaFile {
            version: 1,
            length: Some(top_bytes.len() as u64),
            hashes: Some(HashMap::from([("sha256".to_string(), sha_hex(&top_bytes))])),
        },
    );
    for (role, env) in &delegated_envs {
        let pretty =
            serde_json::to_vec_pretty(env).map_err(|e| CoreError::Internal(e.to_string()))?;
        snap_meta.insert(
            format!("{}.json", role_file(role)),
            MetaFile {
                version: 1,
                length: Some(pretty.len() as u64),
                hashes: Some(HashMap::from([("sha256".to_string(), sha_hex(&pretty))])),
            },
        );
    }
    let snap_signed = SnapshotSigned {
        typ: "snapshot".to_string(),
        version: 1,
        expires: far_future(),
        meta: snap_meta,
    };
    let snap_env = sign_envelope(
        snap_signed,
        &[*snapshot_seed],
        std::slice::from_ref(&snap_id),
    )?;
    let snap_bytes =
        serde_json::to_vec_pretty(&snap_env).map_err(|e| CoreError::Internal(e.to_string()))?;
    profile::atomic_write(&meta_dir.join("1.snapshot.json"), &snap_bytes)?;
    profile::atomic_write(&meta_dir.join("snapshot.json"), &snap_bytes)?;

    // Timestamp: versão do snapshot.
    let ts_signed = TimestampSigned {
        typ: "timestamp".to_string(),
        version: 1,
        expires: far_future(),
        meta: HashMap::from([(
            "snapshot.json".to_string(),
            MetaFile {
                version: 1,
                length: Some(snap_bytes.len() as u64),
                hashes: Some(HashMap::from([(
                    "sha256".to_string(),
                    sha_hex(&snap_bytes),
                )])),
            },
        )]),
    };
    let ts_env = sign_envelope(ts_signed, &[*timestamp_seed], std::slice::from_ref(&ts_id))?;
    let ts_bytes =
        serde_json::to_vec_pretty(&ts_env).map_err(|e| CoreError::Internal(e.to_string()))?;
    profile::atomic_write(&meta_dir.join("timestamp.json"), &ts_bytes)?;

    // Root (versão 1): papéis de topo com limiares da spec §2.
    let mut keys = HashMap::new();
    keys.insert(root_id.clone(), root_key);
    let (snap_id2, snap_key) = tuf_key_of(snapshot_seed)?;
    let (ts_id2, ts_key) = tuf_key_of(timestamp_seed)?;
    keys.insert(snap_id2.clone(), snap_key);
    keys.insert(ts_id2.clone(), ts_key);
    let role = |ids: Vec<String>, threshold: u32| Role {
        keyids: ids,
        threshold,
    };
    let roles = HashMap::from([
        ("root".to_string(), role(vec![root_id.clone()], 1)),
        ("targets".to_string(), role(vec![root_id.clone()], 1)),
        ("snapshot".to_string(), role(vec![snap_id2], 1)),
        ("timestamp".to_string(), role(vec![ts_id2], 1)),
    ]);
    let root_signed = RootSigned {
        typ: "root".to_string(),
        version: 1,
        expires: far_future(),
        keys,
        roles,
        consistent_snapshot: false,
    };
    let root_env = sign_envelope(root_signed, &[*root_seed], std::slice::from_ref(&root_id))?;
    let root_bytes =
        serde_json::to_vec_pretty(&root_env).map_err(|e| CoreError::Internal(e.to_string()))?;
    profile::atomic_write(&meta_dir.join("1.root.json"), &root_bytes)?;

    // `1.root.json`: sha para `atom source add --root`.
    let one =
        std::fs::read(meta_dir.join("1.root.json")).map_err(|e| CoreError::Io(e.to_string()))?;
    Ok(format!(
        "v2 ok: {} publisher(s), root sha256 {}",
        pubs.len(),
        sha_hex(&one)
    ))
}

fn target_file_of(bytes: &[u8]) -> TargetFile {
    TargetFile {
        length: bytes.len() as u64,
        hashes: HashMap::from([("sha256".to_string(), sha_hex(bytes))]),
        custom: None,
    }
}

fn sha_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn far_future() -> String {
    // `shortcut: sem chrono; expiração fixa longe (comparação lexicográfica
    // com o mesmo formato em `check_expiry`).
    "9999999999".to_string()
}

fn sign_envelope<T>(
    signed: T,
    seeds: &[[u8; 32]],
    keyids: &[String],
) -> Result<Envelope<T>, CoreError>
where
    T: Serialize + serde::de::DeserializeOwned + Clone,
{
    use ed25519_dalek::{Signer, SigningKey};
    let v =
        serde_json::to_value(&signed).map_err(|e| CoreError::Package(format!("assinar: {e}")))?;
    let msg = canonical(&v)?;
    let mut signatures = vec![];
    for (seed, keyid) in seeds.iter().zip(keyids.iter()) {
        let sig = SigningKey::from_bytes(seed).sign(&msg);
        signatures.push(Signature {
            keyid: keyid.clone(),
            sig: b64().encode(sig.to_bytes()),
        });
    }
    Ok(Envelope { signatures, signed })
}

fn catalog_from_delegated(
    delegated: &HashMap<String, Envelope<TargetsSigned>>,
) -> Vec<crate::store::CatalogEntry> {
    let mut out = vec![];
    for env in delegated.values() {
        for (path, tf) in &env.signed.targets {
            if !path.starts_with("index/") || !path.ends_with(".json") {
                continue;
            }
            if let Some(custom) = &tf.custom
                && let Ok(index) = serde_json::from_value::<IndexFile>(custom.clone())
                && let Some(v) = index.versions.first()
            {
                out.push(crate::store::CatalogEntry {
                    name: index.name.clone(),
                    description: String::new(),
                    kind: v.kind.clone(),
                    latest: v.version.clone(),
                });
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}
// ---------------------------------------------------------------------------
// Bloco 1: chaves, expiração real e leitura v2 (spec store-v2 §§2,4,7).

/// Lê seeds `ed25519-seed:BASE64` (ou b64 puro), uma por linha; ignora
/// vazias/`#`; vazio = erro.
pub fn load_keys_file(path: &Path) -> Result<Vec<[u8; 32]>, CoreError> {
    let text = std::fs::read_to_string(path).map_err(|e| CoreError::Io(e.to_string()))?;
    let mut out = vec![];
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let part = t.strip_prefix("ed25519-seed:").unwrap_or(t).trim();
        let raw = b64().decode(part).map_err(|e| {
            CoreError::Package(format!("chave privada inválida em {}: {e}", path.display()))
        })?;
        let seed: [u8; 32] = raw.try_into().map_err(|_| {
            CoreError::Package(format!("seed deve ter 32 bytes em {}", path.display()))
        })?;
        out.push(seed);
    }
    if out.is_empty() {
        return Err(CoreError::Package(format!(
            "nenhuma chave em {}",
            path.display()
        )));
    }
    Ok(out)
}

/// Agora + `days` em `AAAA-MM-DDTHH:MM:SSZ` (sem chrono; `i64` permite
/// negativo p/ teste).
pub fn rfc3339_in_days(days: i64) -> String {
    let total = now_secs() as i64 + days * 86400;
    let d = total.div_euclid(86400);
    let sod = total.rem_euclid(86400);
    let (y, mo, day) = civil_from_days(d);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        mo,
        day,
        sod / 3600,
        (sod % 3600) / 60,
        sod % 60
    )
}

/// Expirações reais por papel (spec store-v2 §2).
pub struct ExpiryPolicy {
    pub targets_days: i64,
    pub snapshot_days: i64,
    pub timestamp_days: i64,
}

impl ExpiryPolicy {
    /// Padrão da loja (`[ST2-1]`): targets 90 dias, snapshot 7, timestamp 1.
    pub fn store_default() -> Self {
        Self {
            targets_days: 90,
            snapshot_days: 7,
            timestamp_days: 1,
        }
    }
}

/// Busca no catálogo TUF (`targets/catalog.json`, hash verificado) das
/// sources v2. Sem v2 = `Ok(vec![])`; `NoProvider`/ausente pula a source,
/// `SignatureInvalid` propaga (spec `[ST2-2]`).
pub async fn search_v2(
    core: &Core,
    query: &str,
    kind: Option<&str>,
) -> Result<Vec<(String, CatalogEntry)>, CoreError> {
    let sources: Vec<Source> = crate::store::profile_sources(core)
        .into_iter()
        .filter(|s| is_v2_source(&s.base))
        .collect();
    let q = query.to_lowercase();
    let mut seen = HashSet::new();
    let mut out = vec![];
    for s in &sources {
        let repo = match refresh_repo(core, s).await {
            Ok(r) => r,
            Err(e) => {
                if matches!(e, CoreError::SignatureInvalid(_)) {
                    return Err(e);
                }
                continue;
            }
        };
        let tf = match repo.targets.signed.targets.get("catalog.json") {
            Some(t) => t.clone(),
            None => continue,
        };
        let bytes = match fetch_rel(core, &repo.source, "targets/catalog.json").await {
            Ok(b) => b,
            Err(_) => continue,
        };
        verify_meta_against(
            &MetaFile {
                version: 1,
                length: Some(tf.length),
                hashes: Some(tf.hashes.clone()),
            },
            &bytes,
            "catalog.json",
        )?;
        let entries: Vec<CatalogEntry> = serde_json::from_slice(&bytes)
            .map_err(|e| CoreError::Package(format!("catalog inválido: {e}")))?;
        for e in entries {
            if !seen.insert(e.name.clone()) {
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

/// Índice v2 por source (`refresh_repo` + `tuf_index_for`); vazio = `NoProvider`.
pub async fn info_v2(core: &Core, name: &str) -> Result<Vec<(String, IndexFile)>, CoreError> {
    let sources: Vec<Source> = crate::store::profile_sources(core)
        .into_iter()
        .filter(|s| is_v2_source(&s.base))
        .collect();
    let mut out = vec![];
    for s in &sources {
        let repo = match refresh_repo(core, s).await {
            Ok(r) => r,
            Err(e) => {
                if matches!(e, CoreError::SignatureInvalid(_)) {
                    return Err(e);
                }
                continue;
            }
        };
        match tuf_index_for(&repo, name) {
            Ok((index, _, _)) => out.push((s.name.clone(), index)),
            Err(e) => {
                if matches!(e, CoreError::SignatureInvalid(_)) {
                    return Err(e);
                }
                continue;
            }
        }
    }
    if out.is_empty() {
        return Err(CoreError::NoProvider(name.to_string()));
    }
    Ok(out)
}

/// Espelha `store::update_packages` sobre lock entries de source v2 (req do
/// lock, `force_refresh`, `source_only`).
pub async fn update_v2(
    core: &mut Core,
    name: Option<&str>,
) -> Result<Vec<(String, String, String)>, CoreError> {
    let v2_names: Vec<String> = crate::store::profile_sources(core)
        .into_iter()
        .filter(|s| is_v2_source(&s.base))
        .map(|s| s.name)
        .collect();
    let targets: Vec<(String, String, String)> = {
        let lock = pkg::load_lock(&core.paths)?;
        lock.package
            .iter()
            .filter(|p| v2_names.contains(&p.source) && name.as_ref().is_none_or(|n| *n == p.name))
            .map(|p| (p.name.clone(), p.version.clone(), p.source.clone()))
            .collect()
    };
    if targets.is_empty() {
        return Err(CoreError::Package(
            "nada da loja para atualizar".to_string(),
        ));
    }
    let mut done = vec![];
    for (n, from, src) in targets {
        let req = pkg::load_lock(&core.paths)
            .ok()
            .and_then(|l| {
                l.package
                    .iter()
                    .find(|p| p.name == n)
                    .map(|p| p.requested.clone())
            })
            .unwrap_or_else(|| "*".to_string());
        install_package(core, &n, Some(&req), Some(&src), true).await?;
        let after = pkg::load_lock(&core.paths)?
            .package
            .iter()
            .find(|p| p.name == n)
            .map(|p| p.version.clone())
            .unwrap_or_default();
        if after != from {
            done.push((n, from, after));
        }
    }
    core.refresh()?;
    Ok(done)
}

/// Busca stateless de alvo v2: root (bytes confiáveis) → timestamp →
/// snapshot → targets (nomes por `consistent_snapshot`), expiração real,
/// limiar, versão == anunciada (senão `tuf rollback:`); depois
/// `targets/<rel>` com hash/tamanho conferidos. Erros trazem
/// `tuf expired:`/`tuf rollback:`/`tuf threshold:` p/ mapear `reason`.
pub async fn fetch_v2_target(
    base: &str,
    target_rel: &str,
    trusted_root: &[u8],
) -> Result<Vec<u8>, CoreError> {
    // spec: [ST2-1] sem cache/`seen` (stateless).
    let base = if base.ends_with('/') {
        base.to_string()
    } else {
        format!("{base}/")
    };
    let mut root: Root = serde_json::from_slice(trusted_root)
        .map_err(|e| CoreError::Package(format!("root confiável inválido: {e}")))?;
    root.check_self_signed()?;
    // Cadeia `N+1.root.json` assinada pelo root anterior (e auto-assinada).
    loop {
        let rel = format!("metadata/{}.root.json", root.signed.version + 1);
        let raw = match download_v2(&format!("{base}{rel}")).await {
            Ok(b) => b,
            Err(_) => break,
        };
        let next: Root = serde_json::from_slice(&raw)
            .map_err(|e| CoreError::Package(format!("{rel} inválido: {e}")))?;
        next.check_self_signed()?;
        let v = serde_json::to_value(&next.signed)
            .map_err(|e| CoreError::Package(format!("{rel}: {e}")))?;
        let prev_role = root.signed.role("root")?.clone();
        verify_role(&root.signed.keys, &prev_role, &v, &next.signatures, "root")?;
        root = next;
    }
    check_expiry(&root.signed.expires, "root")?;
    let keys = root.signed.keys.clone();
    let ts_role = root.signed.role("timestamp")?.clone();
    let snap_role = root.signed.role("snapshot")?.clone();
    let tg_role = root.signed.role("targets")?.clone();
    let ts_raw = download_v2(&format!("{base}metadata/timestamp.json")).await?;
    let ts: Envelope<TimestampSigned> = verify_envelope(&keys, &ts_role, &ts_raw, "timestamp")?;
    let snap_meta = ts
        .signed
        .meta
        .get("snapshot.json")
        .ok_or_else(|| CoreError::Package("timestamp sem snapshot.json".to_string()))?;
    let snap_name = if root.signed.consistent_snapshot {
        format!("{}.snapshot.json", snap_meta.version)
    } else {
        "snapshot.json".to_string()
    };
    let snap_raw = download_v2(&format!("{base}metadata/{snap_name}")).await?;
    verify_meta_against(snap_meta, &snap_raw, &snap_name)?;
    let snap: Envelope<SnapshotSigned> = verify_envelope(&keys, &snap_role, &snap_raw, "snapshot")?;
    if snap.signed.version != snap_meta.version {
        return Err(CoreError::SignatureInvalid(format!(
            "tuf rollback: snapshot versão {} != {} do timestamp",
            snap.signed.version, snap_meta.version
        )));
    }
    let tg_meta = snap
        .signed
        .meta
        .get("targets.json")
        .ok_or_else(|| CoreError::Package("snapshot sem targets.json".to_string()))?;
    let tg_name = if root.signed.consistent_snapshot {
        format!("{}.targets.json", tg_meta.version)
    } else {
        "targets.json".to_string()
    };
    let tg_raw = download_v2(&format!("{base}metadata/{tg_name}")).await?;
    verify_meta_against(tg_meta, &tg_raw, &tg_name)?;
    let tg: Envelope<TargetsSigned> = verify_envelope(&keys, &tg_role, &tg_raw, "targets")?;
    if tg.signed.version != tg_meta.version {
        return Err(CoreError::SignatureInvalid(format!(
            "tuf rollback: targets versão {} != {} do snapshot",
            tg.signed.version, tg_meta.version
        )));
    }
    let rel = target_rel.trim_start_matches('/');
    let meta_of = |tf: &TargetFile| MetaFile {
        version: 1,
        length: Some(tf.length),
        hashes: Some(tf.hashes.clone()),
    };
    if let Some(tf) = tg.signed.targets.get(rel) {
        let bytes = download_v2(&format!("{base}targets/{rel}")).await?;
        verify_meta_against(&meta_of(tf), &bytes, rel)?;
        return Ok(bytes);
    }
    // Delegados por publisher (spec `[ST2-5]`: só serve sob o próprio prefixo).
    if let Some(dels) = &tg.signed.delegations {
        for role in &dels.roles {
            if !role_allows(&role.paths, rel) {
                continue;
            }
            let meta_key = format!("{}.json", role_file(&role.name));
            let Some(meta) = snap.signed.meta.get(&meta_key) else {
                continue;
            };
            let rel_name = if root.signed.consistent_snapshot {
                format!("{}.{}.json", meta.version, role_file(&role.name))
            } else {
                format!("{}.json", role_file(&role.name))
            };
            let raw = download_v2(&format!("{base}metadata/{rel_name}")).await?;
            verify_meta_against(meta, &raw, &rel_name)?;
            let role_keys: HashMap<String, TufKey> = dels
                .keys
                .iter()
                .filter(|(id, _)| role.keyids.contains(id))
                .map(|(id, k)| (id.clone(), k.clone()))
                .collect();
            let drole = Role {
                keyids: role.keyids.clone(),
                threshold: role.threshold,
            };
            let env: Envelope<TargetsSigned> =
                verify_envelope(&role_keys, &drole, &raw, "delegated")?;
            if env.signed.version != meta.version {
                return Err(CoreError::SignatureInvalid(format!(
                    "tuf rollback: {} versão {} != {} do snapshot",
                    role.name, env.signed.version, meta.version
                )));
            }
            for path in env.signed.targets.keys() {
                if !role_allows(&role.paths, path) {
                    return Err(CoreError::SignatureInvalid(format!(
                        "tuf threshold: {} assinou fora do prefixo permitido ({path})",
                        role.name
                    )));
                }
            }
            if let Some(tf) = env.signed.targets.get(rel) {
                let bytes = download_v2(&format!("{base}targets/{rel}")).await?;
                verify_meta_against(&meta_of(tf), &bytes, rel)?;
                return Ok(bytes);
            }
        }
    }
    Err(CoreError::NoProvider(format!(
        "{rel} (alvo ausente no repo v2)"
    )))
}

// ---------------------------------------------------------------------------
// Bloco 2: publicação TUF (lado publisher + CI de referência, spec §8).
// Chamadores em `crates/atom/src/main.rs` (`publish --v2`).

/// Chave TUF a partir da seed (`keyid` = 8 hex do sha256 da pubkey).
fn tuf_key_of(seed: &[u8; 32]) -> Result<(String, TufKey), CoreError> {
    use ed25519_dalek::SigningKey;
    let sk = SigningKey::from_bytes(seed);
    let raw = sk.verifying_key().to_bytes();
    let hex: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    let id = keyid_of(&hex)?;
    Ok((
        id,
        TufKey {
            keytype: "ed25519".to_string(),
            scheme: "ed25519".to_string(),
            keyval: KeyVal { public: hex },
            expires: None,
        },
    ))
}

fn keyids_of(seeds: &[[u8; 32]]) -> Result<Vec<String>, CoreError> {
    let mut out = Vec::with_capacity(seeds.len());
    for s in seeds {
        out.push(tuf_key_of(s)?.0);
    }
    Ok(out)
}

/// Limiar do papel no root atual vs nº de chaves passadas (spec §8 item 6).
/// Erro traz `tuf threshold:` p/ mapear `reason` (`[ST2-2]`).
fn check_seed_threshold(role_name: &str, role: &Role, seeds: &[[u8; 32]]) -> Result<(), CoreError> {
    if seeds.len() < role.threshold.max(1) as usize {
        return Err(CoreError::SignatureInvalid(format!(
            "tuf threshold: {role_name}: {} chave(s) p/ limiar {}",
            seeds.len(),
            role.threshold.max(1),
        )));
    }
    Ok(())
}

/// Root atual = `N.root.json` de maior versão no diretório.
fn load_current_root(meta_dir: &Path) -> Result<Root, CoreError> {
    let mut best: Option<Root> = None;
    let entries = std::fs::read_dir(meta_dir)
        .map_err(|e| CoreError::Package(format!("ler {}: {e}", meta_dir.display())))?;
    for entry in entries {
        let entry = entry.map_err(|e| CoreError::Io(e.to_string()))?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".root.json") {
            continue;
        }
        let raw = std::fs::read(entry.path()).map_err(|e| CoreError::Io(e.to_string()))?;
        let root: Root = serde_json::from_slice(&raw)
            .map_err(|e| CoreError::Package(format!("{name} inválido: {e}")))?;
        let newer = match &best {
            None => true,
            Some(b) => root.signed.version > b.signed.version,
        };
        if newer {
            best = Some(root);
        }
    }
    best.ok_or_else(|| CoreError::Package(format!("sem root em {}", meta_dir.display())))
}

/// Nome de arquivo p/ um papel delegado (`:` não vale como nome de arquivo
/// no Windows — `os error 87`; TUF usa o nome do papel com `:` dentro do JSON,
/// o arquivo usa `%3A`). `root`/`targets`/`snapshot`/`timestamp` passam direto.
/// spec: [ST2-1]
fn role_file(role: &str) -> String {
    role.replace(':', "%3A")
}

/// Inverso de [`role_file`]: nome do papel a partir do nome do arquivo.
fn file_role(base: &str) -> String {
    base.replace("%3A", ":")
}

/// Espelha o metadado não-versionado + cópia `N.<base>` (como `publish_repo`
/// faz; leitores com ou sem `consistent_snapshot` encontram o seu nome).
fn write_meta_pair(
    meta_dir: &Path,
    base: &str,
    version: u32,
    bytes: &[u8],
) -> Result<(), CoreError> {
    profile::atomic_write(&meta_dir.join(base), bytes)?;
    profile::atomic_write(&meta_dir.join(format!("{version}.{base}")), bytes)?;
    Ok(())
}

/// Chaves registradas p/ o publisher (`publishers/<pub>.toml`, formato v1:
/// uma ou mais linhas `key = "ed25519:..."`, resto ignorado).
fn load_publisher_keys(
    publishers_dir: &Path,
    publisher: &str,
) -> Result<Vec<(String, TufKey)>, CoreError> {
    let path = publishers_dir.join(format!("{publisher}.toml"));
    let text = std::fs::read_to_string(&path)
        .map_err(|e| CoreError::Package(format!("publishers/{publisher}.toml ausente: {e}")))?;
    let mut out = vec![];
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let Some(s) = t.split('"').nth(1) else {
            continue;
        };
        if !s.starts_with("ed25519:") {
            continue;
        }
        let raw = crate::store::parse_pubkey(s)?;
        let hex: String = raw.iter().map(|b| format!("{b:02x}")).collect();
        let id = keyid_of(&hex)?;
        out.push((
            id,
            TufKey {
                keytype: "ed25519".to_string(),
                scheme: "ed25519".to_string(),
                keyval: KeyVal { public: hex },
                expires: None,
            },
        ));
    }
    if out.is_empty() {
        return Err(CoreError::Package(format!(
            "sem chaves em publishers/{publisher}.toml"
        )));
    }
    Ok(out)
}

/// Lado publisher (spec store-v2 §8 item 1): a partir dos snippets
/// `.index.json` do `pack`, gera/atualiza `metadata/publisher:<pub>.json`
/// (índice em `custom` + alvos dos artefatos), assinado só com a chave do
/// publisher (via `load_keys_file`), `version` = última+1 (ou 1) e
/// `expires` real. Todos os snippets precisam ser do mesmo publisher.
// spec: [ST2-5]
pub fn publish_delegated(
    v2_dir: &Path,
    snippets: &[PathBuf],
    publisher_keyfile: &Path,
    artifacts_dir: Option<&Path>,
    delegated_days: i64,
) -> Result<String, CoreError> {
    use ed25519_dalek::SigningKey;

    if snippets.is_empty() {
        return Err(CoreError::Package("nenhum snippet p/ publicar".to_string()));
    }
    let seeds = load_keys_file(publisher_keyfile)?;
    let meta_dir = v2_dir.join("metadata");
    let targets_dir = v2_dir.join("targets");
    std::fs::create_dir_all(&meta_dir)
        .map_err(|e| CoreError::Io(format!("mkdir {}: {e}", meta_dir.display())))?;
    std::fs::create_dir_all(&targets_dir)
        .map_err(|e| CoreError::Io(format!("mkdir {}: {e}", targets_dir.display())))?;

    let mut packs: Vec<crate::store::PackSnippet> = Vec::with_capacity(snippets.len());
    for snip in snippets {
        let raw = std::fs::read(snip)
            .map_err(|e| CoreError::Package(format!("snippet {}: {e}", snip.display())))?;
        let s: crate::store::PackSnippet = serde_json::from_slice(&raw)
            .map_err(|e| CoreError::Package(format!("snippet {}: {e}", snip.display())))?;
        if !s.name.contains('/') {
            return Err(CoreError::Package(format!(
                "nome inválido no snippet: {:?}",
                s.name
            )));
        }
        packs.push(s);
    }
    let publisher = packs[0].name.split('/').next().unwrap_or("").to_string();
    for s in &packs {
        let p = s.name.split('/').next().unwrap_or("");
        if p != publisher {
            return Err(CoreError::Package(format!(
                "snippets de publishers distintos ({publisher} e {p})"
            )));
        }
    }

    // Copia artefatos p/ `targets/<publisher>/<nome>/<versão>/`.
    for (snip, s) in snippets.iter().zip(packs.iter()) {
        let file = s
            .artifact
            .url
            .rsplit('/')
            .next()
            .unwrap_or(&s.artifact.url)
            .to_string();
        let pkg_short = s.name.split('/').nth(1).unwrap_or("?");
        let rel = format!("{}/{}/{}/{}", publisher, pkg_short, s.version, file);
        let dst = targets_dir.join(&rel);
        if let Some(p) = dst.parent() {
            std::fs::create_dir_all(p).map_err(|e| CoreError::Io(e.to_string()))?;
        }
        let mut copied = false;
        if let Some(parent) = snip.parent() {
            let c = parent.join(&file);
            if c.is_file() {
                std::fs::copy(&c, &dst).map_err(|e| {
                    CoreError::Io(format!("copy {} -> {}: {e}", c.display(), dst.display()))
                })?;
                copied = true;
            }
        }
        if !copied && let Some(dir) = artifacts_dir {
            let c = dir.join(&file);
            if c.is_file() {
                std::fs::copy(&c, &dst).map_err(|e| {
                    CoreError::Io(format!("copy {} -> {}: {e}", c.display(), dst.display()))
                })?;
                copied = true;
            }
        }
        if !copied {
            return Err(CoreError::Package(format!(
                "artefato {file} não encontrado"
            )));
        }
    }

    // Índice por nome (mesmo schema v1) + alvos com hash real do publicado.
    let pubkeys: Vec<String> = seeds
        .iter()
        .map(|sd| crate::store::pubkey_str(&SigningKey::from_bytes(sd).verifying_key().to_bytes()))
        .collect();
    let mut per_name: HashMap<String, Vec<&crate::store::PackSnippet>> = HashMap::new();
    for s in &packs {
        per_name.entry(s.name.clone()).or_default().push(s);
    }
    let mut names: Vec<String> = per_name.keys().cloned().collect();
    names.sort();
    let mut role_targets: HashMap<String, TargetFile> = HashMap::new();
    for name in &names {
        let pkg_short = name.split('/').nth(1).unwrap_or(name);
        let versions: Vec<IndexVersion> = per_name[name]
            .iter()
            .map(|s| IndexVersion {
                version: s.version.clone(),
                kind: s.kind.clone(),
                engine: s.engine.clone(),
                protocol: s.protocol.clone(),
                requires: IndexRequires {
                    packages: s.requires.packages.clone(),
                    contracts: s.requires.contracts.clone(),
                },
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
                    url: format!(
                        "{}/{}/{}/{}",
                        publisher,
                        pkg_short,
                        s.version,
                        s.artifact.url.rsplit('/').next().unwrap_or(&s.artifact.url)
                    ),
                    sha256: s.artifact.sha256.clone(),
                    size: s.artifact.size,
                    signature: String::new(),
                }],
                yanked: false,
                published_at: s.published_at.clone(),
            })
            .collect();
        let index = IndexFile {
            name: name.clone(),
            publisher_keys: pubkeys.clone(),
            versions,
        };
        let index_bytes =
            serde_json::to_vec(&index).map_err(|e| CoreError::Internal(e.to_string()))?;
        role_targets.insert(format!("index/{name}.json"), target_file_of(&index_bytes));
        for s in per_name[name].iter() {
            let file = s.artifact.url.rsplit('/').next().unwrap_or(&s.artifact.url);
            let rel = format!("{}/{}/{}/{}", publisher, pkg_short, s.version, file);
            let bytes =
                std::fs::read(targets_dir.join(&rel)).map_err(|e| CoreError::Io(e.to_string()))?;
            role_targets.insert(
                rel,
                TargetFile {
                    length: bytes.len() as u64,
                    hashes: HashMap::from([("sha256".to_string(), sha_hex(&bytes))]),
                    custom: None,
                },
            );
        }
        // `custom` do alvo-índice carrega o índice completo (offline).
        if let Some(t) = role_targets.get_mut(&format!("index/{name}.json")) {
            t.custom = Some(serde_json::to_value(&index).unwrap_or_default());
        }
    }

    let role_name = format!("publisher:{publisher}");
    let role_path = meta_dir.join(format!("{}.json", role_file(&role_name)));
    let mut version = 1u32;
    if role_path.is_file() {
        let raw = std::fs::read(&role_path).map_err(|e| CoreError::Io(e.to_string()))?;
        let prev: Envelope<TargetsSigned> = serde_json::from_slice(&raw)
            .map_err(|e| CoreError::Package(format!("{role_name} inválido: {e}")))?;
        version = prev.signed.version + 1;
    }
    let expires = rfc3339_in_days(delegated_days);
    let signed = TargetsSigned {
        typ: "targets".to_string(),
        version,
        expires: expires.clone(),
        targets: role_targets,
        delegations: None,
    };
    let keyids = keyids_of(&seeds)?;
    let env = sign_envelope(signed, &seeds, &keyids)?;
    let bytes = serde_json::to_vec_pretty(&env).map_err(|e| CoreError::Internal(e.to_string()))?;
    write_meta_pair(
        &meta_dir,
        &format!("{}.json", role_file(&role_name)),
        version,
        &bytes,
    )?;
    Ok(format!(
        "delegado {role_name} v{version} ({} pacote(s), expira {expires})",
        packs.len()
    ))
}

/// Lado loja (spec store-v2 §8 item 2): verifica cada delegado (sig vs
/// chaves de `publishers/<pub>.toml` + prefixo `[ST2-5]`), regenera
/// `targets/catalog.json`, `targets`/`snapshot`/`timestamp` com `version+1`
/// e `expires` reais (`policy`). O nº de chaves por papel precisa atingir o
/// limiar do root atual. `extra_targets` entram em `targets/` e no papel
/// `targets`. Delegações anteriores preservadas.
// spec: [ST2-1]
// spec: [ST2-2]
// spec: [ST2-3]
// spec: [ST2-5]
pub fn publish_store(
    v2_dir: &Path,
    targets_keys: &[[u8; 32]],
    snapshot_keys: &[[u8; 32]],
    timestamp_keys: &[[u8; 32]],
    publishers_dir: &Path,
    policy: &ExpiryPolicy,
    extra_targets: &[(String, Vec<u8>)],
) -> Result<String, CoreError> {
    let meta_dir = v2_dir.join("metadata");
    let targets_dir = v2_dir.join("targets");
    std::fs::create_dir_all(&meta_dir)
        .map_err(|e| CoreError::Io(format!("mkdir {}: {e}", meta_dir.display())))?;
    std::fs::create_dir_all(&targets_dir)
        .map_err(|e| CoreError::Io(format!("mkdir {}: {e}", targets_dir.display())))?;
    let root = load_current_root(&meta_dir)?;
    root.signed.keyid_check()?;
    let tg_role = root.signed.role("targets")?.clone();
    let snap_role = root.signed.role("snapshot")?.clone();
    let ts_role = root.signed.role("timestamp")?.clone();
    check_seed_threshold("targets", &tg_role, targets_keys)?;
    check_seed_threshold("snapshot", &snap_role, snapshot_keys)?;
    check_seed_threshold("timestamp", &ts_role, timestamp_keys)?;

    // Delegações do `targets.json` anterior (preservadas).
    let prev_top: Option<Envelope<TargetsSigned>> = {
        let p = meta_dir.join("targets.json");
        if p.is_file() {
            let raw = std::fs::read(&p).map_err(|e| CoreError::Io(e.to_string()))?;
            Some(
                serde_json::from_slice(&raw)
                    .map_err(|e| CoreError::Package(format!("targets.json inválido: {e}")))?,
            )
        } else {
            None
        }
    };
    let mut delegation_keys: HashMap<String, TufKey> = HashMap::new();
    let mut delegation_roles: HashMap<String, DelegatedRole> = HashMap::new();
    if let Some(prev) = &prev_top
        && let Some(dels) = &prev.signed.delegations
    {
        for (id, k) in &dels.keys {
            delegation_keys.insert(id.clone(), k.clone());
        }
        for r in &dels.roles {
            delegation_roles.insert(r.name.clone(), r.clone());
        }
    }

    // Delegados presentes no diretório (`publisher:<pub>.json` ou cópia
    // versionada `N.publisher:<pub>.json`).
    let mut role_names: Vec<String> = vec![];
    for entry in std::fs::read_dir(&meta_dir).map_err(|e| CoreError::Io(e.to_string()))? {
        let entry = entry.map_err(|e| CoreError::Io(e.to_string()))?;
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(stem) = name.strip_suffix(".json") else {
            continue;
        };
        let base = match stem.split_once('.') {
            Some((n, rest)) if n.bytes().all(|c| c.is_ascii_digit()) => rest,
            _ => stem,
        };
        let role = file_role(base);
        #[allow(clippy::manual_contains)]
        if role.starts_with("publisher:") && !role_names.iter().any(|r| *r == role) {
            role_names.push(role);
        }
    }
    role_names.sort();

    let mut delegated_envs: HashMap<String, Envelope<TargetsSigned>> = HashMap::new();
    for role_name in &role_names {
        let Some(publisher) = role_name.strip_prefix("publisher:") else {
            continue;
        };
        let prev_role = delegation_roles.get(role_name);
        let paths = prev_role
            .map(|r| r.paths.clone())
            .unwrap_or_else(|| vec![format!("{publisher}/*"), format!("index/{publisher}/*")]);
        let threshold = prev_role.map(|r| r.threshold).unwrap_or(1);
        let reg = load_publisher_keys(publishers_dir, publisher)?;
        let mut rkeys = HashMap::new();
        let mut rids = vec![];
        for (id, k) in &reg {
            rkeys.insert(id.clone(), k.clone());
            rids.push(id.clone());
        }
        let plain = meta_dir.join(format!("{}.json", role_file(role_name)));
        let raw = if plain.is_file() {
            std::fs::read(&plain).map_err(|e| CoreError::Io(e.to_string()))?
        } else {
            // Só havia cópia versionada: usa a de maior versão.
            let mut best: Option<(u32, Vec<u8>)> = None;
            for entry in std::fs::read_dir(&meta_dir).map_err(|e| CoreError::Io(e.to_string()))? {
                let entry = entry.map_err(|e| CoreError::Io(e.to_string()))?;
                let n = entry.file_name().to_string_lossy().to_string();
                let suffix = format!(".{}.json", role_file(role_name));
                let Some(v) = n.strip_suffix(&suffix) else {
                    continue;
                };
                let Ok(num) = v.parse::<u32>() else {
                    continue;
                };
                let b = std::fs::read(entry.path()).map_err(|e| CoreError::Io(e.to_string()))?;
                if best.as_ref().is_none_or(|(v, _)| num > *v) {
                    best = Some((num, b));
                }
            }
            best.map(|(_, b)| b)
                .ok_or_else(|| CoreError::Package(format!("{role_name} ilegível")))?
        };
        let env: Envelope<TargetsSigned> = serde_json::from_slice(&raw)
            .map_err(|e| CoreError::Package(format!("{role_name} inválido: {e}")))?;
        let v = serde_json::to_value(&env.signed)
            .map_err(|e| CoreError::Package(format!("{role_name}: {e}")))?;
        let drole = Role {
            keyids: rids.clone(),
            threshold,
        };
        verify_role(&rkeys, &drole, &v, &env.signatures, role_name)?;
        for path in env.signed.targets.keys() {
            if !role_allows(&paths, path) {
                return Err(CoreError::SignatureInvalid(format!(
                    "tuf threshold: {role_name} assinou fora do prefixo permitido ({path})"
                )));
            }
        }
        for (id, k) in &reg {
            delegation_keys.insert(id.clone(), k.clone());
        }
        delegation_roles.insert(
            role_name.clone(),
            DelegatedRole {
                name: role_name.clone(),
                keyids: rids,
                threshold,
                paths,
                terminating: true,
            },
        );
        delegated_envs.insert(role_name.clone(), env);
    }

    // Alvos extras: bytes em `targets/` + entrada no papel `targets`.
    let mut top_targets_map: HashMap<String, TargetFile> = HashMap::new();
    for (rel, bytes) in extra_targets {
        let rel = rel.trim_start_matches('/').to_string();
        if rel.is_empty() || rel.split('/').any(|p| p == ".." || p.is_empty()) {
            return Err(CoreError::Package(format!("alvo extra inválido: {rel:?}")));
        }
        let dst = targets_dir.join(&rel);
        if let Some(p) = dst.parent() {
            std::fs::create_dir_all(p).map_err(|e| CoreError::Io(e.to_string()))?;
        }
        std::fs::write(&dst, bytes).map_err(|e| CoreError::Io(e.to_string()))?;
        top_targets_map.insert(rel, target_file_of(bytes));
    }

    // Catálogo a partir dos índices em `custom` (spec §1.1).
    let catalog = catalog_from_delegated(&delegated_envs);
    let catalog_bytes =
        serde_json::to_vec(&catalog).map_err(|e| CoreError::Internal(e.to_string()))?;
    profile::atomic_write(&targets_dir.join("catalog.json"), &catalog_bytes)?;
    top_targets_map.insert("catalog.json".to_string(), target_file_of(&catalog_bytes));

    // Papel `targets` (versão+1, expira real).
    let tg_version = prev_top.as_ref().map(|p| p.signed.version + 1).unwrap_or(1);
    let mut roles_sorted: Vec<DelegatedRole> = delegation_roles.values().cloned().collect();
    roles_sorted.sort_by(|a, b| a.name.cmp(&b.name));
    let tg_expires = rfc3339_in_days(policy.targets_days);
    let top_signed = TargetsSigned {
        typ: "targets".to_string(),
        version: tg_version,
        expires: tg_expires.clone(),
        targets: top_targets_map,
        delegations: Some(Delegations {
            keys: delegation_keys,
            roles: roles_sorted,
        }),
    };
    let tg_keyids = keyids_of(targets_keys)?;
    let top_env = sign_envelope(top_signed, targets_keys, &tg_keyids)?;
    let top_bytes =
        serde_json::to_vec_pretty(&top_env).map_err(|e| CoreError::Internal(e.to_string()))?;
    write_meta_pair(&meta_dir, "targets.json", tg_version, &top_bytes)?;

    // Papel `snapshot`: `targets.json` + todos os delegados presentes.
    let mut snap_meta: HashMap<String, MetaFile> = HashMap::new();
    snap_meta.insert(
        "targets.json".to_string(),
        MetaFile {
            version: tg_version,
            length: Some(top_bytes.len() as u64),
            hashes: Some(HashMap::from([("sha256".to_string(), sha_hex(&top_bytes))])),
        },
    );
    let mut dnames: Vec<String> = delegated_envs.keys().cloned().collect();
    dnames.sort();
    for dn in &dnames {
        let fname = format!("{}.json", role_file(dn));
        let raw = std::fs::read(meta_dir.join(&fname))
            .map_err(|e| CoreError::Io(format!("ler {fname}: {e}")))?;
        let dv = delegated_envs[dn].signed.version;
        snap_meta.insert(
            fname,
            MetaFile {
                version: dv,
                length: Some(raw.len() as u64),
                hashes: Some(HashMap::from([("sha256".to_string(), sha_hex(&raw))])),
            },
        );
    }
    let snap_path = meta_dir.join("snapshot.json");
    let mut snap_version = 1u32;
    if snap_path.is_file() {
        let raw = std::fs::read(&snap_path).map_err(|e| CoreError::Io(e.to_string()))?;
        let prev: Envelope<SnapshotSigned> = serde_json::from_slice(&raw)
            .map_err(|e| CoreError::Package(format!("snapshot.json inválido: {e}")))?;
        snap_version = prev.signed.version + 1;
    }
    let snap_expires = rfc3339_in_days(policy.snapshot_days);
    let snap_signed = SnapshotSigned {
        typ: "snapshot".to_string(),
        version: snap_version,
        expires: snap_expires.clone(),
        meta: snap_meta,
    };
    let snap_keyids = keyids_of(snapshot_keys)?;
    let snap_env = sign_envelope(snap_signed, snapshot_keys, &snap_keyids)?;
    let snap_bytes =
        serde_json::to_vec_pretty(&snap_env).map_err(|e| CoreError::Internal(e.to_string()))?;
    write_meta_pair(&meta_dir, "snapshot.json", snap_version, &snap_bytes)?;

    // Papel `timestamp`: aponta p/ o snapshot novo.
    let ts_path = meta_dir.join("timestamp.json");
    let mut ts_version = 1u32;
    if ts_path.is_file() {
        let raw = std::fs::read(&ts_path).map_err(|e| CoreError::Io(e.to_string()))?;
        let prev: Envelope<TimestampSigned> = serde_json::from_slice(&raw)
            .map_err(|e| CoreError::Package(format!("timestamp.json inválido: {e}")))?;
        ts_version = prev.signed.version + 1;
    }
    let ts_expires = rfc3339_in_days(policy.timestamp_days);
    let ts_signed = TimestampSigned {
        typ: "timestamp".to_string(),
        version: ts_version,
        expires: ts_expires.clone(),
        meta: HashMap::from([(
            "snapshot.json".to_string(),
            MetaFile {
                version: snap_version,
                length: Some(snap_bytes.len() as u64),
                hashes: Some(HashMap::from([(
                    "sha256".to_string(),
                    sha_hex(&snap_bytes),
                )])),
            },
        )]),
    };
    let ts_keyids = keyids_of(timestamp_keys)?;
    let ts_env = sign_envelope(ts_signed, timestamp_keys, &ts_keyids)?;
    let ts_bytes =
        serde_json::to_vec_pretty(&ts_env).map_err(|e| CoreError::Internal(e.to_string()))?;
    profile::atomic_write(&meta_dir.join("timestamp.json"), &ts_bytes)?;

    Ok(format!(
        "loja v2 ok: targets v{tg_version}, snapshot v{snap_version}, timestamp v{ts_version} ({} delegado(s))",
        delegated_envs.len()
    ))
}

/// Só timestamp (spec store-v2 §8 item 3): re-assina `timestamp.json` com
/// versão+1 e expiração nova, sem tocar nos demais — usado pelo cron diário.
// spec: [ST2-2]
pub fn refresh_timestamp_only(
    v2_dir: &Path,
    timestamp_keys: &[[u8; 32]],
    timestamp_days: i64,
) -> Result<String, CoreError> {
    let meta_dir = v2_dir.join("metadata");
    let root = load_current_root(&meta_dir)?;
    let ts_role = root.signed.role("timestamp")?.clone();
    check_seed_threshold("timestamp", &ts_role, timestamp_keys)?;
    let ts_path = meta_dir.join("timestamp.json");
    let raw = std::fs::read(&ts_path).map_err(|e| CoreError::Io(e.to_string()))?;
    let prev: Envelope<TimestampSigned> = serde_json::from_slice(&raw)
        .map_err(|e| CoreError::Package(format!("timestamp.json inválido: {e}")))?;
    let version = prev.signed.version + 1;
    let expires = rfc3339_in_days(timestamp_days);
    let signed = TimestampSigned {
        typ: "timestamp".to_string(),
        version,
        expires: expires.clone(),
        meta: prev.signed.meta,
    };
    let keyids = keyids_of(timestamp_keys)?;
    let env = sign_envelope(signed, timestamp_keys, &keyids)?;
    let bytes = serde_json::to_vec_pretty(&env).map_err(|e| CoreError::Internal(e.to_string()))?;
    profile::atomic_write(&meta_dir.join("timestamp.json"), &bytes)?;
    Ok(format!("timestamp v{version} (expira {expires})"))
}

/// Bootstrap do root genesis (primeira publicação; spec §2): grava
/// `1.root.json` com os 4 papéis (`root` N-de-M, demais 1-de-1), expira
/// real de 365 dias e `consistent_snapshot = false`. Recusa se já existir.
/// pelo limiar do root N e auto-assinado pelo novo (keyids novos entram no
/// mapa de chaves; limiar e demais papéis preservados, expira real de 1 ano).
// spec: [ST2-1]
pub fn init_root(
    v2_dir: &Path,
    root_keys: &[[u8; 32]],
    root_threshold: u32,
    targets_keys: &[[u8; 32]],
    snapshot_keys: &[[u8; 32]],
    timestamp_keys: &[[u8; 32]],
) -> Result<String, CoreError> {
    let meta_dir = v2_dir.join("metadata");
    std::fs::create_dir_all(&meta_dir)
        .map_err(|e| CoreError::Io(format!("mkdir {}: {e}", meta_dir.display())))?;
    if meta_dir.join("1.root.json").is_file() {
        return Err(CoreError::Package(
            "1.root.json já existe (use rotate_root)".to_string(),
        ));
    }
    if root_keys.is_empty()
        || targets_keys.is_empty()
        || snapshot_keys.is_empty()
        || timestamp_keys.is_empty()
    {
        return Err(CoreError::Package(
            "init_root exige --root-key/--targets-key/--snapshot-key/--timestamp-key".to_string(),
        ));
    }
    let mut root_ids: Vec<String> = keyids_of(root_keys)?;
    root_ids.sort();
    root_ids.dedup();
    let threshold = root_threshold.max(1).min(root_ids.len() as u32);
    if (root_ids.len() as u32) < threshold {
        return Err(CoreError::SignatureInvalid(format!(
            "tuf threshold: root genesis: {} chave(s) p/ limiar {threshold}",
            root_ids.len(),
        )));
    }
    let mut keys: HashMap<String, TufKey> = HashMap::new();
    for s in root_keys
        .iter()
        .chain(targets_keys)
        .chain(snapshot_keys)
        .chain(timestamp_keys)
    {
        let (id, k) = tuf_key_of(s)?;
        keys.entry(id).or_insert(k);
    }
    let role = |ids: Vec<String>, t: u32| Role {
        keyids: ids,
        threshold: t.max(1),
    };
    let roles = HashMap::from([
        ("root".to_string(), role(root_ids.clone(), threshold)),
        ("targets".to_string(), role(keyids_of(targets_keys)?, 1)),
        ("snapshot".to_string(), role(keyids_of(snapshot_keys)?, 1)),
        ("timestamp".to_string(), role(keyids_of(timestamp_keys)?, 1)),
    ]);
    let expires = rfc3339_in_days(365);
    let signed = RootSigned {
        typ: "root".to_string(),
        version: 1,
        expires: expires.clone(),
        keys,
        roles,
        consistent_snapshot: false,
    };
    let env = sign_envelope(signed, root_keys, &root_ids)?;
    env.check_self_signed()?;
    let bytes = serde_json::to_vec_pretty(&env).map_err(|e| CoreError::Internal(e.to_string()))?;
    profile::atomic_write(&meta_dir.join("1.root.json"), &bytes)?;
    Ok(format!(
        "root v1 (limiar root {threshold}/{}; expira {expires})",
        root_keys.len()
    ))
}

/// Rotação de root (spec store-v2 §8 item 4): grava `N+1.root.json` assinado
/// pelo limiar do root N e auto-assinado pelo novo (keyids novos entram no
/// mapa de chaves; limiar e demais papéis preservados, expira real de 1 ano).
// spec: [ST2-1]
pub fn rotate_root(
    v2_dir: &Path,
    prev_root_keys: &[[u8; 32]],
    next_root_keys: &[[u8; 32]],
) -> Result<String, CoreError> {
    let meta_dir = v2_dir.join("metadata");
    let prev = load_current_root(&meta_dir)?;
    prev.signed.keyid_check()?;
    let root_role = prev.signed.role("root")?.clone();
    check_seed_threshold("root", &root_role, prev_root_keys)?;
    if next_root_keys.is_empty() {
        return Err(CoreError::Package(
            "sem chaves novas p/ rotação".to_string(),
        ));
    }
    let mut next_ids: Vec<String> = keyids_of(next_root_keys)?;
    next_ids.sort();
    next_ids.dedup();
    if next_ids.len() < root_role.threshold.max(1) as usize {
        return Err(CoreError::SignatureInvalid(format!(
            "tuf threshold: root novo: {} chave(s) p/ limiar {}",
            next_ids.len(),
            root_role.threshold.max(1),
        )));
    }

    let mut keys = prev.signed.keys.clone();
    for s in next_root_keys {
        let (id, k) = tuf_key_of(s)?;
        keys.insert(id, k);
    }
    let mut roles = prev.signed.roles.clone();
    roles.insert(
        "root".to_string(),
        Role {
            keyids: next_ids.clone(),
            threshold: root_role.threshold,
        },
    );
    let version = prev.signed.version + 1;
    let expires = rfc3339_in_days(365);
    let signed = RootSigned {
        typ: "root".to_string(),
        version,
        expires: expires.clone(),
        keys,
        roles,
        consistent_snapshot: prev.signed.consistent_snapshot,
    };
    // Assinaturas do limiar antigo (cadeia) + do novo (auto-assinatura).
    let mut all_seeds: Vec<[u8; 32]> =
        Vec::with_capacity(prev_root_keys.len() + next_root_keys.len());
    all_seeds.extend_from_slice(prev_root_keys);
    all_seeds.extend_from_slice(next_root_keys);
    let mut all_ids = keyids_of(prev_root_keys)?;
    all_ids.extend(next_ids.iter().cloned());
    let env = sign_envelope(signed, &all_seeds, &all_ids)?;
    // Confere a cadeia antes de gravar.
    env.check_self_signed()?;
    {
        let v = serde_json::to_value(&env.signed)
            .map_err(|e| CoreError::Package(format!("root: {e}")))?;
        verify_role(&prev.signed.keys, &root_role, &v, &env.signatures, "root")?;
    }
    let bytes = serde_json::to_vec_pretty(&env).map_err(|e| CoreError::Internal(e.to_string()))?;
    profile::atomic_write(&meta_dir.join(format!("{version}.root.json")), &bytes)?;
    Ok(format!("root v{version} (expira {expires})"))
}

/// Verificação só-leitura (spec store-v2 §8 item 5): roda `[ST2-1..5]` sobre
/// o diretório (cadeia, limiar, expiração real, hashes, prefixo, catálogo).
/// Sem rede, sem escrita; usada no CI de PR (`--check-only`).
// spec: [ST2-1]
// spec: [ST2-2]
// spec: [ST2-3]
// spec: [ST2-4]
// spec: [ST2-5]
pub fn check_repo(v2_dir: &Path) -> Result<String, CoreError> {
    let meta_dir = v2_dir.join("metadata");
    let targets_dir = v2_dir.join("targets");

    // --- Cadeia de roots (`[ST2-1]`): `1.root.json` + cada `N+1` assinado
    // pelo anterior e auto-assinado.
    let mut versions: Vec<u32> = vec![];
    for entry in std::fs::read_dir(&meta_dir)
        .map_err(|e| CoreError::Package(format!("ler {}: {e}", meta_dir.display())))?
    {
        let entry = entry.map_err(|e| CoreError::Io(e.to_string()))?;
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(stem) = name.strip_suffix(".root.json") else {
            continue;
        };
        let Ok(v) = stem.parse::<u32>() else {
            continue;
        };
        versions.push(v);
    }
    versions.sort();
    if versions.first() != Some(&1) {
        return Err(CoreError::Package("sem 1.root.json".to_string()));
    }
    let mut root: Option<Root> = None;
    for (i, v) in versions.iter().enumerate() {
        if i > 0 && *v != versions[i - 1] + 1 {
            return Err(CoreError::Package(format!(
                "cadeia de root com salto (falta {}.root.json)",
                versions[i - 1] + 1
            )));
        }
        let raw = std::fs::read(meta_dir.join(format!("{v}.root.json")))
            .map_err(|e| CoreError::Io(e.to_string()))?;
        let next: Root = serde_json::from_slice(&raw)
            .map_err(|e| CoreError::Package(format!("{v}.root.json inválido: {e}")))?;
        next.check_self_signed()?;
        if let Some(prev) = &root {
            let val = serde_json::to_value(&next.signed)
                .map_err(|e| CoreError::Package(format!("{v}.root.json: {e}")))?;
            let prev_role = prev.signed.role("root")?.clone();
            verify_role(
                &prev.signed.keys,
                &prev_role,
                &val,
                &next.signatures,
                "root",
            )?;
        }
        root = Some(next);
    }
    let root = root.expect("cadeia com 1.root.json");
    let keys = root.signed.keys.clone();
    let consistent = root.signed.consistent_snapshot;

    // --- Timestamp → snapshot → targets (`[ST2-1]`, `[ST2-2]`): limiar,
    // expiração real, versão == anunciada (senão `tuf rollback:`).
    let ts_role = root.signed.role("timestamp")?.clone();
    let snap_role = root.signed.role("snapshot")?.clone();
    let tg_role = root.signed.role("targets")?.clone();
    let ts_raw = std::fs::read(meta_dir.join("timestamp.json"))
        .map_err(|e| CoreError::Package(format!("timestamp.json ausente: {e}")))?;
    let ts: Envelope<TimestampSigned> = verify_envelope(&keys, &ts_role, &ts_raw, "timestamp")?;
    let snap_meta = ts
        .signed
        .meta
        .get("snapshot.json")
        .ok_or_else(|| CoreError::Package("timestamp sem snapshot.json".to_string()))?;
    let snap_name = if consistent {
        format!("{}.snapshot.json", snap_meta.version)
    } else {
        "snapshot.json".to_string()
    };
    let snap_raw = std::fs::read(meta_dir.join(&snap_name))
        .map_err(|e| CoreError::Package(format!("{snap_name} ausente: {e}")))?;
    verify_meta_against(snap_meta, &snap_raw, &snap_name)?;
    let snap: Envelope<SnapshotSigned> = verify_envelope(&keys, &snap_role, &snap_raw, "snapshot")?;
    if snap.signed.version != snap_meta.version {
        return Err(CoreError::SignatureInvalid(format!(
            "tuf rollback: snapshot versão {} != {} do timestamp",
            snap.signed.version, snap_meta.version
        )));
    }
    let tg_meta = snap
        .signed
        .meta
        .get("targets.json")
        .ok_or_else(|| CoreError::Package("snapshot sem targets.json".to_string()))?;
    let tg_name = if consistent {
        format!("{}.targets.json", tg_meta.version)
    } else {
        "targets.json".to_string()
    };
    let tg_raw = std::fs::read(meta_dir.join(&tg_name))
        .map_err(|e| CoreError::Package(format!("{tg_name} ausente: {e}")))?;
    verify_meta_against(tg_meta, &tg_raw, &tg_name)?;
    let tg: Envelope<TargetsSigned> = verify_envelope(&keys, &tg_role, &tg_raw, "targets")?;
    if tg.signed.version != tg_meta.version {
        return Err(CoreError::SignatureInvalid(format!(
            "tuf rollback: targets versão {} != {} do snapshot",
            tg.signed.version, tg_meta.version
        )));
    }

    // --- Delegados (`[ST2-1]`, `[ST2-5]` prefixo, `[ST2-3]`/`[ST2-4]` alvos).
    let mut n_deleg = 0u32;
    let mut index_names: HashMap<String, String> = HashMap::new();
    if let Some(dels) = &tg.signed.delegations {
        for role in &dels.roles {
            if role.name.strip_prefix("publisher:").is_none() {
                continue;
            }
            let meta_key = format!("{}.json", role_file(&role.name));
            let Some(meta) = snap.signed.meta.get(&meta_key) else {
                return Err(CoreError::Package(format!(
                    "delegação {} sem meta no snapshot",
                    role.name
                )));
            };
            let rel_name = if consistent {
                format!("{}.{}.json", meta.version, role_file(&role.name))
            } else {
                format!("{}.json", role_file(&role.name))
            };
            let raw = std::fs::read(meta_dir.join(&rel_name))
                .map_err(|e| CoreError::Package(format!("{rel_name} ausente: {e}")))?;
            verify_meta_against(meta, &raw, &rel_name)?;
            let role_keys: HashMap<String, TufKey> = dels
                .keys
                .iter()
                .filter(|(id, _)| role.keyids.contains(id))
                .map(|(id, k)| (id.clone(), k.clone()))
                .collect();
            let drole = Role {
                keyids: role.keyids.clone(),
                threshold: role.threshold,
            };
            let env: Envelope<TargetsSigned> =
                verify_envelope(&role_keys, &drole, &raw, "delegated")?;
            if env.signed.version != meta.version {
                return Err(CoreError::SignatureInvalid(format!(
                    "tuf rollback: {} versão {} != {} do snapshot",
                    role.name, env.signed.version, meta.version
                )));
            }
            for path in env.signed.targets.keys() {
                if !role_allows(&role.paths, path) {
                    return Err(CoreError::SignatureInvalid(format!(
                        "tuf threshold: {} assinou fora do prefixo permitido ({path})",
                        role.name
                    )));
                }
            }
            for (path, tf) in &env.signed.targets {
                let meta_file = MetaFile {
                    version: 1,
                    length: Some(tf.length),
                    hashes: Some(tf.hashes.clone()),
                };
                if path.starts_with("index/") && path.ends_with(".json") {
                    // spec §1.1: índice lido de `custom`; bytes servidos em
                    // `targets/` (quando existirem) conferem com o alvo.
                    let custom = tf.custom.clone().unwrap_or(serde_json::Value::Null);
                    let index: IndexFile = serde_json::from_value(custom).map_err(|e| {
                        CoreError::Package(format!("índice {path} inválido em {}: {e}", role.name))
                    })?;
                    index_names.insert(index.name.clone(), role.name.clone());
                    // `[ST2-4]`: artefatos do índice têm alvo no papel.
                    for v in &index.versions {
                        for a in &v.artifacts {
                            let rel = artifact_target_path(&index.name, &v.version, a);
                            if !env.signed.targets.contains_key(&rel) {
                                return Err(CoreError::SignatureInvalid(format!(
                                    "artefato {rel} ausente no papel {}",
                                    role.name
                                )));
                            }
                        }
                    }
                    let served = targets_dir.join(path);
                    if served.is_file() {
                        let b = std::fs::read(&served).map_err(|e| CoreError::Io(e.to_string()))?;
                        verify_meta_against(&meta_file, &b, path)?;
                    }
                } else {
                    let b = std::fs::read(targets_dir.join(path))
                        .map_err(|e| CoreError::Package(format!("alvo {path} ausente: {e}")))?;
                    verify_meta_against(&meta_file, &b, path)?;
                }
            }
            n_deleg += 1;
        }
    }

    // --- Catálogo (`targets/catalog.json`): bytes vs meta + entradas
    // resolvem p/ índices dos delegados.
    let cat_tf = tg
        .signed
        .targets
        .get("catalog.json")
        .ok_or_else(|| CoreError::Package("targets sem catalog.json".to_string()))?;
    let cat_bytes = std::fs::read(targets_dir.join("catalog.json"))
        .map_err(|e| CoreError::Package(format!("targets/catalog.json ausente: {e}")))?;
    verify_meta_against(
        &MetaFile {
            version: 1,
            length: Some(cat_tf.length),
            hashes: Some(cat_tf.hashes.clone()),
        },
        &cat_bytes,
        "catalog.json",
    )?;
    let catalog: Vec<CatalogEntry> = serde_json::from_slice(&cat_bytes)
        .map_err(|e| CoreError::Package(format!("catalog.json inválido: {e}")))?;
    for e in &catalog {
        if !index_names.contains_key(&e.name) {
            return Err(CoreError::Package(format!(
                "catálogo cita {} sem índice delegado",
                e.name
            )));
        }
    }

    Ok(format!(
        "v2 ok: root v{}, timestamp v{}, snapshot v{}, targets v{} ({n_deleg} delegado(s), {} pacote(s))",
        root.signed.version,
        ts.signed.version,
        snap.signed.version,
        tg.signed.version,
        catalog.len()
    ))
}
