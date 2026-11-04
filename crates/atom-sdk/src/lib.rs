#![forbid(unsafe_code)]
//! SDK Rust mínimo para extensões `atom/1`.
//!
//! Cobre o essencial da v0.9: handshake (`atom.initialize`), `atom.call`,
//! streams (`atom.stream.*`), interceptors (`interceptor.invoke` +
//! `atom.proceed`) e eventos (`atom.events.*`, `event.deliver`, `kind.load`).
//! Todo o resto (storage, secrets, registry) usa `Client::request` genérico.
//!
//! Uso:
//! ```ignore
//! struct Echo;
//! #[tokio::main] async fn main() { atom_sdk::run(Echo).await; }
//! ```

pub use atom_protocol::{CallerCtx, ErrorCode, InitializeResult, ProvidesEntry, RpcError};

use std::collections::HashMap;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

/// Declaração da extensão (subconjunto do manifesto, spec protocol §4).
#[derive(Debug, Clone, Default)]
pub struct Declare {
    pub provides: Vec<ProvidesEntry>,
    pub handles: Vec<String>,
    pub intercepts: Vec<atom_protocol::InterceptDecl>,
    pub subscribes: Vec<String>,
}

/// Chamada `service.invoke` recebida (spec protocol §5.2).
#[derive(Debug, Clone)]
pub struct Invoke {
    pub service: String,
    pub method: String,
    pub params: serde_json::Value,
    /// Id do stream quando o chamador pediu `"stream": true`.
    pub stream: Option<String>,
    pub ctx: CallerCtx,
}

/// Chamada `interceptor.invoke` recebida (spec protocol §6).
#[derive(Debug, Clone)]
pub struct Intercept {
    pub phase: String,
    pub service: String,
    pub method: String,
    pub params: serde_json::Value,
    pub result: Option<serde_json::Value>,
    pub error: Option<serde_json::Value>,
    /// Token p/ `atom.proceed` (só em `around`).
    pub proceed_token: Option<String>,
    pub ctx: CallerCtx,
}

/// Resposta de interceptor (spec protocol §6).
#[derive(Debug, Clone)]
pub enum InterceptAction {
    Continue,
    ModifyParams(serde_json::Value),
    ModifyResult(serde_json::Value),
    Respond(serde_json::Value),
    Reject { code: i32, message: String },
}

impl InterceptAction {
    fn into_json(self) -> serde_json::Value {
        match self {
            Self::Continue => serde_json::json!({"action": "continue"}),
            Self::ModifyParams(p) => serde_json::json!({"action": "modify", "params": p}),
            Self::ModifyResult(r) => serde_json::json!({"action": "modify", "result": r}),
            Self::Respond(r) => serde_json::json!({"action": "respond", "result": r}),
            Self::Reject { code, message } => {
                serde_json::json!({"action": "reject", "error": {"code": code, "message": message}})
            }
        }
    }
}

/// Evento entregue (`event.deliver`, spec protocol §7).
#[derive(Debug, Clone)]
pub struct Event {
    pub topic: String,
    pub data: serde_json::Value,
    pub seq: u64,
    pub ctx: CallerCtx,
}

/// Pacote declarativo p/ `kind.load` (spec protocol §8).
#[derive(Debug, Clone)]
pub struct KindPackage {
    pub manifest: serde_json::Value,
    pub dir: String,
    pub config: serde_json::Value,
}

/// Chunk de stream recebido (lado chamador).
#[derive(Debug, Clone)]
pub struct StreamChunk {
    pub stream: String,
    pub seq: u64,
    pub data: serde_json::Value,
}

/// Fim de stream recebido (lado chamador).
#[derive(Debug, Clone)]
pub struct StreamEnd {
    pub stream: String,
    pub result: Option<serde_json::Value>,
    pub error: Option<RpcError>,
}

type Pending = Arc<std::sync::Mutex<HashMap<String, oneshot::Sender<IncomingResponse>>>>;

#[derive(Debug)]
struct IncomingResponse {
    result: Option<serde_json::Value>,
    error: Option<RpcError>,
}

/// Canal de saída p/ o core (requests `atom.*` + notificações).
#[derive(Debug, Clone)]
pub struct Client {
    tx: mpsc::UnboundedSender<serde_json::Value>,
    pending: Pending,
    next: Arc<AtomicU64>,
}

