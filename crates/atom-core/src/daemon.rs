#![forbid(unsafe_code)]
//! Daemon do perfil: `serve` (socket unix + WS opcional) + cliente mínimo do CLI.
//!
//! Transporte NDJSON sobre `<runtime>/atom-<perfil>.sock` (spec protocol §1,
//! permissão 0600). Métodos: `atom.call`, `atom.status`, `atom.stop`.
//! Socket local = trust-local. WS (`--ws`) exige token bearer (`serve.token`,
//! 0600): query `?token=` ou header `Authorization: Bearer`.
//! `shortcut: WS sem `atom.register` (providers via WS ficam p/ depois,
//! spec §10) e sem push de `event.deliver` (poll via chamadas).

use std::path::{Path, PathBuf};
#[cfg_attr(not(unix), allow(unused_imports))]
use std::sync::Arc;

use crate::bus::Core;
use crate::error::CoreError;
#[cfg(unix)]
use crate::transport;
#[cfg(unix)]
use futures_util::SinkExt;
#[cfg(unix)]
use tokio::io::BufReader;
#[cfg(unix)]
use tokio::net::TcpListener;
#[cfg(unix)]
use tokio::sync::{Mutex, mpsc};
#[cfg(unix)]
use tokio_tungstenite::tungstenite::Message;

/// Resposta de estado para `atom.status` / CLI.
#[cfg(unix)]
pub fn status_json(core: &Core) -> serde_json::Value {
    let running: std::collections::HashMap<String, String> = core
        .host
        .states()
        .iter()
        .map(|(n, (st, _))| (n.clone(), format!("{st:?}")))
        .collect();
    // Flags de adulteração externa (`[C-AMB-2/3]`): grants via espelho `.core`,
    // segurança via diff do `profile.toml` no disco contra o config aberto.
    // spec: [C-AMB-2]
    // spec: [C-AMB-3]
    let tampered = tamper_flags(core);
    serde_json::json!({
        "profile": core.profile_name,
        "packages": core
            .manifests_info()
            .iter()
            .map(|(n, v)| format!("{n}@{v}"))
            .collect::<Vec<_>>(),
        "bindings": core.bindings(),
        "running": running,
        "daemon": true,
        "ws": core.ws_url(),
        "ui": core.ui_url(),
        "tampered": tampered,
    })
}

/// Flags de adulteração externa p/ `atom.status` (`[C-AMB-2/3]`).
/// `grants`: `grants.toml` difere do `.core` (ignorado, vale o `.core`);
/// `security_fields`: campos de segurança de `profile.toml` no disco que
/// divergem do config aberto (só valem via `atom profile reload` confiável).
// spec: [C-AMB-2]
// spec: [C-AMB-3]
#[cfg(unix)]
pub fn tamper_flags(core: &Core) -> serde_json::Value {
    let grants = crate::profile::load_grants_checked(&core.paths).tampered;
    let security_fields = crate::profile::load_or_create(&core.paths)
        .map(|disk| crate::profile::security_fields_diff(&core.config, &disk))
        .unwrap_or_default();
    serde_json::json!({"grants": grants, "security_fields": security_fields})
}

#[cfg(unix)]
fn socket_perms(sock: &Path) -> Result<(), CoreError> {
    use std::os::unix::fs::PermissionsExt;
    let mut p = std::fs::metadata(sock)
        .map_err(|e| CoreError::Io(e.to_string()))?
        .permissions();
    p.set_mode(0o600);
    std::fs::set_permissions(sock, p).map_err(|e| CoreError::Io(e.to_string()))
}

/// Ancestralidade de processo p/ `[P-CONN-2]` (spec protocol §1, ADR-019): `true` se `peer`
/// é `daemon` ou descendente dele caminhando ppid via [`crate::platform::parent_of`]
/// (Linux: `/proc/<pid>/stat` com o daemon como `PR_SET_CHILD_SUBREAPER`;
/// macOS: `sysctl KERN_PROC_PID`; Windows: sem árvore — só o próprio pid).
/// `read_parent` é injetável p/ teste puro com pids sintéticos (`// spec: [P-CONN-2]`).
/// Sem info no meio da caminhada = fim da caminhada = confiável (só recusa
/// descendente CONFIRMADO).
/// spec: [P-CONN-2]
pub fn is_descendant_with(
    peer: u32,
    daemon: u32,
    mut read_parent: impl FnMut(u32) -> Option<u32>,
) -> bool {
    if peer == daemon {
        return true;
    }
    let mut cur = peer;
    // Corta passeios degenerados (pid 0/1 = raiz; limite = profundidade absurda).
    for _ in 0..1024 {
        if cur == daemon {
            return true;
        }
        if cur <= 1 {
            return false;
        }
        let ppid = match read_parent(cur) {
            Some(p) => p,
            None => return false,
        };
        if ppid == cur {
            return false;
        }
        cur = ppid;
    }
    false
}

/// Ancestralidade com leitura real do SO (delega ao [`crate::platform`]).
/// Pura quanto à decisão: sem info = confiável (`false`).
/// spec: [P-CONN-2]
pub fn peer_is_descendant(peer: u32, daemon: u32) -> bool {
    #[cfg(windows)]
    {
        // Sem árvore de processos via Win32 neste escopo (código usa AF_UNIX
        // em toda plataforma): só o próprio daemon é "descendente".
        peer == daemon
    }
    #[cfg(not(windows))]
    {
        is_descendant_with(peer, daemon, crate::platform::parent_of)
    }
}

/// Pid do peer no socket unix a partir do fd cru (`SO_PEERCRED`/`LOCAL_PEERPID`
/// via [`crate::platform::peer_pid_of_fd`]). `None` = desconhecido explícito
/// (tratar como confiável + warn — spec §1 só recusa descendente confirmado).
/// spec: [P-CONN-2]
#[cfg(unix)]
pub fn peer_pid_of(fd: std::os::unix::io::RawFd) -> Option<u32> {
    match crate::platform::peer_pid_of_fd(fd) {
        crate::platform::PeerPid::Known(p) => Some(p),
        crate::platform::PeerPid::Unknown(_) => {
            tracing::warn!(
                "peer do socket sem credencial do SO; tratando como confiável (só descendente confirmado é recusado)"
            );
            None
        }
    }
}

