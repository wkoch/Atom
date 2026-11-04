#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use atom_protocol::{
    CallerCtx, InitializeParams, InitializeResult, RpcError, RpcRequest, RpcResponse,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::error::CoreError;
use crate::transport;

/// Estado de extensão supervisionada (spec protocol §4.1, parcial v0.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtState {
    Starting,
    Running,
    Stopped,
    Failed(String),
}

struct Pending {
    map: HashMap<String, oneshot::Sender<RpcResponse>>,
}

/// Mensagem de uma extensão ao core que exige roteamento do bus
/// (`atom.call`, `atom.stream.*`, `atom.registry.*`, ...). Métodos locais
/// (`atom.storage.*`, `atom.config.get`, `atom.log`, `atom.secrets.*`)
/// são respondidos na task de leitura, sem passar por aqui.
pub struct Incoming {
    pub ext: String,
    pub id: Option<serde_json::Value>,
    pub method: String,
    pub params: serde_json::Value,
}

pub struct RunningExt {
    pub name: String,
    pub state: ExtState,
    pub init: InitializeResult,
    writer: mpsc::Sender<String>,
    pending: Arc<Mutex<Pending>>,
    next_id: u64,
    child: Child,
    _reader_task: tokio::task::JoinHandle<()>,
    pub pkg_dir: PathBuf,
    pub entrypoint: PathBuf,
}

fn req_id(n: u64) -> serde_json::Value {
    serde_json::json!(format!("core-{n}"))
}

fn key_of(id: &serde_json::Value) -> String {
    match id {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

impl RunningExt {
    /// Envia request sem aguardar a resposta (o chamador dirige o pump).
    async fn post_request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<oneshot::Receiver<RpcResponse>, CoreError> {
        let id = req_id(self.next_id);
        self.next_id += 1;
        let req = RpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(id.clone()),
            method: method.to_string(),
            params: Some(params),
        };
        let line = serde_json::to_string(&req).map_err(|e| CoreError::Internal(e.to_string()))?;
        let (tx, rx) = oneshot::channel();
        {
            let mut p = self.pending.lock().await;
            p.map.insert(key_of(&id), tx);
        }
        self.writer
            .send(line)
            .await
            .map_err(|_| CoreError::ExtensionUnavailable(self.name.clone()))?;
        Ok(rx)
    }

    async fn send_request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, CoreError> {
        let rx = self.post_request(method, params).await?;
        let resp = rx
            .await
            .map_err(|_| CoreError::ExtensionUnavailable(self.name.clone()))?;
        if let Some(err) = resp.error {
            return Err(CoreError::Protocol(err));
        }
        Ok(resp.result.unwrap_or(serde_json::Value::Null))
    }

    async fn send_notification(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<(), CoreError> {
        let req = RpcRequest {
            jsonrpc: "2.0".to_string(),
            id: None,
            method: method.to_string(),
            params: Some(params),
        };
        let line = serde_json::to_string(&req).map_err(|e| CoreError::Internal(e.to_string()))?;
        self.writer
            .send(line)
            .await
            .map_err(|_| CoreError::ExtensionUnavailable(self.name.clone()))
    }

    /// Desligamento gracioso: `atom.shutdown` (5s) + `atom.exit`.
    pub async fn shutdown(&mut self) {
        let params = serde_json::json!({});
        let fut = self.send_request("atom.shutdown", params);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), fut).await;
        let _ = self
            .send_notification("atom.exit", serde_json::json!({}))
            .await;
    }
}

/// Host: inicia extensões como processos com env filtrado (spec protocol §2, §4).
pub struct Host {
    running: HashMap<String, RunningExt>,
    kv_dir: PathBuf,
    secrets: crate::secrets::Secrets,
    incoming_tx: mpsc::UnboundedSender<Incoming>,
    incoming_rx: mpsc::UnboundedReceiver<Incoming>,
    /// Espelho vivo pacote -> caps concedidas (grants; v0.6).
    grants: Arc<tokio::sync::RwLock<HashMap<String, Vec<String>>>>,
    /// Mortes recentes p/ backoff (spec protocol §4.1): pacote -> instantes.
    restarts: HashMap<String, Vec<std::time::Instant>>,
}

