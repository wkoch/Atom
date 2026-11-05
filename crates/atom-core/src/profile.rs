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
    /// Token bearer do WS (`serve --ws`, v0.8, 0600).
    pub serve_token: PathBuf,
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
            serve_token: atom.data.join("profiles").join(name).join("serve.token"),
        })
    }

    pub fn ensure(&self) -> Result<(), CoreError> {
        for d in [&self.dir, &self.data_dir, &self.kv_dir, &self.lock_prev] {
            std::fs::create_dir_all(d).map_err(|e| CoreError::Io(e.to_string()))?;
        }
        Ok(())
    }

    /// Espelho autoritativo `grants.toml.core` (spec capabilities `[C-AMB-2]`, v1.1 ADR-019).
    /// Derivado de `grants` (não é campo) para não quebrar literais existentes de `ProfilePaths`.
    pub fn grants_core(&self) -> PathBuf {
        // `grants.toml` -> `grants.toml.core` (troca a extensão `toml` por `toml.core`).
        self.grants.with_extension("toml.core")
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

/// Conteúdo de `profile.toml` (spec profiles §3, parcial v0.5).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProfileConfig {
    #[serde(default = "default_profile_ver")]
    pub profile: u32,
    #[serde(default)]
    pub bindings: HashMap<String, String>,
    #[serde(default)]
    pub config: HashMap<String, toml::Value>,
    #[serde(default)]
    pub validation: ValidationConfig,
    /// Journal opcional do bus (v0.6): append-only JSONL em `data_dir/journal/`.
    #[serde(default)]
    pub journal: JournalCfg,
    /// Política de auto-update (v0.7, spec updates.md).
    #[serde(default)]
    pub update: UpdateCfg,
    /// Sources da loja `store/v1` (spec store §6), ordem por `priority`.
    #[serde(default)]
    pub sources: Vec<SourceCfg>,
    /// (v1.1) Limites e prazos do bus (protocol §3, §13, `[P-LIM-1]`, `[P-DL-2]`).
    #[serde(default)]
    pub bus: BusLimits,
    /// (v1.1) Retenção de eventos p/ retomada (protocol §7, `[P-EV-2]`).
    #[serde(default)]
    pub events: EventsCfg,
}

/// Source `store/v1`: chave raiz fixada pelo usuário (spec store §1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceCfg {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub keys: Vec<String>,
    #[serde(default)]
    pub priority: i32,
}

/// Journal do bus: `enabled` grava `bus.jsonl` (v0.6, opcional, best-effort).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct JournalCfg {
    #[serde(default)]
    pub enabled: bool,
}

/// Limites do bus (v1.1, protocol §13 `[P-LIM-1]`; ADR-020).
/// Excedido = `-32050 Overloaded`, nunca memória ilimitada.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BusLimits {
    /// Teto p/ `$ctx.deadline` em ms a partir de agora (`[P-DL-2]`). 0 = sem teto.
    #[serde(default = "default_max_deadline_ms")]
    pub max_deadline_ms: u64,
    /// Prazo aplicado quando o chamador não envia `deadline`. 0 = sem prazo (v1.0).
    #[serde(default)]
    pub default_deadline_ms: u64,
    /// Requests em voo por conexão.
    #[serde(default = "default_max_inflight")]
    pub max_inflight: usize,
    /// Fila de saída por conexão (mensagens).
    #[serde(default = "default_out_queue")]
    pub out_queue: usize,
    /// Buffer por stream (chunks, `[P-STREAM-1]`).
    #[serde(default = "default_stream_buf")]
    pub stream_buf: usize,
    /// Assinaturas de eventos por conexão (`[P-EV-5]`).
    #[serde(default = "default_max_subs")]
    pub max_subs: usize,
}

fn default_max_deadline_ms() -> u64 {
    600_000
}
fn default_max_inflight() -> usize {
    1024
}
fn default_out_queue() -> usize {
    1024
}
fn default_stream_buf() -> usize {
    256
}
fn default_max_subs() -> usize {
    256
}

impl Default for BusLimits {
    fn default() -> Self {
        Self {
            max_deadline_ms: default_max_deadline_ms(),
            default_deadline_ms: 0,
            max_inflight: default_max_inflight(),
            out_queue: default_out_queue(),
            stream_buf: default_stream_buf(),
            max_subs: default_max_subs(),
        }
    }
}

/// Retenção de eventos (v1.1, protocol §7 `[P-EV-2]`; ADR-022).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventsCfg {
    #[serde(default = "default_events_retain")]
    pub retain: usize,
}

