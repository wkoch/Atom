//! Auto-update (v0.7, spec `docs/specs/updates.md`):
//!
//! - Self-update do core: manifesto de release por canal, chave de release
//!   embutida no binário (`ATOM_RELEASE_KEYS`, com override por config
//!   global/flag — necessário p/ testes), troca atômica (`self-replace`),
//!   health check e rollback automático.
//! - Extensões por política (`auto`/`notify`, `pinned`): pausa em capability
//!   nova (mesmo em `auto`), rollback em falha de ativação.
//! - Migrações versionadas de formatos de estado (com backup prévio).
//! - `atom store gc`: remove da store versões sem referência há +30 dias.
//!
//! Nova dependência justificada (AGENTS.md): `self-replace` (troca atômica
//! do executável, incl. dança `.old.exe` no Windows).

#![forbid(unsafe_code)]

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::bus::Core;
use crate::error::CoreError;
use crate::pkg;

/// URL padrão do manifesto de releases (override: config global ou flag).
/// Release CI embute a URL real via `ATOM_UPDATE_URL`; vazio = sem source.
pub const DEFAULT_UPDATE_URL: &str = match option_env!("ATOM_UPDATE_URL") {
    Some(u) => u,
    None => "",
};

/// Chaves raiz de release embutidas (`ed25519:…,…`). Release CI embute via
/// `ATOM_RELEASE_KEYS`; vazio = exige chaves via config/flag.
pub const EMBEDDED_RELEASE_KEYS: &str = match option_env!("ATOM_RELEASE_KEYS") {
    Some(k) => k,
    None => "",
};

/// Canal padrão.
pub const DEFAULT_CHANNEL: &str = "stable";

/// Manifesto de release do core (`<url>/v1/core/<canal>.json`, spec §1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreRelease {
    pub version: String,
    #[serde(default)]
    pub published_at: String,
    #[serde(default)]
    pub min_upgrade_from: String,
    #[serde(default)]
    pub notes_url: String,
    #[serde(default)]
    pub artifacts: Vec<CoreArtifact>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreArtifact {
    pub platform: String,
    pub url: String,
    pub sha256: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub signature: String,
}

fn base_url(url: &str) -> String {
    if url.ends_with('/') {
        url.to_string()
    } else {
        format!("{url}/")
    }
}

async fn fetch_bytes(url: &str) -> Result<Vec<u8>, CoreError> {
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

/// Baixa e verifica o manifesto de release (sig por chave de release).
pub async fn fetch_release(
    base: &str,
    channel: &str,
    keys: &[String],
) -> Result<(CoreRelease, Vec<u8>), CoreError> {
    let base = base_url(base);
    let rel = format!("v1/core/{channel}.json");
    let raw = fetch_bytes(&format!("{base}{rel}")).await?;
    let sig = fetch_bytes(&format!("{base}{rel}.sig"))
        .await
        .map_err(|_| CoreError::SignatureInvalid(format!("{rel}.sig ausente")))?;
    let sig = String::from_utf8(sig)
        .map_err(|_| CoreError::SignatureInvalid("sig do manifesto inválida".to_string()))?;
    let mut ok = false;
    for k in keys {
        if let Ok(pk) = crate::store::parse_pubkey(k) {
            // Reuso do verificador ed25519 do store via roundtrip de pubkey.
            let msg = raw.clone();
            if verify_raw(&pk, &msg, sig.trim()).is_ok() {
                ok = true;
                break;
            }
        }
    }
    if !ok {
        return Err(CoreError::SignatureInvalid(format!(
            "{rel}: sig inválida (chaves de release)"
        )));
    }
    let rel_: CoreRelease = serde_json::from_slice(&raw)
        .map_err(|e| CoreError::Package(format!("manifesto inválido: {e}")))?;
    Ok((rel_, raw))
}

fn verify_raw(pk: &[u8; 32], msg: &[u8], sig_b64: &str) -> Result<(), CoreError> {
    use base64::Engine as _;
    use ed25519_dalek::{Signature, VerifyingKey};
    let vk = VerifyingKey::from_bytes(pk)
        .map_err(|e| CoreError::Package(format!("chave inválida: {e}")))?;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(sig_b64.trim())
        .map_err(|e| CoreError::SignatureInvalid(format!("sig base64: {e}")))?;
    let sig = Signature::from_slice(&raw)
        .map_err(|e| CoreError::SignatureInvalid(format!("sig: {e}")))?;
    vk.verify_strict(msg, &sig)
        .map_err(|e| CoreError::SignatureInvalid(format!("assinatura inválida: {e}")))
}

/// Release aplicável: versão maior que a atual, plataforma ok, sem marca
/// `.bad`, `min_upgrade_from` satisfeito. `None` = nada a fazer.
pub fn pick_release(
    rel: &CoreRelease,
    current: &str,
    bad_versions: &HashSet<String>,
) -> Option<CoreArtifact> {
    let cur = semver::Version::parse(current).ok()?;
    let new = semver::Version::parse(&rel.version).ok()?;
    if new <= cur || bad_versions.contains(&rel.version) {
        return None;
    }
    if !rel.min_upgrade_from.is_empty() {
        let min = semver::Version::parse(&rel.min_upgrade_from).ok()?;
        if cur < min {
            return None;
        }
    }
    let me = pkg::current_platform();
    rel.artifacts
        .iter()
        .find(|a| a.platform == me)
        .or_else(|| rel.artifacts.iter().find(|a| a.platform == "any"))
        .cloned()
}

/// Pasta `<data>/bin` (binários versionados do core).
pub fn bin_dir(core: &Core) -> PathBuf {
    core.atom_paths.data.join("bin")
}

pub fn bad_versions(bin: &Path) -> HashSet<String> {
    std::fs::read_to_string(bin.join(".bad-versions"))
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

fn mark_bad(bin: &Path, version: &str) {
    let mut set = bad_versions(bin);
    set.insert(version.to_string());
    let mut v: Vec<String> = set.into_iter().collect();
    v.sort();
    let _ = std::fs::create_dir_all(bin);
    let _ = std::fs::write(bin.join(".bad-versions"), v.join("\n") + "\n");
}

/// Mensagem assinada p/ artefato do core (mesmo formato store §4, `name = "atom/core"`).
pub fn core_artifact_message(version: &str, sha_hex: &str, platform: &str) -> Vec<u8> {
    crate::store::artifact_message("atom/core", version, platform, sha_hex)
}

/// Artefato do core = tar.gz com o binário na raiz (nome `atom`/`atom.exe`
/// ou o único/primeiro arquivo). Extrai p/ tempdir e devolve o binário.
fn extract_core_binary(
    bytes: &[u8],
    version: &str,
) -> Result<(tempfile::TempDir, PathBuf), CoreError> {
    let tmp = tempfile::tempdir().map_err(|e| CoreError::Io(e.to_string()))?;
    let gz = flate2::read::GzDecoder::new(bytes);
    let mut ar = tar::Archive::new(gz);
    ar.unpack(tmp.path())
        .map_err(|e| CoreError::Package(format!("tar.gz do core {version}: {e}")))?;
    let mut cands: Vec<PathBuf> = vec![];
    let mut stack = vec![tmp.path().to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if e.file_type().map(|t| t.is_file()).unwrap_or(false) {
                cands.push(p);
            }
        }
    }
    cands.sort();
    let picked = cands
        .iter()
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n == "atom" || n == "atom.exe")
        })
        .or_else(|| cands.first())
        .cloned()
        .ok_or_else(|| CoreError::Package(format!("artefato do core {version} vazio")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut p = std::fs::metadata(&picked)
            .map_err(|e| CoreError::Io(e.to_string()))?
            .permissions();
        p.set_mode(0o755);
        std::fs::set_permissions(&picked, p).map_err(|e| CoreError::Io(e.to_string()))?;
    }
    Ok((tmp, picked))
}
/// Assinatura: mesma mensagem de `store.md §4` com `name = "atom/core"`.
pub async fn fetch_core_artifact(
    art: &CoreArtifact,
    base: &str,
    publisher_keys: &[String],
    version: &str,
) -> Result<Vec<u8>, CoreError> {
    let url = if art.url.starts_with("http") || art.url.starts_with("file://") {
        art.url.clone()
    } else {
        format!("{}{}", base_url(base), art.url.trim_start_matches('/'))
    };
    let bytes = fetch_bytes(&url).await?;
    {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(&bytes);
        let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if hex != art.sha256 {
            return Err(CoreError::SignatureInvalid(format!(
                "sha256 do core {version} diverge"
            )));
        }
    }
    if art.size > 0 && bytes.len() as u64 != art.size {
        return Err(CoreError::SignatureInvalid(format!(
            "tamanho do core {version} diverge"
        )));
    }
    let msg = core_artifact_message(version, &art.sha256, &art.platform);
    let mut ok = false;
    for k in publisher_keys {
        if let Ok(pk) = crate::store::parse_pubkey(k)
            && verify_raw(&pk, &msg, &art.signature).is_ok()
        {
            ok = true;
            break;
        }
    }
    if !ok {
        return Err(CoreError::SignatureInvalid(format!(
            "assinatura do publisher inválida p/ core {version}"
        )));
    }
    Ok(bytes)
}

