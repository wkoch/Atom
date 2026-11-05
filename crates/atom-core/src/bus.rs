#![forbid(unsafe_code)]

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Notify;

use atom_protocol::{AtomCallParams, CallerCtx, RpcError, RpcResponse};
use base64::Engine as _;
use tokio::sync::{mpsc, oneshot};

use crate::error::CoreError;
use crate::host::{Host, Incoming};
use crate::pkg;
use crate::profile::{self, AtomPaths, ProfileConfig, ProfilePaths};
use crate::registry::Registry;
use crate::storage::Kv;

/// Epoch ms UTC agora (relógio do SO; `deadline` usa a mesma base, `[P-DL-1]`).
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Erro `-32022 DeadlineExceeded` com trace (`[P-DL-3]`).
// spec: [P-DL-3]
fn deadline_err(trace: &str) -> CoreError {
    CoreError::Protocol(
        RpcError::new(
            atom_protocol::ErrorCode::DEADLINE_EXCEEDED,
            "prazo esgotado",
        )
        .with_trace(trace),
    )
}

/// Profundidade máxima de `atom.call` encadeado (extensão → core → extensão).
const MAX_DEPTH: u8 = 8;

/// Traço temporário de debug do drive (`ATOM_DEBUG=1`).
fn dbgtrace(msg: &str) {
    if std::env::var("ATOM_DEBUG")
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        eprintln!("[drive] {msg}");
    }
}

/// Evento de stream repassado ao consumidor.
#[derive(Debug, Clone)]
pub struct StreamEvent {
    pub method: String,
    pub params: serde_json::Value,
}

/// Cursor de retomada de eventos (`[P-EV-3]`, v1.1, ADR-022).
// spec: [P-EV-3]
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct EventCursor {
    pub epoch: String,
    #[serde(default)]
    pub offset: u64,
}

/// Entrada do ring de eventos (`[P-EV-2]`, v1.1, ADR-022).
// spec: [P-EV-2]
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RetainedEvent {
    pub epoch: String,
    pub offset: u64,
    pub topic: String,
    pub data: serde_json::Value,
}

/// Parâmetros de `Core::call_stream_chan` (agrupados; clippy 7 args).
#[derive(Debug, Clone)]
pub struct StreamCall {
    pub id: String,
    pub service: String,
    pub method: String,
    pub params: serde_json::Value,
    pub target: Option<String>,
    /// `$ctx` forjado pelo chamador (deadline propaga, `[P-DL-1]`).
    // spec: [P-DL-1]
    pub ctx: Option<CallerCtx>,
    /// Opt-in de backpressure do consumidor (`[P-STREAM-2]`); `None` = v1.0.
    // spec: [P-STREAM-2]
    pub flow: Option<atom_protocol::FlowOpts>,
}

/// Quem consome um stream ativo.
#[derive(Debug)]
enum StreamConsumer {
    /// Extensão (notificações no stdin dela).
    Ext(String),
    /// Canal fora do bus (socket do daemon, CLI `--stream`).
    /// Limitado (`[bus] stream_buf`, `[P-LIM-1]`): cheia = backpressure (`[P-STREAM-1]`).
    // spec: [P-LIM-1]
    // spec: [P-STREAM-1]
    Chan(mpsc::Sender<StreamEvent>),
}

struct StreamRec {
    producer: String,
    consumer: StreamConsumer,
    /// Id do `service.invoke` no salto do produtor (`[P-CANCEL-2]`).
    provider_req: String,
    /// Opt-in do consumidor: só envia `seq <= ack + window` (`[P-STREAM-2]`).
    // spec: [P-STREAM-2]
    window: Option<u32>,
    /// Último `ack.seq` cumulativo do consumidor (`[P-STREAM-2]`).
    // spec: [P-STREAM-2]
    acked: u64,
    /// Próximo `seq` de chunk a emitir (`[P-STREAM-4]`).
    // spec: [P-STREAM-4]
    next_seq: u64,
    /// Chunks retidos pela janela, liberados pelo `ack` (`[P-STREAM-2]`).
    // spec: [P-STREAM-2]
    pending: VecDeque<serde_json::Value>,
    /// Teto dos retidos (`[bus] stream_buf`, `[P-STREAM-1]`).
    // spec: [P-STREAM-1]
    pending_cap: usize,
    /// Produtor em modo crédito (`initialize` com `"flow": "credit"`, `[P-STREAM-3]`).
    // spec: [P-STREAM-3]
    credit_producer: bool,
    /// Teto de `seq` autorizado pelo crédito (`[P-STREAM-3]`).
    // spec: [P-STREAM-3]
    credit_until: u64,
}
/// Resultado da admissão de um `atom.stream.chunk` (`[P-STREAM-2/3/4]`).
// spec: [P-STREAM-2]
// spec: [P-STREAM-3]
// spec: [P-STREAM-4]
enum StreamChunkOutcome {
    Delivered {
        stream: String,
        params: serde_json::Value,
    },
    Terminated {
        stream: String,
        code: i32,
        message: String,
    },
    Dropped,
}
/// Chamada em voo para mapeamento de `$/cancelRequest` por salto
/// (`[P-CANCEL-1/2]`, v1.1).
// spec: [P-CANCEL-1]
// spec: [P-CANCEL-2]
struct CallFrame {
    caller_ext: String,
    caller_id: Option<serde_json::Value>,
    provider: Option<String>,
    provider_req: Option<String>,
    cancel: Arc<Notify>,
    cancelled: bool,
}

type InvokeRx = oneshot::Receiver<RpcResponse>;

/// Como `drive` termina a espera pelo provedor.
enum DriveDone {
    /// Retorna o resultado final ao chamador; `cancel` aborta com `-32800`
    /// após cortesia de 2s (`[P-CANCEL-2/3]`, v1.1).
    // spec: [P-CANCEL-2]
    // spec: [P-CANCEL-3]
    Return { cancel: Arc<Notify> },
    /// Resultado final descartado (fim já foi notificado); `cancel` aborta.
    StreamDiscard { id: String, cancel: Arc<Notify> },
}

/// Núcleo em processo: carrega perfil, resolve provedores, roteia `atom.call`.
pub struct Core {
    pub profile_name: String,
    pub atom_paths: AtomPaths,
    pub paths: ProfilePaths,
    pub config: ProfileConfig,
    pub data_store: PathBuf,
    pub registry: Registry,
    pub host: Host,
    pub secrets: crate::secrets::Secrets,
    manifests: HashMap<String, (atom_protocol::LockPackage, atom_protocol::Manifest)>,
    pkg_dirs: HashMap<String, PathBuf>,
    streams: HashMap<String, StreamRec>,
    /// Cursor global do log de eventos (`[P-EV-1]`): `epoch` do boot +
    /// `offset` monotônico (`seq` legado removido, era por entrega).
    // spec: [P-EV-1]
    event_epoch: String,
    /// Próximo `offset` a atribuir (`offset` do último + 1, `[P-EV-1]`).
    // spec: [P-EV-1]
    event_next: u64,
    /// Ring de eventos retidos (`[events] retain`, `[P-EV-2]`).
    // spec: [P-EV-2]
    event_ring: VecDeque<RetainedEvent>,
    /// Último `offset` já entregue por assinante, p/ `atom.events.gap` (`[P-EV-5]`).
    // spec: [P-EV-5]
    event_delivered: HashMap<String, u64>,
    // `refresh()` reconstrói o registry dos manifestos e reaplica estes.
    // `shortcut: sem expiração por conexão; limpeza de clientes no socket é v0.5+.`
    temp_providers: Vec<(String, String)>,
    temp_intercepts: Vec<(String, String, String, i32)>,
    temp_subs: Vec<(String, String)>,
    // `around`: tokens pendentes de `atom.proceed` (uso único).
    proceeds: HashMap<String, ProceedState>,
    proceed_results: HashMap<String, serde_json::Value>,
    proceeded: std::collections::HashSet<String>,
    /// Pacote declarativo -> handler (`"core"` para kinds de bootstrap).
    kind_loaded: HashMap<String, String>,
    /// `sync_kinds` já rodou nesta vida do `Core` (invalida no `refresh`).
    kinds_synced: bool,
    /// Aprovações de capabilities (`grants.toml`, spec capabilities §3).
    grants: profile::GrantsFile,
    /// Pedidos `atom.grants.request` pendentes: pacote -> caps.
    pending_grants: HashMap<String, Vec<String>>,
    /// Pilha de chamadas em voo para `$/cancelRequest` por salto
    /// (`[P-CANCEL-1/2]`, v1.1). Topo = invoke mais interno.
    // spec: [P-CANCEL-1]
    // spec: [P-CANCEL-2]
    call_stack: Vec<CallFrame>,
    /// Espelho vivo pacote -> caps concedidas p/ o fast path do host
    /// (secrets) e filtro `env:` no spawn.
    grants_live: Arc<tokio::sync::RwLock<HashMap<String, Vec<String>>>>,
    /// Journal opcional do bus (v0.6).
    journal: crate::journal::Journal,
    /// Endpoint WS do daemon (`serve --ws`, v0.8): (url pública, token).
    /// `None` = sem WS (ex.: CLI em processo, testes sem serve).
    ws_info: Option<(String, String)>,
    /// URL base do shell ui-web (`atom.ui.serve`, v0.8). `None` = inativo.
    ui_url: Option<String>,
    test_replayed: usize,
    test_gap: bool,
}

/// Estado de um `around` aguardando `atom.proceed` (uso único).
#[derive(Debug, Clone)]
struct ProceedState {
    owner: String,
    service: String,
    method: String,
    params: serde_json::Value,
    ctx: CallerCtx,
    provider: String,
    caller: String,
    depth: u8,
    remaining: Vec<(String, i32)>,
}

/// Resposta de `interceptor.invoke` (spec protocol §6).
#[derive(Debug)]
enum InterceptorAction {
    Continue,
    ModifyParams(serde_json::Value),
    ModifyResult(serde_json::Value),
    Respond(serde_json::Value),
    Reject(RpcError),
}

/// Timeout por interceptor (spec §6); configurável no futuro.
const INTERCEPTOR_TIMEOUT_SECS: u64 = 30;

use crate::profile::cap_allows;

/// Espelho pacote -> caps concedidas p/ o fast path (host/secrets, `env:`).
fn grants_view(grants: &profile::GrantsFile) -> HashMap<String, Vec<String>> {
    grants
        .iter()
        .map(|(k, e)| (k.clone(), e.granted.clone()))
        .collect()
}

impl Core {
    /// Aplica `[P-DL-2]` a um ctx já autorizado: teto `max_deadline_ms` e
    /// prazo padrão `default_deadline_ms`. Retorna `true` se já estourou.
    // spec: [P-DL-2]
    fn clamp_deadline(&self, ctx: &mut CallerCtx) -> bool {
        let now = now_ms();
        let max_ms = self.config.bus.max_deadline_ms;
        if ctx.deadline.is_none() {
            let dflt = self.config.bus.default_deadline_ms;
            if dflt > 0 {
                ctx.deadline = Some(now.saturating_add(dflt as i64));
            }
        }
        if max_ms > 0 {
            let cap = now.saturating_add(max_ms as i64);
            if ctx.deadline.map(|d| d > cap).unwrap_or(true) {
                ctx.deadline = Some(cap);
            }
        }
        ctx.deadline.is_some_and(|d| d <= now)
    }
    /// Abre (criando se necessário) o perfil.
    pub async fn open(profile_name: &str) -> Result<Self, CoreError> {
        Self::open_with(profile_name, None).await
    }

    /// Como `open`, com override de `ATOM_HOME` (flag `--home` do CLI).
    pub async fn open_with(
        profile_name: &str,
        home: Option<&std::path::Path>,
    ) -> Result<Self, CoreError> {
        let atom_paths = AtomPaths::resolve_with(home);
        atom_paths.ensure()?;
        let paths = ProfilePaths::new(&atom_paths, profile_name)?;
        // Migrações versionadas com backup (v0.7): idempotente.
        if let Err(e) = crate::update::migrate_state(&paths) {
            tracing::warn!("migrate: {e}");
        }
        let config = profile::load_or_create(&paths)?;
        let data_store = atom_paths.data.clone();
        let secrets = crate::secrets::Secrets::new(profile_name, &paths.data_dir);
        let kv_dir = paths.kv_dir.clone();
        let manifests = pkg::installed_manifests(&paths, &data_store).unwrap_or_default();
        let mut pkg_dirs = HashMap::new();
        let mut registry = Registry::new(config.bindings.clone());
        Self::fill_registry(&mut registry, &manifests, &mut pkg_dirs, &data_store);
        let grants = profile::load_grants(&paths);
        let grants_live = Arc::new(tokio::sync::RwLock::new(grants_view(&grants)));
        let journal = crate::journal::Journal::open(&paths, config.journal.enabled);
        let (event_epoch, event_ring, event_next) = Self::load_event_handover(&paths);
        Ok(Self {
            profile_name: profile_name.to_string(),
            atom_paths,
            paths,
            config,
            data_store,
            registry,
            host: Host::new(kv_dir, secrets.clone(), grants_live.clone()),
            secrets,
            streams: HashMap::new(),
            manifests,
            pkg_dirs,
            event_epoch,
            event_next,
            event_ring,
            event_delivered: HashMap::new(),
            temp_providers: vec![],
            temp_intercepts: vec![],
            temp_subs: vec![],
            proceeds: HashMap::new(),
            proceed_results: HashMap::new(),
            proceeded: std::collections::HashSet::new(),
            kind_loaded: HashMap::new(),
            kinds_synced: false,
            grants,
            pending_grants: HashMap::new(),
            call_stack: Vec::new(),
            grants_live,
            journal,
            ws_info: None,
            ui_url: None,
            test_replayed: 0,
            test_gap: false,
        })
    }

    /// Recarrega lock/manifestos/bindings do disco.
    pub fn refresh(&mut self) -> Result<(), CoreError> {
        self.config = profile::load_or_create(&self.paths)?;
        let manifests = pkg::installed_manifests(&self.paths, &self.data_store).unwrap_or_default();
        self.pkg_dirs.clear();
        self.registry = Registry::new(self.config.bindings.clone());
        let data_store = self.data_store.clone();
        Self::fill_registry(
            &mut self.registry,
            &manifests,
            &mut self.pkg_dirs,
            &data_store,
        );
        // Reaplica registros temporários (v0.3).
        for (c, p) in self.temp_providers.clone() {
            self.registry.add_provider(&c, &p);
        }
        for (p, m, ph, pr) in self.temp_intercepts.clone() {
            let _ = self.registry.add_intercept(&p, &m, &ph, pr);
        }
        for (s, t) in self.temp_subs.clone() {
            let _ = self.registry.add_subscription(&s, &t);
        }
        self.manifests = manifests;
        // Kinds: mantém `loaded` só do que continua instalado; re-sincroniza depois.
        self.kind_loaded
            .retain(|k, _| self.manifests.contains_key(k));
        self.kinds_synced = false;
        // Grants: recarrega do disco (CLI pode ter alterado) + resync do espelho.
        self.sync_grants();
        // Journal: reabre se `[journal]` mudou no `profile.toml`.
        self.journal = crate::journal::Journal::open(&self.paths, self.config.journal.enabled);
        Ok(())
    }