/// Compat: ponto de troca antigo sem fd. Mantido p/ não quebrar chamadores
/// (retorna `None` = desconhecido explícito); o gate usa [`peer_pid_of`].
/// spec: [P-CONN-2]
#[cfg(unix)]
pub fn peer_pid_fallback() -> Option<u32> {
    tracing::warn!(
        "peer do socket não identificável sem fd; tratando como confiável (limitação documentada ADR-019)"
    );
    None
}

/// Isenção de teste p/ o gate: em `cfg(test)` o peer real é sempre o próprio
/// processo de teste (que seria "descendente" trivialmente). Retorna o peer
/// real quando difere do daemon (cobre `evil_cannot_connect_as_client`-like),
/// e `None` (confiável) quando peer == daemon — i.e., o loopback de teste.
/// Produção (`not(test)`) nunca chama isto: usa `peer_pid_of` direto.
/// spec: [P-CONN-2]
#[cfg(all(test, unix))]
fn peer_pid_of_test_exempt(fd: std::os::unix::io::RawFd) -> Option<u32> {
    let peer = peer_pid_of(fd);
    match peer {
        Some(p) if p == daemon_pid_for_gate() => None,
        other => other,
    }
}

/// Checagem de gate no `handle_conn`: pid do daemon + peer opcional → decisão.
/// `None` peer = confiável (spec: sem info ≠ descendente). Pura p/ teste.
/// spec: [P-CONN-2]
#[cfg(unix)]
pub fn untrusted_peer(daemon_pid: u32, peer: Option<u32>) -> bool {
    match peer {
        Some(p) => peer_is_descendant(p, daemon_pid),
        None => false,
    }
}

#[cfg(unix)]
fn daemon_pid_for_gate() -> u32 {
    std::process::id()
}
#[cfg(unix)]
async fn handle_conn(
    core: Arc<Mutex<Core>>,
    sock: tokio::net::UnixStream,
) -> Result<bool, CoreError> {
    // spec: [P-CONN-2] — identidade fixada no accept: peer descendente do
    // daemon NÃO é cliente confiável (`-32005 UntrustedPeer`, fecha a conexão).
    // `SO_PEERCRED`/`LOCAL_PEERPID` via `platform::peer_pid_of_fd`; sem info =
    // confiável + warn (só descendente CONFIRMADO é recusado).
    // Em teste, cliente e "daemon" são o mesmo processo (`cargo test`) — sem
    // isenção, `SO_PEERCRED` diria que todo cliente de teste é descendente e
    // `serve_loop_with_echo` falharia com `-32005`. A isenção é só `cfg(test)`.
    #[cfg(test)]
    let peer = {
        use std::os::unix::io::AsRawFd as _;
        peer_pid_of_test_exempt(sock.as_raw_fd())
    };
    #[cfg(not(test))]
    let peer = {
        use std::os::unix::io::AsRawFd;
        peer_pid_of(sock.as_raw_fd())
    };
    let untrusted = untrusted_peer(daemon_pid_for_gate(), peer);
    if untrusted {
        tracing::warn!("peer descendente do daemon recusado (UntrustedPeer)");
    }
    let (r, w) = sock.into_split();
    let mut r = BufReader::new(r);
    let w = Arc::new(Mutex::new(w));
    let mut stop = false;
    while let Some(line) = transport::read_message(&mut r).await? {
        if line.is_empty() {
            continue;
        }
        let req: atom_protocol::RpcRequest = serde_json::from_str(&line).map_err(|e| {
            CoreError::Protocol(atom_protocol::RpcError::new(
                atom_protocol::ErrorCode::PARSE_ERROR,
                format!("request inválido: {e}"),
            ))
        })?;
        let id = req.id.clone().unwrap_or(serde_json::Value::Null);
        if untrusted {
            // Qualquer request de peer não-confiável recebe `-32005` e a conexão fecha.
            let denied = atom_protocol::RpcResponse::err(
                id,
                atom_protocol::RpcError::new(
                    atom_protocol::ErrorCode::UNTRUSTED_PEER,
                    "peer descendente do daemon não é cliente confiável",
                ),
            );
            let line = transport::to_line(
                &serde_json::to_value(&denied).map_err(|e| CoreError::Internal(e.to_string()))?,
            )?;
            transport::write_message(&mut *w.lock().await, &line).await?;
            break;
        }
        // `atom.call` com `stream:true`: resposta imediata + relay de chunks.
        if req.method == "atom.call"
            && let Some(p) = req.params.clone()
            && let Ok(cp) = serde_json::from_value::<atom_protocol::AtomCallParams>(p)
            && cp.stream
        {
            if stream_call(core.clone(), w.clone(), &mut r, id, cp).await {
                stop = true;
                break;
            }
            continue;
        }
        let resp = match req.method.as_str() {
            "atom.call" => match req.params {
                Some(p) => match serde_json::from_value::<atom_protocol::AtomCallParams>(p) {
                    Ok(cp) => {
                        let mut c = core.lock().await;
                        match c.call_params(cp).await {
                            Ok(v) => atom_protocol::RpcResponse::ok(id, v),
                            Err(e) => atom_protocol::RpcResponse::err(id, e),
                        }
                    }
                    Err(e) => atom_protocol::RpcResponse::err(
                        id,
                        atom_protocol::RpcError::new(
                            atom_protocol::ErrorCode::INVALID_PARAMS,
                            e.to_string(),
                        ),
                    ),
                },
                None => atom_protocol::RpcResponse::err(
                    id,
                    atom_protocol::RpcError::new(
                        atom_protocol::ErrorCode::INVALID_PARAMS,
                        "params ausentes",
                    ),
                ),
            },
            "atom.status" => {
                let c = core.lock().await;
                atom_protocol::RpcResponse::ok(id, status_json(&c))
            }
            // Trusted-client-only (ADR-019 `[C-AMB-2]`): adota o `grants.toml`
            // externo como base (reescreve o espelho `.core`). Peer não
            // confiável nunca chega aqui (gate acima).
            // spec: [C-AMB-2]
            "atom.grants.reload" => {
                let mut c = core.lock().await;
                match c.adopt_external_grants() {
                    Ok(()) => {
                        atom_protocol::RpcResponse::ok(id, serde_json::json!({"reloaded": true}))
                    }
                    Err(e) => atom_protocol::RpcResponse::err(id, e.rpc("")),
                }
            }
            // Trusted-client-only (ADR-019 `[C-AMB-3]`): recarrega `profile.toml`
            // (bindings/segurança) + emite `atom.binding.changed`/`atom.config.changed`.
            // spec: [C-AMB-3]
            "atom.profile.reload" => {
                let mut c = core.lock().await;
                match c.reload_profile().await {
                    Ok(()) => {
                        atom_protocol::RpcResponse::ok(id, serde_json::json!({"reloaded": true}))
                    }
                    Err(e) => atom_protocol::RpcResponse::err(id, e.rpc("")),
                }
            }
            "atom.stop" => {
                stop = true;
                atom_protocol::RpcResponse::ok(id, serde_json::json!({"stopping": true}))
            }
            m => atom_protocol::RpcResponse::err(
                id,
                atom_protocol::RpcError::new(
                    atom_protocol::ErrorCode::METHOD_NOT_FOUND,
                    format!("método {m} desconhecido"),
                ),
            ),
        };
        let line = transport::to_line(
            &serde_json::to_value(&resp).map_err(|e| CoreError::Internal(e.to_string()))?,
        )?;
        transport::write_message(&mut *w.lock().await, &line).await?;
        if stop {
            break;
        }
    }
    Ok(stop)
}