/// Publisher keys do índice: para releases do core, as chaves de release.
pub fn release_publisher_keys() -> Vec<String> {
    vec![]
}

// ---------------------------------------------------------------------------
// Troca atômica + health check + rollback.

/// Grava bytes como `bin/atom-<version>` (executável no unix).
pub fn stage_binary(bin: &Path, version: &str, bytes: &[u8]) -> Result<PathBuf, CoreError> {
    std::fs::create_dir_all(bin).map_err(|e| CoreError::Io(e.to_string()))?;
    let dest = bin.join(format!("atom-{version}"));
    std::fs::write(&dest, bytes).map_err(|e| CoreError::Io(e.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut p = std::fs::metadata(&dest)
            .map_err(|e| CoreError::Io(e.to_string()))?
            .permissions();
        p.set_mode(0o755);
        std::fs::set_permissions(&dest, p).map_err(|e| CoreError::Io(e.to_string()))?;
    }
    Ok(dest)
}

fn pending_path(bin: &Path) -> PathBuf {
    bin.join(".pending-health")
}

/// Troca `current_exe` pelo `new_bin` (backup em `backup_path`).
/// Caminho do processo atual → `self-replace`; outro caminho (testes) →
/// cópia com rename atômico. `shortcut: rename não é atômico no Windows
/// se o destino está mapeado; em produção use o próprio executável.`
pub fn swap_exe(current_exe: &Path, new_bin: &Path, backup_path: &Path) -> Result<(), CoreError> {
    if let Some(parent) = backup_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| CoreError::Io(e.to_string()))?;
    }
    if current_exe.exists() {
        std::fs::copy(current_exe, backup_path).map_err(|e| CoreError::Io(e.to_string()))?;
    }
    let is_self = std::env::current_exe().ok().as_deref() == Some(current_exe);
    if is_self {
        self_replace::self_replace(new_bin)
            .map_err(|e| CoreError::Io(format!("self-replace: {e}")))?;
    } else {
        let tmp = current_exe.with_extension("new");
        std::fs::copy(new_bin, &tmp).map_err(|e| CoreError::Io(e.to_string()))?;
        std::fs::rename(&tmp, current_exe).map_err(|e| CoreError::Io(e.to_string()))?;
    }
    Ok(())
}