    /// providers + intercepts + subscribes declarados nos manifestos.
    /// Globs inválidos são ignorados com aviso (não derrubam o perfil).
    fn fill_registry(
        registry: &mut Registry,
        manifests: &HashMap<String, (atom_protocol::LockPackage, atom_protocol::Manifest)>,
        pkg_dirs: &mut HashMap<String, PathBuf>,
        data_store: &Path,
    ) {
        for (name, (lp, m)) in manifests {
            if let Ok(dir) = pkg::package_dir(data_store, lp) {
                pkg_dirs.insert(name.clone(), dir);
            }
            for p in &m.provides {
                registry.add_provider(&p.contract, name);
            }
            for i in &m.intercepts {
                if let Err(e) = registry.add_intercept(name, &i.match_, &i.phase, i.priority) {
                    tracing::warn!(pacote = %name, "{e}");
                }
            }
            for s in &m.subscribes {
                if let Err(e) = registry.add_subscription(name, s) {
                    tracing::warn!(pacote = %name, "{e}");
                }
            }
            for h in &m.handles {
                registry.add_handler(h, name);
            }
        }
    }

    fn entrypoint_of(&self, name: &str) -> Result<(PathBuf, PathBuf, String), CoreError> {
        let (lp, m) = self
            .manifests
            .get(name)
            .ok_or_else(|| CoreError::NoProvider(format!("pacote {name} não instalado")))?;
        let ep_rel = m.entrypoint.as_ref().ok_or_else(|| {
            CoreError::ExtensionUnavailable(format!("{name} é declarativo (sem entrypoint)"))
        })?;
        let dir = self
            .pkg_dirs
            .get(name)
            .ok_or_else(|| CoreError::ExtensionUnavailable(format!("{name} sem diretório")))?;
        let ep = dir.join(ep_rel);
        #[cfg(windows)]
        let ep = {
            if !ep.exists() {
                let exe = ep.with_extension("exe");
                if exe.exists() { exe } else { ep }
            } else {
                ep
            }
        };
        Ok((dir.clone(), ep, lp.version.clone()))
    }

    fn activation(&self, name: &str) -> Vec<String> {
        self.manifests
            .get(name)
            .map(|(_, m)| m.activation.clone())
            .unwrap_or_default()
    }

    fn wants_startup(&self, name: &str) -> bool {
        let a = self.activation(name);
        a.is_empty() || a.iter().any(|s| s == "onStartup")
    }

    fn wants_service(&self, name: &str, service: &str) -> bool {
        self.activation(name)
            .iter()
            .any(|s| s == &format!("onService:{service}"))
    }

    /// Sobe extensões `onStartup` e carrega kinds declarativos.
    /// Lazy (v0.6): handlers sobem sob demanda via `load_package_kind`, não
    /// no boot — `sync_kinds`/`ensure_kinds` cobre o resto fora do boot.
    /// Config inválida impede só aquele pacote (warn), não o boot.
    /// Sem handler = `dormant` sem erro (aceite v0.4).
    pub async fn boot(&mut self) -> Result<Vec<String>, CoreError> {
        let names: Vec<String> = self.manifests.keys().cloned().collect();
        let mut started = vec![];
        for n in &names {
            let executable = self
                .manifests
                .get(n)
                .map(|(_, m)| m.entrypoint.is_some())
                .unwrap_or(false);
            if !executable {
                continue;
            }
            if self.wants_startup(n) {
                match self.start_ext(n).await {
                    Ok(()) => started.push(n.clone()),
                    Err(CoreError::Package(e)) => {
                        tracing::warn!(pacote = %n, "config inválida, pacote inativo: {e}");
                    }
                    Err(e) => return Err(e),
                }
            }
        }
        // Declarativos: bootstrap no core, resto via handler ou dormant.
        for n in &names {
            let executable = self
                .manifests
                .get(n)
                .map(|(_, m)| m.entrypoint.is_some())
                .unwrap_or(true);
            if executable {
                continue;
            }
            match self.load_package_kind(n).await {
                Ok(st) if st == "dormant" => {
                    tracing::debug!(pacote = %n, "sem handler: dormant");
                }
                Ok(_) => {}
                Err(CoreError::Package(e)) => {
                    tracing::warn!(pacote = %n, "kind inválido, dormant: {e}");
                }
                Err(e) => return Err(e),
            }
        }
        self.kinds_synced = true;
        // Evento de ciclo de vida (spec protocol §7); sem assinantes no boot = no-op.
        let ctx = CallerCtx::local(&self.profile_name, "core");
        let _ = Box::pin(self.publish_event(
            "atom.profile.started",
            serde_json::json!({"profile": self.profile_name}),
            &ctx,
        ))
        .await;
        Ok(started)
    }

    /// Sobe um pacote instalado (para `atom run` / `onCommand`).
    pub async fn start_package(&mut self, name: &str) -> Result<(), CoreError> {
        self.start_ext(name).await
    }

    async fn start_ext(&mut self, name: &str) -> Result<(), CoreError> {
        self.validate_package_config(name)?;
        let (prog, args, dir, version) = self.spawn_spec_of(name)?;
        let cfg = self
            .config
            .config
            .get(name)
            .cloned()
            .unwrap_or(toml::Value::Table(toml::map::Map::new()));
        let cfg_json: serde_json::Value =
            serde_json::to_value(&cfg).map_err(|e| CoreError::Internal(e.to_string()))?;
        let granted = self.granted_of(name);
        let started = self
            .host
            .ensure_started(name, &dir, &prog, &args, &version, cfg_json, &granted)
            .await;
        if started.is_ok() {
            let ctx = CallerCtx::local(&self.profile_name, "core");
            let _ = Box::pin(self.publish_event(
                "atom.extension.started",
                serde_json::json!({"package": name}),
                &ctx,
            ))
            .await;
        }
        started
    }

    /// `true` se `ext` tem a capability (globs no escopo). Core/client = confiável.
    fn grant_ok(&self, ext: &str, needed: &str) -> bool {
        if ext == "core" || ext == "client" {
            return true;
        }
        self.grants
            .get(ext)
            .map(|e| e.granted.iter().any(|g| cap_allows(g, needed)))
            .unwrap_or(false)
    }

    /// Exige capability do chamador (extensões). Negado → `-32003`.
    fn check_cap(&self, caller: &str, needed: &str) -> Result<(), RpcError> {
        if self.grant_ok(caller, needed) {
            return Ok(());
        }
        Err(RpcError::new(
            atom_protocol::ErrorCode::CAPABILITY_DENIED,
            format!("{caller} sem capability {needed}"),
        ))
    }

    /// Recarrega `grants.toml` e sincroniza o espelho vivo do host.
    fn sync_grants(&mut self) {
        self.grants = profile::load_grants(&self.paths);
        if let Ok(mut live) = self.grants_live.try_write() {
            *live = grants_view(&self.grants);
        }
    }

    /// Persiste grants + atualiza espelho + journal.
    fn store_grants(&mut self, what: &str, package: &str) -> Result<(), CoreError> {
        profile::save_grants(&self.paths, &self.grants)?;
        if let Ok(mut live) = self.grants_live.try_write() {
            *live = grants_view(&self.grants);
        }
        self.journal.grant(package, what);
        Ok(())
    }

    /// Concede capabilities a um pacote (CLI `grants approve`, install).
    pub fn approve_grants(&mut self, package: &str, caps: &[String]) -> Result<(), CoreError> {
        let entry = self.grants.entry(package.to_string()).or_default();
        for c in caps {
            if !entry.granted.iter().any(|g| g == c) {
                entry.granted.push(c.clone());
            }
            entry.denied.retain(|d| d != c);
        }
        entry.approved_at = profile::stamp_now();
        if let Some(pending) = self.pending_grants.get_mut(package) {
            pending.retain(|c| !caps.contains(c));
        }
        self.store_grants("approved", package)
    }

    /// Revoga capabilities (negação explícita; enforcement imediato).
    pub fn deny_grants(&mut self, package: &str, caps: &[String]) -> Result<(), CoreError> {
        let entry = self.grants.entry(package.to_string()).or_default();
        for c in caps {
            if !entry.denied.iter().any(|d| d == c) {
                entry.denied.push(c.clone());
            }
            entry.granted.retain(|g| g != c);
        }
        entry.approved_at = profile::stamp_now();
        if let Some(pending) = self.pending_grants.get_mut(package) {
            pending.retain(|c| !caps.contains(c));
        }
        self.store_grants("denied", package)
    }

    /// Grants carregados (`grants.toml`).
    pub fn grants(&self) -> &profile::GrantsFile {
        &self.grants
    }

    /// Pedidos `atom.grants.request` pendentes: pacote -> caps.
    pub fn pending_grants(&self) -> &HashMap<String, Vec<String>> {
        &self.pending_grants
    }

    /// Caps concedidas de um pacote (p/ UI de aprovação).
    pub fn granted_of(&self, package: &str) -> Vec<String> {
        self.grants
            .get(package)
            .map(|e| e.granted.clone())
            .unwrap_or_default()
    }

    /// Caps ainda não concedidas do manifesto (p/ aprovação na instalação).
    pub fn missing_grants(&self, package: &str) -> Vec<String> {
        let declared = self
            .manifests
            .get(package)
            .map(|(_, m)| m.capabilities.clone())
            .unwrap_or_default();
        let granted = self.granted_of(package);
        declared
            .into_iter()
            .filter(|c| !granted.iter().any(|g| g == c))
            .collect()
    }