/// `atom.call` com `stream:true` sobre o socket: responde `{"stream": id}`
/// na hora, repassa chunks/end como notificações na mesma conexão e trata
/// `atom.stream.cancel` / EOF do cliente como cancelamento.
/// Retorna `true` se o cliente pediu `atom.stop`.
#[cfg(unix)]
async fn stream_call(
    core: Arc<Mutex<Core>>,
    w: Arc<Mutex<tokio::net::unix::OwnedWriteHalf>>,
    r: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
    id: serde_json::Value,
    cp: atom_protocol::AtomCallParams,
) -> bool {
    let stream_id = atom_protocol::new_stream_id();
    let immediate = atom_protocol::RpcResponse::ok(id, serde_json::json!({"stream": stream_id}));
    let send = |v: &serde_json::Value| serde_json::to_string(v).unwrap_or_default();
    {
        let line = send(&serde_json::to_value(&immediate).unwrap_or_default());
        let mut w = w.lock().await;
        if transport::write_message(&mut *w, &line).await.is_err() {
            return false;
        }
    }
    let (tx, mut rx) = mpsc::channel::<crate::bus::StreamEvent>(256);
    let w2 = w.clone();
    let drain = tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            let notif = serde_json::json!({
                "jsonrpc": "2.0",
                "method": ev.method,
                "params": ev.params,
            });
            let mut w = w2.lock().await;
            if transport::write_message(&mut *w, &send(&notif))
                .await
                .is_err()
            {
                break;
            }
        }
    });
    let cancel = Arc::new(tokio::sync::Notify::new());
    let cancel2 = cancel.clone();
    let core2 = core.clone();
    let sid = stream_id.clone();
    let svc = cp.service.clone();
    let m = cp.method.clone();
    let flow = cp.flow.clone();
    let mut task = tokio::spawn(async move {
        let mut c = core2.lock().await;
        let sc = crate::bus::StreamCall {
            id: sid,
            service: svc,
            method: m,
            params: cp.params,
            target: cp.target,
            ctx: cp.ctx,
            flow,
        };
        c.call_stream_chan(sc, tx, cancel2).await
    });
    let mut stop = false;
    loop {
        tokio::select! {
            biased;
            done = &mut task => {
                match done {
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) => {
                        // Propaga o erro como `end`, senão o cliente espera para sempre.
                        // Preserva o código do protocolo (ex.: -32800 cancelado).
                        let (code, msg) = match e {
                            CoreError::Protocol(rpc) => (rpc.code, rpc.message),
                            other => (-32603, other.to_string()),
                        };
                        let end = serde_json::json!({
                            "jsonrpc": "2.0",
                            "method": "atom.stream.end",
                            "params": {"stream": stream_id, "error": {"code": code, "message": msg}},
                        });
                        let mut w = w.lock().await;
                        let _ = transport::write_message(&mut *w, &send(&end)).await;
                    }
                    Err(_) => {}
                }
                break;
            }
            line = transport::read_message(r) => {
                match line {
                    Ok(Some(l)) if !l.is_empty() => {
                        let v: serde_json::Value = serde_json::from_str(&l).unwrap_or_default();
                        let method = v.get("method").and_then(|x| x.as_str()).unwrap_or("");
                        let stream = v.get("params").and_then(|p| p.get("stream")).and_then(|s| s.as_str()).unwrap_or("");
                        if method == "atom.stream.cancel" && stream == stream_id {
                            cancel.notify_one();
                        } else if method == "atom.stop" {
                            cancel.notify_one();
                            stop = true;
                            break;
                        }
                    }
                    _ => {
                        cancel.notify_one();
                        break;
                    }
                }
            }
        }
    }
    // A task pode já ter concluído (ramo `done`): segundo await panica.
    if !task.is_finished() {
        let _ = task.await;
    }
    if !drain.is_finished() {
        let _ = drain.await;
    }
    stop
}

/// Token bearer do WS (`serve.token`, 0600). Gera uma vez, reusa depois.
/// Unix apenas: o WS só serve junto ao daemon local (AF_UNIX).
#[cfg(unix)]
pub fn ensure_ws_token(paths: &crate::profile::ProfilePaths) -> Result<String, CoreError> {
    if let Ok(t) = std::fs::read_to_string(&paths.serve_token) {
        let t = t.trim().to_string();
        if !t.is_empty() {
            return Ok(t);
        }
    }
    if let Some(parent) = paths.serve_token.parent() {
        std::fs::create_dir_all(parent).map_err(|e| CoreError::Io(e.to_string()))?;
    }
    let mut buf = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut buf);
    let tok: String = buf.iter().map(|b| format!("{b:02x}")).collect();
    // 0600 desde a criação (revisão v1.0): sem janela legível entre escrita e chmod.
    crate::profile::atomic_write_private(&paths.serve_token, format!("{tok}\n").as_bytes())
        .map_err(|e| CoreError::Io(e.to_string()))?;
    Ok(tok)
}