fn default_events_retain() -> usize {
    10_000
}

impl Default for EventsCfg {
    fn default() -> Self {
        Self {
            retain: default_events_retain(),
        }
    }
}

/// Política de update: `auto` aplica sozinho, `notify` só avisa,
/// `off` desliga (core). `pinned` = pacotes nunca auto-atualizados (v0.7).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdateCfg {
    #[serde(default = "default_update_core")]
    pub core: String,
    #[serde(default = "default_update_ext")]
    pub extensions: String,
    #[serde(default)]
    pub pinned: Vec<String>,
    /// Horas entre checagens do daemon. 0 = só no boot.
    #[serde(default = "default_check_hours")]
    pub check_interval_hours: u64,
}

impl Default for UpdateCfg {
    fn default() -> Self {
        Self {
            core: default_update_core(),
            extensions: default_update_ext(),
            pinned: vec![],
            check_interval_hours: default_check_hours(),
        }
    }
}

fn default_update_core() -> String {
    "notify".to_string()
}

fn default_update_ext() -> String {
    "notify".to_string()
}

fn default_check_hours() -> u64 {
    24
}

/// Entrada de `grants.toml`: aprovação por pacote (spec capabilities §3).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GrantEntry {
    #[serde(default)]
    pub granted: Vec<String>,
    #[serde(default)]
    pub denied: Vec<String>,
    #[serde(default)]
    pub approved_at: String,
}

/// `grants.toml`: pacote → grants. Ausente = vazio (nada aprovado).
pub type GrantsFile = HashMap<String, GrantEntry>;

/// `granted` (com glob no escopo) autoriza `needed`? (`nome[:escopo]`).
/// Casa `service:<c>`, `intercept:<c>`, `events.*:<glob>`, `secrets*:<n>`.
pub fn cap_allows(granted: &str, needed: &str) -> bool {
    let (gn, gs) = match granted.split_once(':') {
        Some((n, s)) => (n, Some(s)),
        None => (granted, None),
    };
    let (nn, ns) = match needed.split_once(':') {
        Some((n, s)) => (n, Some(s)),
        None => (needed, None),
    };
    if gn != nn {
        return false;
    }
    match (gs, ns) {
        (_, None) => gs.is_none(),
        (None, Some(_)) => false,
        (Some(pat), Some(scope)) => {
            globset::Glob::new(pat).is_ok_and(|g| g.compile_matcher().is_match(scope))
        }
    }
}

#[cfg(test)]
mod cap_tests {
    use super::cap_allows;

    #[test]
    fn cap_globs() {
        assert!(cap_allows("service:ai.tools@0", "service:ai.tools@0"));
        assert!(cap_allows("events.publish:*", "events.publish:a.b"));
        assert!(cap_allows("events.publish:a.*", "events.publish:a.b"));
        assert!(!cap_allows("events.publish:a.*", "events.publish:b.c"));
        assert!(!cap_allows("service:ai.tools@0", "service:ai.llm@0"));
        assert!(!cap_allows("other", "service:x"));
        assert!(!cap_allows("service:x", "service:x/y"));
    }
}
pub fn stamp_now() -> String {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => format!("{}", d.as_secs()),
        Err(_) => "0".to_string(),
    }
}

