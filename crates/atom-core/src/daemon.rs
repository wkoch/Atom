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
use std::sync::Arc;

use futures_util::SinkExt;
use tokio::io::BufReader;
use tokio::net::TcpListener;
use tokio::sync::{Mutex, mpsc};
use tokio_tungstenite::tungstenite::Message;

use crate::bus::Core;
use crate::error::CoreError;
use crate::transport;

/// Resposta de estado para `atom.status` / CLI.
pub fn status_json(core: &Core) -> serde_json::Value {
    let running: std::collections::HashMap<String, String> = core
        .host
        .states()
        .iter()
        .map(|(n, (st, _))| (n.clone(), format!("{st:?}")))
        .collect();
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
    })
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

#[cfg(unix)]
async fn handle_conn(
    core: Arc<Mutex<Core>>,
    sock: tokio::net::UnixStream,
) -> Result<bool, CoreError> {
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
    let (tx, mut rx) = mpsc::unbounded_channel::<crate::bus::StreamEvent>();
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
    let mut task = tokio::spawn(async move {
        let mut c = core2.lock().await;
        let sc = crate::bus::StreamCall {
            id: sid,
            service: svc,
            method: m,
            params: cp.params,
            target: cp.target,
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
    let _ = task.await;
    let _ = drain.await;
    stop
}

/// Token bearer do WS (`serve.token`, 0600). Gera uma vez, reusa depois.
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
    std::fs::write(&paths.serve_token, format!("{tok}\n"))
        .map_err(|e| CoreError::Io(e.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut p = std::fs::metadata(&paths.serve_token)
            .map_err(|e| CoreError::Io(e.to_string()))?
            .permissions();
        p.set_mode(0o600);
        std::fs::set_permissions(&paths.serve_token, p)
            .map_err(|e| CoreError::Io(e.to_string()))?;
    }
    Ok(tok)
}

fn bearer_ok(
    req: &tokio_tungstenite::tungstenite::handshake::server::Request,
    token: &str,
) -> bool {
    if let Some(auth) = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        && let Some(bearer) = auth.strip_prefix("Bearer ")
        && bearer.trim() == token
    {
        return true;
    }
    req.uri().query().is_some_and(|q| {
        q.split('&').any(|kv| {
            let mut it = kv.splitn(2, '=');
            it.next() == Some("token") && it.next() == Some(token)
        })
    })
}

type WsSink = Arc<
    Mutex<
        futures_util::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
            Message,
        >,
    >,
>;

type WsHandshakeRequest = tokio_tungstenite::tungstenite::handshake::server::Request;
type WsHandshakeResponse = tokio_tungstenite::tungstenite::handshake::server::Response;
type WsHandshakeError = tokio_tungstenite::tungstenite::handshake::server::ErrorResponse;

/// Resposta 401 p/ token inválido (corpo grande por construção do tungstenite).
#[allow(clippy::result_large_err)]
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

async fn ws_send(sink: &WsSink, v: &serde_json::Value) -> bool {
    let text = serde_json::to_string(v).unwrap_or_default();
    sink.lock()
        .await
        .send(Message::Text(text.into()))
        .await
        .is_ok()
}

fn ws_err(id: serde_json::Value, code: i32, msg: impl Into<String>) -> serde_json::Value {
    serde_json::to_value(atom_protocol::RpcResponse::err(
        id,
        atom_protocol::RpcError::new(code, msg),
    ))
    .unwrap_or(serde_json::Value::Null)
}

/// `atom.call` com `stream:true` sobre WS: mesma semântica do socket.
async fn ws_stream_call(
    core: Arc<Mutex<Core>>,
    sink: WsSink,
    id: serde_json::Value,
    cp: atom_protocol::AtomCallParams,
    incoming: &mut mpsc::UnboundedReceiver<serde_json::Value>,
) {
    let stream_id = atom_protocol::new_stream_id();
    let immediate = atom_protocol::RpcResponse::ok(id, serde_json::json!({"stream": stream_id}));
    if !ws_send(&sink, &serde_json::to_value(&immediate).unwrap_or_default()).await {
        return;
    }
    let (tx, mut rx) = mpsc::unbounded_channel::<crate::bus::StreamEvent>();
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
    let (svc, m, params, target) = (
        cp.service.clone(),
        cp.method.clone(),
        cp.params.clone(),
        cp.target.clone(),
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
    let _ = task.await;
    let _ = drain.await;
}

/// Uma conexão WS autenticada: `atom.hello`/`atom.call`/`atom.status`.
/// `atom.stop` aqui só desconecta (não derruba o daemon).
/// `result_large_err`: o tipo de resposta do handshake é grande por
/// construção do tungstenite; não há como enxugar sem quebrar o protocolo.
#[allow(clippy::result_large_err)]
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
    let (ntx, mut nrx) = mpsc::unbounded_channel::<serde_json::Value>();
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
            let _ = ntx.send(serde_json::from_str(&msg).unwrap_or(serde_json::Value::Null));
        }
    }
    Ok(())
}

/// Loop de accept do WS (uma task por conexão; nunca retorna sozinho).
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
    ws_addr: Option<&str>,
) -> Result<(), CoreError> {
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
    let interval_h = core.config.update.check_interval_hours;
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
pub async fn status_via_daemon(socket: &Path) -> Result<serde_json::Value, CoreError> {
    rpc(socket, "atom.status", serde_json::json!({})).await
}

/// `atom.stop` via daemon.
pub async fn stop_via_daemon(socket: &Path) -> Result<serde_json::Value, CoreError> {
    rpc(socket, "atom.stop", serde_json::json!({})).await
}