/// Comparação em tempo constante (sem early-exit por conteúdo).
#[cfg(unix)]
fn ct_eq(a: &str, b: &str) -> bool {
    let (x, y) = (a.as_bytes(), b.as_bytes());
    if x.len() != y.len() {
        return false;
    }
    x.iter()
        .zip(y.iter())
        .fold(0u8, |acc, (p, q)| acc | (p ^ q))
        == 0
}

#[cfg(unix)]
fn bearer_ok(
    req: &tokio_tungstenite::tungstenite::handshake::server::Request,
    token: &str,
) -> bool {
    if let Some(auth) = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        && let Some(bearer) = auth.strip_prefix("Bearer ")
    {
        // Tempo constante (revisão v1.0): sem early-exit por conteúdo.
        return ct_eq(bearer.trim(), token);
    }
    req.uri().query().is_some_and(|q| {
        q.split('&').any(|kv| {
            let mut it = kv.splitn(2, '=');
            it.next() == Some("token") && it.next().is_some_and(|t| ct_eq(t, token))
        })
    })
}

#[cfg(unix)]
type WsSink = Arc<
    Mutex<
        futures_util::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
            Message,
        >,
    >,
>;

#[cfg(unix)]
type WsHandshakeRequest = tokio_tungstenite::tungstenite::handshake::server::Request;
#[cfg(unix)]
type WsHandshakeResponse = tokio_tungstenite::tungstenite::handshake::server::Response;
#[cfg(unix)]
type WsHandshakeError = tokio_tungstenite::tungstenite::handshake::server::ErrorResponse;

/// Resposta 401 p/ token inválido (corpo grande por construção do tungstenite).
#[allow(clippy::result_large_err)]
#[cfg(unix)]
fn check_bearer(
    req: &WsHandshakeRequest,
    resp: WsHandshakeResponse,
    token: &str,
) -> Result<WsHandshakeResponse, WsHandshakeError> {
    if bearer_ok(req, token) {
        Ok(resp)
    } else {
        Err(WsHandshakeError::new(Some("token inválido".to_string())))
    }
}

#[cfg(unix)]
async fn ws_send(sink: &WsSink, v: &serde_json::Value) -> bool {
    let text = serde_json::to_string(v).unwrap_or_default();
    sink.lock()
        .await
        .send(Message::Text(text.into()))
        .await
        .is_ok()
}

#[cfg(unix)]
fn ws_err(id: serde_json::Value, code: i32, msg: impl Into<String>) -> serde_json::Value {
    serde_json::to_value(atom_protocol::RpcResponse::err(
        id,
        atom_protocol::RpcError::new(code, msg),
    ))
    .unwrap_or(serde_json::Value::Null)
}

/// `atom.call` com `stream:true` sobre WS: mesma semântica do socket.
#[cfg(unix)]
async fn ws_stream_call(
    core: Arc<Mutex<Core>>,
    sink: WsSink,
    id: serde_json::Value,
    cp: atom_protocol::AtomCallParams,
    incoming: &mut mpsc::Receiver<serde_json::Value>,
) {
    let stream_id = atom_protocol::new_stream_id();
    let immediate = atom_protocol::RpcResponse::ok(id, serde_json::json!({"stream": stream_id}));
    if !ws_send(&sink, &serde_json::to_value(&immediate).unwrap_or_default()).await {
        return;
    }
    let (tx, mut rx) = mpsc::channel::<crate::bus::StreamEvent>(256);
    let sink2 = sink.clone();
    let drain = tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            let notif = serde_json::json!({
                "jsonrpc": "2.0",
                "method": ev.method,
                "params": ev.params,
            });
            if !ws_send(&sink2, &notif).await {
                break;
            }
        }
    });
    let cancel = Arc::new(tokio::sync::Notify::new());
    let cancel2 = cancel.clone();
    let core2 = core.clone();
    let sid = stream_id.clone();
    let (svc, m, params, target, ctx, flow) = (
        cp.service.clone(),
        cp.method.clone(),
        cp.params.clone(),
        cp.target.clone(),
        cp.ctx.clone(),
        cp.flow.clone(),
    );
    let mut task = tokio::spawn(async move {
        let mut c = core2.lock().await;
        c.call_stream_chan(
            crate::bus::StreamCall {
                id: sid,
                service: svc,
                method: m,
                params,
                target,
                ctx,
                flow,
            },
            tx,
            cancel2,
        )
        .await
    });
    loop {
        tokio::select! {
            biased;
            done = &mut task => {
                if let Ok(Err(e)) = done {
                    let (code, msg) = match e {
                        CoreError::Protocol(rpc) => (rpc.code, rpc.message),
                        other => (-32603, other.to_string()),
                    };
                    let end = serde_json::json!({
                        "jsonrpc": "2.0",
                        "method": "atom.stream.end",
                        "params": {"stream": stream_id, "error": {"code": code, "message": msg}},
                    });
                    ws_send(&sink, &end).await;
                }
                break;
            }
            msg = incoming.recv() => {
                match msg {
                    Some(v) => {
                        let method = v.get("method").and_then(|x| x.as_str()).unwrap_or("");
                        let stream = v.get("params").and_then(|p| p.get("stream")).and_then(|s| s.as_str()).unwrap_or("");
                        if method == "atom.stream.cancel" && stream == stream_id {
                            cancel.notify_one();
                        }
                    }
                    None => {
                        cancel.notify_one();
                        break;
                    }
                }
            }
        }
    }
    // A task pode já ter concluído (ramo `done`): segundo await panica.
    if !task.is_finished() {
        let _ = task.await;
    }
    if !drain.is_finished() {
        let _ = drain.await;
    }
}