impl Client {
    /// Request genérico ao core (ex.: `atom.call`, `atom.events.publish`,
    /// `atom.storage.get`, `atom.secrets.get`, `atom.registry.list`).
    /// Notificações (sem resposta) usam `notify`.
    pub async fn request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, RpcError> {
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        let id = format!("sdk-{n}");
        let (tx, rx) = oneshot::channel();
        if let Ok(mut p) = self.pending.lock() {
            p.insert(id.clone(), tx);
        }
        let msg =
            serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        if self.tx.send(msg).is_err() {
            if let Ok(mut p) = self.pending.lock() {
                p.remove(&id);
            }
            return Err(RpcError::new(ErrorCode::INTERNAL_ERROR, "core desligado"));
        }
        match rx.await {
            Ok(r) => match r.error {
                Some(e) => Err(e),
                None => Ok(r.result.unwrap_or(serde_json::Value::Null)),
            },
            Err(_) => Err(RpcError::new(
                ErrorCode::EXTENSION_UNAVAILABLE,
                "resposta perdida",
            )),
        }
    }

    /// Notificação ao core (sem `id`, sem resposta).
    pub fn notify(&self, method: &str, params: serde_json::Value) {
        let _ = self
            .tx
            .send(serde_json::json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    /// `atom.call` (spec protocol §5.1). Com `stream=true`, a resposta
    /// imediata é `{"stream": "st_..."}` e os chunks chegam em
    /// `Handler::on_stream_chunk/end`.
    pub async fn call(
        &self,
        service: &str,
        method: &str,
        params: serde_json::Value,
        target: Option<&str>,
        stream: bool,
    ) -> Result<serde_json::Value, RpcError> {
        let mut p = serde_json::json!({"service": service, "method": method, "params": params, "stream": stream});
        if let Some(t) = target {
            p["target"] = serde_json::json!(t);
        }
        self.request("atom.call", p).await
    }

    /// `atom.proceed` p/ interceptors `around` (uso único, spec §6).
    pub async fn proceed(&self, token: &str) -> Result<serde_json::Value, RpcError> {
        self.request("atom.proceed", serde_json::json!({"token": token}))
            .await
    }

    /// `atom.events.publish` (spec §7).
    pub async fn publish(
        &self,
        topic: &str,
        data: serde_json::Value,
    ) -> Result<serde_json::Value, RpcError> {
        self.request(
            "atom.events.publish",
            serde_json::json!({"topic": topic, "data": data}),
        )
        .await
    }

    /// `atom.events.subscribe` → id da assinatura.
    pub async fn subscribe(&self, topics: &[&str]) -> Result<serde_json::Value, RpcError> {
        self.request(
            "atom.events.subscribe",
            serde_json::json!({"topics": topics}),
        )
        .await
    }

    /// `atom.log` (notificação).
    pub fn log(&self, level: &str, msg: &str) {
        self.notify("atom.log", serde_json::json!({"level": level, "msg": msg}));
    }

    // -- streams (lado produtor, spec §5.3) --
    pub fn stream_chunk(&self, stream: &str, seq: u64, data: serde_json::Value) {
        self.notify(
            "atom.stream.chunk",
            serde_json::json!({"stream": stream, "seq": seq, "data": data}),
        );
    }

    pub fn stream_end(&self, stream: &str, result: serde_json::Value) {
        self.notify(
            "atom.stream.end",
            serde_json::json!({"stream": stream, "result": result}),
        );
    }

    pub fn stream_error(&self, stream: &str, error: RpcError) {
        let e = serde_json::to_value(&error).unwrap_or(serde_json::json!({}));
        self.notify(
            "atom.stream.end",
            serde_json::json!({"stream": stream, "error": e}),
        );
    }

    /// `atom.stream.cancel` (lado chamador).
    pub fn stream_cancel(&self, stream: &str) {
        self.notify("atom.stream.cancel", serde_json::json!({"stream": stream}));
    }
}

/// Lógica da extensão. Todos os métodos têm default noop exceto `declare`;
/// só implemente o que o manifesto declara.
///
/// Retornos são `impl Future + Send` (não `async fn`) para o `run` poder
/// despachar em `tokio::spawn`; na implementação continue usando `async fn`.
pub trait Handler: Send + Sync + 'static {
    fn declare(&self) -> Declare {
        Declare::default()
    }
    fn on_invoke(
        &self,
        _client: &Client,
        inv: Invoke,
    ) -> impl std::future::Future<Output = Result<serde_json::Value, RpcError>> + Send {
        async move {
            Err(RpcError::new(
                ErrorCode::METHOD_NOT_FOUND,
                format!("{}#{} desconhecido", inv.service, inv.method),
            ))
        }
    }
    fn on_intercept(
        &self,
        _client: &Client,
        _it: Intercept,
    ) -> impl std::future::Future<Output = InterceptAction> + Send {
        async move { InterceptAction::Continue }
    }
    fn on_event(
        &self,
        _client: &Client,
        _ev: Event,
    ) -> impl std::future::Future<Output = ()> + Send {
        async move {}
    }
    fn on_kind_load(
        &self,
        _client: &Client,
        _pkg: KindPackage,
    ) -> impl std::future::Future<Output = Result<(), RpcError>> + Send {
        async move { Ok(()) }
    }
    fn on_kind_unload(
        &self,
        _client: &Client,
        _package: &str,
    ) -> impl std::future::Future<Output = ()> + Send {
        async move {}
    }
    fn on_stream_chunk(
        &self,
        _client: &Client,
        _chunk: StreamChunk,
    ) -> impl std::future::Future<Output = ()> + Send {
        async move {}
    }
    fn on_stream_end(
        &self,
        _client: &Client,
        _end: StreamEnd,
    ) -> impl std::future::Future<Output = ()> + Send {
        async move {}
    }
    fn on_stream_cancel(
        &self,
        _client: &Client,
        _stream: &str,
    ) -> impl std::future::Future<Output = ()> + Send {
        async move {}
    }
    fn on_shutdown(&self, _client: &Client) -> impl std::future::Future<Output = ()> + Send {
        async move {}
    }
}

fn ok(id: &serde_json::Value, result: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn err(id: &serde_json::Value, e: &RpcError) -> serde_json::Value {
    let ev = serde_json::to_value(e).unwrap_or(serde_json::json!({}));
    serde_json::json!({"jsonrpc": "2.0", "id": id, "error": ev})
}

fn str_field(v: &serde_json::Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

fn parse_ctx(p: &serde_json::Value) -> CallerCtx {
    atom_protocol::ctx_from_params(p).unwrap_or(CallerCtx {
        tenant: "local".to_string(),
        user: "local".to_string(),
        profile: String::new(),
        session: None,
        trace: None,
        caller: None,
    })
}

/// Loop principal: handshake + dispatch NDJSON até `atom.exit`.
/// Nunca retorna em uso normal (encerra quando o stdin fecha).
pub async fn run<H: Handler>(handler: H) {
    let handler = Arc::new(handler);
    let (tx, mut rx) = mpsc::unbounded_channel::<serde_json::Value>();
    let pending: Pending = Arc::new(std::sync::Mutex::new(HashMap::new()));
    let client = Client {
        tx: tx.clone(),
        pending: pending.clone(),
        next: Arc::new(AtomicU64::new(1)),
    };

    // Escritor único (stdout NDJSON).
    tokio::spawn(async move {
        let mut out = tokio::io::stdout();
        while let Some(msg) = rx.recv().await {
            let mut s = serde_json::to_string(&msg).unwrap_or_default();
            s.push('\n');
            if out.write_all(s.as_bytes()).await.is_err() {
                break;
            }
            if out.flush().await.is_err() {
                break;
            }
        }
    });

    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let msg: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("linha inválida: {e}");
                continue;
            }
        };
        // Resposta ao nosso request (sem `method`): completa pendente.
        if msg.get("method").is_none() {
            if let Some(id) = msg.get("id").and_then(|i| i.as_str()) {
                let tx = pending.lock().ok().and_then(|mut p| p.remove(id));
                if let Some(tx) = tx {
                    let _ = tx.send(IncomingResponse {
                        result: msg.get("result").cloned(),
                        error: msg
                            .get("error")
                            .and_then(|e| serde_json::from_value(e.clone()).ok()),
                    });
                }
            }
            continue;
        }
        let method = str_field(&msg, "method");
        let id = msg.get("id").cloned();
        let params = msg
            .get("params")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let has_id = id.is_some();
        let null_id = serde_json::Value::Null;
        let id_ref = id.as_ref().unwrap_or(&null_id);
        match method.as_str() {
            "atom.initialize" => {
                let d = handler.declare();
                let provides: Vec<_> = d
                    .provides
                    .iter()
                    .map(|p| serde_json::json!({"contract": p.contract, "version": p.version}))
                    .collect();
                let intercepts: Vec<_> = d.intercepts.iter().map(|i| serde_json::json!({"match": i.match_, "phase": i.phase, "priority": i.priority})).collect();
                if has_id {
                    let _ = client.tx.send(ok(
                        id_ref,
                        serde_json::json!({
                            "protocol": "atom/1",
                            "provides": provides,
                            "handles": d.handles,
                            "intercepts": intercepts,
                            "subscribes": d.subscribes,
                        }),
                    ));
                }
            }
            "atom.shutdown" => {
                handler.on_shutdown(&client).await;
                if has_id {
                    let _ = client.tx.send(ok(id_ref, serde_json::json!({})));
                }
            }
            "atom.exit" => break,
            "service.invoke" => {
                let (h, c, tx) = (handler.clone(), client.clone(), client.tx.clone());
                let (id, params) = (id.clone(), params.clone());
                tokio::spawn(async move {
                    let inv = Invoke {
                        service: str_field(&params, "service"),
                        method: str_field(&params, "method"),
                        params: params
                            .get("params")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null),
                        stream: params
                            .get("stream")
                            .and_then(|s| s.as_str())
                            .map(|s| s.to_string()),
                        ctx: parse_ctx(&params),
                    };
                    if let Some(i) = id {
                        match h.on_invoke(&c, inv).await {
                            Ok(r) => {
                                let _ = tx.send(ok(&i, r));
                            }
                            Err(e) => {
                                let _ = tx.send(err(&i, &e));
                            }
                        }
                    }
                });
            }
            "interceptor.invoke" => {
                let (h, c, tx) = (handler.clone(), client.clone(), client.tx.clone());
                let (id, params) = (id.clone(), params.clone());
                tokio::spawn(async move {
                    let it = Intercept {
                        phase: str_field(&params, "phase"),
                        service: str_field(&params, "service"),
                        method: str_field(&params, "method"),
                        params: params
                            .get("params")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null),
                        result: params.get("result").cloned(),
                        error: params.get("error").cloned(),
                        proceed_token: params
                            .get("$proceed")
                            .and_then(|t| t.as_str())
                            .map(|s| s.to_string()),
                        ctx: parse_ctx(&params),
                    };
                    let action = h.on_intercept(&c, it).await.into_json();
                    if let Some(i) = id {
                        let _ = tx.send(ok(&i, action));
                    }
                });
            }
            "event.deliver" => {
                let (h, c) = (handler.clone(), client.clone());
                tokio::spawn(async move {
                    h.on_event(
                        &c,
                        Event {
                            topic: str_field(&params, "topic"),
                            data: params
                                .get("data")
                                .cloned()
                                .unwrap_or(serde_json::Value::Null),
                            seq: params.get("seq").and_then(|s| s.as_u64()).unwrap_or(0),
                            ctx: parse_ctx(&params),
                        },
                    )
                    .await;
                });
            }
            "kind.load" => {
                let (h, c, tx) = (handler.clone(), client.clone(), client.tx.clone());
                let (id, params) = (id.clone(), params.clone());
                tokio::spawn(async move {
                    let pkg = KindPackage {
                        manifest: params
                            .get("package")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null),
                        dir: str_field(&params, "dir"),
                        config: params
                            .get("config")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null),
                    };
                    if let Some(i) = id {
                        match h.on_kind_load(&c, pkg).await {
                            Ok(()) => {
                                let _ = tx.send(ok(&i, serde_json::json!({})));
                            }
                            Err(e) => {
                                let _ = tx.send(err(&i, &e));
                            }
                        }
                    }
                });
            }
            "kind.unload" => {
                let (h, c, tx) = (handler.clone(), client.clone(), client.tx.clone());
                let (id, params) = (id.clone(), params.clone());
                tokio::spawn(async move {
                    let name = params
                        .get("package")
                        .and_then(|p| p.as_str())
                        .unwrap_or("")
                        .to_string();
                    h.on_kind_unload(&c, &name).await;
                    if let Some(i) = id {
                        let _ = tx.send(ok(&i, serde_json::json!({})));
                    }
                });
            }
            "atom.stream.chunk" => {
                let (h, c) = (handler.clone(), client.clone());
                tokio::spawn(async move {
                    h.on_stream_chunk(
                        &c,
                        StreamChunk {
                            stream: str_field(&params, "stream"),
                            seq: params.get("seq").and_then(|s| s.as_u64()).unwrap_or(0),
                            data: params
                                .get("data")
                                .cloned()
                                .unwrap_or(serde_json::Value::Null),
                        },
                    )
                    .await;
                });
            }
            "atom.stream.end" => {
                let (h, c) = (handler.clone(), client.clone());
                tokio::spawn(async move {
                    h.on_stream_end(
                        &c,
                        StreamEnd {
                            stream: str_field(&params, "stream"),
                            result: params.get("result").cloned(),
                            error: params
                                .get("error")
                                .and_then(|e| serde_json::from_value(e.clone()).ok()),
                        },
                    )
                    .await;
                });
            }
            "atom.stream.cancel" => {
                let (h, c) = (handler.clone(), client.clone());
                tokio::spawn(async move {
                    h.on_stream_cancel(&c, &str_field(&params, "stream")).await;
                });
            }
            _ => {
                if has_id {
                    let _ = client.tx.send(err(
                        id_ref,
                        &RpcError::new(
                            ErrorCode::METHOD_NOT_FOUND,
                            format!("método {method} desconhecido"),
                        ),
                    ));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intercept_continue_json() {
        let v = InterceptAction::Continue.into_json();
        assert_eq!(v, serde_json::json!({"action": "continue"}));
    }

    #[test]
    fn intercept_reject_json() {
        let v = InterceptAction::Reject {
            code: -32020,
            message: "não".to_string(),
        }
        .into_json();
        assert_eq!(v.get("action").and_then(|a| a.as_str()), Some("reject"));
    }

    /// Núcleo falso: completa pendentes como `run()` faria.
    /// `service == "err.svc"` responde erro para testar propagação.
    fn fake_core(mut rx: mpsc::UnboundedReceiver<serde_json::Value>, pending: Pending) {
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                let key = msg
                    .get("id")
                    .and_then(|i| i.as_str())
                    .unwrap_or("")
                    .to_string();
                let svc = msg
                    .get("params")
                    .and_then(|p| p.get("service"))
                    .and_then(|s| s.as_str())
                    .unwrap_or("");
                let resp = if svc == "err.svc" {
                    IncomingResponse {
                        result: None,
                        error: Some(RpcError::new(ErrorCode::NO_PROVIDER, "sem provedor")),
                    }
                } else {
                    let echoed = msg
                        .get("params")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null);
                    IncomingResponse {
                        result: Some(serde_json::json!({ "echo": echoed })),
                        error: None,
                    }
                };
                if let Some(tx) = pending.lock().ok().and_then(|mut p| p.remove(&key)) {
                    let _ = tx.send(resp);
                }
            }
        });
    }

    fn test_client() -> (Client, Pending, mpsc::UnboundedReceiver<serde_json::Value>) {
        let (tx, rx) = mpsc::unbounded_channel::<serde_json::Value>();
        let pending: Pending = Arc::new(std::sync::Mutex::new(HashMap::new()));
        (
            Client {
                tx,
                pending: pending.clone(),
                next: Arc::new(AtomicU64::new(1)),
            },
            pending,
            rx,
        )
    }

    #[tokio::test]
    async fn client_call_ok_and_error() {
        let (c, pending, rx) = test_client();
        fake_core(rx, pending);
        let r = c
            .call("ai.tools@0", "list", serde_json::json!({}), None, false)
            .await
            .unwrap();
        assert_eq!(r["echo"]["service"], serde_json::json!("ai.tools@0"));
        let e = c
            .call("err.svc", "m", serde_json::json!({}), None, false)
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::NO_PROVIDER);
    }

    #[tokio::test]
    async fn client_concurrent_calls() {
        let (c, pending, rx) = test_client();
        fake_core(rx, pending);
        let (a, b) = tokio::join!(
            c.call("s1", "m", serde_json::json!({"n": 1}), None, false),
            c.call("s2", "m", serde_json::json!({"n": 2}), None, false)
        );
        assert_eq!(a.unwrap()["echo"]["service"], serde_json::json!("s1"));
        assert_eq!(b.unwrap()["echo"]["service"], serde_json::json!("s2"));
    }
}