impl Host {
    pub fn new(
        kv_dir: PathBuf,
        secrets: crate::secrets::Secrets,
        grants: Arc<tokio::sync::RwLock<HashMap<String, Vec<String>>>>,
    ) -> Self {
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        Self {
            running: HashMap::new(),
            kv_dir,
            secrets,
            incoming_tx,
            incoming_rx,
            grants,
            restarts: HashMap::new(),
        }
    }

    /// Sender p/ timers do supervisor (ex.: backoff de restart).
    pub fn sender(&self) -> mpsc::UnboundedSender<Incoming> {
        self.incoming_tx.clone()
    }

    /// Tem entrada (qualquer estado)? Distingue morte inesperada de `stop`.
    pub fn has_entry(&self, name: &str) -> bool {
        self.running.contains_key(name)
    }

    /// Log de mortes recentes p/ backoff (spec protocol §4.1).
    pub fn restart_log_mut(&mut self, name: &str) -> &mut Vec<std::time::Instant> {
        self.restarts.entry(name.to_string()).or_default()
    }

    /// Caps concedidas (espelho vivo) p/ enforcement no fast path.
    pub fn granted(&self, ext: &str) -> Vec<String> {
        self.grants
            .try_read()
            .map(|g| g.get(ext).cloned().unwrap_or_default())
            .unwrap_or_default()
    }

    /// Extensões cujo processo morreu (liveness via `try_wait`).
    pub fn poll_exits(&mut self) -> Vec<String> {
        let mut dead = vec![];
        for (name, ext) in self.running.iter_mut() {
            let exited = ext.child.try_wait().map(|s| s.is_some()).unwrap_or(false);
            if exited {
                dead.push(name.clone());
            }
        }
        dead
    }

    pub fn is_running(&self, name: &str) -> bool {
        matches!(
            self.running.get(name).map(|e| &e.state),
            Some(ExtState::Running)
        )
    }