/// Uma conexão WS autenticada: `atom.hello`/`atom.call`/`atom.status`.
/// `atom.stop` aqui só desconecta (não derruba o daemon).
/// `result_large_err`: o tipo de resposta do handshake é grande por
/// construção do tungstenite; não há como enxugar sem quebrar o protocolo.
#[allow(clippy::result_large_err)]
#[cfg(unix)]
async fn handle_ws(
    core: Arc<Mutex<Core>>,
    stream: tokio::net::TcpStream,
    token: String,
) -> Result<(), CoreError> {
    let ws = tokio_tungstenite::accept_hdr_async(
        stream,
        |req: &WsHandshakeRequest, resp: WsHandshakeResponse| check_bearer(req, resp, &token),
    )
    .await
    .map_err(|e| CoreError::Package(format!("handshake WS: {e}")))?;
    let (sink_half, mut stream_half) = futures_util::StreamExt::split(ws);
    let sink: WsSink = Arc::new(Mutex::new(sink_half));
    // Fila p/ notificações do cliente durante um stream (cancel).
    let (ntx, mut nrx) = mpsc::channel::<serde_json::Value>(1024);
    while let Some(msg) = futures_util::StreamExt::next(&mut stream_half).await {
        let msg = match msg {
            Ok(Message::Text(t)) => t,
            Ok(Message::Close(_)) | Err(_) => break,
            _ => continue,
        };
        let req: atom_protocol::RpcRequest = match serde_json::from_str(&msg) {
            Ok(r) => r,
            Err(e) => {
                let v = ws_err(
                    serde_json::Value::Null,
                    atom_protocol::ErrorCode::PARSE_ERROR,
                    format!("request inválido: {e}"),
                );
                ws_send(&sink, &v).await;
                continue;
            }
        };
        let id = req.id.clone().unwrap_or(serde_json::Value::Null);
        let has_id = req.id.is_some();
        if req.method == "atom.call"
            && let Some(p) = req.params.clone()
            && let Ok(cp) = serde_json::from_value::<atom_protocol::AtomCallParams>(p)
            && cp.stream
        {
            ws_stream_call(core.clone(), sink.clone(), id, cp, &mut nrx).await;
            continue;
        }
        let resp = match req.method.as_str() {
            "atom.hello" => {
                let c = core.lock().await;
                serde_json::to_value(atom_protocol::RpcResponse::ok(
                    id,
                    serde_json::json!({"core": {"version": crate::CORE_VERSION, "protocol": crate::PROTOCOL}, "profile": c.profile_name}),
                ))
                .unwrap_or(serde_json::Value::Null)
            }
            "atom.call" => match req.params {
                Some(p) => match serde_json::from_value::<atom_protocol::AtomCallParams>(p) {
                    Ok(cp) => {
                        let mut c = core.lock().await;
                        match c.call_params(cp).await {
                            Ok(v) => serde_json::to_value(atom_protocol::RpcResponse::ok(id, v))
                                .unwrap_or_default(),
                            Err(e) => serde_json::to_value(atom_protocol::RpcResponse::err(id, e))
                                .unwrap_or_default(),
                        }
                    }
                    Err(e) => ws_err(id, atom_protocol::ErrorCode::INVALID_PARAMS, e.to_string()),
                },
                None => ws_err(
                    id,
                    atom_protocol::ErrorCode::INVALID_PARAMS,
                    "params ausentes",
                ),
            },
            "atom.status" => {
                let c = core.lock().await;
                let mut st = status_json(&c);
                if let Some(o) = st.as_object_mut() {
                    o.insert("ws".to_string(), serde_json::json!(c.ws_url()));
                }
                serde_json::to_value(atom_protocol::RpcResponse::ok(id, st)).unwrap_or_default()
            }
            "atom.stop" => {
                // No WS, `stop` só desconecta (não derruba o daemon).
                serde_json::to_value(atom_protocol::RpcResponse::ok(
                    id,
                    serde_json::json!({"stopping": false}),
                ))
                .unwrap_or_default()
            }
            m => ws_err(
                id,
                atom_protocol::ErrorCode::METHOD_NOT_FOUND,
                format!("método {m} desconhecido"),
            ),
        };
        if has_id {
            ws_send(&sink, &resp).await;
        } else if req.method == "atom.stream.cancel" {
            let _ = ntx
                .send(serde_json::from_str(&msg).unwrap_or(serde_json::Value::Null))
                .await;
        }
    }
    Ok(())
}

/// Loop de accept do WS (uma task por conexão; nunca retorna sozinho).
#[cfg(unix)]
async fn serve_ws(
    core: Arc<Mutex<Core>>,
    listener: TcpListener,
    token: String,
) -> Result<(), CoreError> {
    loop {
        let (stream, _) = listener
            .accept()
            .await
            .map_err(|e| CoreError::Io(e.to_string()))?;
        let c = core.clone();
        let t = token.clone();
        tokio::spawn(async move {
            let _ = handle_ws(c, stream, t).await;
        });
    }
}

#[cfg(unix)]
async fn serve_loop(core: Arc<Mutex<Core>>, sock_path: PathBuf) -> Result<(), CoreError> {
    if sock_path.exists() {
        // Socket obsoleto (daemon morto) ou daemon ativo: sonda antes de remover.
        if tokio::net::UnixStream::connect(&sock_path).await.is_ok() {
            return Err(CoreError::Io(format!(
                "daemon já rodando ({} existe)",
                sock_path.display()
            )));
        }
        std::fs::remove_file(&sock_path).map_err(|e| CoreError::Io(e.to_string()))?;
    }
    let listener =
        tokio::net::UnixListener::bind(&sock_path).map_err(|e| CoreError::Io(e.to_string()))?;
    socket_perms(&sock_path)?;
    loop {
        let (stream, _) = listener
            .accept()
            .await
            .map_err(|e| CoreError::Io(e.to_string()))?;
        let c = core.clone();
        let stop = handle_conn(c, stream).await?;
        if stop {
            break;
        }
    }
    std::fs::remove_file(&sock_path).ok();
    Ok(())
}

