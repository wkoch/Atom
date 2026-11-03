#![forbid(unsafe_code)]
//! Daemon do perfil: `serve` (socket unix) + cliente mínimo do CLI.
//!
//! Transporte NDJSON sobre `<runtime>/atom-<perfil>.sock` (spec protocol §1,
//! permissão 0600). Métodos: `atom.call`, `atom.status`, `atom.stop`.
//! Sem auth além do socket local (trust-local na v0.1).
//! `shortcut: sem named pipe no Windows (serve estaciona sem socket), quando houver daemon de verdade.`

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::io::BufReader;
use tokio::sync::{Mutex, mpsc};

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
pub async fn run(profile: &str, home: Option<&Path>) -> Result<(), CoreError> {
    let mut core = Core::open_with(profile, home).await?;
    let started = core.boot().await?;
    tracing::info!(perfil = %profile, ?started, "daemon no ar");
    let pid = std::process::id();
    std::fs::write(&core.paths.pid_file, pid.to_string())
        .map_err(|e| CoreError::Io(e.to_string()))?;
    #[cfg(unix)]
    {
        let core = Arc::new(Mutex::new(core));
        let sock = core.lock().await.paths.socket.clone();
        tokio::select! {
            r = serve_loop(core.clone(), sock) => {
                r?;
                core.lock().await.shutdown().await;
            }
            _ = tokio::signal::ctrl_c() => {
                core.lock().await.shutdown().await;
            }
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