/// Resultado de `load_grants` com detecção de alteração externa.
/// `tampered=true` → o chamador (bus) deve publicar `atom.grants.tampered`; NUNCA publica aqui.
/// spec: [C-AMB-2]
#[derive(Debug, Clone, Default)]
pub struct GrantsLoad {
    /// Grants efetivos (sempre o último estado escrito pelo core — `.core` — quando houver divergência).
    pub grants: GrantsFile,
    /// `true` quando `grants.toml` difere do último `.core` (edição externa ignorada).
    pub tampered: bool,
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    let mut s = String::with_capacity(64);
    for b in h.finalize() {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Lê `grants.toml` (ausente = vazio, nunca é erro).
/// Compat: retorna só os grants efetivos.
pub fn load_grants(paths: &ProfilePaths) -> GrantsFile {
    load_grants_checked(paths).grants
}

/// Lê `grants.toml` comparando com o espelho autoritativo `grants.toml.core`.
/// Se o arquivo externo difere do último estado escrito pelo core → vale o `.core`
/// e `tampered=true` (o bus publica `atom.grants.tampered`).
/// Sem `.core` (perfil antigo) → confia no arquivo e o adota como base.
/// spec: [C-AMB-2]
pub fn load_grants_checked(paths: &ProfilePaths) -> GrantsLoad {
    let core_bytes = std::fs::read(paths.grants_core()).ok();
    let ext_bytes = std::fs::read(&paths.grants).ok();
    // Extrai `(texto_toml, sha)` do espelho: header `# sha256:<hex>\n` + corpo.
    let core_state: Option<(String, String)> = core_bytes.as_deref().and_then(|b| {
        let s = std::str::from_utf8(b).ok()?;
        let (head, body) = s.split_once('\n')?;
        let sha = head.strip_prefix("# sha256:")?.trim().to_string();
        if sha.len() == 64 && sha.chars().all(|c| c.is_ascii_hexdigit()) {
            Some((body.to_string(), sha))
        } else {
            None
        }
    });
    let parse = |t: &str| toml::from_str::<GrantsFile>(t).ok();
    match (core_state, ext_bytes) {
        // Espelho íntegro: compara sha do externo com o registrado.
        (Some((core_text, sha)), Some(ext)) if sha256_hex(&ext) != sha => GrantsLoad {
            grants: parse(&core_text).unwrap_or_default(),
            tampered: true,
        },
        // Externo bate com o espelho (ou ambos ausentes): usa o externo.
        (Some((core_text, _)), Some(ext)) => GrantsLoad {
            grants: parse(std::str::from_utf8(&ext).unwrap_or(&core_text))
                .or_else(|| parse(&core_text))
                .unwrap_or_default(),
            tampered: false,
        },
        // Sem espelho: confia no externo (migração de perfil antigo).
        (None, Some(ext)) => GrantsLoad {
            grants: std::str::from_utf8(&ext)
                .ok()
                .and_then(parse)
                .unwrap_or_default(),
            tampered: false,
        },
        // Só espelho existe (grants.toml apagado): vale o core, sem flag de tamper.
        (Some((core_text, _)), None) => GrantsLoad {
            grants: parse(&core_text).unwrap_or_default(),
            tampered: false,
        },
        (None, None) => GrantsLoad::default(),
    }
}

/// Salva `grants.toml` (atômico) + espelho `grants.toml.core` (`# sha256:<hex>` + corpo).
/// spec: [C-AMB-2]
pub fn save_grants(paths: &ProfilePaths, grants: &GrantsFile) -> Result<(), CoreError> {
    paths.ensure()?;
    let text = toml::to_string_pretty(grants).map_err(|e| CoreError::Profile(e.to_string()))?;
    atomic_write(&paths.grants, text.as_bytes())?;
    let mirror = format!("# sha256:{}\n{text}", sha256_hex(text.as_bytes()));
    atomic_write(&paths.grants_core(), mirror.as_bytes())
}

/// Aceita o `grants.toml` externo atual como base (cliente confiável: `atom grants reload`).
/// Reescreve o espelho `.core` a partir do arquivo externo. Sem arquivo externo = limpa tudo.
/// spec: [C-AMB-2]
pub fn reload_grants_from_disk(paths: &ProfilePaths) -> Result<GrantsFile, CoreError> {
    let text = std::fs::read_to_string(&paths.grants).unwrap_or_default();
    let grants: GrantsFile =
        toml::from_str(&text).map_err(|e| CoreError::Profile(format!("grants.toml: {e}")))?;
    save_grants(paths, &grants)?;
    Ok(grants)
}

/// `validation.contracts`: valida params/result contra schemas de contrato.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ValidationConfig {
    #[serde(default)]
    pub contracts: bool,
}

fn default_profile_ver() -> u32 {
    1
}

/// Versão do formato (congelada na v1.0; ver `docs/roadmap.md`).
pub const PROFILE_VERSION: u32 = 1;

pub fn load_or_create(paths: &ProfilePaths) -> Result<ProfileConfig, CoreError> {
    paths.ensure()?;
    if !paths.profile_toml.exists() {
        let cfg = ProfileConfig {
            profile: 1,
            bindings: HashMap::new(),
            config: HashMap::new(),
            validation: ValidationConfig::default(),
            journal: JournalCfg::default(),
            update: UpdateCfg::default(),
            sources: vec![],
            bus: BusLimits::default(),
            events: EventsCfg::default(),
        };
        save(paths, &cfg)?;
        return Ok(cfg);
    }
    let text =
        std::fs::read_to_string(&paths.profile_toml).map_err(|e| CoreError::Io(e.to_string()))?;
    let cfg: ProfileConfig =
        toml::from_str(&text).map_err(|e| CoreError::Profile(format!("profile.toml: {e}")))?;
    if cfg.profile > PROFILE_VERSION {
        return Err(CoreError::Profile(format!(
            "profile {} maior que o suportado ({PROFILE_VERSION})",
            cfg.profile
        )));
    }
    Ok(cfg)
}

pub fn save(paths: &ProfilePaths, cfg: &ProfileConfig) -> Result<(), CoreError> {
    paths.ensure()?;
    let text = toml::to_string_pretty(cfg).map_err(|e| CoreError::Profile(e.to_string()))?;
    atomic_write(&paths.profile_toml, text.as_bytes())
}

/// Campos de segurança de `profile.toml`: mudança externa só vale via reload de cliente
/// confiável (`atom profile reload`); watch automático aplica apenas `[config.*]`
/// (o watch em si é montado pelo dono do loop — NÃO implementado aqui; ver nota abaixo).
/// NOTA-WATCH: o ponto de chamada ideal é o loop do daemon/CLI que observa `profile.toml`
/// (ex.: `notify` debounced): ao detectar escrita, recarregar só `cfg.config` e IGNORAR o
/// restante se `security_fields_changed(&old, &new)` — nesse caso o dono do watch deve
/// restaurar o arquivo a partir de `old` e o bus publica `atom.config.tampered`.
/// spec: [C-AMB-3]
pub fn security_fields_changed(old: &ProfileConfig, new: &ProfileConfig) -> bool {
    // Compara via TOML canônico (ordem de chaves de mapa não afeta `to_string_pretty`
    // para `HashMap`? afeta — por isso compara campo a campo com `==` derivado).
    old.sources != new.sources
        || old.bindings != new.bindings
        || old.validation != new.validation
        || old.update != new.update
}

/// Diferença detalhada dos campos de segurança (p/ payload `{file, fields}` do evento).
/// spec: [C-AMB-3]
pub fn security_fields_diff(old: &ProfileConfig, new: &ProfileConfig) -> Vec<String> {
    let mut out = Vec::new();
    if old.sources != new.sources {
        out.push("sources".to_string());
    }
    if old.bindings != new.bindings {
        out.push("bindings".to_string());
    }
    if old.validation != new.validation {
        out.push("validation".to_string());
    }
    if old.update != new.update {
        out.push("update".to_string());
    }
    out
}

/// Escrita atômica: temp + fsync + rename.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), CoreError> {
    atomic_write_mode(path, bytes, None)
}