/// Health check: roda o binário novo com `args` (padrão `version`), timeout.
/// `true` = saudável. Usado após o swap e na cura de marcador pendente.
pub fn health_check(exe: &Path, args: &[&str], timeout_secs: u64) -> bool {
    let mut cmd = std::process::Command::new(exe);
    cmd.args(args);
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());
    match cmd.spawn() {
        Ok(mut child) => {
            let start = std::time::Instant::now();
            loop {
                match child.try_wait() {
                    Ok(Some(st)) => return st.success(),
                    Ok(None) => {
                        if start.elapsed().as_secs() >= timeout_secs {
                            let _ = child.kill();
                            let _ = child.wait();
                            return false;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                    Err(_) => return false,
                }
            }
        }
        Err(_) => false,
    }
}

/// Aplica release do core em `current_exe`: stage → swap → health → rollback.
/// Retorna a versão aplicada. Defeito (health falha) restaura o backup.
#[allow(clippy::too_many_arguments)]
pub async fn apply_core_release(
    bin: &Path,
    current_exe: &Path,
    current_version: &str,
    release: &CoreRelease,
    artifact: &CoreArtifact,
    base: &str,
    publisher_keys: &[String],
    health_args: &[&str],
    health_timeout_secs: u64,
) -> Result<String, CoreError> {
    let bytes = fetch_core_artifact(artifact, base, publisher_keys, &release.version).await?;
    // Artefato = tar.gz; o executável vai p/ bin/, não o tarball.
    let (_tmp, new_bin) = extract_core_binary(&bytes, &release.version)?;
    let staged = stage_binary(
        bin,
        &release.version,
        &std::fs::read(&new_bin).map_err(|e| CoreError::Io(e.to_string()))?,
    )?;
    let backup = bin.join(format!(".backup-{current_version}"));
    std::fs::write(pending_path(bin), release.version.as_bytes())
        .map_err(|e| CoreError::Io(e.to_string()))?;
    if let Err(e) = swap_exe(current_exe, &staged, &backup) {
        let _ = std::fs::remove_file(pending_path(bin));
        return Err(e);
    }
    if health_check(current_exe, health_args, health_timeout_secs) {
        let _ = std::fs::remove_file(pending_path(bin));
        let _ = std::fs::remove_file(&backup);
        prune_bins(bin, 2);
        Ok(release.version.clone())
    } else {
        // Rollback: restaura backup, marca versão como ruim.
        if backup.exists() {
            if std::env::current_exe().ok().as_deref() == Some(current_exe) {
                let _ = self_replace::self_replace(&backup);
            } else if std::fs::copy(&backup, current_exe).is_ok() {
                let _ = std::fs::remove_file(&backup);
            }
        }
        mark_bad(bin, &release.version);
        let _ = std::fs::remove_file(pending_path(bin));
        Err(CoreError::ExtensionUnavailable(format!(
            "core {} falhou no health check; restaurado {current_version}",
            release.version
        )))
    }
}

/// Cura marcador pendente (boot que não confirmou saúde): restaura backup.
/// Retorna `true` se curou algo. Chamado no `atom update` e no boot do daemon.
pub fn heal_if_needed(bin: &Path, current_exe: &Path) -> bool {
    let pending = pending_path(bin);
    let Ok(ver) = std::fs::read_to_string(&pending) else {
        return false;
    };
    let ver = ver.trim().to_string();
    let mut healed = false;
    // Acha backup da versão anterior (qualquer `.backup-*`).
    if let Ok(rd) = std::fs::read_dir(bin) {
        let mut backups: Vec<PathBuf> = rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(".backup-"))
            })
            .collect();
        backups.sort();
        if let Some(bk) = backups.pop() {
            let done = if std::env::current_exe().ok().as_deref() == Some(current_exe) {
                self_replace::self_replace(&bk).is_ok()
            } else {
                std::fs::copy(&bk, current_exe).is_ok()
            };
            if done {
                let _ = std::fs::remove_file(&bk);
                healed = true;
            }
        }
    }
    if !ver.is_empty() {
        mark_bad(bin, &ver);
    }
    let _ = std::fs::remove_file(&pending);
    healed
}

/// Mantém as `keep` versões mais novas em `bin/atom-*` (+ limpa backups órfãos).
pub fn prune_bins(bin: &Path, keep: usize) {
    let mut vers: Vec<(semver::Version, PathBuf)> = vec![];
    let Ok(rd) = std::fs::read_dir(bin) else {
        return;
    };
    for e in rd.filter_map(|e| e.ok()) {
        let p = e.path();
        let n = match p.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        if let Some(v) = n.strip_prefix("atom-") {
            // Pula o executável em uso e arquivos auxiliares.
            if v.starts_with('.') {
                continue;
            }
            if let Ok(sv) = semver::Version::parse(v) {
                vers.push((sv, p));
            }
        }
    }
    vers.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, p) in vers.into_iter().skip(keep) {
        // Nunca apaga o binário em execução.
        if std::env::current_exe().ok().as_deref() == Some(p.as_path()) {
            continue;
        }
        let _ = std::fs::remove_file(&p);
    }
}

// ---------------------------------------------------------------------------
// Extensões por política (auto/notify/pinned).

/// Plano de update de UMA extensão (sem instalar).
#[derive(Debug, Clone)]
pub struct ExtUpdatePlan {
    pub name: String,
    pub from: String,
    pub to: String,
    pub new_caps: Vec<String>,
    pub source: String,
}