/// Sobe o daemon: boot + loop do socket (unix) até Ctrl+C ou `atom.stop`.
/// Cura marcador de update pendente e agenda checagem periódica (v0.7).
/// `ws_addr`: `Some("127.0.0.1:0")` liga o WS do `serve --ws` (v0.8).
pub async fn run(
    profile: &str,
    home: Option<&Path>,
    #[cfg_attr(not(unix), allow(unused_variables))] ws_addr: Option<&str>,
) -> Result<(), CoreError> {
    // spec: [P-CONN-2] — daemon como subreaper (Linux): órfãos de double-fork
    // das extensões continuam descendentes na caminhada `/proc/<pid>/stat`.
    // Best-effort: falha = warn, sem info ≠ descendente.
    if let Err(e) = crate::platform::claim_subreaper() {
        tracing::warn!("subreaper indisponível ({e}); double-fork pode escapar da árvore");
    }
    let mut core = Core::open_with(profile, home).await?;
    // Cura update cujo boot anterior não confirmou saúde.
    {
        let bin = crate::update::bin_dir(&core);
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("atom"));
        if crate::update::heal_if_needed(&bin, &exe) {
            tracing::warn!("rollback de update anterior aplicado na cura");
            core.refresh()?;
        }
    }
    // WS antes do boot: a ui-web (onStartup) já enxerga `atom.profile.info.ws`.
    // Unix apenas: o socket do daemon é AF_UNIX; sem ele não há daemon local
    // e o WS não serve (Windows aguarda transporte nomeado — fora do escopo).
    #[cfg(unix)]
    let (ws_listener, ws_url): (Option<TcpListener>, Option<String>) = {
        let mut ws_listener: Option<TcpListener> = None;
        let mut ws_url: Option<String> = None;
        if let Some(addr) = ws_addr {
            let listener = TcpListener::bind(addr)
                .await
                .map_err(|e| CoreError::Io(format!("bind WS {addr}: {e}")))?;
            let bound = listener
                .local_addr()
                .map_err(|e| CoreError::Io(e.to_string()))?;
            let token = ensure_ws_token(&core.paths)?;
            let url = format!("ws://{bound}");
            eprintln!("ws {url} (token em {})", core.paths.serve_token.display());
            core.set_ws_info(url.clone(), token.clone());
            ws_url = Some(url);
            ws_listener = Some(listener);
        }
        (ws_listener, ws_url)
    };
    let started = core.boot().await?;
    tracing::info!(perfil = %profile, ?started, "daemon no ar");
    // Checagem de update no boot (best-effort) + timer periódico.
    // Origem mesclada uma vez: flags n/a no daemon, só config global + embutido.
    let (gurl, gkeys, gchan) = crate::update::global_update_cfg(home);
    let (ubase, ukeys, uchan) =
        crate::update::merge_update_source(None, vec![], "stable".to_string(), gurl, gkeys, gchan);
    {
        let mut guard = core;
        Box::pin(crate::update::update_tick(
            &mut guard,
            ubase.as_deref(),
            ukeys.clone(),
            &uchan,
        ))
        .await;
        core = guard;
    }
    #[allow(unused_variables)]
    let interval_h = core.config.update.check_interval_hours;
    #[allow(unused_variables)]
    let pid = std::process::id();
    std::fs::write(&core.paths.pid_file, pid.to_string())
        .map_err(|e| CoreError::Io(e.to_string()))?;
    #[cfg(unix)]
    {
        let core = Arc::new(Mutex::new(core));
        let sock = core.lock().await.paths.socket.clone();
        // Timer de update: aplica política a cada intervalo (0 = só no boot).
        // `try_lock`: nunca bloqueia o atendimento p/ checar update; se o
        // daemon estiver ocupado, pula a rodada (best-effort, v0.7).
        let updater = core.clone();
        let tick = async move {
            if interval_h == 0 {
                std::future::pending::<()>().await;
                return;
            }
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(interval_h * 3600)).await;
                if let Ok(mut c) = updater.try_lock() {
                    Box::pin(crate::update::update_tick(
                        &mut c,
                        ubase.as_deref(),
                        ukeys.clone(),
                        &uchan,
                    ))
                    .await;
                }
            }
        };
        // WS em task própria (nunca termina sozinho).
        if let Some(listener) = ws_listener {
            let wcore = core.clone();
            let wtoken = core.lock().await.ws_token().unwrap_or_default();
            let _ = ws_url;
            tokio::spawn(async move {
                let _ = serve_ws(wcore, listener, wtoken).await;
            });
        }
        // Bomba de inatividade (v0.8): extensões também iniciam chamadas
        // (`atom.call` da ui-web p/ `/api/*`) fora de qualquer drive.
        // `try_lock`: nunca bloqueia atendimento; quem está dirigindo já
        // bombeia (pump próprio). Cada mensagem é roteada sob o lock.
        let pumper = core.clone();
        let pump = async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                if let Ok(mut c) = pumper.try_lock() {
                    while let Some(i) = c.host.try_incoming() {
                        c.pump_one(i).await;
                    }
                }
            }
        };
        tokio::select! {
            r = serve_loop(core.clone(), sock) => {
                r?;
                core.lock().await.shutdown().await;
            }
            _ = tokio::signal::ctrl_c() => {
                core.lock().await.shutdown().await;
            }
            _ = tick => {}
            _ = pump => {}
        }
        let paths = core.lock().await.paths.clone();
        std::fs::remove_file(&paths.pid_file).ok();
        std::fs::remove_file(&paths.socket).ok();
    }
    #[cfg(not(unix))]
    {
        #[allow(unused_mut)]
        let mut core = core;
        let _ = tokio::signal::ctrl_c().await;
        core.shutdown().await;
        std::fs::remove_file(&core.paths.pid_file).ok();
    }
    Ok(())
}

#[cfg(unix)]
async fn rpc(
    socket: &Path,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, CoreError> {
    let stream = tokio::net::UnixStream::connect(socket)
        .await
        .map_err(|e| CoreError::Io(format!("daemon inacessível: {e}")))?;
    let (r, mut w) = stream.into_split();
    let mut r = BufReader::new(r);
    let req = atom_protocol::RpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(serde_json::json!(1)),
        method: method.to_string(),
        params: Some(params),
    };
    transport::write_message(
        &mut w,
        &transport::to_line(
            &serde_json::to_value(&req).map_err(|e| CoreError::Internal(e.to_string()))?,
        )?,
    )
    .await?;
    let line = transport::read_message(&mut r)
        .await?
        .ok_or_else(|| CoreError::Io("daemon fechou a conexão".to_string()))?;
    let resp: atom_protocol::RpcResponse =
        serde_json::from_str(&line).map_err(|e| CoreError::Internal(e.to_string()))?;
    if let Some(e) = resp.error {
        return Err(CoreError::Protocol(e));
    }
    Ok(resp.result.unwrap_or(serde_json::Value::Null))
}

#[cfg(not(unix))]
#[allow(dead_code)]
async fn rpc(
    _socket: &Path,
    _method: &str,
    _params: serde_json::Value,
) -> Result<serde_json::Value, CoreError> {
    Err(CoreError::Io(
        "daemon via socket não suportado nesta plataforma".to_string(),
    ))
}