    pub fn states(&self) -> HashMap<String, (ExtState, InitializeResult)> {
        self.running
            .iter()
            .map(|(k, v)| (k.clone(), (v.state.clone(), v.init.clone())))
            .collect()
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut RunningExt> {
        self.running.get_mut(name)
    }

    /// Envia `service.invoke` sem aguardar (o chamador dirige o pump via `drive`).
    pub async fn post_invoke(
        &mut self,
        provider: &str,
        service: &str,
        method: &str,
        params: serde_json::Value,
        stream: Option<&str>,
        ctx: &CallerCtx,
    ) -> Result<oneshot::Receiver<RpcResponse>, CoreError> {
        let mut full = serde_json::json!({
            "service": service,
            "method": method,
            "params": params,
            "stream": stream.map(|s| serde_json::Value::String(s.to_string())).unwrap_or(serde_json::Value::Null),
        });
        if let Some(obj) = full.as_object_mut() {
            obj.insert(
                "$ctx".to_string(),
                serde_json::to_value(ctx).map_err(|e| CoreError::Internal(e.to_string()))?,
            );
        }
        let ext = self
            .running
            .get_mut(provider)
            .ok_or_else(|| CoreError::ExtensionUnavailable(provider.to_string()))?;
        ext.post_request("service.invoke", full).await
    }

    /// Request genérico a uma extensão (ex.: `command.run`), com resposta.
    pub async fn request(
        &mut self,
        ext: &str,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, CoreError> {
        let e = self
            .running
            .get_mut(ext)
            .ok_or_else(|| CoreError::ExtensionUnavailable(ext.to_string()))?;
        e.send_request(method, params).await
    }

    /// Request genérico sem aguardar (quem chama dirige o pump).
    pub async fn post(
        &mut self,
        ext: &str,
        method: &str,
        params: serde_json::Value,
    ) -> Result<oneshot::Receiver<RpcResponse>, CoreError> {
        let e = self
            .running
            .get_mut(ext)
            .ok_or_else(|| CoreError::ExtensionUnavailable(ext.to_string()))?;
        e.post_request(method, params).await
    }

    /// Responde um `Incoming` com request id.
    pub async fn answer(&mut self, ext: &str, id: serde_json::Value, body: RpcResponse) {
        if let Some(e) = self.running.get_mut(ext) {
            let mut body = body;
            body.id = id;
            if let Ok(line) =
                transport::to_line(&serde_json::to_value(&body).unwrap_or(serde_json::Value::Null))
            {
                let _ = e.writer.send(line).await;
            }
        }
    }

    /// Notificação a uma extensão (chunks, cancel, comandos).
    pub async fn notify(&mut self, ext: &str, method: &str, params: serde_json::Value) {
        if let Some(e) = self.running.get_mut(ext) {
            let _ = e.send_notification(method, params).await;
        }
    }

    /// Writer bruto de uma extensão (forwarder de streams fora do bus).
    pub fn ext_writer(&self, ext: &str) -> Option<mpsc::Sender<String>> {
        self.running.get(ext).map(|e| e.writer.clone())
    }

    /// Próxima mensagem de extensão ao core que exige roteamento.
    pub async fn next_incoming(&mut self) -> Option<Incoming> {
        self.incoming_rx.recv().await
    }

    /// Versão sem bloqueio (drena chunks que chegaram junto com a resposta).
    pub fn try_incoming(&mut self) -> Option<Incoming> {
        self.incoming_rx.try_recv().ok()
    }

    /// Inicia (ou retorna a já ativa) extensão. `program` pode ser o
    /// entrypoint direto ou um comando de runtime (`runtime=`); `args`
    /// extras vão ao processo; `cwd` = raiz do pacote da extensão.
    /// `granted` alimenta `atom.initialize` + filtro `env:` (v0.6).
    #[allow(clippy::too_many_arguments)]
    pub async fn ensure_started(
        &mut self,
        name: &str,
        pkg_dir: &Path,
        program: &Path,
        args: &[String],
        version: &str,
        config: serde_json::Value,
        granted: &[String],
    ) -> Result<(), CoreError> {
        if self.is_running(name) {
            return Ok(());
        }
        self.running.remove(name);
        // `env:<NOME>` concedido: repassa a variável do ambiente (spec §2).
        let mut env_extra = vec![];
        for g in granted {
            if let Some(var) = g.strip_prefix("env:")
                && let Ok(v) = std::env::var(var)
            {
                env_extra.push((var.to_string(), v));
            }
        }
        let ext = spawn_extension(
            name,
            pkg_dir,
            program,
            args,
            version,
            config,
            granted,
            &env_extra,
            &HostCtx {
                kv_dir: self.kv_dir.clone(),
                secrets: self.secrets.clone(),
                incoming: self.incoming_tx.clone(),
                grants: self.grants.clone(),
            },
        )
        .await?;
        self.running.insert(name.to_string(), ext);
        Ok(())
    }

    pub async fn stop(&mut self, name: &str) {
        if let Some(mut e) = self.running.remove(name) {
            e.shutdown().await;
        }
    }

    pub async fn stop_all(&mut self) {
        let names: Vec<String> = self.running.keys().cloned().collect();
        for n in names {
            self.stop(&n).await;
        }
    }
}

/// Contexto do host repassado ao spawn (evita estourar nº de argumentos).
#[derive(Clone)]
struct HostCtx {
    kv_dir: PathBuf,
    secrets: crate::secrets::Secrets,
    incoming: mpsc::UnboundedSender<Incoming>,
    grants: Arc<tokio::sync::RwLock<HashMap<String, Vec<String>>>>,
}

impl Default for Host {
    fn default() -> Self {
        let dir = std::env::temp_dir().join("atom-host-default");
        let secrets = crate::secrets::Secrets::file_only("default", &dir);
        let grants = Arc::new(tokio::sync::RwLock::new(HashMap::new()));
        Self::new(dir, secrets, grants)
    }
}

#[allow(clippy::too_many_arguments)]
async fn spawn_extension(
    name: &str,
    pkg_dir: &Path,
    program: &Path,
    args: &[String],
    version: &str,
    config: serde_json::Value,
    granted: &[String],
    env_extra: &[(String, String)],
    ctx: &HostCtx,
) -> Result<RunningExt, CoreError> {
    let kv_dir = &ctx.kv_dir;
    let secrets = ctx.secrets.clone();
    let incoming = ctx.incoming.clone();
    let mut cmd = Command::new(program);
    cmd.args(args);
    cmd.current_dir(pkg_dir);
    // Env filtrado (spec protocol §2) + `env:<NOME>` concedidos (v0.6).
    cmd.env_clear();
    for (k, v) in filtered_env() {
        cmd.env(k, v);
    }
    for (k, v) in env_extra {
        cmd.env(k, v);
    }
    cmd.env("ATOM_PROTOCOL", "atom/1");
    cmd.env("ATOM_EXTENSION", format!("{name}@{version}"));
    cmd.env("ATOM_PKG_DIR", pkg_dir);
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(md) = std::fs::metadata(program) {
            let mut perms = md.permissions();
            let mode = perms.mode();
            if mode & 0o111 == 0 {
                perms.set_mode(mode | 0o755);
                let _ = std::fs::set_permissions(program, perms);
            }
        }
    }

    let mut child: Child = cmd
        .spawn()
        .map_err(|e| CoreError::ExtensionUnavailable(format!("spawn {name}: {e}")))?;
    let stdin: ChildStdin = child
        .stdin
        .take()
        .ok_or_else(|| CoreError::ExtensionUnavailable(name.to_string()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| CoreError::ExtensionUnavailable(name.to_string()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| CoreError::ExtensionUnavailable(name.to_string()))?;

    // stderr → log.
    let log_name = name.to_string();
    tokio::spawn(async move {
        let mut r = BufReader::new(stderr);
        let mut line = String::new();
        loop {
            line.clear();
            match r.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => tracing::info!(ext = %log_name, "{}", line.trim_end()),
                Err(_) => break,
            }
        }
    });

    let (wtx, mut wrx) = mpsc::channel::<String>(64);
    tokio::spawn(async move {
        let mut stdin = stdin;
        while let Some(line) = wrx.recv().await {
            if transport::write_message(&mut stdin, &line).await.is_err() {
                break;
            }
        }
        let _ = stdin.shutdown().await;
    });

    let pending = Arc::new(Mutex::new(Pending {
        map: HashMap::new(),
    }));
    let mut reader = BufReader::new(stdout);
    let pr = pending.clone();
    let wtx2 = wtx.clone();
    let local_ext = name.to_string();
    let local_kv = kv_dir.to_path_buf();
    let local_cfg = config.clone();
    let local_secrets = secrets.clone();
    let local_grants = ctx.grants.clone();
    let incoming_tx = incoming.clone();
    let exit_tx = incoming.clone();
    let exit_ext = name.to_string();
    let reader_task = tokio::spawn(async move {
        loop {
            match transport::read_message(&mut reader).await {
                Ok(Some(line)) if !line.is_empty() => {
                    let v: Result<serde_json::Value, _> = serde_json::from_str(&line);
                    let Ok(v) = v else { continue };
                    // Resposta a um request do core?
                    if let Some(id) = v.get("id") {
                        let tx = {
                            let mut p = pr.lock().await;
                            p.map.remove(&key_of(id))
                        };
                        if let Some(tx) = tx {
                            if let Ok(resp) = serde_json::from_value::<RpcResponse>(v) {
                                let _ = tx.send(resp);
                            }
                            continue;
                        }
                    }
                    // Chamada/notificação da extensão: local (storage/config/log/
                    // secrets) ou roteada pelo bus (atom.call, atom.stream.*, ...).
                    if let Some(method) = v.get("method").and_then(|m| m.as_str()) {
                        let id = v.get("id").cloned();
                        let params = v.get("params").cloned().unwrap_or(serde_json::Value::Null);
                        // v0.6: secrets atrás de grants, no fast path do reader.
                        if (method == "atom.secrets.get" || method == "atom.secrets.set")
                            && id.is_some()
                        {
                            let name_param =
                                params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                            let need = if method == "atom.secrets.get" {
                                format!("secrets:{name_param}")
                            } else {
                                format!("secrets.write:{name_param}")
                            };
                            let allowed = local_grants.read().await.get(&local_ext).is_some_and(
                                |caps: &Vec<String>| {
                                    caps.iter().any(|c| crate::profile::cap_allows(c, &need))
                                },
                            );
                            if !allowed {
                                if let Some(rid) = id {
                                    let resp = RpcResponse::err(
                                        rid,
                                        RpcError::new(
                                            atom_protocol::ErrorCode::CAPABILITY_DENIED,
                                            format!("{local_ext} sem capability {need}"),
                                        ),
                                    );
                                    if let Ok(line) = transport::to_line(
                                        &serde_json::to_value(&resp)
                                            .unwrap_or(serde_json::Value::Null),
                                    ) {
                                        let _ = wtx2.send(line).await;
                                    }
                                }
                                continue;
                            }
                        }
                        match answer_local(
                            method,
                            params.clone(),
                            &local_ext,
                            &local_kv,
                            &local_cfg,
                            &local_secrets,
                        ) {
                            Some(out) => {
                                if let Some(id) = id {
                                    let resp = match out {
                                        Ok(result) => RpcResponse::ok(id, result),
                                        Err(e) => RpcResponse::err(id, e),
                                    };
                                    if let Ok(line) = transport::to_line(
                                        &serde_json::to_value(&resp)
                                            .unwrap_or(serde_json::Value::Null),
                                    ) {
                                        let _ = wtx2.send(line).await;
                                    }
                                }
                            }
                            None => {
                                let _ = incoming_tx.send(Incoming {
                                    ext: local_ext.clone(),
                                    id,
                                    method: method.to_string(),
                                    params,
                                });
                            }
                        }
                    }
                }
                Ok(_) => break,
                Err(_) => break,
            }
        }
        // Processo morreu ou stdout fechou: avisa o bus p/ supervisão (v0.6).
        let _ = exit_tx.send(Incoming {
            ext: exit_ext,
            id: None,
            method: "__ext_exit".to_string(),
            params: serde_json::Value::Null,
        });
    });

    let mut ext = RunningExt {
        name: name.to_string(),
        state: ExtState::Starting,
        init: InitializeResult::default(),
        writer: wtx,
        pending,
        next_id: 1,
        child,
        _reader_task: reader_task,
        pkg_dir: pkg_dir.to_path_buf(),
        entrypoint: program.to_path_buf(),
    };

    // Handshake `atom.initialize` (timeout 10s).
    let params = InitializeParams {
        protocol: "atom/1".to_string(),
        core: atom_protocol::CoreInfo {
            version: crate::CORE_VERSION.to_string(),
        },
        extension: atom_protocol::ExtensionInfo {
            name: name.to_string(),
            version: version.to_string(),
        },
        config,
        grants: granted.to_vec(),
        ctx: CallerCtx {
            tenant: "local".to_string(),
            user: "local".to_string(),
            profile: String::new(),
            session: None,
            trace: Some(atom_protocol::new_trace_id()),
            caller: Some("core".to_string()),
        },
    };
    let p = serde_json::to_value(&params).map_err(|e| CoreError::Internal(e.to_string()))?;
    let fut = ext.send_request("atom.initialize", p);
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), fut)
        .await
        .map_err(|_| {
            CoreError::ExtensionUnavailable(format!("{name}: timeout em atom.initialize"))
        })??;
    let init: InitializeResult = serde_json::from_value(result).map_err(|e| {
        CoreError::Protocol(atom_protocol::RpcError::new(
            atom_protocol::ErrorCode::PROTOCOL_MISMATCH,
            format!("initialize inválido: {e}"),
        ))
    })?;
    if init.protocol != "atom/1" {
        return Err(CoreError::Protocol(atom_protocol::RpcError::new(
            atom_protocol::ErrorCode::PROTOCOL_MISMATCH,
            format!("protocolo {:?}, esperado atom/1", init.protocol),
        )));
    }
    ext.init = init;
    ext.state = ExtState::Running;
    ext.send_notification("atom.initialized", serde_json::json!({}))
        .await?;
    Ok(ext)
}

