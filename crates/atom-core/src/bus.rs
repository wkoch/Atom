#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Notify;

use atom_protocol::{AtomCallParams, CallerCtx, RpcError, RpcResponse};
use tokio::sync::{mpsc, oneshot};

use crate::error::CoreError;
use crate::host::{Host, Incoming};
use crate::pkg;
use crate::profile::{self, AtomPaths, ProfileConfig, ProfilePaths};
use crate::registry::Registry;
use crate::storage::Kv;

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

/// Parâmetros de `Core::call_stream_chan` (agrupados; clippy 7 args).
#[derive(Debug, Clone)]
pub struct StreamCall {
    pub id: String,
    pub service: String,
    pub method: String,
    pub params: serde_json::Value,
    pub target: Option<String>,
}

/// Quem consome um stream ativo.
#[derive(Debug)]
enum StreamConsumer {
    /// Extensão (notificações no stdin dela).
    Ext(String),
    /// Canal fora do bus (socket do daemon, CLI `--stream`).
    Chan(mpsc::UnboundedSender<StreamEvent>),
}

struct StreamRec {
    producer: String,
    consumer: StreamConsumer,
}

type InvokeRx = oneshot::Receiver<RpcResponse>;

/// Como `drive` termina a espera pelo provedor.
enum DriveDone {
    /// Retorna o resultado final ao chamador.
    Return,
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
}