    /// Supervisão (spec protocol §4.1): morte inesperada → restart com
    /// backoff 1s→60s; 5 mortes em 5 min → `failed` + evento. Saídas
    /// limpas (`stop`) removem a entrada antes, então são ignoradas aqui.
    async fn handle_ext_exit(&mut self, ext: &str) {
        if !self.host.is_running(ext) && !self.host.has_entry(ext) {
            return;
        }
        // Marca como parado p/ `poll_exits`/status não mentir.
        if let Some(e) = self.host.get_mut(ext) {
            e.state = crate::host::ExtState::Stopped;
        }
        let now = std::time::Instant::now();
        let log = self.host.restart_log_mut(ext);
        log.push(now);
        log.retain(|t| now.duration_since(*t) < std::time::Duration::from_secs(300));
        let n = log.len();
        if n >= 5 {
            if let Some(e) = self.host.get_mut(ext) {
                e.state = crate::host::ExtState::Failed("5 mortes em 5 min".to_string());
            }
            let ctx = CallerCtx::local(&self.profile_name, "core");
            let _ = Box::pin(self.publish_event(
                "atom.extension.failed",
                serde_json::json!({"package": ext, "restarts": n}),
                &ctx,
            ))
            .await;
            return;
        }
        let delay = std::cmp::min(1u64 << n.saturating_sub(1).min(6), 60);
        tracing::warn!(ext = %ext, "morreu; restart em {delay}s (morte {n}/5)");
        let tx = self.host.sender();
        let name = ext.to_string();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
            let _ = tx
                .send(Incoming {
                    ext: name,
                    id: None,
                    method: "__ext_restart".to_string(),
                    params: serde_json::Value::Null,
                })
                .await;
        });
    }

    /// Varre mortes detectadas por `try_wait` (entre pumps) e supervisiona.
    pub async fn reap_dead(&mut self) {
        let dead = self.host.poll_exits();
        for ext in dead {
            Box::pin(self.handle_ext_exit(&ext)).await;
        }
    }

    /// Para extensão com evento `atom.extension.stopped`.
    pub async fn stop_package(&mut self, name: &str) {
        let was_running = self.host.is_running(name);
        self.host.stop(name).await;
        if was_running {
            let ctx = CallerCtx::local(&self.profile_name, "core");
            let _ = Box::pin(self.publish_event(
                "atom.extension.stopped",
                serde_json::json!({"package": name}),
                &ctx,
            ))
            .await;
        }
    }

    /// Hot reload por pacote (v0.6): descarrega kind, para, refresca,
    /// revalida, sobe, recarrega kind. Erro de config não derruba o resto.
    pub async fn reload_package(&mut self, name: &str) -> Result<(), CoreError> {
        Box::pin(self.unload_package_kind(name)).await;
        let was_running = self.host.is_running(name);
        if was_running {
            self.stop_package(name).await;
        }
        self.refresh()?;
        self.validate_package_config(name)?;
        let executable = self
            .manifests
            .get(name)
            .map(|(_, m)| m.entrypoint.is_some())
            .unwrap_or(false);
        if executable && (was_running || self.wants_startup(name)) {
            self.start_ext(name).await?;
        }
        if !executable {
            let _ = Box::pin(self.load_package_kind(name)).await?;
        }
        self.kinds_synced = false;
        Box::pin(self.ensure_kinds()).await
    }

    async fn ensure_provider(&mut self, service: &str, provider: &str) -> Result<(), CoreError> {
        // Supervisão preguiçosa: mortes detectadas por `try_wait` viram
        // restart/failed antes de qualquer post (v0.6).
        Box::pin(self.reap_dead()).await;
        if self.host.is_running(provider) {
            return Ok(());
        }
        if self.wants_startup(provider) || self.wants_service(provider, service) {
            return self.start_ext(provider).await;
        }
        // Sem ativação restritiva: sobe sob demanda (padrão v0.1).
        self.start_ext(provider).await
    }

    fn wants_event(&self, name: &str, topic: &str) -> bool {
        self.activation(name).iter().any(|s| {
            s.strip_prefix("onEvent:")
                .map(|g| globset::Glob::new(g).is_ok_and(|gl| gl.compile_matcher().is_match(topic)))
                .unwrap_or(false)
        })
    }

    /// Kinds que o core entende sem handler (bootstrap, spec manifest §5).
    fn is_bootstrap_kind(kind: &str) -> bool {
        matches!(kind, "contract" | "runtime" | "distribution")
    }

    /// Handler do kind (primeiro instalado; warn se múltiplos).
    fn handler_for_kind(&self, kind: &str) -> Option<String> {
        let list = self.registry.handlers_of(kind);
        if list.len() > 1 {
            tracing::warn!(kind = %kind, handlers = ?list, "múltiplos handlers, usando o primeiro");
        }
        list.into_iter().next()
    }

    fn wants_kind(&self, name: &str, kind: &str) -> bool {
        if self.activation(name).iter().any(|s| {
            s.strip_prefix("onKind:")
                .map(|g| globset::Glob::new(g).is_ok_and(|gl| gl.compile_matcher().is_match(kind)))
                .unwrap_or(false)
        }) {
            return true;
        }
        // Handlers recebem implicitamente `onKind:<kind>` (spec manifest §4).
        self.manifests
            .get(name)
            .map(|(_, m)| m.handles.iter().any(|h| h == kind))
            .unwrap_or(false)
    }

    /// Estado de um pacote declarativo: `loaded:<handler>`, `core` ou `dormant`.
    pub fn kind_state(&self, name: &str) -> Option<String> {
        let (_, m) = self.manifests.get(name)?;
        if m.entrypoint.is_some() {
            return Some("executable".to_string());
        }
        if Self::is_bootstrap_kind(&m.kind) {
            return Some("core".to_string());
        }
        Some(
            self.kind_loaded
                .get(name)
                .map(|h| format!("loaded:{h}"))
                .unwrap_or_else(|| "dormant".to_string()),
        )
    }

    /// Especificação de spawn com `runtime=` (spec manifest §5.2).
    /// Retorna `(programa, args, cwd)`. Sem `runtime=`: entrypoint direto + `args`.
    fn spawn_spec_of(
        &self,
        name: &str,
    ) -> Result<(PathBuf, Vec<String>, PathBuf, String), CoreError> {
        let (dir, ep, version) = self.entrypoint_of(name)?;
        let (_, m) = self
            .manifests
            .get(name)
            .ok_or_else(|| CoreError::NoProvider(format!("pacote {name} não instalado")))?;
        let rt = match &m.runtime {
            Some(r) => r.clone(),
            None => return Ok((ep, m.args.clone(), dir, version)),
        };
        let (_, rm) = self
            .manifests
            .get(&rt)
            .ok_or_else(|| CoreError::Package(format!("{name}: runtime {rt} não instalado")))?;
        let rdir = self
            .pkg_dirs
            .get(&rt)
            .ok_or_else(|| CoreError::Package(format!("{name}: runtime {rt} sem diretório")))?;
        let cmd = rm
            .contributes
            .get("runtime")
            .and_then(|v| v.get("command"))
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                CoreError::Package(format!("runtime {rt}: contributes.runtime.command ausente"))
            })?;
        let mut parts: Vec<String> = vec![];
        for c in cmd {
            let s = c.as_str().ok_or_else(|| {
                CoreError::Package(format!("runtime {rt}: command deve ser array de strings"))
            })?;
            parts.push(
                s.replace("{entry}", &ep.to_string_lossy())
                    .replace("{pkg}", &dir.to_string_lossy()),
            );
        }
        if parts.is_empty() {
            return Err(CoreError::Package(format!("runtime {rt}: command vazio")));
        }
        let prog = PathBuf::from(&parts[0]);
        let prog = if prog.is_absolute() {
            prog
        } else {
            rdir.join(&prog)
        };
        let mut args = parts[1..].to_vec();
        args.extend(m.args.clone());
        Ok((prog, args, dir, version))
    }

    /// Carrega um pacote declarativo no handler (`kind.load`).
    /// Bootstrap kinds = ok sem handler. Sem handler = `dormant` (não é erro).
    pub async fn load_package_kind(&mut self, name: &str) -> Result<String, CoreError> {
        let (kind, entrypoint) = {
            let (_, m) = self
                .manifests
                .get(name)
                .ok_or_else(|| CoreError::NoProvider(format!("pacote {name} não instalado")))?;
            (m.kind.clone(), m.entrypoint.is_some())
        };
        if entrypoint {
            return Ok("executable".to_string());
        }
        if Self::is_bootstrap_kind(&kind) {
            self.kind_loaded
                .insert(name.to_string(), "core".to_string());
            return Ok("core".to_string());
        }
        let handler = match self.handler_for_kind(&kind) {
            Some(h) => h,
            None => return Ok("dormant".to_string()),
        };
        self.start_ext(&handler).await?;
        let (manifest_json, dir_str, cfg_json) = {
            let (_, m) = self.manifests.get(name).unwrap();
            let manifest_json =
                serde_json::to_value(m).map_err(|e| CoreError::Internal(e.to_string()))?;
            let dir_str = self
                .pkg_dirs
                .get(name)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            let cfg = self
                .config
                .config
                .get(name)
                .cloned()
                .unwrap_or(toml::Value::Table(toml::map::Map::new()));
            let cfg_json: serde_json::Value =
                serde_json::to_value(&cfg).map_err(|e| CoreError::Internal(e.to_string()))?;
            (manifest_json, dir_str, cfg_json)
        };
        let r = self
            .request_pumped(
                &handler,
                "kind.load",
                serde_json::json!({"package": manifest_json, "dir": dir_str, "config": cfg_json}),
            )
            .await;
        match r {
            Ok(_) => {
                self.kind_loaded.insert(name.to_string(), handler.clone());
                Ok(handler)
            }
            Err(e) => {
                tracing::warn!(pacote = %name, handler = %handler, "kind.load falhou, dormant: {e}");
                Ok("dormant".to_string())
            }
        }
    }

    /// Descarrega kind (`kind.unload`, best-effort) antes de remover pacote.
    pub async fn unload_package_kind(&mut self, name: &str) {
        let handler = match self.kind_loaded.remove(name) {
            Some(h) => h,
            None => return,
        };
        if handler == "core" || !self.host.is_running(&handler) {
            return;
        }
        let lp = match self.manifests.get(name) {
            Some((lp, _)) => lp.clone(),
            None => return,
        };
        let id = format!("{}@{}", lp.name, lp.version);
        if let Err(e) = self
            .request_pumped(&handler, "kind.unload", serde_json::json!({"package": id}))
            .await
        {
            tracing::warn!(pacote = %name, "kind.unload falhou: {e}");
        }
    }

    /// Carrega kinds pendentes (após boot/refresh/install). Idempotente.
    pub async fn sync_kinds(&mut self) -> Result<(), CoreError> {
        let names: Vec<String> = self.manifests.keys().cloned().collect();
        for n in &names {
            let executable = self
                .manifests
                .get(n)
                .map(|(_, m)| m.entrypoint.is_some())
                .unwrap_or(true);
            if executable || self.kind_loaded.contains_key(n) {
                continue;
            }
            let _ = self.load_package_kind(n).await?;
        }
        // Ativação `onKind`: extensões (não-handlers) que pediram kinds
        // presentes sobem aqui — lazy, sem boot ansioso (v0.6).
        let decl_kinds: Vec<String> = names
            .iter()
            .filter_map(|n| self.manifests.get(n))
            .filter(|(_, m)| m.entrypoint.is_none())
            .map(|(_, m)| m.kind.clone())
            .collect();
        for n in &names {
            let executable = self
                .manifests
                .get(n)
                .map(|(_, m)| m.entrypoint.is_some())
                .unwrap_or(false);
            if !executable || self.host.is_running(n) {
                continue;
            }
            if decl_kinds.iter().any(|k| self.wants_kind(n, k)) {
                let _ = self.start_ext(n).await;
            }
        }
        self.kinds_synced = true;
        Ok(())
    }

    /// Garante kinds sincronizados (chamado uma vez por vida do `Core`).
    pub async fn ensure_kinds(&mut self) -> Result<(), CoreError> {
        if self.kinds_synced {
            return Ok(());
        }
        self.sync_kinds().await
    }

    /// Remove pacote: descarrega kind, remove do store, refresca e re-sincroniza.
    pub async fn remove_package(&mut self, name: &str) -> Result<bool, CoreError> {
        Box::pin(self.unload_package_kind(name)).await;
        // Para o processo (supervisão: `stop` remove antes do exit, sem restart).
        Box::pin(self.stop_package(name)).await;
        let ok = pkg::remove(&self.paths, name)?;
        if ok {
            self.registry.remove_provider_all(name);
            self.kind_loaded.remove(name);
            self.refresh()?;
            self.kinds_synced = false;
        }
        Ok(ok)
    }

    /// Valida `config[package]` contra `config_schema` do manifesto.
    /// Sem schema = sem validação. Schema ausente/inválido = pacote não ativa.
    fn validate_package_config(&self, name: &str) -> Result<(), CoreError> {
        let (_, m) = self
            .manifests
            .get(name)
            .ok_or_else(|| CoreError::NoProvider(format!("pacote {name} não instalado")))?;
        let rel = match &m.config_schema {
            Some(r) => r.clone(),
            None => return Ok(()),
        };
        let dir = self
            .pkg_dirs
            .get(name)
            .ok_or_else(|| CoreError::Package(format!("{name}: sem diretório para ler {rel}")))?;
        let text = std::fs::read_to_string(dir.join(&rel))
            .map_err(|e| CoreError::Package(format!("{name}: {rel}: {e}")))?;
        let schema: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| CoreError::Package(format!("{name}: {rel}: {e}")))?;
        let cfg_toml = self
            .config
            .config
            .get(name)
            .cloned()
            .unwrap_or(toml::Value::Table(toml::map::Map::new()));
        let cfg: serde_json::Value =
            serde_json::to_value(&cfg_toml).map_err(|e| CoreError::Internal(e.to_string()))?;
        jsonschema::validate(&schema, &cfg)
            .map_err(|e| CoreError::Package(format!("{name}: config inválida: {e}")))?;
        Ok(())
    }

    /// Localiza schema de contrato (`kind=contract`, `contributes.contract.schema`).
    fn contract_schema(&self, service: &str) -> Option<serde_json::Value> {
        for (pkg, (_, m)) in &self.manifests {
            if m.kind != "contract" {
                continue;
            }
            let id = m.contributes.get("contract")?.get("id")?.as_str()?;
            if id != service {
                continue;
            }
            let rel = m.contributes.get("contract")?.get("schema")?.as_str()?;
            let dir = self.pkg_dirs.get(pkg)?;
            let text = std::fs::read_to_string(dir.join(rel)).ok()?;
            return serde_json::from_str(&text).ok();
        }
        None
    }

    /// Valida params/result contra contrato se `validation.contracts=true`.
    /// Sem contrato instalado ou sem subschema = sem validação.
    fn validate_contract(
        &self,
        service: &str,
        method: &str,
        kind: &str,
        value: &serde_json::Value,
    ) -> Result<(), RpcError> {
        if !self.config.validation.contracts {
            return Ok(());
        }
        if service.starts_with("atom.") {
            return Ok(());
        }
        let schema = match self.contract_schema(service) {
            Some(s) => s,
            None => return Ok(()),
        };
        let sub = schema
            .get("methods")
            .and_then(|m| m.get(method))
            .and_then(|m| m.get(kind));
        let sub = match sub {
            Some(s) => s,
            None => return Ok(()),
        };
        // Valida `value` contra o subschema, com o schema raiz como contexto
        // de `$ref`/`$defs`: embute o subschema mantendo `$defs` da raiz.
        let mut effective = sub.clone();
        if let (Some(eff), Some(root)) = (effective.as_object_mut(), schema.as_object()) {
            for key in ["$defs", "definitions"] {
                if !eff.contains_key(key)
                    && let Some(d) = root.get(key)
                {
                    eff.insert(key.to_string(), d.clone());
                }
            }
        }
        // `shortcut: validação por subschema isolado; `$ref` para fora de
        // `$defs` não resolve. Suficiente para contratos `@0` atuais.`
        jsonschema::validate(&effective, value).map_err(|e| {
            RpcError::new(
                atom_protocol::ErrorCode::CONTRACT_VALIDATION_FAILED,
                format!("{service}/{method} {kind} inválido: {e}"),
            )
        })
    }

    /// Novo `epoch` do log de eventos (`ep_<ULID>`, `[P-EV-1]`).
    // spec: [P-EV-1]
    fn new_event_epoch() -> String {
        format!("ep_{}", ulid::Ulid::new())
    }

    /// Diretório do handover de eventos (`[P-EV-2]`).
    // spec: [P-EV-2]
    fn event_handover_dir(paths: &ProfilePaths) -> PathBuf {
        paths.data_dir.join("events")
    }

    /// Arquivo do handover de eventos (`[P-EV-2]`, 0600, apagado no boot).
    // spec: [P-EV-2]
    fn event_handover_file(paths: &ProfilePaths) -> PathBuf {
        Self::event_handover_dir(paths).join("handover.jsonl")
    }

    /// Carrega o handover do self-update (`[P-EV-2]`): `(epoch, ring, next)`.
    /// Handover ausente = boot frio, novo `epoch`, ring vazio.
    /// Arquivo é apagado após leitura (uso único).
    // spec: [P-EV-2]
    fn load_event_handover(paths: &ProfilePaths) -> (String, VecDeque<RetainedEvent>, u64) {
        let file = Self::event_handover_file(paths);
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        let _ = std::fs::remove_file(&file);
        if text.trim().is_empty() {
            return (Self::new_event_epoch(), VecDeque::new(), 1);
        }
        let mut epoch: Option<String> = None;
        let mut ring = VecDeque::new();
        let mut max: u64 = 0;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(ev) = serde_json::from_str::<RetainedEvent>(line) {
                if epoch.is_none() {
                    epoch = Some(ev.epoch.clone());
                }
                if Some(&ev.epoch) != epoch.as_ref() {
                    continue;
                }
                max = max.max(ev.offset);
                ring.push_back(ev);
            }
        }
        match epoch {
            Some(e) => (e, ring, max.saturating_add(1).max(1)),
            None => (Self::new_event_epoch(), VecDeque::new(), 1),
        }
    }

    /// Grava o ring em `handover.jsonl` (0600, via escrita atômica, `[P-EV-2]`).
    /// Best-effort: falha de escrita nunca bloqueia o shutdown.
    // spec: [P-EV-2]
    fn store_event_handover(&self) {
        let dir = Self::event_handover_dir(&self.paths);
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let mut out = String::new();
        for ev in &self.event_ring {
            if let Ok(line) = serde_json::to_string(ev) {
                out.push_str(&line);
                out.push('\n');
            }
        }
        let file = Self::event_handover_file(&self.paths);
        if let Ok(tmp) = tempfile::NamedTempFile::new_in(&dir) {
            use std::io::Write as _;
            let mut ok = true;
            {
                let mut f = tmp.as_file();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt as _;
                    let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
                }
                if f.write_all(out.as_bytes()).is_err() || f.sync_all().is_err() {
                    ok = false;
                }
            }
            if ok {
                let _ = tmp.persist(&file);
            }
        }
    }

    /// Reentrega eventos retidos a um assinante com `from` (`[P-EV-3]`):
    /// replays com `offset > from.offset` que casam os tópicos. `epoch`
    /// divergente ou `from.offset` fora do ring = `atom.events.gap` antes.
    // spec: [P-EV-3]
    async fn replay_events(&mut self, sub: &str, topics: &[String], from: &EventCursor) {
        // `test_gap` registra que o gap foi devido, mesmo sem entregador
        // (extensão pode estar parada; o flag é observabilidade de teste).
        let diverge = from.epoch != self.event_epoch.clone();
        if diverge {
            self.test_gap = true;
        }
        let epoch = self.event_epoch.clone();
        let ctx = CallerCtx::local(&self.profile_name, "core");
        if !self.host.is_running(sub) {
            self.test_replayed = self
                .event_ring
                .iter()
                .filter(|e| e.epoch == self.event_epoch && e.offset > from.offset)
                .filter(|e| topics.iter().any(|t| Self::topic_matches(t, &e.topic)))
                .count();
            return;
        }
        if from.epoch != epoch {
            self.send_gap(sub, None, None);
        } else {
            let oldest = self.event_ring.front().map(|e| e.offset);
            let newest = self.event_ring.back().map(|e| e.offset);
            let out_of_ring = match (oldest, newest) {
                (Some(o), Some(_)) => from.offset < o.saturating_sub(1),
                (None, None) => from.offset + 1 < self.event_next,
                _ => false,
            };
            if out_of_ring {
                self.send_gap(sub, Some(from.offset), newest);
            }
        }
        let retained: Vec<RetainedEvent> = self.event_ring.iter().cloned().collect();
        for ev in retained {
            if ev.epoch != epoch || ev.offset <= from.offset {
                continue;
            }
            if !topics.iter().any(|t| Self::topic_matches(t, &ev.topic)) {
                continue;
            }
            self.deliver_event_to(sub, &ev.topic, ev.data.clone(), ev.offset, &ctx)
                .await;
            self.test_replayed += 1;
        }
    }

    /// Assinaturas ativas de um assinante (p/ `max_subs`, `[P-EV-5]`).
    // spec: [P-EV-5]
    fn subscription_count(&self, sub: &str) -> usize {
        self.temp_subs.iter().filter(|(s, _)| s == sub).count()
    }

    /// `true` se o glob `pat` casa `topic` (glob inválido = sem replay).
    fn topic_matches(pat: &str, topic: &str) -> bool {
        globset::Glob::new(pat).is_ok_and(|g| g.compile_matcher().is_match(topic))
    }

    /// Entrega um evento retido/ao vivo a um assinante com fila limitada
    /// (`[P-EV-5]`): `Host::notify` usa `try_send` no writer (cap 64);
    /// estouro descarta para aquele assinante e envia `atom.events.gap`.
    // spec: [P-EV-1]
    // spec: [P-EV-5]
    async fn deliver_event_to(
        &mut self,
        sub: &str,
        topic: &str,
        data: serde_json::Value,
        offset: u64,
        ctx: &CallerCtx,
    ) {
        if !self.host.is_running(sub) {
            return;
        }
        let mut full_ctx = ctx.clone();
        full_ctx.authorize(&self.profile_name, "core");
        let params = serde_json::json!({
            "topic": topic,
            "data": data,
            "seq": offset,
            "epoch": self.event_epoch.clone(),
            "offset": offset,
            "$ctx": serde_json::to_value(&full_ctx).unwrap_or(serde_json::Value::Null),
        });
        if self.host.try_notify(sub, "event.deliver", params) {
            self.event_delivered.insert(sub.to_string(), offset);
            return;
        }
        let from = self.event_delivered.get(sub).copied();
        self.send_gap(sub, from, Some(offset));
        self.event_delivered.insert(sub.to_string(), offset);
    }

    /// Envia `atom.events.gap` a um assinante (`[P-EV-3]`, `[P-EV-5]`).
    /// `from/to` nulos quando desconhecidos.
    // spec: [P-EV-3]
    // spec: [P-EV-5]
    fn send_gap(&mut self, sub: &str, from: Option<u64>, to: Option<u64>) {
        self.test_gap = true;
        let epoch = self.event_epoch.clone();
        let last = to.or(from);
        if let Some(until) = last {
            self.event_delivered.insert(sub.to_string(), until);
        }
        let sub = sub.to_string();
        let params = serde_json::json!({
            "subscription": serde_json::json!(format!("sub_{}", sub)),
            "epoch": epoch,
            "from": from.map(serde_json::Value::from).unwrap_or(serde_json::Value::Null),
            "to": to.map(serde_json::Value::from).unwrap_or(serde_json::Value::Null),
        });
        let writer = self.host.ext_writer(&sub);
        if let Some(w) = writer {
            let line = serde_json::json!({
                "jsonrpc": "2.0",
                "method": "atom.events.gap",
                "params": params,
            });
            if let Ok(line) = serde_json::to_string(&line) {
                let _ = w.try_send(line);
            }
        }
    }

    /// Publica evento a assinantes declarados + temporários.
    /// `[P-EV-1]`: `event.deliver` carrega `epoch`/`offset` globais.
    /// `[P-EV-2]`: registra no ring limitado por `[events] retain`.
    /// Sem `from`, semântica v1 at-most-once (`[P-EV-4]`).
    /// Sobe assinante `onEvent:*` sob demanda; falha de start vira warn.
    // spec: [P-EV-1]
    // spec: [P-EV-2]
    // spec: [P-EV-4]
    pub async fn publish_event(
        &mut self,
        topic: &str,
        data: serde_json::Value,
        ctx: &CallerCtx,
    ) -> Result<(), CoreError> {
        self.journal
            .event(topic, ctx.caller.as_deref().unwrap_or("core"));
        let offset = self.event_next;
        self.event_next = self.event_next.saturating_add(1).max(1);
        let epoch = self.event_epoch.clone();
        self.event_ring.push_back(RetainedEvent {
            epoch: epoch.clone(),
            offset,
            topic: topic.to_string(),
            data: data.clone(),
        });
        let retain = self.config.events.retain.max(1);
        while self.event_ring.len() > retain {
            self.event_ring.pop_front();
        }
        let subs = self.registry.subscribers_of(topic);
        for sub in subs {
            if !self.host.is_running(&sub)
                && (self.wants_event(&sub, topic) || self.wants_startup(&sub))
                && let Err(e) = self.start_ext(&sub).await
            {
                tracing::warn!(ext = %sub, "onEvent {topic}: start falhou: {e}");
                continue;
            }
            self.deliver_event_to(&sub, topic, data.clone(), offset, ctx)
                .await;
        }
        Ok(())
    }

    /// Recarrega `profile.toml`: atualiza bindings, emite
    /// `atom.binding.changed` / `atom.config.changed`.
    pub async fn reload_profile(&mut self) -> Result<(), CoreError> {
        let new_cfg = profile::load_or_create(&self.paths)?;
        let old_bind = self.config.bindings.clone();
        let old_conf = self.config.config.clone();
        self.config = new_cfg.clone();
        self.registry = Registry::new(new_cfg.bindings.clone());
        let data_store = self.data_store.clone();
        Self::fill_registry(
            &mut self.registry,
            &self.manifests.clone(),
            &mut self.pkg_dirs,
            &data_store,
        );
        for (c, p) in self.temp_providers.clone() {
            self.registry.add_provider(&c, &p);
        }
        for (p, m, ph, pr) in self.temp_intercepts.clone() {
            let _ = self.registry.add_intercept(&p, &m, &ph, pr);
        }
        for (s, t) in self.temp_subs.clone() {
            let _ = self.registry.add_subscription(&s, &t);
        }
        let ctx = CallerCtx::local(&self.profile_name, "core");
        for (c, b) in &new_cfg.bindings {
            if old_bind.get(c) != Some(b) {
                let _ = Box::pin(self.publish_event(
                    "atom.binding.changed",
                    serde_json::json!({"contract": c}),
                    &ctx,
                ))
                .await;
            }
        }
        for (p, c) in &new_cfg.config {
            if old_conf.get(p) != Some(c) {
                let _ = Box::pin(self.publish_event(
                    "atom.config.changed",
                    serde_json::json!({"package": p}),
                    &ctx,
                ))
                .await;
            }
        }
        Ok(())
    }

    fn parse_action(v: serde_json::Value) -> Result<InterceptorAction, RpcError> {
        let action = v.get("action").and_then(|a| a.as_str()).unwrap_or("");
        match action {
            "continue" => Ok(InterceptorAction::Continue),
            "modify" => {
                if let Some(p) = v.get("params").cloned() {
                    Ok(InterceptorAction::ModifyParams(p))
                } else if let Some(r) = v.get("result").cloned() {
                    Ok(InterceptorAction::ModifyResult(r))
                } else {
                    Err(RpcError::new(
                        atom_protocol::ErrorCode::INVALID_PARAMS,
                        "modify exige params ou result",
                    ))
                }
            }
            "respond" => v
                .get("result")
                .cloned()
                .map(InterceptorAction::Respond)
                .ok_or_else(|| {
                    RpcError::new(
                        atom_protocol::ErrorCode::INVALID_PARAMS,
                        "respond exige result",
                    )
                }),
            "reject" => {
                let e = v.get("error").cloned().unwrap_or(serde_json::json!({}));
                let code = e
                    .get("code")
                    .and_then(|c| c.as_i64())
                    .unwrap_or(atom_protocol::ErrorCode::REJECTED as i64)
                    as i32;
                let msg = e
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("rejeitado por interceptor")
                    .to_string();
                Ok(InterceptorAction::Reject(RpcError::new(code, msg)))
            }
            _ => Err(RpcError::new(
                atom_protocol::ErrorCode::INVALID_PARAMS,
                format!("action inválida: {action:?}"),
            )),
        }
    }

    /// Invoca um interceptor com pump (atende `atom.call` aninhados do
    /// próprio interceptor, ex.: guard-shell → ui.prompt). Timeout 30s
    /// ou o restante do `deadline`, o que for menor (`[P-DL-4]`).
    // spec: [P-DL-4]
    async fn invoke_interceptor_pumped(
        &mut self,
        ext: &str,
        payload: serde_json::Value,
        ctx: &CallerCtx,
        depth: u8,
    ) -> Result<InterceptorAction, CoreError> {
        self.ensure_provider("", ext).await?;
        let (rx, _) = self.host.post(ext, "interceptor.invoke", payload).await?;
        let done = DriveDone::Return {
            cancel: Arc::new(Notify::new()),
        };
        // `[P-DL-4]`: prazo por salto = `min(30s, restante do deadline)`.
        let wait = match ctx.deadline {
            Some(d) => {
                let rest = d.saturating_sub(now_ms()).max(0) as u64;
                std::time::Duration::from_millis(rest.min(INTERCEPTOR_TIMEOUT_SECS * 1000))
            }
            None => std::time::Duration::from_secs(INTERCEPTOR_TIMEOUT_SECS),
        };
        let expired = ctx.deadline.is_some();
        let mut rx = rx;
        loop {
            tokio::select! {
                biased;
                resp = &mut rx => {
                    if expired && ctx.deadline.is_some_and(|d| d <= now_ms()) {
                        return Err(deadline_err(ctx.trace.as_deref().unwrap_or("")));
                    }
                    let resp = resp.map_err(|_| CoreError::ExtensionUnavailable(ext.to_string()))?;
                    if let Some(err) = resp.error {
                        return Err(CoreError::Protocol(err));
                    }
                    let v = resp.result.unwrap_or(serde_json::Value::Null);
                    return Self::parse_action(v).map_err(CoreError::Protocol);
                }
                inc = self.host.next_incoming() => {
                    match inc {
                        Some(i) => self.route_incoming(i, &done, depth + 1).await,
                        None => continue,
                    }
                }
                _ = tokio::time::sleep(wait) => {
                    let trace = ctx.trace.as_deref().unwrap_or("");
                    if expired {
                        return Err(deadline_err(trace));
                    }
                    return Err(CoreError::Protocol(RpcError::new(
                        atom_protocol::ErrorCode::INTERCEPTOR_TIMEOUT,
                        format!("{ext}: timeout em interceptor.invoke"),
                    )));
                }
            }
        }
    }

    /// Execução direta do provider (sem interceptors), com validação de contrato.
    #[allow(clippy::too_many_arguments)]
    async fn run_provider_direct(
        &mut self,
        provider: &str,
        service: &str,
        method: &str,
        params: serde_json::Value,
        ctx: &CallerCtx,
        trace: &str,
        depth: u8,
    ) -> Result<serde_json::Value, CoreError> {
        if let Err(e) = self.validate_contract(service, method, "params", &params) {
            return Err(CoreError::Protocol(e.with_trace(trace)));
        }
        self.ensure_provider(service, provider)
            .await
            .map_err(|e| match e {
                CoreError::Protocol(rpc) => CoreError::Protocol(rpc.with_trace(trace)),
                other => other,
            })?;
        let (rx, req_id) = self
            .host
            .post_invoke(provider, service, method, params, None, ctx)
            .await
            .map_err(|e| match e {
                CoreError::Protocol(rpc) => CoreError::Protocol(rpc.with_trace(trace)),
                other => other,
            })?;
        // Completa o topo da pilha com o id do salto (`[P-CANCEL-2]`).
        if let Some(top) = self.call_stack.last_mut() {
            top.provider = Some(provider.to_string());
            top.provider_req = Some(req_id);
        }
        // `cancel` do topo da pilha se houver (propagação `[P-CANCEL-2]`),
        // senão fresco que nunca dispara (chamada CLI/top-level).
        let cancel = self
            .call_stack
            .last()
            .map(|f| f.cancel.clone())
            .unwrap_or_else(|| Arc::new(Notify::new()));
        let done = DriveDone::Return { cancel };
        let v = Box::pin(self.drive(rx, &done, provider, depth, ctx.deadline))
            .await
            .map_err(|e| match e {
                CoreError::Protocol(rpc) => CoreError::Protocol(rpc.with_trace(trace)),
                other => other,
            })?;
        if let Err(e) = self.validate_contract(service, method, "result", &v) {
            return Err(CoreError::Protocol(e.with_trace(trace)));
        }
        Ok(v)
    }

    /// Cadeia `around` recursiva. Cada interceptor recebe `$proceed` (uso único).
    /// Sem `proceed` explícito, `continue`/`modify params` executa o resto
    /// implicitamente (auto-proceed).
    #[allow(clippy::too_many_arguments)]
    async fn run_arounds(
        &mut self,
        list: &[(String, i32)],
        idx: usize,
        service: &str,
        method: &str,
        params: serde_json::Value,
        ctx: &CallerCtx,
        trace: &str,
        caller: &str,
        depth: u8,
    ) -> Result<serde_json::Value, CoreError> {
        if idx >= list.len() {
            let (provider, _, _) =
                self.prepare(service, None, None, caller)
                    .map_err(|e| match e {
                        CoreError::Protocol(rpc) => CoreError::Protocol(rpc.with_trace(trace)),
                        other => other,
                    })?;
            // `prepare` acima resolve sem target; o provider real já foi
            // resolvido pelo chamador — re-resolve aqui só para o caso
            // `around` sem `before`. O caller passa o provider via params?
            // Para não duplicar resolução, o nível 0 sempre recebe o provider
            // correto via `invoke_with_interceptors`; aqui mantemos resolve.
            return Box::pin(
                self.run_provider_direct(&provider, service, method, params, ctx, trace, depth),
            )
            .await;
        }
        // `shortcut: re-resolve a cada nível evita carregar provider no
        // ProceedState; binding mutável no meio da cadeia usa valor atual.`
        let (ext, _) = list[idx].clone();
        let token = format!("proceed_{}", atom_protocol::new_trace_id());
        self.proceeds.insert(
            token.clone(),
            ProceedState {
                owner: ext.clone(),
                service: service.to_string(),
                method: method.to_string(),
                params: params.clone(),
                ctx: ctx.clone(),
                provider: String::new(),
                caller: caller.to_string(),
                depth,
                remaining: list[idx + 1..].to_vec(),
            },
        );
        let payload = serde_json::json!({
            "phase": "around",
            "service": service,
            "method": method,
            "params": params,
            "$proceed": token,
            "$ctx": serde_json::to_value(ctx).unwrap_or(serde_json::Value::Null),
        });
        let action = Box::pin(self.invoke_interceptor_pumped(&ext, payload, ctx, depth)).await;
        let _ = self.proceeded.remove(&token);
        let inner_val = self.proceed_results.remove(&token);
        self.proceeds.remove(&token);
        let action = action?;
        match action {
            InterceptorAction::Continue => {
                if let Some(v) = inner_val {
                    Ok(v)
                } else {
                    Box::pin(self.run_arounds(
                        list,
                        idx + 1,
                        service,
                        method,
                        params,
                        ctx,
                        trace,
                        caller,
                        depth,
                    ))
                    .await
                }
            }
            InterceptorAction::ModifyParams(p) => {
                if let Some(v) = inner_val {
                    // `modify params` após `proceed`: ignora novos params,
                    // mantém resultado do inner (proceed já executou).
                    Ok(v)
                } else {
                    Box::pin(self.run_arounds(
                        list,
                        idx + 1,
                        service,
                        method,
                        p,
                        ctx,
                        trace,
                        caller,
                        depth,
                    ))
                    .await
                }
            }
            InterceptorAction::ModifyResult(r) | InterceptorAction::Respond(r) => Ok(r),
            InterceptorAction::Reject(e) => Err(CoreError::Protocol(e.with_trace(trace))),
        }
    }

    /// Chamada com cadeia completa: `before` → `around` → provider → `after`.
    /// `caller` = extensão chamadora ou `"core"`/`"client"` (anti-recursão).
    /// `atom.*` nunca é interceptado. Exige `service:<contrato>` do chamador.
    #[allow(clippy::too_many_arguments)]
    async fn invoke_with_interceptors(
        &mut self,
        service: &str,
        method: &str,
        params: serde_json::Value,
        target: Option<&str>,
        ctx: &CallerCtx,
        trace: &str,
        caller: &str,
        depth: u8,
    ) -> Result<serde_json::Value, CoreError> {
        let r =
            Box::pin(self.invoke_inner(service, method, params, target, ctx, trace, caller, depth))
                .await;
        let (ok, code) = match &r {
            Ok(_) => (true, 0),
            Err(e) => (false, e.code()),
        };
        self.journal.call(trace, caller, service, method, ok, code);
        r
    }

    /// Corpo da cadeia (separado p/ journal único no wrapper).
    #[allow(clippy::too_many_arguments)]
    async fn invoke_inner(
        &mut self,
        service: &str,
        method: &str,
        params: serde_json::Value,
        target: Option<&str>,
        ctx: &CallerCtx,
        trace: &str,
        caller: &str,
        depth: u8,
    ) -> Result<serde_json::Value, CoreError> {
        if service.starts_with("atom.") {
            let provider = self
                .registry
                .resolve(service, target)
                .map_err(|e| match e {
                    CoreError::Protocol(rpc) => CoreError::Protocol(rpc.with_trace(trace)),
                    other => other,
                })?;
            return Box::pin(
                self.run_provider_direct(&provider, service, method, params, ctx, trace, depth),
            )
            .await;
        }
        if depth >= MAX_DEPTH {
            return Err(CoreError::Protocol(
                RpcError::new(
                    atom_protocol::ErrorCode::INVALID_REQUEST,
                    "profundidade máxima de chamadas",
                )
                .with_trace(trace),
            ));
        }
        // v0.6: chamador (extensão) precisa de `service:<contrato>`.
        if let Err(e) = self.check_cap(caller, &format!("service:{service}")) {
            return Err(CoreError::Protocol(e.with_trace(trace)));
        }
        let provider = self
            .registry
            .resolve(service, target)
            .map_err(|e| match e {
                CoreError::Protocol(rpc) => CoreError::Protocol(rpc.with_trace(trace)),
                other => other,
            })?;
        // `before` (prioridade crescente), sem auto-interceptação.
        // Sem grant `intercept:<contrato>` o interceptor é pulado (warn).
        let mut cur = params;
        let mut short_circuit: Option<serde_json::Value> = None;
        for (ext, _) in self
            .registry
            .intercepts_for(service, method, "before")
            .into_iter()
            .filter(|(e, _)| e != caller)
        {
            if !self.grant_ok(&ext, &format!("intercept:{service}")) {
                tracing::warn!(interceptor = %ext, "sem grant intercept:{service}, pulando");
                continue;
            }
            let payload = serde_json::json!({
                "phase": "before",
                "service": service,
                "method": method,
                "params": cur,
                "$ctx": serde_json::to_value(ctx).unwrap_or(serde_json::Value::Null),
            });
            match Box::pin(self.invoke_interceptor_pumped(&ext, payload, ctx, depth)).await? {
                InterceptorAction::Continue => {}
                InterceptorAction::ModifyParams(p) => cur = p,
                InterceptorAction::Respond(r) => {
                    short_circuit = Some(r);
                    break;
                }
                InterceptorAction::Reject(e) => {
                    return Err(CoreError::Protocol(e.with_trace(trace)));
                }
                InterceptorAction::ModifyResult(_) => {}
            }
        }
        // `around` ou provider direto.
        let arounds: Vec<(String, i32)> = self
            .registry
            .intercepts_for(service, method, "around")
            .into_iter()
            .filter(|(e, _)| e != caller)
            .filter(|(e, _)| {
                if self.grant_ok(e, &format!("intercept:{service}")) {
                    true
                } else {
                    tracing::warn!(interceptor = %e, "sem grant intercept:{service}, pulando");
                    false
                }
            })
            .collect();
        let mut result = if let Some(v) = short_circuit {
            v
        } else if arounds.is_empty() {
            Box::pin(self.run_provider_direct(
                &provider,
                service,
                method,
                cur.clone(),
                ctx,
                trace,
                depth,
            ))
            .await
            .map_err(|e| match e {
                CoreError::Protocol(rpc)
                    if rpc.code == atom_protocol::ErrorCode::CONTRACT_VALIDATION_FAILED =>
                {
                    CoreError::Protocol(rpc)
                }
                other => other,
            })?
        } else {
            // Provider já resolvido: `run_arounds` re-resolve; para respeitar
            // `target`, injeta binding temporário? Não: `prepare` sem target
            // usa binding único. `shortcut: around + target explícito usa o
            // mesmo provider resolvido acima via validate+direct quando há
            // apenas 1 around?` Para manter `target`, executa inner manual:
            // se target foi explícito e difere do resolve, o run_arounds
            // re-resolveria errado. Contorna: quando target explícito,
            // executa arounds com provider fixo via proceed direto.
            if target.is_some() {
                Box::pin(self.run_arounds_with_provider(
                    &arounds,
                    0,
                    &provider,
                    service,
                    method,
                    cur.clone(),
                    ctx,
                    trace,
                    caller,
                    depth,
                ))
                .await?
            } else {
                Box::pin(self.run_arounds(
                    &arounds,
                    0,
                    service,
                    method,
                    cur.clone(),
                    ctx,
                    trace,
                    caller,
                    depth,
                ))
                .await?
            }
        };
        // `after` em ordem reversa; recebe params finais + result/erro.
        // `shortcut: after não roda se before deu short-circuit? Roda sim:
        // permite auditoria de `respond`. Mantido simples: sempre roda.`
        let mut afters = self.registry.intercepts_for(service, method, "after");
        afters.reverse();
        for (ext, _) in afters.into_iter().filter(|(e, _)| e != caller) {
            if !self.grant_ok(&ext, &format!("intercept:{service}")) {
                tracing::warn!(interceptor = %ext, "sem grant intercept:{service}, pulando");
                continue;
            }
            let payload = serde_json::json!({
                "phase": "after",
                "service": service,
                "method": method,
                "params": cur,
                "result": result,
                "$ctx": serde_json::to_value(ctx).unwrap_or(serde_json::Value::Null),
            });
            match Box::pin(self.invoke_interceptor_pumped(&ext, payload, ctx, depth)).await? {
                InterceptorAction::Continue => {}
                InterceptorAction::ModifyResult(r) => result = r,
                InterceptorAction::Respond(r) => result = r,
                InterceptorAction::Reject(e) => {
                    return Err(CoreError::Protocol(e.with_trace(trace)));
                }
                InterceptorAction::ModifyParams(_) => {}
            }
        }
        if let Err(e) = self.validate_contract(service, method, "result", &result) {
            return Err(CoreError::Protocol(e.with_trace(trace)));
        }
        Ok(result)
    }

    /// Variante de `run_arounds` com provider fixo (para `target` explícito).
    #[allow(clippy::too_many_arguments)]
    async fn run_arounds_with_provider(
        &mut self,
        list: &[(String, i32)],
        idx: usize,
        provider: &str,
        service: &str,
        method: &str,
        params: serde_json::Value,
        ctx: &CallerCtx,
        trace: &str,
        caller: &str,
        depth: u8,
    ) -> Result<serde_json::Value, CoreError> {
        if idx >= list.len() {
            return Box::pin(
                self.run_provider_direct(provider, service, method, params, ctx, trace, depth),
            )
            .await;
        }
        let (ext, _) = list[idx].clone();
        let token = format!("proceed_{}", atom_protocol::new_trace_id());
        self.proceeds.insert(
            token.clone(),
            ProceedState {
                owner: ext.clone(),
                service: service.to_string(),
                method: method.to_string(),
                params: params.clone(),
                ctx: ctx.clone(),
                provider: provider.to_string(),
                caller: caller.to_string(),
                depth,
                remaining: list[idx + 1..].to_vec(),
            },
        );
        let payload = serde_json::json!({
            "phase": "around",
            "service": service,
            "method": method,
            "params": params,
            "$proceed": token,
            "$ctx": serde_json::to_value(ctx).unwrap_or(serde_json::Value::Null),
        });
        let action = Box::pin(self.invoke_interceptor_pumped(&ext, payload, ctx, depth)).await;
        let _ = self.proceeded.remove(&token);
        let inner_val = self.proceed_results.remove(&token);
        self.proceeds.remove(&token);
        let action = action?;
        match action {
            InterceptorAction::Continue => {
                if let Some(v) = inner_val {
                    Ok(v)
                } else {
                    Box::pin(self.run_arounds_with_provider(
                        list,
                        idx + 1,
                        provider,
                        service,
                        method,
                        params,
                        ctx,
                        trace,
                        caller,
                        depth,
                    ))
                    .await
                }
            }
            InterceptorAction::ModifyParams(p) => {
                if let Some(v) = inner_val {
                    Ok(v)
                } else {
                    Box::pin(self.run_arounds_with_provider(
                        list,
                        idx + 1,
                        provider,
                        service,
                        method,
                        p,
                        ctx,
                        trace,
                        caller,
                        depth,
                    ))
                    .await
                }
            }
            InterceptorAction::ModifyResult(r) | InterceptorAction::Respond(r) => Ok(r),
            InterceptorAction::Reject(e) => Err(CoreError::Protocol(e.with_trace(trace))),
        }
    }
    fn prepare(
        &self,
        service: &str,
        target: Option<&str>,
        forged: Option<CallerCtx>,
        caller: &str,
    ) -> Result<(String, CallerCtx, String), CoreError> {
        let trace = forged
            .as_ref()
            .and_then(|c| c.trace.clone())
            .unwrap_or_else(atom_protocol::new_trace_id);
        let provider = self.registry.resolve(service, target)?;
        let mut ctx = forged.unwrap_or_else(|| CallerCtx::local(&self.profile_name, caller));
        ctx.authorize(&self.profile_name, caller);
        Ok((provider, ctx, trace))
    }

    fn provider_failed(&mut self, producer: &str) {
        self.streams.retain(|_, r| r.producer != producer);
    }

    /// `true` se o produtor optou por crédito (`flow: "credit"`, `[P-STREAM-3]`).
    // spec: [P-STREAM-3]
    fn producer_wants_credit(&self, producer: &str) -> bool {
        self.host
            .states()
            .get(producer)
            .is_some_and(|(_, init)| init.flow.as_deref() == Some("credit"))
    }

    /// Envia `atom.stream.credit {stream, until}` ao produtor (`[P-STREAM-3]`).
    // spec: [P-STREAM-3]
    async fn send_credit(&mut self, stream: &str, until: u64) {
        let producer = self
            .streams
            .get(stream)
            .map(|r| r.producer.clone())
            .unwrap_or_default();
        if producer.is_empty() {
            return;
        }
        self.host
            .notify(
                &producer,
                "atom.stream.credit",
                serde_json::json!({"stream": stream, "until": until}),
            )
            .await;
    }

    /// Aplica `atom.stream.ack {stream, seq}` (`[P-STREAM-2/3]`): atualiza o
    /// `acked` cumulativo do consumidor e, se o produtor usa crédito,
    /// emite `atom.stream.credit {until = ack + window}`. Sem `flow`,
    /// o ack é ignorado.
    // spec: [P-STREAM-2]
    // spec: [P-STREAM-3]
    async fn apply_stream_ack(&mut self, inc: &Incoming) {
        let stream = inc
            .params
            .get("stream")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        let seq = inc.params.get("seq").and_then(|s| s.as_u64()).unwrap_or(0);
        if stream.is_empty() {
            return;
        }
        let (window, credit_producer, ready) = match self.streams.get_mut(&stream) {
            Some(rec) => {
                let Some(w) = rec.window else { return };
                if seq > rec.acked {
                    rec.acked = seq;
                }
                let limit = rec.acked.saturating_add(w as u64);
                let mut ready = vec![];
                while rec
                    .pending
                    .front()
                    .and_then(|p| p.get("seq").and_then(|s| s.as_u64()))
                    .is_some_and(|s| s <= limit)
                {
                    if let Some(p) = rec.pending.pop_front() {
                        ready.push(p);
                    } else {
                        break;
                    }
                }
                (w, rec.credit_producer, ready)
            }
            None => return,
        };
        for params in ready {
            self.deliver(&stream, "atom.stream.chunk", params).await;
        }
        if credit_producer {
            let until = seq.saturating_add(window as u64);
            if let Some(rec) = self.streams.get_mut(&stream) {
                rec.credit_until = until;
            }
            self.send_credit(&stream, until).await;
        }
    }

    /// Admite um `atom.stream.chunk` do produtor (`[P-STREAM-2/3/4]`):
    /// numera com `seq` contíguo do core, aplica janela do consumidor
    /// (retido em `pending_chunks` até o `ack` liberar) e crédito do
    /// produtor (excesso = `-32050` + fim do stream). Sem opt-ins,
    /// repassa como v1.0.
    // spec: [P-STREAM-2]
    // spec: [P-STREAM-3]
    // spec: [P-STREAM-4]
    async fn admit_stream_chunk(&mut self, inc: &Incoming) -> StreamChunkOutcome {
        let Some(stream) = inc
            .params
            .get("stream")
            .and_then(|s| s.as_str())
            .map(|s| s.to_string())
        else {
            return StreamChunkOutcome::Dropped;
        };
        let Some(rec) = self.streams.get_mut(&stream) else {
            return StreamChunkOutcome::Dropped;
        };
        // `[P-STREAM-3]`: produtor em modo crédito não pode exceder `until`.
        if rec.credit_producer && rec.next_seq > rec.credit_until {
            return StreamChunkOutcome::Terminated {
                stream,
                code: atom_protocol::ErrorCode::OVERLOADED,
                message: "crédito de stream excedido".to_string(),
            };
        }
        let seq = rec.next_seq;
        rec.next_seq = rec.next_seq.saturating_add(1);
        // `[P-STREAM-2]`: sem janela = entrega imediata (v1.0).
        let Some(window) = rec.window else {
            let mut params = inc.params.clone();
            if let Some(obj) = params.as_object_mut() {
                obj.insert("seq".to_string(), serde_json::json!(seq));
            }
            return StreamChunkOutcome::Delivered { stream, params };
        };
        // `[P-STREAM-2]`: `seq <= ack + window` entrega; senão retém.
        if seq <= rec.acked.saturating_add(window as u64) {
            let mut params = inc.params.clone();
            if let Some(obj) = params.as_object_mut() {
                obj.insert("seq".to_string(), serde_json::json!(seq));
            }
            StreamChunkOutcome::Delivered { stream, params }
        } else {
            let mut params = inc.params.clone();
            if let Some(obj) = params.as_object_mut() {
                obj.insert("seq".to_string(), serde_json::json!(seq));
            }
            if let Some(rec) = self.streams.get_mut(&stream) {
                rec.pending.push_back(params);
                while rec.pending.len() > rec.pending_cap {
                    rec.pending.pop_front();
                }
            }
            StreamChunkOutcome::Dropped
        }
    }

    async fn deliver(&mut self, stream: &str, method: &str, params: serde_json::Value) {
        dbgtrace(&format!("deliver {method} stream={stream}"));
        let consumer = self.streams.get(stream).map(|r| match &r.consumer {
            StreamConsumer::Ext(e) => StreamConsumer::Ext(e.clone()),
            StreamConsumer::Chan(tx) => StreamConsumer::Chan(tx.clone()),
        });
        match consumer {
            Some(StreamConsumer::Ext(e)) => {
                self.host.notify(&e, method, params).await;
            }
            Some(StreamConsumer::Chan(tx)) => {
                let _ = tx
                    .send(StreamEvent {
                        method: method.to_string(),
                        params,
                    })
                    .await;
            }
            None => {}
        }
    }

    /// `atom.call`: contexto forjado é sobrescrito (tenant sempre `local`).
    /// Passa por `before` → `around` → provider → `after` (v0.3).
    pub async fn call(
        &mut self,
        service: &str,
        method: &str,
        params: serde_json::Value,
        target: Option<&str>,
        forged: Option<CallerCtx>,
    ) -> Result<serde_json::Value, CoreError> {
        let trace = forged
            .as_ref()
            .and_then(|c| c.trace.clone())
            .unwrap_or_else(atom_protocol::new_trace_id);
        let mut ctx = forged.unwrap_or_else(|| CallerCtx::local(&self.profile_name, "core"));
        ctx.authorize(&self.profile_name, "core");
        // `[P-DL-1/2]`: propaga `deadline` por toda a cadeia; aplica teto e
        // padrão do perfil. Já estourado = `-32022` sem invocar nada.
        // spec: [P-DL-1]
        // spec: [P-DL-2]
        // spec: [P-DL-3]
        if self.clamp_deadline(&mut ctx) {
            return Err(deadline_err(&trace));
        }
        Box::pin(
            self.invoke_with_interceptors(service, method, params, target, &ctx, &trace, "core", 0),
        )
        .await
    }

    /// Chamada a partir de `AtomCallParams` (socket/CLI).
    pub async fn call_params(&mut self, p: AtomCallParams) -> Result<serde_json::Value, RpcError> {
        let trace = p
            .ctx
            .as_ref()
            .and_then(|c| c.trace.clone())
            .unwrap_or_else(atom_protocol::new_trace_id);
        self.call(&p.service, &p.method, p.params, p.target.as_deref(), p.ctx)
            .await
            .map_err(|e| e.rpc(&trace))
    }

    /// Valida `flow.window` opt-in (`[P-STREAM-2]`): 1..=4096, senão `INVALID_PARAMS`.
    // spec: [P-STREAM-2]
    fn flow_window(flow: &Option<atom_protocol::FlowOpts>) -> Result<Option<u32>, CoreError> {
        match flow {
            Some(f) if !(1..=4096).contains(&f.window) => Err(CoreError::Protocol(RpcError::new(
                atom_protocol::ErrorCode::INVALID_PARAMS,
                format!("flow.window {} fora de 1..=4096", f.window),
            ))),
            Some(f) => Ok(Some(f.window)),
            None => Ok(None),
        }
    }

    /// `atom.call` com `stream: true` e consumidor fora do bus.
    /// O chamador gera `id`, responde `{"stream": id}` e repassa chunks/end
    /// ao canal. Retorna o resultado final (o fim já foi notificado).
    pub async fn call_stream_chan(
        &mut self,
        sc: StreamCall,
        tx: mpsc::Sender<StreamEvent>,
        cancel: Arc<Notify>,
    ) -> Result<serde_json::Value, CoreError> {
        let StreamCall {
            id,
            service,
            method,
            params,
            target,
            ctx: forged,
            flow,
        } = sc;
        // `[P-STREAM-2]`: `window` fora de 1..=4096 = `INVALID_PARAMS`.
        // spec: [P-STREAM-2]
        let window = Self::flow_window(&flow).map_err(|e| match e {
            CoreError::Protocol(rpc) => CoreError::Protocol(
                rpc.with_trace(
                    &forged
                        .as_ref()
                        .and_then(|c| c.trace.clone())
                        .unwrap_or_else(atom_protocol::new_trace_id),
                ),
            ),
            other => other,
        })?;
        let mut tmp_ctx = forged.unwrap_or_else(|| CallerCtx::local(&self.profile_name, "core"));
        tmp_ctx.authorize(&self.profile_name, "core");
        // `[P-DL-1/2]`: deadline do socket/CLI propaga ao provider.
        // spec: [P-DL-1]
        // spec: [P-DL-2]
        // spec: [P-DL-3]
        let tmp_trace = tmp_ctx
            .trace
            .clone()
            .unwrap_or_else(atom_protocol::new_trace_id);
        if self.clamp_deadline(&mut tmp_ctx) {
            return Err(deadline_err(&tmp_trace));
        }
        let mut sparams = params.clone();
        for (ext, _) in self
            .registry
            .intercepts_for(&service, &method, "before")
            .into_iter()
            .filter(|(e, _)| e != "core")
        {
            let payload = serde_json::json!({
                "phase": "before",
                "service": service,
                "method": method,
                "params": sparams,
                "$ctx": serde_json::to_value(&tmp_ctx).unwrap_or(serde_json::Value::Null),
            });
            let act = Box::pin(self.invoke_interceptor_pumped(&ext, payload, &tmp_ctx, 0))
                .await
                .map_err(|e| match e {
                    CoreError::Protocol(rpc) => CoreError::Protocol(rpc.with_trace(&tmp_trace)),
                    other => other,
                })?;
            match act {
                InterceptorAction::Continue => {}
                InterceptorAction::ModifyParams(p) => sparams = p,
                InterceptorAction::Respond(r) => return Ok(r),
                InterceptorAction::Reject(e) => {
                    let trace2 = tmp_ctx
                        .trace
                        .clone()
                        .unwrap_or_else(atom_protocol::new_trace_id);
                    return Err(CoreError::Protocol(e.with_trace(&trace2)));
                }
                InterceptorAction::ModifyResult(_) => {}
            }
        }
        let (provider, ctx, trace) =
            self.prepare(&service, target.as_deref(), Some(tmp_ctx.clone()), "core")?;
        let map_err = |e: CoreError| match e {
            CoreError::Protocol(rpc) => CoreError::Protocol(rpc.with_trace(&trace)),
            other => other,
        };
        self.ensure_provider(&service, &provider)
            .await
            .map_err(map_err)?;
        self.streams.insert(
            id.to_string(),
            StreamRec {
                producer: provider.clone(),
                consumer: StreamConsumer::Chan(tx),
                provider_req: String::new(),
                window,
                acked: 0,
                next_seq: 0,
                pending: VecDeque::new(),
                pending_cap: self.config.bus.stream_buf.max(1),
                credit_producer: self.producer_wants_credit(&provider),
                credit_until: u64::MAX,
            },
        );
        let (rx, req_id) = self
            .host
            .post_invoke(
                &provider,
                &service,
                &method,
                sparams,
                Some(id.as_str()),
                &ctx,
            )
            .await
            .map_err(map_err)?;
        if let Some(rec) = self.streams.get_mut(&id) {
            rec.provider_req = req_id;
            if rec.credit_producer && rec.window.is_some() {
                rec.credit_until = rec.window.unwrap_or(0) as u64;
            }
        }
        if self
            .streams
            .get(&id)
            .is_some_and(|r| r.credit_producer && r.window.is_some())
        {
            let until = self.streams.get(&id).and_then(|r| r.window).unwrap_or(0) as u64;
            self.send_credit(&id, until).await;
        }
        let done = DriveDone::StreamDiscard {
            id: id.to_string(),
            cancel,
        };
        let r = self
            .drive(rx, &done, &provider, 0, tmp_ctx.deadline)
            .await
            .map_err(map_err);
        let (ok, code) = match &r {
            Ok(_) => (true, 0),
            Err(e) => (false, e.code()),
        };
        self.journal
            .call(&trace, "core", &service, &method, ok, code);
        r
    }

    /// Bomba do bus: espera o provedor servindo chamadas aninhadas de extensões.
    async fn drive(
        &mut self,
        mut rx: InvokeRx,
        done: &DriveDone,
        producer: &str,
        depth: u8,
        deadline: Option<i64>,
    ) -> Result<serde_json::Value, CoreError> {
        // `rx_done`: resposta chegou antes do `end` (corrida entre o `oneshot`
        // e as notificações na fila). Nesse caso drena a fila e espera o `end`.
        let mut rx_done = false;
        // `[P-DL-3]`: prazo absoluto; ao estourar, a cadeia é cancelada e o
        // chamador recebe `-32022` (resposta tardia é descartada, `[P-CANCEL-4]`).
        // spec: [P-DL-3]
        // spec: [P-CANCEL-4]
        let expiry = async move {
            match deadline {
                Some(d) => {
                    let rest = d.saturating_sub(now_ms()).max(0) as u64;
                    tokio::time::sleep(std::time::Duration::from_millis(rest)).await;
                }
                None => std::future::pending::<()>().await,
            }
        };
        tokio::pin!(expiry);
        loop {
            tokio::select! {
                biased;
                _ = &mut expiry => {
                    return Err(deadline_err(""));
                }
                _ = async {
                    match done {
                        DriveDone::StreamDiscard { cancel, .. } => cancel.notified().await,
                        DriveDone::Return { cancel } => cancel.notified().await,
                    }
                } => {
                    let is_stream = matches!(done, DriveDone::StreamDiscard { .. });
                    let id = match done {
                        DriveDone::StreamDiscard { id, .. } => id.clone(),
                        DriveDone::Return { .. } => String::new(),
                    };
                    let target = self
                        .streams
                        .get(&id)
                        .map(|r| r.producer.clone())
                        .unwrap_or_else(|| producer.to_string());
                    if is_stream {
                        self.host
                            .notify(
                                &target,
                                "atom.stream.cancel",
                                serde_json::json!({"stream": id}),
                            )
                            .await;
                        self.streams.remove(&id);
                    }
                    // `[P-CANCEL-3/4]`: resposta tardia do provedor é descartada
                    // (`rx` é dropado aqui); exatamente uma resposta ao chamador.
                    // spec: [P-CANCEL-3]
                    // spec: [P-CANCEL-4]
                    return Err(CoreError::Protocol(atom_protocol::RpcError::new(
                        atom_protocol::ErrorCode::REQUEST_CANCELLED,
                        "cancelado",
                    )));
                }
                resp = &mut rx, if !rx_done => {
                    rx_done = true;
                    match resp {
                        Err(_) => {
                            self.provider_failed(producer);
                            self.drain_incoming(done, depth).await;
                            return Err(CoreError::ExtensionUnavailable(format!(
                                "{producer} caiu"
                            )));
                        }
                        Ok(resp) => {
                            if let Some(err) = resp.error {
                                self.provider_failed(producer);
                                self.drain_incoming(done, depth).await;
                                return Err(CoreError::Protocol(err));
                            }
                            let v = resp.result.unwrap_or(serde_json::Value::Null);
                            match done {
                                DriveDone::Return { .. } => return Ok(v),
                                DriveDone::StreamDiscard { id, .. } => {
                                    // Drena chunks/end que já estão na fila; se o
                                    // `end` já foi processado, a entrada sumiu.
                                    self.drain_incoming(done, depth).await;
                                    if !self.streams.contains_key(id) {
                                        return Ok(v);
                                    }
                                    // Resposta antes do `end`: espera o `end`
                                    // (todo produtor de stream o emite).
                                    continue;
                                }
                            }
                        }
                    }
                }
                inc = self.host.next_incoming() => {
                    match inc {
                        Some(i) => {
                            dbgtrace(&format!("incoming {} from {}", i.method, i.ext));
                            self.route_incoming(i, done, depth).await
                        }
                        None => continue,
                    }
                }
            }
        }
    }

    /// Drena mensagens já enfileiradas (corrida resposta × chunks).
    async fn drain_incoming(&mut self, done: &DriveDone, depth: u8) {
        while let Some(i) = self.host.try_incoming() {
            dbgtrace(&format!("dreno {} from {}", i.method, i.ext));
            self.route_incoming(i, done, depth).await;
        }
    }

    /// Responde `atom.call` etc. vindo de extensão (só se tinha id).
    async fn answer_inc(&mut self, inc: &Incoming, r: Result<serde_json::Value, RpcError>) {
        if let Some(id) = inc.id.clone() {
            let resp = match r {
                Ok(v) => RpcResponse::ok(id.clone(), v),
                Err(e) => RpcResponse::err(id.clone(), e),
            };
            self.host.answer(&inc.ext, id, resp).await;
        }
    }

    /// Roteia mensagem de extensão ao core durante um `drive`.
    async fn route_incoming(&mut self, inc: Incoming, done: &DriveDone, depth: u8) {
        match inc.method.as_str() {
            "atom.call" => match self.nested_call(&inc, depth).await {
                Ok(Some(v)) => self.answer_inc(&inc, Ok(v)).await,
                Ok(None) => {}
                Err(e) => self.answer_inc(&inc, Err(e)).await,
            },
            "atom.stream.chunk" => {
                // `[P-STREAM-3/4]`: valida crédito do produtor e numera o chunk.
                // spec: [P-STREAM-3]
                // spec: [P-STREAM-4]
                let outcome = self.admit_stream_chunk(&inc).await;
                match outcome {
                    StreamChunkOutcome::Delivered { stream, params } => {
                        self.deliver(&stream, "atom.stream.chunk", params).await;
                    }
                    StreamChunkOutcome::Terminated {
                        stream,
                        code,
                        message,
                    } => {
                        let err_params = serde_json::json!({
                            "stream": stream,
                            "error": {"code": code, "message": message},
                        });
                        self.deliver(&stream, "atom.stream.end", err_params).await;
                        self.streams.remove(&stream);
                    }
                    StreamChunkOutcome::Dropped => {}
                }
                if inc.id.is_some() {
                    self.answer_inc(&inc, Ok(serde_json::Value::Null)).await;
                }
            }
            "atom.stream.end" => {
                if let Some(s) = inc.params.get("stream").and_then(|s| s.as_str()) {
                    let s = s.to_string();
                    let params = inc.params.clone();
                    self.deliver(&s, "atom.stream.end", params).await;
                    self.streams.remove(&s);
                }
                if inc.id.is_some() {
                    self.answer_inc(&inc, Ok(serde_json::Value::Null)).await;
                }
            }
            "atom.stream.cancel" => {
                if let Some(s) = inc.params.get("stream").and_then(|s| s.as_str()) {
                    // só repassar: o loop trata (notifica produtor + erro -32800).
                    let mine = matches!(done, DriveDone::StreamDiscard { id, .. } if id == s);
                    // `[P-CANCEL-2]`: Ctrl+C chega ao provedor também como
                    // `$/cancelRequest` com o id do salto dele.
                    // spec: [P-CANCEL-2]
                    let prod = self
                        .streams
                        .get(s)
                        .map(|r| (r.producer.clone(), r.provider_req.clone()));
                    if let Some((target, req)) = prod {
                        if !req.is_empty() {
                            self.host
                                .notify(&target, "$/cancelRequest", serde_json::json!({"id": req}))
                                .await;
                        }
                        if !mine {
                            self.host
                                .notify(
                                    &target,
                                    "atom.stream.cancel",
                                    serde_json::json!({"stream": s}),
                                )
                                .await;
                        }
                    }
                    if mine {
                        if let DriveDone::StreamDiscard { cancel, .. } = done {
                            cancel.notify_one();
                        }
                    } else if let DriveDone::StreamDiscard { cancel, .. } = done {
                        cancel.notify_one();
                    }
                }
                if inc.id.is_some() {
                    self.answer_inc(&inc, Ok(serde_json::Value::Null)).await;
                }
            }
            "$/cancelRequest" => {
                // `[P-CANCEL-1/2]`: `id` é do salto do remetente; mapeia para o
                // salto do provedor e encaminha. Desconhecido = ignorado.
                // spec: [P-CANCEL-1]
                // spec: [P-CANCEL-2]
                if let Some(cid) = inc.params.get("id") {
                    let mut hit: Option<(String, String, Arc<Notify>)> = None;
                    for f in self.call_stack.iter_mut().rev() {
                        let same_ext = f.caller_ext == inc.ext;
                        let same_id = f.caller_id.as_ref().is_some_and(|v| v == cid);
                        if same_ext && same_id && !f.cancelled {
                            f.cancelled = true;
                            if let (Some(p), Some(q)) = (f.provider.clone(), f.provider_req.clone())
                            {
                                hit = Some((p, q, f.cancel.clone()));
                            }
                            break;
                        }
                    }
                    if let Some((target, req, cancel)) = hit {
                        self.host
                            .notify(&target, "$/cancelRequest", serde_json::json!({"id": req}))
                            .await;
                        // `[P-CANCEL-3]`: cortesia de 2s; se o provedor não
                        // responder, `drive` aborta com `-32800`.
                        // spec: [P-CANCEL-3]
                        tokio::spawn(async move {
                            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                            cancel.notify_one();
                        });
                    }
                }
                if inc.id.is_some() {
                    self.answer_inc(&inc, Ok(serde_json::Value::Null)).await;
                }
            }
            "atom.stream.ack" => {
                // `[P-STREAM-2]`: `ack.seq` cumulativo libera `seq <= ack + window`
                // e renova o crédito do produtor (`until = ack + window`).
                // Sem `flow` no setup, ack é ignorado (responde null).
                // spec: [P-STREAM-2]
                // spec: [P-STREAM-3]
                self.apply_stream_ack(&inc).await;
                if inc.id.is_some() {
                    self.answer_inc(&inc, Ok(serde_json::Value::Null)).await;
                }
            }
            "atom.secrets.get" => {
                let r = inc
                    .params
                    .get("name")
                    .and_then(|n| n.as_str())
                    .map(|n| self.secrets.rpc_get(n))
                    .unwrap_or_else(|| {
                        Err(RpcError::new(
                            atom_protocol::ErrorCode::INVALID_PARAMS,
                            "name",
                        ))
                    });
                self.answer_inc(&inc, r).await;
            }
            "atom.secrets.set" => {
                let r = (|| {
                    let n = inc
                        .params
                        .get("name")
                        .and_then(|n| n.as_str())
                        .ok_or_else(|| {
                            RpcError::new(atom_protocol::ErrorCode::INVALID_PARAMS, "name")
                        })?;
                    let v = inc
                        .params
                        .get("value")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            RpcError::new(atom_protocol::ErrorCode::INVALID_PARAMS, "value")
                        })?;
                    self.secrets.rpc_set(n, v)
                })();
                self.answer_inc(&inc, r).await;
            }
            "atom.registry.list" => {
                let mut providers: HashMap<String, Vec<String>> = HashMap::new();
                for (contract, name) in self.registry.providers_snapshot() {
                    providers.entry(contract).or_default().push(name);
                }
                self.answer_inc(
                    &inc,
                    Ok(serde_json::json!({
                        "providers": providers,
                        "bindings": self.registry.bindings(),
                    })),
                )
                .await;
            }
            "atom.profile.info" => {
                let (ws_url, _token) = self.ws_info.clone().unzip();
                self.answer_inc(
                    &inc,
                    Ok(serde_json::json!({
                        "profile": self.profile_name,
                        "core": crate::CORE_VERSION,
                        "protocol": crate::PROTOCOL,
                        "platform": crate::pkg::current_platform(),
                        "ws": ws_url,
                        "ui": self.ui_url,
                    })),
                )
                .await;
            }
            "atom.packages.mods" => {
                // Pacotes `kind="ui-mod"` instalados + contributes (v0.8).
                // Sem caminhos absolutos: assets saem via `atom.packages.asset`.
                let mut mods: Vec<serde_json::Value> = vec![];
                let mut names: Vec<&String> = self.manifests.keys().collect();
                names.sort();
                for n in names {
                    if let Some((lp, m)) = self.manifests.get(n) {
                        if m.kind != "ui-mod" {
                            continue;
                        }
                        mods.push(serde_json::json!({
                            "name": m.name,
                            "version": lp.version,
                            "contributes": serde_json::to_value(&m.contributes).unwrap_or(serde_json::Value::Null),
                        }));
                    }
                }
                let mut bindings: HashMap<String, String> = HashMap::new();
                for (k, v) in self.registry.bindings() {
                    if k.starts_with("ui.") {
                        bindings.insert(k.clone(), v.clone());
                    }
                }
                self.answer_inc(
                    &inc,
                    Ok(serde_json::json!({"mods": mods, "bindings": bindings})),
                )
                .await;
            }
            "atom.packages.asset" => {
                let r = (|| {
                    let pkg = inc
                        .params
                        .get("package")
                        .and_then(|p| p.as_str())
                        .ok_or_else(|| {
                            RpcError::new(atom_protocol::ErrorCode::INVALID_PARAMS, "package")
                        })?;
                    let rel = inc
                        .params
                        .get("path")
                        .and_then(|p| p.as_str())
                        .ok_or_else(|| {
                            RpcError::new(atom_protocol::ErrorCode::INVALID_PARAMS, "path")
                        })?;
                    // Sem escapes: relativo, sem `..`, sem absoluto, teto 2 MiB.
                    let relp = Path::new(rel);
                    if relp.is_absolute() || rel.contains("..") {
                        return Err(RpcError::new(
                            atom_protocol::ErrorCode::INVALID_PARAMS,
                            "path fora do pacote",
                        ));
                    }
                    let (_, m) = self.manifests.get(pkg).ok_or_else(|| {
                        RpcError::new(
                            atom_protocol::ErrorCode::NO_PROVIDER,
                            format!("pacote {pkg} não instalado"),
                        )
                    })?;
                    if m.kind != "ui-mod" {
                        return Err(RpcError::new(
                            atom_protocol::ErrorCode::INVALID_PARAMS,
                            "só ui-mod expõe assets",
                        ));
                    }
                    let dir = self.pkg_dirs.get(pkg).ok_or_else(|| {
                        RpcError::new(
                            atom_protocol::ErrorCode::NO_PROVIDER,
                            format!("pacote {pkg} sem diretório"),
                        )
                    })?;
                    let data = std::fs::read(dir.join(relp)).map_err(|_| {
                        RpcError::new(
                            atom_protocol::ErrorCode::PACKAGE_ERROR,
                            format!("asset {rel} ausente"),
                        )
                    })?;
                    if data.len() > 2 * 1024 * 1024 {
                        return Err(RpcError::new(
                            atom_protocol::ErrorCode::INVALID_PARAMS,
                            "asset > 2 MiB",
                        ));
                    }
                    Ok(serde_json::json!({
                        "encoding": "base64",
                        "data": base64::engine::general_purpose::STANDARD.encode(&data),
                    }))
                })();
                self.answer_inc(&inc, r).await;
            }
            "atom.ui.serve" => {
                // Shell ui-web anuncia a base HTTP (v0.8); some no `status`.
                let url = inc
                    .params
                    .get("url")
                    .and_then(|u| u.as_str())
                    .unwrap_or("")
                    .to_string();
                if url.is_empty() {
                    self.answer_inc(
                        &inc,
                        Err(RpcError::new(
                            atom_protocol::ErrorCode::INVALID_PARAMS,
                            "url",
                        )),
                    )
                    .await;
                } else {
                    self.ui_url = Some(url);
                    self.answer_inc(&inc, Ok(serde_json::json!({}))).await;
                }
            }
            "atom.ui.session" => {
                // Sessão p/ o shell web: URL do WS + token (v0.8). Extensões
                // já falam com o bus, então expor aqui não eleva privilégio.
                let (ws_url, token) = match &self.ws_info {
                    Some((u, t)) => (serde_json::json!(u), serde_json::json!(t)),
                    None => (serde_json::Value::Null, serde_json::Value::Null),
                };
                self.answer_inc(
                    &inc,
                    Ok(serde_json::json!({"wsUrl": ws_url, "token": token})),
                )
                .await;
            }
            "atom.events.publish" => {
                let topic = inc
                    .params
                    .get("topic")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
                let data = inc
                    .params
                    .get("data")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                if topic.is_empty() {
                    self.answer_inc(
                        &inc,
                        Err(RpcError::new(
                            atom_protocol::ErrorCode::INVALID_PARAMS,
                            "topic",
                        )),
                    )
                    .await;
                } else if let Err(e) = self.check_cap(&inc.ext, &format!("events.publish:{topic}"))
                {
                    self.journal.event(&topic, &inc.ext);
                    self.answer_inc(&inc, Err(e)).await;
                } else {
                    let ctx = CallerCtx::local(&self.profile_name, &inc.ext);
                    let r = Box::pin(self.publish_event(&topic, data, &ctx))
                        .await
                        .map(|_| serde_json::json!({}));
                    self.answer_inc(&inc, r.map_err(|e| e.rpc(&atom_protocol::new_trace_id())))
                        .await;
                }
            }
            "atom.events.subscribe" => {
                // `[P-EV-3/4]`: `from: {epoch, offset}` reentrega retidos com
                // `offset > from.offset` que casam os tópicos, depois ao vivo.
                // `epoch` divergente ou offset fora do ring = `atom.events.gap`.
                // Sem `from` = v1 at-most-once. `max_subs` limita (`[P-EV-5]`).
                // spec: [P-EV-3]
                // spec: [P-EV-4]
                // spec: [P-EV-5]
                let topics: Vec<String> = inc
                    .params
                    .get("topics")
                    .and_then(|t| t.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .or_else(|| {
                        inc.params
                            .get("topic")
                            .and_then(|t| t.as_str())
                            .map(|s| vec![s.to_string()])
                    })
                    .unwrap_or_default();
                let from: Option<EventCursor> = inc
                    .params
                    .get("from")
                    .and_then(|v| serde_json::from_value(v.clone()).ok());
                if topics.is_empty() {
                    self.answer_inc(
                        &inc,
                        Err(RpcError::new(
                            atom_protocol::ErrorCode::INVALID_PARAMS,
                            "topics",
                        )),
                    )
                    .await;
                } else if self.subscription_count(&inc.ext) + topics.len()
                    > self.config.bus.max_subs.max(1)
                {
                    self.answer_inc(
                        &inc,
                        Err(RpcError::new(
                            atom_protocol::ErrorCode::OVERLOADED,
                            "assinaturas demais",
                        )),
                    )
                    .await;
                } else if let Some(t) = topics.iter().find(|t| {
                    !t.starts_with("atom.")
                        && self
                            .check_cap(&inc.ext, &format!("events.subscribe:{t}"))
                            .is_err()
                }) {
                    self.answer_inc(
                        &inc,
                        Err(RpcError::new(
                            atom_protocol::ErrorCode::CAPABILITY_DENIED,
                            format!("{} sem capability events.subscribe:{t}", inc.ext),
                        )),
                    )
                    .await;
                } else {
                    for t in &topics {
                        let _ = self.registry.add_subscription(&inc.ext, t);
                        if !self
                            .temp_subs
                            .iter()
                            .any(|(s, tp)| s == &inc.ext && tp == t)
                        {
                            self.temp_subs.push((inc.ext.clone(), t.clone()));
                        }
                    }
                    let sub = format!("sub_{}", atom_protocol::new_trace_id());
                    self.answer_inc(&inc, Ok(serde_json::json!({"subscription": sub})))
                        .await;
                    if let Some(from) = from {
                        self.replay_events(&inc.ext, &topics, &from).await;
                    }
                }
            }
            "atom.events.unsubscribe" => {
                let topics: Vec<String> = inc
                    .params
                    .get("topics")
                    .and_then(|t| t.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .or_else(|| {
                        inc.params
                            .get("topic")
                            .and_then(|t| t.as_str())
                            .map(|s| vec![s.to_string()])
                    })
                    .unwrap_or_default();
                if topics.is_empty() {
                    self.registry.subscribes_retain(&inc.ext);
                    self.temp_subs.retain(|(s, _)| s != &inc.ext);
                } else {
                    for t in &topics {
                        self.registry.remove_subscription(&inc.ext, t);
                    }
                    self.temp_subs
                        .retain(|(s, tp)| !(s == &inc.ext && topics.contains(tp)));
                }
                self.answer_inc(&inc, Ok(serde_json::json!({}))).await;
            }
            "atom.register" => {
                if let Some(arr) = inc.params.get("provides").and_then(|v| v.as_array()) {
                    for p in arr {
                        if let Some(c) = p.get("contract").and_then(|v| v.as_str()) {
                            self.registry.add_provider(c, &inc.ext);
                            if !self
                                .temp_providers
                                .iter()
                                .any(|(cc, pp)| cc == c && pp == &inc.ext)
                            {
                                self.temp_providers.push((c.to_string(), inc.ext.clone()));
                            }
                        }
                    }
                }
                if let Some(arr) = inc.params.get("intercepts").and_then(|v| v.as_array()) {
                    for item in arr {
                        let m = item.get("match").and_then(|v| v.as_str()).unwrap_or("");
                        let ph = item
                            .get("phase")
                            .and_then(|v| v.as_str())
                            .unwrap_or("before");
                        let pr =
                            item.get("priority").and_then(|v| v.as_i64()).unwrap_or(100) as i32;
                        if !m.is_empty() && self.registry.add_intercept(&inc.ext, m, ph, pr).is_ok()
                        {
                            self.temp_intercepts.push((
                                inc.ext.clone(),
                                m.to_string(),
                                ph.to_string(),
                                pr,
                            ));
                        }
                    }
                }
                let subs: Vec<String> = inc
                    .params
                    .get("subscribes")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                for t in subs {
                    let _ = self.registry.add_subscription(&inc.ext, &t);
                    if !self
                        .temp_subs
                        .iter()
                        .any(|(s, tp)| s == &inc.ext && tp == &t)
                    {
                        self.temp_subs.push((inc.ext.clone(), t));
                    }
                }
                self.answer_inc(&inc, Ok(serde_json::json!({}))).await;
            }
            "atom.grants.request" => {
                let caps: Vec<String> = inc
                    .params
                    .get("capabilities")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str())
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect()
                    })
                    .unwrap_or_default();
                if caps.is_empty() {
                    self.answer_inc(
                        &inc,
                        Err(RpcError::new(
                            atom_protocol::ErrorCode::INVALID_PARAMS,
                            "capabilities",
                        )),
                    )
                    .await;
                } else {
                    let entry = self.pending_grants.entry(inc.ext.clone()).or_default();
                    for c in &caps {
                        if !entry.contains(c) {
                            entry.push(c.clone());
                        }
                    }
                    self.journal.grant(&inc.ext, "requested");
                    self.answer_inc(&inc, Ok(serde_json::json!({"status": "pending"})))
                        .await;
                }
            }
            "__ext_exit" => {
                // Morte detectada pela task de leitura (supervisão v0.6).
                Box::pin(self.handle_ext_exit(&inc.ext)).await;
            }
            "__ext_restart" => {
                let name = inc.ext.clone();
                // Restart com backoff vencido: revalida config e sobe.
                let r = self.start_ext(&name).await;
                match r {
                    Ok(()) => {
                        let ctx = CallerCtx::local(&self.profile_name, "core");
                        let _ = Box::pin(self.publish_event(
                            "atom.extension.started",
                            serde_json::json!({"package": name}),
                            &ctx,
                        ))
                        .await;
                    }
                    Err(CoreError::Package(e)) => {
                        tracing::warn!(pacote = %name, "restart: config inválida: {e}");
                    }
                    Err(e) => {
                        if let Some(st) = self.host.get_mut(&name) {
                            st.state = crate::host::ExtState::Failed(e.to_string());
                        }
                        let ctx = CallerCtx::local(&self.profile_name, "core");
                        let _ = Box::pin(self.publish_event(
                            "atom.extension.failed",
                            serde_json::json!({"package": name, "restarts": -1}),
                            &ctx,
                        ))
                        .await;
                    }
                }
            }
            "atom.proceed" => {
                let token = inc
                    .params
                    .get("token")
                    .and_then(|t| t.as_str())
                    .or_else(|| inc.params.get("$proceed").and_then(|t| t.as_str()))
                    .unwrap_or("")
                    .to_string();
                let st = self.proceeds.remove(&token);
                match st {
                    None => {
                        self.answer_inc(
                            &inc,
                            Err(RpcError::new(
                                atom_protocol::ErrorCode::INVALID_PARAMS,
                                "token inválido",
                            )),
                        )
                        .await;
                    }
                    Some(s) if s.owner != inc.ext => {
                        // Devolve token para o dono legítimo usar.
                        self.proceeds.insert(token, s);
                        self.answer_inc(
                            &inc,
                            Err(RpcError::new(
                                atom_protocol::ErrorCode::INVALID_PARAMS,
                                "proceed de outro interceptor",
                            )),
                        )
                        .await;
                    }
                    Some(s) => {
                        self.proceeded.insert(token.clone());
                        let inner = if s.provider.is_empty() {
                            Box::pin(self.run_arounds(
                                &s.remaining,
                                0,
                                &s.service,
                                &s.method,
                                s.params.clone(),
                                &s.ctx,
                                &atom_protocol::new_trace_id(),
                                &s.caller,
                                s.depth + 1,
                            ))
                            .await
                        } else {
                            Box::pin(self.run_arounds_with_provider(
                                &s.remaining,
                                0,
                                &s.provider,
                                &s.service,
                                &s.method,
                                s.params.clone(),
                                &s.ctx,
                                &atom_protocol::new_trace_id(),
                                &s.caller,
                                s.depth + 1,
                            ))
                            .await
                        };
                        match inner {
                            Ok(v) => {
                                self.proceed_results.insert(token, v.clone());
                                self.answer_inc(&inc, Ok(v)).await;
                            }
                            Err(e) => {
                                self.answer_inc(&inc, Err(e.rpc(&atom_protocol::new_trace_id())))
                                    .await;
                            }
                        }
                    }
                }
            }
            m => {
                if inc.id.is_some() {
                    self.answer_inc(
                        &inc,
                        Err(RpcError::new(
                            atom_protocol::ErrorCode::METHOD_NOT_FOUND,
                            format!("método {m} indisponível na v0.2"),
                        )),
                    )
                    .await;
                }
            }
        }
    }

    /// `atom.call` vindo de extensão (pode aninhar; pode pedir stream).
    /// Retorna `None` quando a resposta imediata já foi enviada (stream).
    async fn nested_call(
        &mut self,
        inc: &Incoming,
        depth: u8,
    ) -> Result<Option<serde_json::Value>, RpcError> {
        dbgtrace(&format!("nested_call depth={depth} from={}", inc.ext));
        if depth >= MAX_DEPTH {
            return Err(RpcError::new(
                atom_protocol::ErrorCode::INVALID_REQUEST,
                "profundidade máxima de chamadas",
            ));
        }
        let cp: AtomCallParams = serde_json::from_value(inc.params.clone())
            .map_err(|e| RpcError::new(atom_protocol::ErrorCode::INVALID_PARAMS, e.to_string()))?;
        dbgtrace(&format!(
            "nested {} {} target={:?} stream={}",
            cp.service, cp.method, cp.target, cp.stream
        ));
        let trace = atom_protocol::new_trace_id();
        let map_err = |e: CoreError| e.rpc(&trace);
        let mut ctx = cp
            .ctx
            .clone()
            .unwrap_or_else(|| CallerCtx::local(&self.profile_name, &inc.ext));
        ctx.authorize(&self.profile_name, &inc.ext);
        // `[P-DL-1/2]`: `deadline` do chamador propaga; teto/padrão do perfil.
        // Já estourado = `-32022` sem invocar nada.
        // spec: [P-DL-1]
        // spec: [P-DL-2]
        // spec: [P-DL-3]
        if self.clamp_deadline(&mut ctx) {
            let e = deadline_err(&trace);
            return Err(e.rpc(&trace));
        }
        // v0.6: chamador precisa de `service:<contrato>` (até p/ stream).
        // `atom.*` são métodos do próprio core: sem grant (igual invoke_inner).
        if !cp.service.starts_with("atom.")
            && let Err(e) = self.check_cap(&inc.ext, &format!("service:{}", cp.service))
        {
            self.journal
                .call(&trace, &inc.ext, &cp.service, &cp.method, false, e.code);
            return Err(e);
        }
        // `before` pode reescrever params do stream; `after`/`around` não
        // tocam em chunks — `shortcut` documentado (só setup interceptado).
        let mut stream_params = cp.params.clone();
        for (ext, _) in self
            .registry
            .intercepts_for(&cp.service, &cp.method, "before")
            .into_iter()
            .filter(|(e, _)| e != &inc.ext)
        {
            let payload = serde_json::json!({
                "phase": "before",
                "service": cp.service,
                "method": cp.method,
                "params": stream_params,
                "$ctx": serde_json::to_value(&ctx).unwrap_or(serde_json::Value::Null),
            });
            match Box::pin(self.invoke_interceptor_pumped(&ext, payload, &ctx, depth))
                .await
                .map_err(map_err)?
            {
                InterceptorAction::Continue => {}
                InterceptorAction::ModifyParams(p) => stream_params = p,
                InterceptorAction::Respond(r) => {
                    self.answer_inc(
                        inc,
                        Ok(serde_json::json!({"stream": "rejected", "result": r})),
                    )
                    .await;
                    return Ok(None);
                }
                InterceptorAction::Reject(e) => return Err(e),
                InterceptorAction::ModifyResult(_) => {}
            }
        }
        let (provider, ctx, _) = self
            .prepare(
                &cp.service,
                cp.target.as_deref(),
                Some(ctx.clone()),
                &inc.ext,
            )
            .map_err(map_err)?;
        self.ensure_provider(&cp.service, &provider)
            .await
            .map_err(map_err)?;
        if cp.stream {
            let id = atom_protocol::new_stream_id();
            let cancel = Arc::new(Notify::new());
            // Empilha para `$/cancelRequest` (`[P-CANCEL-1/2]`).
            self.call_stack.push(CallFrame {
                caller_ext: inc.ext.clone(),
                caller_id: inc.id.clone(),
                provider: None,
                provider_req: None,
                cancel: cancel.clone(),
                cancelled: false,
            });
            self.streams.insert(
                id.clone(),
                StreamRec {
                    producer: provider.clone(),
                    consumer: StreamConsumer::Ext(inc.ext.clone()),
                    provider_req: String::new(),
                    window: Self::flow_window(&cp.flow).map_err(map_err)?,
                    acked: 0,
                    next_seq: 0,
                    pending: VecDeque::new(),
                    pending_cap: self.config.bus.stream_buf.max(1),
                    credit_producer: self.producer_wants_credit(&provider),
                    credit_until: u64::MAX,
                },
            );
            let (rx, req_id) = self
                .host
                .post_invoke(
                    &provider,
                    &cp.service,
                    &cp.method,
                    stream_params,
                    Some(&id),
                    &ctx,
                )
                .await
                .map_err(map_err)?;
            if let Some(rec) = self.streams.get_mut(&id) {
                rec.provider_req = req_id.clone();
                if rec.credit_producer && rec.window.is_some() {
                    rec.credit_until = rec.window.unwrap_or(0) as u64;
                }
            }
            if self
                .streams
                .get(&id)
                .is_some_and(|r| r.credit_producer && r.window.is_some())
            {
                let until = self.streams.get(&id).and_then(|r| r.window).unwrap_or(0) as u64;
                self.send_credit(&id, until).await;
            }
            if let Some(top) = self.call_stack.last_mut() {
                top.provider = Some(provider.clone());
                top.provider_req = Some(req_id);
            }
            // Resposta imediata ANTES de dirigir: o chamador precisa do id
            // para casar chunks/end (se chegasse só no fim, ele os ignoraria).
            self.answer_inc(inc, Ok(serde_json::json!({"stream": id})))
                .await;
            // `Box::pin` quebra a recursão async (drive → route → nested → drive).
            let r = Box::pin(self.drive(
                rx,
                &DriveDone::StreamDiscard {
                    id: id.clone(),
                    cancel,
                },
                &provider,
                depth + 1,
                ctx.deadline,
            ))
            .await;
            self.call_stack.pop();
            let (ok, code) = match &r {
                Ok(_) => (true, 0),
                Err(e) => (false, e.code()),
            };
            self.journal
                .call(&trace, &inc.ext, &cp.service, &cp.method, ok, code);
            let _ = r;
            self.streams.remove(&id);
            dbgtrace(&format!("nested stream {id} done"));
            Ok(None)
        } else {
            // Não-stream: cadeia completa before→around→provider→after.
            // Empilha para `$/cancelRequest` (`[P-CANCEL-1/2]`).
            let cancel = Arc::new(Notify::new());
            self.call_stack.push(CallFrame {
                caller_ext: inc.ext.clone(),
                caller_id: inc.id.clone(),
                provider: None,
                provider_req: None,
                cancel,
                cancelled: false,
            });
            let r = Box::pin(self.invoke_with_interceptors(
                &cp.service,
                &cp.method,
                cp.params,
                cp.target.as_deref(),
                &ctx,
                &trace,
                &inc.ext,
                depth + 1,
            ))
            .await
            .map_err(map_err)
            .map(Some);
            self.call_stack.pop();
            r
        }
    }

    /// Request genérico com pump do bus (para `atom run`: o comando pode
    /// fazer `atom.call` aninhados enquanto o CLI faz proxy do stdio).
    pub async fn request_pumped(
        &mut self,
        ext: &str,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, CoreError> {
        let (mut rx, _) = self.host.post(ext, method, params).await?;
        loop {
            tokio::select! {
                biased;
                r = &mut rx => {
                    let resp = r
                        .map_err(|_| CoreError::ExtensionUnavailable(ext.to_string()))?;
                    if let Some(err) = resp.error {
                        return Err(CoreError::Protocol(err));
                    }
                    return Ok(resp.result.unwrap_or(serde_json::Value::Null));
                }
                inc = self.host.next_incoming() => {
                    if let Some(i) = inc {
                        let done = DriveDone::Return {
                            cancel: Arc::new(Notify::new()),
                        };
                        self.route_incoming(i, &done, 0).await;
                    }
                }
            }
        }
    }

    /// Bombeia UMA mensagem extensão→core (para loops externos como `run`).
    pub async fn pump_one(&mut self, inc: Incoming) {
        let done = DriveDone::Return {
            cancel: Arc::new(Notify::new()),
        };
        self.route_incoming(inc, &done, 0).await;
    }
    /// Retorna `(pacote, interativo)`.
    pub fn find_command(&self, name: &str) -> Option<(String, bool)> {
        for (pkg, (_, m)) in &self.manifests {
            if let Some(toml::Value::Table(cmds)) = m.contributes.get("commands")
                && let Some(def) = cmds.get(name)
            {
                let interactive = def
                    .get("interactive")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                return Some((pkg.clone(), interactive));
            }
        }
        None
    }

    pub fn kv(&self, extension: &str) -> Kv {
        Kv::new(&self.paths.kv_dir, extension)
    }

    /// `(nome, versão)` dos pacotes instalados (para `atom.status`).
    pub fn manifests_info(&self) -> Vec<(String, String)> {
        self.manifests
            .iter()
            .map(|(n, (lp, _))| (n.clone(), lp.version.clone()))
            .collect()
    }

    /// Define endpoint + token do WS (`serve --ws`, v0.8).
    pub fn set_ws_info(&mut self, url: String, token: String) {
        self.ws_info = Some((url, token));
    }

    /// URL pública do WS (`None` = WS inativo). Token nunca sai por aqui.
    pub fn ws_url(&self) -> Option<String> {
        self.ws_info.as_ref().map(|(u, _)| u.clone())
    }

    /// URL base do shell ui-web (`atom.ui.serve`, v0.8). `None` = inativo.
    pub fn ui_url(&self) -> Option<String> {
        self.ui_url.clone()
    }

    /// Token do WS (`None` = WS inativo). Só quem já fala com o bus obtém.
    pub(crate) fn ws_token(&self) -> Option<String> {
        self.ws_info.as_ref().map(|(_, t)| t.clone())
    }

    /// Manifestos instalados (pacote -> (lock, manifesto)).
    pub fn installed(
        &self,
    ) -> &HashMap<String, (atom_protocol::LockPackage, atom_protocol::Manifest)> {
        &self.manifests
    }

    /// Bindings ativos do perfil.
    pub fn bindings(&self) -> &std::collections::HashMap<String, String> {
        self.registry.bindings()
    }
    /// Desliga o core gravando o ring de eventos p/ handover (`[P-EV-2]`).
    // spec: [P-EV-2]
    pub async fn shutdown(&mut self) {
        self.store_event_handover();
        self.host.stop_all().await;
    }

    pub fn event_ring_len_for_test(&self) -> usize {
        self.event_ring.len()
    }

    pub fn event_cursors_for_test(&self) -> (String, Vec<u64>) {
        (
            self.event_epoch.clone(),
            self.event_ring.iter().map(|e| e.offset).collect(),
        )
    }

    pub fn event_gap_sent_for_test(&self, _sub: &str) -> bool {
        self.test_gap
    }

    pub fn event_replayed_for_test(&self) -> usize {
        self.test_replayed
    }

    pub fn clear_test_flags(&mut self) {
        self.test_replayed = 0;
        self.test_gap = false;
    }
}