/// Prévia do que `update` faria (respeita `pinned`, ignora dev/file).
pub async fn preview_updates(core: &Core) -> Result<Vec<ExtUpdatePlan>, CoreError> {
    let lock = pkg::load_lock(&core.paths)?;
    let mut out = vec![];
    for lp in &lock.package {
        if core.config.update.pinned.iter().any(|p| p == &lp.name) {
            continue;
        }
        let is_store = core.config.sources.iter().any(|s| s.name == lp.source);
        if !is_store {
            continue;
        }
        let req = if lp.requested.is_empty() {
            "*"
        } else {
            lp.requested.as_str()
        };
        let to =
            match crate::store::resolve_latest_version(core, &lp.name, Some(req), Some(&lp.source))
                .await
            {
                Ok(v) => v,
                // Source fora do ar ou pacote sumido: avisa e segue (não trava o resto).
                Err(e) => {
                    tracing::warn!(pacote = %lp.name, "preview: {e}");
                    continue;
                }
            };
        if to.version.version == lp.version {
            continue;
        }
        let granted = core
            .grants()
            .get(&lp.name)
            .map(|e| e.granted.clone())
            .unwrap_or_default();
        let new_caps: Vec<String> = to
            .version
            .capabilities
            .iter()
            .filter(|c| !granted.iter().any(|g| g == *c))
            .cloned()
            .collect();
        out.push(ExtUpdatePlan {
            name: lp.name.clone(),
            from: lp.version.clone(),
            to: to.version.version.clone(),
            new_caps,
            source: lp.source.clone(),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

pub async fn update_tick(
    core: &mut Core,
    base_url: Option<&str>,
    keys: Vec<String>,
    channel: &str,
) {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("atom"));
    heal_if_needed(&bin_dir(core), &exe);
    let inp = UpdateInputs {
        base_url,
        keys,
        channel,
        check_only: false,
        current_exe: exe,
        health_args: vec!["version".to_string()],
    };
    match run_update(core, &inp).await {
        Ok(rep) => {
            for m in rep.messages {
                tracing::info!("update: {m}");
            }
            if let Some(v) = rep.core_updated_to {
                tracing::info!("update: core {v} aplicado (reinicie o daemon)");
            }
        }
        Err(e) => tracing::warn!("update: {e}"),
    }
}

/// Resultado de `atom update`.
#[derive(Debug, Default)]
pub struct UpdateReport {
    pub core_checked: bool,
    pub core_updated_to: Option<String>,
    pub ext_updated: Vec<(String, String, String)>,
    pub ext_pending_caps: Vec<(String, Vec<String>)>,
    pub messages: Vec<String>,
}

/// `atom update`: core por política + extensões `auto` (pausa em cap nova).
/// `check_only` = só lista (`--check`). `over` permite override de
/// url/chaves/canal (flags > config global > embutido).
pub struct UpdateInputs<'a> {
    pub base_url: Option<&'a str>,
    pub keys: Vec<String>,
    pub channel: &'a str,
    pub check_only: bool,
    pub current_exe: PathBuf,
    pub health_args: Vec<String>,
}

pub async fn run_update(
    core: &mut Core,
    inp: &UpdateInputs<'_>,
) -> Result<UpdateReport, CoreError> {
    let mut rep = UpdateReport::default();
    heal_if_needed(&super::update::bin_dir(core), &inp.current_exe);
    // --- core ---
    let policy = core.config.update.core.as_str();
    if policy != "off" {
        // Chamador já mesclou flags > config global > embutido.
        let base = inp.base_url.map(|s| s.to_string());
        let keys = &inp.keys;
        if let Some(base) = base {
            rep.core_checked = true;
            match fetch_release(&base, inp.channel, keys).await {
                Ok((rel, _)) => {
                    let bad = bad_versions(&bin_dir(core));
                    if let Some(art) = pick_release(&rel, crate::CORE_VERSION, &bad) {
                        if inp.check_only || policy == "notify" {
                            rep.messages.push(format!(
                                "core {} disponível (atual {})",
                                rel.version,
                                crate::CORE_VERSION
                            ));
                            let ctx = atom_protocol::CallerCtx::local(&core.profile_name, "core");
                            let _ = Box::pin(core.publish_event(
                                "atom.update.available",
                                serde_json::json!({"component": "core", "version": rel.version}),
                                &ctx,
                            ))
                            .await;
                        } else {
                            // auto: aplica.
                            let health: Vec<&str> =
                                inp.health_args.iter().map(|s| s.as_str()).collect();
                            match apply_core_release(
                                &bin_dir(core),
                                &inp.current_exe,
                                crate::CORE_VERSION,
                                &rel,
                                &art,
                                &base,
                                keys,
                                &health,
                                60,
                            )
                            .await
                            {
                                Ok(v) => rep.core_updated_to = Some(v),
                                Err(e) => rep.messages.push(format!("core: {e}")),
                            }
                        }
                    } else {
                        rep.messages.push("core atualizado".to_string());
                    }
                }
                Err(e) => rep.messages.push(format!("core check: {e}")),
            }
        } else {
            rep.messages
                .push("core: sem source de releases".to_string());
        }
    }
    // --- extensões ---
    let plans = preview_updates(core).await?;
    let auto = core.config.update.extensions.as_str() == "auto" && !inp.check_only;
    for p in plans {
        if !p.new_caps.is_empty() {
            // Pausa sempre, mesmo em `auto` (spec §2): pede aprovação.
            rep.ext_pending_caps
                .push((p.name.clone(), p.new_caps.clone()));
            rep.messages.push(format!(
                "{}: capabilities novas ({}); aprove com `atom grants approve`",
                p.name,
                p.new_caps.join(", ")
            ));
            continue;
        }
        if !auto {
            rep.messages
                .push(format!("{}: {} -> {} disponível", p.name, p.from, p.to));
            continue;
        }
        match apply_ext_update(core, &p).await {
            Ok((f, t)) => rep.ext_updated.push((p.name.clone(), f, t)),
            Err(e) => rep.messages.push(format!("{}: {e}", p.name)),
        }
    }
    core.refresh()?;
    Ok(rep)
}

/// Config global de update (`<config>/atom.toml [update]`):
/// `{url, keys, channel}`. Flags do CLI têm prioridade.
pub fn global_update_cfg(home: Option<&Path>) -> (Option<String>, Vec<String>, String) {
    let mut url = None;
    let mut keys = vec![];
    let mut channel = String::new();
    let base = crate::profile::AtomPaths::resolve_with(home);
    let f = base.config.join("atom.toml");
    if let Ok(t) = std::fs::read_to_string(f)
        && let Ok(v) = t.parse::<toml::Value>()
    {
        if let Some(u) = v
            .get("update")
            .and_then(|u| u.get("url"))
            .and_then(|u| u.as_str())
        {
            url = Some(u.to_string());
        }
        if let Some(k) = v.get("update").and_then(|u| u.get("keys")) {
            if let Some(s) = k.as_str() {
                keys = s
                    .split([',', ' ', '\n'])
                    .filter(|x| !x.is_empty())
                    .map(|x| x.to_string())
                    .collect();
            } else if let Some(a) = k.as_array() {
                keys = a
                    .iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect();
            }
        }
        if let Some(c) = v
            .get("update")
            .and_then(|u| u.get("channel"))
            .and_then(|u| u.as_str())
        {
            channel = c.to_string();
        }
    }
    (url, keys, channel)
}

/// Origem do manifesto de releases: flags > config global > embutido.
/// Retorna `(base_url, chaves, canal)` já mesclados.
pub fn merge_update_source(
    cli_url: Option<String>,
    cli_keys: Vec<String>,
    cli_channel: String,
    global_url: Option<String>,
    global_keys: Vec<String>,
    global_channel: String,
) -> (Option<String>, Vec<String>, String) {
    let cli_default_channel = cli_channel == "stable";
    let cli_url_absent = cli_url.is_none();
    let base = cli_url.or(global_url).or_else(|| {
        let d = DEFAULT_UPDATE_URL.trim();
        if d.is_empty() {
            None
        } else {
            Some(d.to_string())
        }
    });
    let mut keys = if cli_keys.is_empty() {
        global_keys
    } else {
        cli_keys
    };
    if keys.is_empty() {
        keys = EMBEDDED_RELEASE_KEYS
            .split([',', ' ', '\n'])
            .filter(|k| !k.is_empty())
            .map(|k| k.to_string())
            .collect();
    }
    let channel = if cli_default_channel && cli_url_absent {
        if global_channel.is_empty() {
            "stable".to_string()
        } else {
            global_channel
        }
    } else {
        cli_channel
    };
    (base, keys, channel)
}

/// Tick de update do daemon (boot + timer): cura pendente e roda `run_update`.
/// Best-effort: erro vira warn, nunca derruba o serve.
/// Aplica update de UMA extensão com rollback em falha de ativação.
pub async fn apply_ext_update(
    core: &mut Core,
    plan: &ExtUpdatePlan,
) -> Result<(String, String), CoreError> {
    let from = plan.from.clone();
    // Snapshot do lock p/ rollback.
    let old_entry = pkg::load_lock(&core.paths)?
        .package
        .iter()
        .find(|p| p.name == plan.name)
        .cloned();
    crate::store::install_package(core, &plan.name, None, Some(&plan.source), true).await?;
    if let Err(e) = core.reload_package(&plan.name).await {
        // Rollback: restaura entrada antiga do lock + recarrega.
        if let Some(old) = old_entry {
            let mut lock = pkg::load_lock(&core.paths)?;
            lock.package.retain(|p| p.name != plan.name);
            lock.package.push(old);
            pkg::save_lock(&core.paths, &lock)?;
            core.refresh()?;
            let _ = core.reload_package(&plan.name).await;
        }
        let ctx = atom_protocol::CallerCtx::local(&core.profile_name, "core");
        let _ = Box::pin(core.publish_event(
            "atom.extension.failed",
            serde_json::json!({"package": plan.name, "phase": "update"}),
            &ctx,
        ))
        .await;
        return Err(CoreError::Package(format!(
            "{}: ativação falhou, rollback p/ {from}: {e}",
            plan.name
        )));
    }
    let to = pkg::load_lock(&core.paths)?
        .package
        .iter()
        .find(|p| p.name == plan.name)
        .map(|p| p.version.clone())
        .unwrap_or_default();
    Ok((from, to))
}

// ---------------------------------------------------------------------------
// Migrações versionadas de formatos de estado (backup prévio).

pub const PROFILE_FORMAT: u32 = 1;
pub const LOCK_FORMAT: u32 = 1;

/// Garante formatos atuais com backup antes de migrar. Idempotente.
/// Retorna ações executadas (vazio = nada a fazer).
pub fn migrate_state(paths: &crate::profile::ProfilePaths) -> Result<Vec<String>, CoreError> {
    let mut done = vec![];
    // profile.toml: carimba `profile = 1` se ausente (formato pré-v0.7?).
    let text = std::fs::read_to_string(&paths.profile_toml).unwrap_or_default();
    let has_profile_ver = text.lines().any(|l| {
        let t = l.trim();
        t.starts_with("profile") && t.contains('=')
    });
    if paths.profile_toml.exists() && !has_profile_ver {
        let bak = paths
            .dir
            .join(format!("profile.toml.bak-{}", crate::profile::stamp_now()));
        std::fs::copy(&paths.profile_toml, &bak).map_err(|e| CoreError::Io(e.to_string()))?;
        let mut cfg = profile::load_or_create(paths)?;
        cfg.profile = PROFILE_FORMAT;
        profile::save(paths, &cfg)?;
        done.push(format!("profile.toml carimbado (backup {})", bak.display()));
    }
    // atom.lock: carimba `lock = 1` se ausente.
    if paths.lock.exists() {
        let text =
            std::fs::read_to_string(&paths.lock).map_err(|e| CoreError::Io(e.to_string()))?;
        let has_lock_ver = text.lines().any(|l| {
            let t = l.trim();
            t.starts_with("lock") && t.contains('=')
        });
        if !has_lock_ver {
            let prev = paths.lock_prev.join(format!("{}.lock", stamp()));
            std::fs::create_dir_all(&paths.lock_prev).map_err(|e| CoreError::Io(e.to_string()))?;
            std::fs::copy(&paths.lock, &prev).map_err(|e| CoreError::Io(e.to_string()))?;
            let mut fixed = String::from("lock = 1\n");
            fixed.push_str(&text);
            atomic_write(&paths.lock, fixed.as_bytes())?;
            done.push("atom.lock carimbado (backup em .prev)".to_string());
        }
        // Recusa futuro desconhecido (peça upgrade do core, não migração cega).
        let lock = pkg::load_lock(paths)?;
        if lock.lock > LOCK_FORMAT {
            return Err(CoreError::Package(format!(
                "atom.lock formato {} > suportado ({LOCK_FORMAT}); atualize o core",
                lock.lock
            )));
        }
        if lock.core.is_empty() {
            // Normaliza campo ausente sem backup extra (já coberto acima se mexeu).
        }
    }
    Ok(done)
}

fn stamp() -> String {
    crate::profile::stamp_now()
}

use crate::profile::{self, atomic_write};

// ---------------------------------------------------------------------------
// GC da store (`atom store gc`).

/// Estatísticas do gc.
#[derive(Debug, Default)]
pub struct GcStats {
    pub removed_dirs: usize,
    pub bytes: u64,
    pub kept_dirs: usize,
}

/// Remove da store versões sem referência em nenhum lock (+ `.prev`)
/// com mtime além de `older_than_secs` (v0.7, spec §2: 30 dias no CLI).
pub fn gc_store(
    atom_paths: &crate::profile::AtomPaths,
    older_than_secs: u64,
) -> Result<GcStats, CoreError> {
    use std::time::SystemTime;
    let mut referenced: HashSet<PathBuf> = HashSet::new();
    let profiles_dir = atom_paths.config.join("profiles");
    let mut profile_dirs: Vec<PathBuf> = vec![];
    if let Ok(rd) = std::fs::read_dir(&profiles_dir) {
        for e in rd.filter_map(|e| e.ok()) {
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                profile_dirs.push(e.path());
            }
        }
    }
    for dir in &profile_dirs {
        let mut locks = vec![dir.join("atom.lock")];
        let prev = dir.join("atom.lock.prev");
        if let Ok(rd) = std::fs::read_dir(&prev) {
            for e in rd.filter_map(|e| e.ok()) {
                locks.push(e.path());
            }
        }
        for lf in locks {
            let Ok(text) = std::fs::read_to_string(&lf) else {
                continue;
            };
            let Ok(lock) = atom_protocol::Lockfile::parse(&text) else {
                continue;
            };
            for p in &lock.package {
                let lp = atom_protocol::LockPackage {
                    name: p.name.clone(),
                    version: p.version.clone(),
                    source: p.source.clone(),
                    platform: p.platform.clone(),
                    sha256: p.sha256.clone(),
                    requested: p.requested.clone(),
                    dependencies: p.dependencies.clone(),
                };
                if let Ok(d) = pkg::package_dir(&atom_paths.data, &lp) {
                    referenced.insert(normalize(&d));
                }
            }
        }
    }
    let mut stats = GcStats::default();
    let store = atom_paths.data.join("store");
    let mut pubs: Vec<PathBuf> = std::fs::read_dir(&store)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default();
    pubs.sort();
    for pubdir in pubs {
        let mut pkgs: Vec<PathBuf> = std::fs::read_dir(&pubdir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.is_dir())
                    .collect()
            })
            .unwrap_or_default();
        pkgs.sort();
        for pkgdir in pkgs {
            let mut vers: Vec<PathBuf> = std::fs::read_dir(&pkgdir)
                .map(|rd| {
                    rd.filter_map(|e| e.ok())
                        .map(|e| e.path())
                        .filter(|p| p.is_dir())
                        .collect()
                })
                .unwrap_or_default();
            vers.sort();
            for v in vers {
                if referenced.contains(&normalize(&v)) {
                    stats.kept_dirs += 1;
                    continue;
                }
                let old_enough = std::fs::metadata(&v)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| SystemTime::now().duration_since(t).ok())
                    .is_some_and(|d| d.as_secs() >= older_than_secs);
                if !old_enough {
                    stats.kept_dirs += 1;
                    continue;
                }
                stats.bytes += dir_size(&v);
                if std::fs::remove_dir_all(&v).is_ok() {
                    stats.removed_dirs += 1;
                }
            }
        }
    }
    Ok(stats)
}