impl Core {
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
        let config = profile::load_or_create(&paths)?;
        let data_store = atom_paths.data.clone();
        let secrets = crate::secrets::Secrets::new(profile_name, &paths.data_dir);
        let kv_dir = paths.kv_dir.clone();
        let manifests = pkg::installed_manifests(&paths, &data_store).unwrap_or_default();
        let mut pkg_dirs = HashMap::new();
        let mut registry = Registry::new(config.bindings.clone());
        Self::fill_registry(&mut registry, &manifests, &mut pkg_dirs, &data_store);
        Ok(Self {
            profile_name: profile_name.to_string(),
            atom_paths,
            paths,
            config,
            data_store,
            registry,
            host: Host::new(kv_dir, secrets.clone()),
            secrets,
            streams: HashMap::new(),
            manifests,
            pkg_dirs,
        })
    }

    /// Recarrega lock/manifestos/bindings do disco.
    pub fn refresh(&mut self) -> Result<(), CoreError> {
        self.config = profile::load_or_create(&self.paths)?;
        let manifests = pkg::installed_manifests(&self.paths, &self.data_store).unwrap_or_default();
        self.pkg_dirs.clear();
        self.registry = Registry::new(self.config.bindings.clone());
        let data_store = self.data_store.clone();
        Self::fill_registry(&mut self.registry, &manifests, &mut self.pkg_dirs, &data_store);
        self.manifests = manifests;
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

    /// Sobe extensões `onStartup` (e handlers implícitos: sempre sobem no boot).
    /// Pacotes declarativos (sem entrypoint) ficam `dormant` (v0.4 dá handlers).
    pub async fn boot(&mut self) -> Result<Vec<String>, CoreError> {
        let names: Vec<String> = self.manifests.keys().cloned().collect();
        let mut started = vec![];
        for n in names {
            let executable = self
                .manifests
                .get(&n)
                .map(|(_, m)| m.entrypoint.is_some())
                .unwrap_or(false);
            if !executable {
                tracing::debug!(pacote = %n, "declarativo: dormant");
                continue;
            }
            let startup = self.wants_startup(&n);
            let handles = self
                .manifests
                .get(&n)
                .map(|(_, m)| !m.handles.is_empty())
                .unwrap_or(false);
            if startup || handles {
                self.start_ext(&n).await?;
                started.push(n);
            }
        }
        Ok(started)
    }

    /// Sobe um pacote instalado (para `atom run` / `onCommand`).
    pub async fn start_package(&mut self, name: &str) -> Result<(), CoreError> {
        self.start_ext(name).await
    }

    async fn start_ext(&mut self, name: &str) -> Result<(), CoreError> {
        let (dir, ep, version) = self.entrypoint_of(name)?;
        let cfg = self
            .config
            .config
            .get(name)
            .cloned()
            .unwrap_or(toml::Value::Table(toml::map::Map::new()));
        let cfg_json: serde_json::Value =
            serde_json::to_value(&cfg).map_err(|e| CoreError::Internal(e.to_string()))?;
        self.host
            .ensure_started(name, &dir, &ep, &version, cfg_json)
            .await
    }

    async fn ensure_provider(&mut self, service: &str, provider: &str) -> Result<(), CoreError> {
        if self.host.is_running(provider) {
            return Ok(());
        }
        if self.wants_startup(provider) || self.wants_service(provider, service) {
            return self.start_ext(provider).await;
        }
        // Sem ativação restritiva: sobe sob demanda (padrão v0.1).
        self.start_ext(provider).await
    }

    /// Resolve provedor + contexto autoritativo (tenant sempre `local`).
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
                let _ = tx.send(StreamEvent {
                    method: method.to_string(),
                    params,
                });
            }
            None => {}
        }
    }

    /// `atom.call`: contexto forjado é sobrescrito (tenant sempre `local`).
    pub async fn call(
        &mut self,
        service: &str,
        method: &str,
        params: serde_json::Value,
        target: Option<&str>,
        forged: Option<CallerCtx>,
    ) -> Result<serde_json::Value, CoreError> {
        let (provider, ctx, trace) = self.prepare(service, target, forged, "core")?;
        self.ensure_provider(service, &provider)
            .await
            .map_err(|e| match e {
                CoreError::Protocol(rpc) => CoreError::Protocol(rpc.with_trace(&trace)),
                other => other,
            })?;
        let rx = self
            .host
            .post_invoke(&provider, service, method, params, None, &ctx)
            .await
            .map_err(|e| match e {
                CoreError::Protocol(rpc) => CoreError::Protocol(rpc.with_trace(&trace)),
                other => other,
            })?;
        self.drive(rx, &DriveDone::Return, &provider, 0)
            .await
            .map_err(|e| match e {
                CoreError::Protocol(rpc) => CoreError::Protocol(rpc.with_trace(&trace)),
                other => other,
            })
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

    /// `atom.call` com `stream: true` e consumidor fora do bus.
    /// O chamador gera `id`, responde `{"stream": id}` e repassa chunks/end
    /// ao canal. Retorna o resultado final (o fim já foi notificado).
    pub async fn call_stream_chan(
        &mut self,
        sc: StreamCall,
        tx: mpsc::UnboundedSender<StreamEvent>,
        cancel: Arc<Notify>,
    ) -> Result<serde_json::Value, CoreError> {
        let StreamCall {
            id,
            service,
            method,
            params,
            target,
        } = sc;
        let (provider, ctx, trace) = self.prepare(&service, target.as_deref(), None, "core")?;
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
            },
        );
        let rx = self
            .host
            .post_invoke(
                &provider,
                &service,
                &method,
                params,
                Some(id.as_str()),
                &ctx,
            )
            .await
            .map_err(map_err)?;
        let done = DriveDone::StreamDiscard {
            id: id.to_string(),
            cancel,
        };
        let v = self.drive(rx, &done, &provider, 0).await.map_err(map_err)?;
        Ok(v)
    }

    /// Bomba do bus: espera o provedor servindo chamadas aninhadas de extensões.
    async fn drive(
        &mut self,
        mut rx: InvokeRx,
        done: &DriveDone,
        producer: &str,
        depth: u8,
    ) -> Result<serde_json::Value, CoreError> {
        // `rx_done`: resposta chegou antes do `end` (corrida entre o `oneshot`
        // e as notificações na fila). Nesse caso drena a fila e espera o `end`.
        let mut rx_done = false;
        loop {
            tokio::select! {
                biased;
                _ = async {
                    match done {
                        DriveDone::StreamDiscard { cancel, .. } => cancel.notified().await,
                        DriveDone::Return => std::future::pending().await,
                    }
                } => {
                    let id = match done {
                        DriveDone::StreamDiscard { id, .. } => id.clone(),
                        DriveDone::Return => unreachable!(),
                    };
                    let target = self
                        .streams
                        .get(&id)
                        .map(|r| r.producer.clone())
                        .unwrap_or_else(|| producer.to_string());
                    self.host
                        .notify(
                            &target,
                            "atom.stream.cancel",
                            serde_json::json!({"stream": id}),
                        )
                        .await;
                    self.streams.remove(&id);
                    return Err(CoreError::Protocol(atom_protocol::RpcError::new(
                        atom_protocol::ErrorCode::REQUEST_CANCELLED,
                        "stream cancelado",
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
                                DriveDone::Return => return Ok(v),
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
                if let Some(s) = inc.params.get("stream").and_then(|s| s.as_str()) {
                    let s = s.to_string();
                    let params = inc.params.clone();
                    self.deliver(&s, "atom.stream.chunk", params).await;
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
                    // Se o cancel mira o stream do `drive` atual, sinaliza em vez de
                    // só repassar: o loop trata (notifica produtor + erro -32800).
                    let mine = matches!(done, DriveDone::StreamDiscard { id, .. } if id == s);
                    if !mine {
                        let params = inc.params.clone();
                        if let Some(rec) = self.streams.get(s) {
                            let target = rec.producer.clone();
                            self.host
                                .notify(&target, "atom.stream.cancel", params)
                                .await;
                        }
                    } else if let DriveDone::StreamDiscard { cancel, .. } = done {
                        cancel.notify_one();
                    }
                }
                if inc.id.is_some() {
                    self.answer_inc(&inc, Ok(serde_json::Value::Null)).await;
                }
            }
            "atom.stream.ack" | "$/cancelRequest" => {
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
        let (provider, ctx, _) = self
            .prepare(&cp.service, cp.target.as_deref(), None, &inc.ext)
            .map_err(map_err)?;
        self.ensure_provider(&cp.service, &provider)
            .await
            .map_err(map_err)?;
        if cp.stream {
            let id = atom_protocol::new_stream_id();
            self.streams.insert(
                id.clone(),
                StreamRec {
                    producer: provider.clone(),
                    consumer: StreamConsumer::Ext(inc.ext.clone()),
                },
            );
            let rx = self
                .host
                .post_invoke(
                    &provider,
                    &cp.service,
                    &cp.method,
                    cp.params,
                    Some(&id),
                    &ctx,
                )
                .await
                .map_err(map_err)?;
            // Resposta imediata ANTES de dirigir: o chamador precisa do id
            // para casar chunks/end (se chegasse só no fim, ele os ignoraria).
            self.answer_inc(inc, Ok(serde_json::json!({"stream": id})))
                .await;
            // `Box::pin` quebra a recursão async (drive → route → nested → drive).
            let _ = Box::pin(self.drive(
                rx,
                &DriveDone::StreamDiscard {
                    id: id.clone(),
                    cancel: Arc::new(Notify::new()),
                },
                &provider,
                depth + 1,
            ))
            .await;
            self.streams.remove(&id);
            dbgtrace(&format!("nested stream {id} done"));
            Ok(None)
        } else {
            let rx = self
                .host
                .post_invoke(&provider, &cp.service, &cp.method, cp.params, None, &ctx)
                .await
                .map_err(map_err)?;
            // Chamada aninhada simples: completa com a resposta. Usa Return
            // (nunca o `done` externo): herdar StreamDiscard faria o drive
            // encerrar o stream externo na primeira resposta aninhada.
            Box::pin(self.drive(rx, &DriveDone::Return, &provider, depth + 1))
                .await
                .map_err(map_err)
                .map(Some)
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
        let mut rx = self.host.post(ext, method, params).await?;
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
                        let done = DriveDone::Return;
                        self.route_incoming(i, &done, 0).await;
                    }
                }
            }
        }
    }

    /// Bombeia UMA mensagem extensão→core (para loops externos como `run`).
    pub async fn pump_one(&mut self, inc: Incoming) {
        let done = DriveDone::Return;
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

    /// Bindings ativos do perfil.
    pub fn bindings(&self) -> &std::collections::HashMap<String, String> {
        self.registry.bindings()
    }

    pub async fn shutdown(&mut self) {
        self.host.stop_all().await;
    }
}