/// `atom.call` via daemon (o daemon aplica `$ctx` autoritativo).
#[cfg(unix)]
pub async fn call_via_daemon(
    socket: &Path,
    service: &str,
    method: &str,
    params: serde_json::Value,
    target: Option<&str>,
) -> Result<serde_json::Value, CoreError> {
    rpc(
        socket,
        "atom.call",
        serde_json::json!({
            "service": service,
            "method": method,
            "params": params,
            "target": target,
        }),
    )
    .await
}

/// `atom.status` via daemon.
#[cfg(unix)]
pub async fn status_via_daemon(socket: &Path) -> Result<serde_json::Value, CoreError> {
    rpc(socket, "atom.status", serde_json::json!({})).await
}

/// `atom.stop` via daemon.
#[cfg(unix)]
pub async fn stop_via_daemon(socket: &Path) -> Result<serde_json::Value, CoreError> {
    rpc(socket, "atom.stop", serde_json::json!({})).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ct_eq_basico() {
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "abcd"));
        assert!(!ct_eq("", "x"));
        assert!(ct_eq("", ""));
    }

    fn test_paths(t: &tempfile::TempDir) -> crate::profile::ProfilePaths {
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

    #[test]
    fn ws_token_roundtrip() {
        let t = tempfile::tempdir().unwrap();
        let p = test_paths(&t);
        let a = ensure_ws_token(&p).unwrap();
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(ensure_ws_token(&p).unwrap(), a);
        // Arquivo vazio regenera.
        std::fs::write(&p.serve_token, "\n").unwrap();
        assert_eq!(ensure_ws_token(&p).unwrap().len(), 64);
    }

    #[cfg(unix)]
    #[test]
    fn ws_token_private() {
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::tempdir().unwrap();
        let p = test_paths(&t);
        ensure_ws_token(&p).unwrap();
        let mode = std::fs::metadata(&p.serve_token)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn ws_err_shape() {
        let v = ws_err(serde_json::json!(1), -32003, "negado");
        assert_eq!(v["jsonrpc"], serde_json::json!("2.0"));
        assert_eq!(v["error"]["code"], serde_json::json!(-32003));
    }

    #[test]
    fn bearer_ok_header_query() {
        let req = WsHandshakeRequest::builder()
            .header("authorization", "Bearer tok123")
            .uri("ws://127.0.0.1/x")
            .body(())
            .unwrap();
        assert!(bearer_ok(&req, "tok123"));
        assert!(!bearer_ok(&req, "outro"));
        let req = WsHandshakeRequest::builder()
            .uri("ws://127.0.0.1/x?token=tok123&other=1")
            .body(())
            .unwrap();
        assert!(bearer_ok(&req, "tok123"));
        let req = WsHandshakeRequest::builder()
            .uri("ws://127.0.0.1/x")
            .body(())
            .unwrap();
        assert!(!bearer_ok(&req, "tok123"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rpc_wrappers_roundtrip() {
        let t = tempfile::tempdir().unwrap();
        let sock = t.path().join("atom-dev.sock");
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let (r, mut w) = stream.into_split();
                    let mut r = BufReader::new(r);
                    while let Ok(Some(line)) = transport::read_message(&mut r).await {
                        if line.is_empty() {
                            continue;
                        }
                        let req: atom_protocol::RpcRequest = serde_json::from_str(&line).unwrap();
                        let id = req.id.clone().unwrap_or(serde_json::Value::Null);
                        let svc = req
                            .params
                            .as_ref()
                            .and_then(|p| p.get("service"))
                            .and_then(|s| s.as_str())
                            .unwrap_or("");
                        let resp = match req.method.as_str() {
                            "atom.status" => atom_protocol::RpcResponse::ok(
                                id,
                                serde_json::json!({"daemon": true}),
                            ),
                            "atom.stop" => atom_protocol::RpcResponse::ok(
                                id,
                                serde_json::json!({"stopped": true}),
                            ),
                            _ if svc == "err.svc" => atom_protocol::RpcResponse::err(
                                id,
                                atom_protocol::RpcError::new(
                                    atom_protocol::ErrorCode::NO_PROVIDER,
                                    "sem provedor",
                                ),
                            ),
                            _ => atom_protocol::RpcResponse::ok(
                                id,
                                serde_json::json!({"echo": req.params}),
                            ),
                        };
                        let line =
                            transport::to_line(&serde_json::to_value(&resp).unwrap()).unwrap();
                        if transport::write_message(&mut w, &line).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        let v = status_via_daemon(&sock).await.unwrap();
        assert_eq!(v["daemon"], serde_json::json!(true));
        let v = stop_via_daemon(&sock).await.unwrap();
        assert_eq!(v["stopped"], serde_json::json!(true));
        let v = call_via_daemon(
            &sock,
            "test.echo@0",
            "echo",
            serde_json::json!({"x": 1}),
            None,
        )
        .await
        .unwrap();
        assert_eq!(v["echo"]["method"], serde_json::json!("echo"));
        let e = call_via_daemon(&sock, "err.svc", "m", serde_json::json!({}), None).await;
        assert!(matches!(e, Err(CoreError::Protocol(_))));
        // Socket inexistente vira Io.
        assert!(
            status_via_daemon(&t.path().join("nope.sock"))
                .await
                .is_err()
        );
    }

    /// Sobe o `serve_loop` com o echo instalado e dirige o protocolo real.
    #[cfg(unix)]
    #[tokio::test]
    async fn serve_loop_with_echo() {
        let home = tempfile::tempdir().unwrap();
        let mut core = Core::open_with("default", Some(home.path())).await.unwrap();
        install_echo_for_serve(&core);
        core.refresh().unwrap();
        core.boot().await.unwrap();
        let sock = core.paths.socket.clone();
        let core = Arc::new(Mutex::new(core));
        let srv = tokio::spawn(serve_loop(core.clone(), sock.clone()));
        for _ in 0..100 {
            if sock.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        // Segunda instância no mesmo socket recusa (daemon já rodando).
        assert!(serve_loop(core.clone(), sock.clone()).await.is_err());
        // status + call pelo protocolo real (handle_conn + invoke).
        let st = status_via_daemon(&sock).await.unwrap();
        assert_eq!(st["daemon"], serde_json::json!(true));
        let v = call_via_daemon(
            &sock,
            "test.echo@0",
            "echo",
            serde_json::json!({"x": 1}),
            None,
        )
        .await
        .unwrap();
        assert_eq!(v, serde_json::json!({"x": 1}));
        // Método desconhecido responde erro e mantém a conexão.
        let raw = raw_request(&sock, "atom.bogus", serde_json::json!({})).await;
        assert!(raw.get("error").is_some());
        // Stream com provider não-streaming: cancela e espera o fim (-32800).
        let stream_id = raw_stream_call(&sock).await;
        assert!(stream_id.starts_with("st_"));
        // stop derruba o loop.
        stop_via_daemon(&sock).await.unwrap();
        srv.await.unwrap().unwrap();
        assert!(!sock.exists());
        core.lock().await.shutdown().await;
    }

    /// WS com token: hello/call/status/stop/erro + token inválido.
    #[tokio::test]
    async fn ws_hello_call_status() {
        use futures_util::{SinkExt, StreamExt};

        let home = tempfile::tempdir().unwrap();
        let mut core = Core::open_with("default", Some(home.path())).await.unwrap();
        install_echo_for_serve(&core);
        core.refresh().unwrap();
        core.boot().await.unwrap();
        let token = ensure_ws_token(&core.paths).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let core = Arc::new(Mutex::new(core));
        tokio::spawn(serve_ws(core, listener, token.clone()));
        let url = format!("ws://{addr}/?token={token}");
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let mut next_id = 0u64;
        let mut req = async |method: &str, params: serde_json::Value| {
            next_id += 1;
            ws.send(tokio_tungstenite::tungstenite::Message::Text(
                serde_json::json!({"jsonrpc": "2.0", "id": next_id, "method": method, "params": params})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            loop {
                let msg = StreamExt::next(&mut ws).await.unwrap().unwrap();
                if let tokio_tungstenite::tungstenite::Message::Text(t) = msg {
                    let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                    if v.get("id").is_some() {
                        return v;
                    }
                }
            }
        };
        let hello = req("atom.hello", serde_json::json!({})).await;
        assert!(hello["result"]["core"].is_object());
        let st = req("atom.status", serde_json::json!({})).await;
        assert_eq!(st["result"]["daemon"], serde_json::json!(true));
        let echo = req(
            "atom.call",
            serde_json::json!({"service": "test.echo@0", "method": "echo", "params": {"x": 1}}),
        )
        .await;
        assert_eq!(echo["result"], serde_json::json!({"x": 1}));
        let bad = req("atom.bogus", serde_json::json!({})).await;
        assert!(bad.get("error").is_some());
        let no_params = req("atom.call", serde_json::json!({"service": 1})).await;
        assert!(no_params.get("error").is_some());
        let stop = req("atom.stop", serde_json::json!({})).await;
        assert_eq!(stop["result"]["stopping"], serde_json::json!(false));
        // Token inválido recusa o handshake.
        let bad_url = format!("ws://{addr}/?token=errado");
        assert!(tokio_tungstenite::connect_async(&bad_url).await.is_err());
    }

    fn install_echo_for_serve(core: &Core) {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let bin = root.join("target/debug/echo");
        if !bin.exists() {
            let st = std::process::Command::new("cargo")
                .args(["build", "-p", "test-echo"])
                .status()
                .expect("cargo build");
            assert!(st.success());
        }
        let src = root.join("testdata/echo");
        let id = crate::pkg::install_dev(&core.paths, &core.data_store, &src).unwrap();
        assert_eq!(id, "test/echo@0.1.0");
        let lock = crate::pkg::load_lock(&core.paths).unwrap();
        let lp = lock.find("test/echo").unwrap();
        let dir = crate::pkg::package_dir(&core.data_store, lp).unwrap();
        let m = crate::pkg::read_manifest(&dir).unwrap();
        let dst = dir.join(m.entrypoint.unwrap());
        std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
        std::fs::copy(&bin, &dst).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut p = std::fs::metadata(&dst).unwrap().permissions();
            p.set_mode(0o755);
            std::fs::set_permissions(&dst, p).unwrap();
        }
    }

    #[cfg(unix)]
    async fn raw_request(
        sock: &Path,
        method: &str,
        params: serde_json::Value,
    ) -> serde_json::Value {
        let stream = tokio::net::UnixStream::connect(sock).await.unwrap();
        let (r, mut w) = stream.into_split();
        let mut r = BufReader::new(r);
        let line = transport::to_line(&serde_json::json!({
            "jsonrpc": "2.0", "id": 7, "method": method, "params": params
        }))
        .unwrap();
        transport::write_message(&mut w, &line).await.unwrap();
        let back = transport::read_message(&mut r).await.unwrap().unwrap();
        serde_json::from_str(&back).unwrap()
    }

    #[cfg(unix)]
    async fn raw_stream_call(sock: &Path) -> String {
        let stream = tokio::net::UnixStream::connect(sock).await.unwrap();
        let (r, mut w) = stream.into_split();
        let mut r = BufReader::new(r);
        let line = transport::to_line(&serde_json::json!({
            "jsonrpc": "2.0", "id": 9, "method": "atom.call",
            "params": {"service": "test.echo@0", "method": "echo",
                       "params": {"x": 2}, "stream": true}
        }))
        .unwrap();
        transport::write_message(&mut w, &line).await.unwrap();
        let immediate: serde_json::Value =
            serde_json::from_str(&transport::read_message(&mut r).await.unwrap().unwrap()).unwrap();
        let sid = immediate["result"]["stream"].as_str().unwrap().to_string();
        // Echo não é streaming: cancela e espera o fim com -32800.
        let cancel = transport::to_line(&serde_json::json!({
            "jsonrpc": "2.0", "method": "atom.stream.cancel",
            "params": {"stream": sid}
        }))
        .unwrap();
        transport::write_message(&mut w, &cancel).await.unwrap();
        let end: serde_json::Value =
            tokio::time::timeout(std::time::Duration::from_secs(15), async {
                loop {
                    let l = transport::read_message(&mut r).await.unwrap().unwrap();
                    let v: serde_json::Value = serde_json::from_str(&l).unwrap();
                    if v.get("method") == Some(&serde_json::json!("atom.stream.end")) {
                        return v;
                    }
                }
            })
            .await
            .expect("stream do echo termina");
        assert_eq!(end["params"]["error"]["code"], serde_json::json!(-32800));
        sid
    }
}