/// Responde chamadas da extensão ao core que não precisam do bus
/// (spec protocol, tabela de métodos do núcleo, parcial v0.1).
/// Retorna `None` quando o método exige roteamento (`atom.call`,
/// `atom.stream.*`, `atom.registry.*`, ...) — a leitura encaminha ao bus.
fn answer_local(
    method: &str,
    params: serde_json::Value,
    ext: &str,
    kv_dir: &Path,
    config: &serde_json::Value,
    secrets: &crate::secrets::Secrets,
) -> Option<Result<serde_json::Value, atom_protocol::RpcError>> {
    match method {
        "atom.log"
        | "atom.config.get"
        | "atom.secrets.get"
        | "atom.secrets.set"
        | "atom.storage.get"
        | "atom.storage.set"
        | "atom.storage.delete"
        | "atom.storage.list" => Some(answer_local_inner(
            method, params, ext, kv_dir, config, secrets,
        )),
        _ => None,
    }
}

fn answer_local_inner(
    method: &str,
    params: serde_json::Value,
    ext: &str,
    kv_dir: &Path,
    config: &serde_json::Value,
    secrets: &crate::secrets::Secrets,
) -> Result<serde_json::Value, atom_protocol::RpcError> {
    use atom_protocol::{ErrorCode, RpcError};
    let bad = |msg: &str| RpcError::new(ErrorCode::INVALID_PARAMS, msg);
    match method {
        "atom.log" => {
            let msg = params.get("msg").and_then(|m| m.as_str()).unwrap_or("");
            tracing::info!(ext = %ext, "{msg}");
            Ok(serde_json::Value::Null)
        }
        "atom.config.get" => Ok(config.clone()),
        "atom.secrets.get" => {
            let n = params
                .get("name")
                .and_then(|n| n.as_str())
                .ok_or_else(|| bad("name"))?;
            secrets.rpc_get(n)
        }
        "atom.secrets.set" => {
            let n = params
                .get("name")
                .and_then(|n| n.as_str())
                .ok_or_else(|| bad("name"))?;
            let val = params
                .get("value")
                .and_then(|v| v.as_str())
                .ok_or_else(|| bad("value"))?;
            secrets.rpc_set(n, val)
        }
        "atom.storage.get" => {
            let k = params
                .get("key")
                .and_then(|k| k.as_str())
                .ok_or_else(|| bad("key"))?;
            let kv = crate::storage::Kv::new(kv_dir, ext);
            let v = kv
                .get(k)
                .map_err(|e| RpcError::new(ErrorCode::STORAGE_ERROR, e.to_string()))?;
            Ok(v.unwrap_or(serde_json::Value::Null))
        }
        "atom.storage.set" => {
            let k = params
                .get("key")
                .and_then(|k| k.as_str())
                .ok_or_else(|| bad("key"))?;
            let val = params.get("value").cloned().ok_or_else(|| bad("value"))?;
            let kv = crate::storage::Kv::new(kv_dir, ext);
            kv.set(k, val)
                .map_err(|e| RpcError::new(ErrorCode::STORAGE_ERROR, e.to_string()))?;
            Ok(serde_json::json!({"stored": true}))
        }
        "atom.storage.delete" => {
            let k = params
                .get("key")
                .and_then(|k| k.as_str())
                .ok_or_else(|| bad("key"))?;
            let kv = crate::storage::Kv::new(kv_dir, ext);
            let had = kv
                .delete(k)
                .map_err(|e| RpcError::new(ErrorCode::STORAGE_ERROR, e.to_string()))?;
            Ok(serde_json::json!({"deleted": had}))
        }
        "atom.storage.list" => {
            let prefix = params.get("prefix").and_then(|p| p.as_str()).unwrap_or("");
            let kv = crate::storage::Kv::new(kv_dir, ext);
            let keys = kv
                .list(prefix)
                .map_err(|e| RpcError::new(ErrorCode::STORAGE_ERROR, e.to_string()))?;
            Ok(serde_json::json!({"keys": keys}))
        }
        m => Err(RpcError::new(
            ErrorCode::METHOD_NOT_FOUND,
            format!("método {m} fora do escopo local"),
        )),
    }
}

fn filtered_env() -> Vec<(String, String)> {
    const PASS: &[&str] = &["PATH", "HOME", "USERPROFILE", "TMPDIR", "TEMP", "LANG"];
    let mut out = vec![];
    for k in PASS {
        if let Ok(v) = std::env::var(k) {
            out.push((k.to_string(), v));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filtered_env_has_path_or_is_empty_safe() {
        let _ = filtered_env();
    }
}