/// Como `atomic_write`, mas o temp já nasce 0600 (segredos, tokens, chaves).
/// Fecha a janela entre escrita e chmod do padrão escrever-depois-proteger.
pub fn atomic_write_private(path: &Path, bytes: &[u8]) -> Result<(), CoreError> {
    atomic_write_mode(path, bytes, Some(0o600))
}

#[cfg(unix)]
fn atomic_write_mode(path: &Path, bytes: &[u8], mode: Option<u32>) -> Result<(), CoreError> {
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| CoreError::Io(e.to_string()))?;
    }
    let tmp = path.with_extension("tmp");
    let mut opt = std::fs::OpenOptions::new();
    opt.write(true).create(true).truncate(true);
    if let Some(m) = mode {
        opt.mode(m);
    }
    let mut f = opt.open(&tmp).map_err(|e| CoreError::Io(e.to_string()))?;
    use std::io::Write;
    f.write_all(bytes)
        .map_err(|e| CoreError::Io(e.to_string()))?;
    let _ = f.sync_all();
    std::fs::rename(&tmp, path).map_err(|e| CoreError::Io(e.to_string()))?;
    Ok(())
}

#[cfg(not(unix))]
fn atomic_write_mode(path: &Path, bytes: &[u8], _mode: Option<u32>) -> Result<(), CoreError> {
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

    #[test]
    fn rejects_future_profile_version() {
        let t = tempfile::tempdir().unwrap();
        let a = AtomPaths {
            config: t.path().join("c"),
            data: t.path().join("d"),
            cache: t.path().join("ca"),
            runtime: t.path().join("r"),
        };
        let p = ProfilePaths::new(&a, "dev").unwrap();
        load_or_create(&p).unwrap();
        std::fs::write(&p.profile_toml, "profile = 2\n").unwrap();
        let err = load_or_create(&p).unwrap_err().to_string();
        assert!(err.contains("maior que o suportado"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn private_write_nasce_0600() {
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::tempdir().unwrap();
        let f = t.path().join("segredo.json");
        atomic_write_private(&f, b"{}").unwrap();
        let mode = std::fs::metadata(&f).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{mode:o}");
    }
}