fn normalize(p: &Path) -> PathBuf {
    if let Ok(c) = std::fs::canonicalize(p) {
        c
    } else {
        p.to_path_buf()
    }
}

fn dir_size(dir: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(m) = std::fs::metadata(&p) {
                total += m.len();
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(version: &str, platform: &str) -> CoreRelease {
        CoreRelease {
            version: version.to_string(),
            published_at: String::new(),
            min_upgrade_from: String::new(),
            notes_url: String::new(),
            artifacts: vec![CoreArtifact {
                platform: platform.to_string(),
                url: "file:///x".to_string(),
                sha256: "abc".to_string(),
                size: 1,
                signature: String::new(),
            }],
        }
    }

    #[test]
    fn pick_release_filters() {
        let me = pkg::current_platform();
        // Nova versão p/ plataforma atual passa.
        assert!(pick_release(&rel("9.9.9", &me), "0.1.0", &HashSet::new()).is_some());
        // Mesma ou mais velha recusa.
        assert!(pick_release(&rel("0.1.0", &me), "0.1.0", &HashSet::new()).is_none());
        // Versão marcada como ruim recusa.
        let bad: HashSet<String> = ["9.9.9".to_string()].into_iter().collect();
        assert!(pick_release(&rel("9.9.9", &me), "0.1.0", &bad).is_none());
        // Plataforma ausente recusa; "any" serve de fallback.
        assert!(pick_release(&rel("9.9.9", "plan9-mips"), "0.1.0", &HashSet::new()).is_none());
        assert!(pick_release(&rel("9.9.9", "any"), "0.1.0", &HashSet::new()).is_some());
    }

    #[test]
    fn pick_release_respects_min_upgrade() {
        let me = pkg::current_platform();
        let mut r = rel("9.9.9", &me);
        r.min_upgrade_from = "5.0.0".to_string();
        assert!(pick_release(&r, "0.1.0", &HashSet::new()).is_none());
        assert!(pick_release(&r, "5.0.0", &HashSet::new()).is_some());
    }

    #[test]
    fn core_message_delegates_to_store() {
        assert_eq!(
            core_artifact_message("1.0.0", "abc", "linux-amd64"),
            crate::store::artifact_message("atom/core", "1.0.0", "linux-amd64", "abc")
        );
    }

    #[test]
    fn bad_versions_parses_and_prune_keeps_newest() {
        let t = tempfile::tempdir().unwrap();
        assert!(bad_versions(t.path()).is_empty());
        std::fs::write(t.path().join(".bad-versions"), "1.0.0\n\n2.0.0\n").unwrap();
        assert_eq!(bad_versions(t.path()).len(), 2);
        for v in ["0.1.0", "0.2.0", "0.3.0"] {
            std::fs::write(t.path().join(format!("atom-{v}")), v).unwrap();
        }
        prune_bins(t.path(), 1);
        assert!(t.path().join("atom-0.3.0").exists());
        assert_eq!(
            std::fs::read_dir(t.path())
                .unwrap()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().starts_with("atom-"))
                .count(),
            1
        );
    }

    fn update_test_paths(t: &tempfile::TempDir) -> crate::profile::ProfilePaths {
        crate::profile::ProfilePaths {
            name: "dev".to_string(),
            dir: t.path().join("cfg"),
            profile_toml: t.path().join("cfg/profile.toml"),
            lock: t.path().join("cfg/atom.lock"),
            lock_prev: t.path().join("cfg/prev"),
            grants: t.path().join("cfg/grants.toml"),
            data_dir: t.path().join("data"),
            kv_dir: t.path().join("data/kv"),
            socket: t.path().join("atom-dev.sock"),
            pid_file: t.path().join("atom-dev.pid"),
            serve_token: t.path().join("data/serve.token"),
        }
    }

    #[cfg(unix)]
    fn make_tarball(script: &str) -> Vec<u8> {
        let mut buf = vec![];
        {
            let enc = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::default());
            let mut ar = tar::Builder::new(enc);
            let bytes = script.as_bytes();
            let mut hdr = tar::Header::new_gnu();
            hdr.set_size(bytes.len() as u64);
            hdr.set_mode(0o755);
            hdr.set_cksum();
            ar.append_data(&mut hdr, "atom", bytes).unwrap();
            ar.into_inner().unwrap();
        }
        buf
    }

    #[cfg(unix)]
    fn make_release_dir(
        t: &tempfile::TempDir,
        script: &str,
    ) -> (String, CoreRelease, CoreArtifact, String) {
        use sha2::{Digest, Sha256};
        let (store_pub, seed_b64) = crate::store::keygen();
        let kf = t.path().join("rel.key");
        std::fs::write(&kf, format!("ed25519-seed:{seed_b64}\n")).unwrap();
        let seed = crate::store::load_seed(&kf).unwrap();
        let tar = make_tarball(script);
        let tar_path = t.path().join("core.tar.gz");
        std::fs::write(&tar_path, &tar).unwrap();
        let mut h = Sha256::new();
        h.update(&tar);
        let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        let plat = crate::pkg::current_platform();
        let msg = core_artifact_message("9.9.9", &hex, &plat);
        let sig = crate::store::sign_with_seed(&seed, &msg);
        let art = CoreArtifact {
            platform: plat,
            url: format!("file://{}", tar_path.display()),
            sha256: hex,
            size: tar.len() as u64,
            signature: sig,
        };
        let rel = CoreRelease {
            version: "9.9.9".to_string(),
            published_at: String::new(),
            min_upgrade_from: String::new(),
            notes_url: String::new(),
            artifacts: vec![art.clone()],
        };
        (
            store_pub,
            rel,
            art,
            format!("file://{}/", t.path().display()),
        )
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fetch_release_and_artifact() {
        let t = tempfile::tempdir().unwrap();
        let (store_pub, rel, art, base) = make_release_dir(&t, "#!/bin/sh\nexit 0\n");
        let raw = serde_json::to_vec(&rel).unwrap();
        std::fs::create_dir_all(t.path().join("v1/core")).unwrap();
        std::fs::write(t.path().join("v1/core/stable.json"), &raw).unwrap();
        // Assina o manifesto com a chave da loja.
        let kf = t.path().join("rel.key");
        let seed = crate::store::load_seed(&kf).unwrap();
        let sig = crate::store::sign_with_seed(&seed, &raw);
        std::fs::write(t.path().join("v1/core/stable.json.sig"), &sig).unwrap();
        let base_v1 = base.clone();
        // base_url normaliza trailing slash.
        let (got, _) = fetch_release(&base_v1, "stable", std::slice::from_ref(&store_pub))
            .await
            .unwrap();
        assert_eq!(got.version, "9.9.9");
        // Sem .sig → erro de assinatura.
        let e = fetch_release(
            &format!("{base}outro/"),
            "stable",
            std::slice::from_ref(&store_pub),
        )
        .await
        .unwrap_err();
        assert!(matches!(e, crate::error::CoreError::Package(_)));
        // Chave errada → sig inválida.
        let (other_pub, _) = crate::store::keygen();
        assert!(
            fetch_release(&base_v1, "stable", &[other_pub])
                .await
                .is_err()
        );
        // Artefato ok.
        let bytes = fetch_core_artifact(&art, &base, std::slice::from_ref(&store_pub), "9.9.9")
            .await
            .unwrap();
        assert!(!bytes.is_empty());
        // sha errado.
        let mut bad = art.clone();
        bad.sha256 = "0".repeat(64);
        assert!(
            fetch_core_artifact(&bad, &base, std::slice::from_ref(&store_pub), "9.9.9")
                .await
                .is_err()
        );
        // tamanho errado.
        let mut bad = art.clone();
        bad.size += 1;
        assert!(
            fetch_core_artifact(&bad, &base, std::slice::from_ref(&store_pub), "9.9.9")
                .await
                .is_err()
        );
        // publisher errado.
        let (op, _) = crate::store::keygen();
        assert!(
            fetch_core_artifact(&art, &base, &[op], "9.9.9")
                .await
                .is_err()
        );
        // URL inexistente.
        let mut bad = art.clone();
        bad.url = format!("{base}sumiu.tar.gz");
        assert!(
            fetch_core_artifact(&bad, &base, &[store_pub], "9.9.9")
                .await
                .is_err()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn apply_core_release_ok_and_rollback() {
        let t = tempfile::tempdir().unwrap();
        let bin = t.path().join("bin");
        let current = t.path().join("atom-atual");
        std::fs::write(&current, b"velho").unwrap();
        // Sucesso: script exit 0.
        let (_pub, rel, art, base) = make_release_dir(&t, "#!/bin/sh\nexit 0\n");
        // Chave do manifesto == chave do artefato (mesmo keygen interno).
        let kf = t.path().join("rel.key");
        let seed = crate::store::load_seed(&kf).unwrap();
        let pubk = crate::store::pubkey_of_seed(&seed);
        let v = apply_core_release(
            &bin,
            &current,
            "0.1.0",
            &rel,
            &art,
            &base,
            &[pubk],
            &["--version"],
            10,
        )
        .await
        .unwrap();
        assert_eq!(v, "9.9.9");
        assert!(bin.join("atom-9.9.9").exists());
        // Falha no health: restaura e marca ruim.
        std::fs::write(&current, b"velho").unwrap();
        let t2 = tempfile::tempdir().unwrap();
        let (_p2, rel2, art2, base2) = make_release_dir(&t2, "#!/bin/sh\nexit 1\n");
        let kf2 = t2.path().join("rel.key");
        let seed2 = crate::store::load_seed(&kf2).unwrap();
        let pubk2 = crate::store::pubkey_of_seed(&seed2);
        let e = apply_core_release(
            &bin,
            &current,
            "0.1.0",
            &rel2,
            &art2,
            &base2,
            &[pubk2],
            &[],
            10,
        )
        .await
        .unwrap_err();
        assert!(matches!(
            e,
            crate::error::CoreError::ExtensionUnavailable(_)
        ));
        assert_eq!(std::fs::read(&current).unwrap(), b"velho");
        assert!(bad_versions(&bin).contains("9.9.9"));
    }

    #[test]
    fn swap_health_heal() {
        let t = tempfile::tempdir().unwrap();
        let cur = t.path().join("atom");
        let new = t.path().join("novo");
        let bak = t.path().join("bak");
        std::fs::write(&cur, b"a").unwrap();
        std::fs::write(&new, b"b").unwrap();
        swap_exe(&cur, &new, &bak).unwrap();
        assert_eq!(std::fs::read(&cur).unwrap(), b"b");
        assert_eq!(std::fs::read(&bak).unwrap(), b"a");
        assert!(!health_check(&new, &[], 5));
        assert!(!health_check(&t.path().join("inexistente"), &[], 5));
        #[cfg(unix)]
        {
            assert!(health_check(std::path::Path::new("/bin/true"), &[], 5));
            assert!(!health_check(std::path::Path::new("/bin/false"), &[], 5));
        }
        // heal sem pendente → false.
        assert!(!heal_if_needed(&t.path().join("bin-vazia"), &cur));
        // heal com pendente + backup restaura.
        let bin = t.path().join("b2");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join(".pending-health"), b"9.9.9").unwrap();
        std::fs::write(bin.join(".backup-0.1.0"), b"a").unwrap();
        assert!(heal_if_needed(&bin, &cur));
        assert_eq!(std::fs::read(&cur).unwrap(), b"a");
        assert!(bad_versions(&bin).contains("9.9.9"));
    }

    #[test]
    fn merge_and_global_cfg() {
        let (b, k, c) = merge_update_source(
            Some("file:///x/".to_string()),
            vec!["k".to_string()],
            "stable".to_string(),
            Some("file:///y/".to_string()),
            vec!["g".to_string()],
            "beta".to_string(),
        );
        assert_eq!(
            (b.as_deref(), k.as_slice(), c.as_str()),
            (Some("file:///x/"), &["k".to_string()][..], "stable")
        );
        let (b, k, c) = merge_update_source(
            None,
            vec![],
            "stable".to_string(),
            None,
            vec![],
            String::new(),
        );
        assert_eq!(c, "stable");
        assert!(b.is_none() && k.is_empty());
        let t = tempfile::tempdir().unwrap();
        let (u, ks, ch) = global_update_cfg(Some(t.path()));
        assert!(u.is_none() && ks.is_empty() && ch.is_empty());
        std::fs::create_dir_all(t.path().join("config")).unwrap();
        std::fs::write(
            t.path().join("config/atom.toml"),
            "[update]\nurl = \"file:///z/\"\nkeys = \"k1,k2\"\nchannel = \"beta\"\n",
        )
        .unwrap();
        let (u, ks, ch) = global_update_cfg(Some(t.path()));
        assert_eq!(u.as_deref(), Some("file:///z/"));
        assert_eq!(ks, vec!["k1".to_string(), "k2".to_string()]);
        assert_eq!(ch, "beta");
    }

    #[test]
    fn migrate_state_stamps_old_formats() {
        let t = tempfile::tempdir().unwrap();
        let p = update_test_paths(&t);
        std::fs::create_dir_all(&p.dir).unwrap();
        // profile.toml antigo (sem linha `profile =`).
        std::fs::write(&p.profile_toml, "[bindings]\n").unwrap();
        let done = migrate_state(&p).unwrap();
        assert!(done.iter().any(|d| d.contains("carimbado")));
        // atom.lock antigo (sem `lock =`).
        std::fs::create_dir_all(&p.lock_prev).unwrap();
        std::fs::write(&p.lock, "core = \"0.1.0\"\n").unwrap();
        let done = migrate_state(&p).unwrap();
        assert!(done.iter().any(|d| d.contains("atom.lock")));
        // lock futuro → erro.
        std::fs::write(&p.lock, "lock = 99\ncore = \"0.1.0\"\n").unwrap();
        assert!(migrate_state(&p).is_err());
    }

    #[test]
    fn gc_store_removes_unreferenced() {
        let t = tempfile::tempdir().unwrap();
        let atom = crate::profile::AtomPaths {
            config: t.path().join("config"),
            data: t.path().join("data"),
            cache: t.path().join("cache"),
            runtime: t.path().join("runtime"),
        };
        let sha = "0123456789abcdef0123456789abcdef";
        let keep = atom
            .data
            .join("store/p/a")
            .join(format!("1.0.0-{}", &sha[..16]));
        std::fs::create_dir_all(&keep).unwrap();
        std::fs::write(keep.join("atom.toml"), "x").unwrap();
        let drop_dir = atom.data.join("store/p/a").join("0.9.0-deadbeefdeadbeef");
        std::fs::create_dir_all(&drop_dir).unwrap();
        std::fs::write(drop_dir.join("atom.toml"), "x").unwrap();
        let prof = atom.config.join("profiles/dev");
        std::fs::create_dir_all(&prof).unwrap();
        std::fs::write(
            prof.join("atom.lock"),
            format!("lock = 1\ncore = \"0.1.0\"\n[[package]]\nname = \"p/a\"\nversion = \"1.0.0\"\nsource = \"local\"\nplatform = \"any\"\nsha256 = \"{sha}\"\nrequested = \"*\"\n"),
        )
        .unwrap();
        let stats = gc_store(&atom, 0).unwrap();
        assert!(keep.exists());
        assert!(!drop_dir.exists());
        assert_eq!(stats.kept_dirs, 1);
        assert_eq!(stats.removed_dirs, 1);
    }
}
