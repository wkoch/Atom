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
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Notify, mpsc, oneshot};

/// Token de cancelamento do request em tratamento (spec protocol §5.4,
/// `[P-CANCEL-1]`). O core envia `$/cancelRequest` com o id do request; o
/// SDK dispara o token correspondente. O handler observa via
/// [`CancelToken::cancelled`] sem mudar a assinatura de `on_invoke`/`on_intercept`.
#[derive(Debug, Clone, Default)]
pub struct CancelToken {
    inner: Arc<CancelInner>,
}

#[derive(Debug, Default)]
struct CancelInner {
    fired: AtomicBool,
    notify: Notify,
}

impl CancelToken {
    /// `true` se o core já pediu cancelamento deste request.
    pub fn is_cancelled(&self) -> bool {
        self.inner.fired.load(Ordering::SeqCst)
    }
    /// Resolve quando o core pedir cancelamento deste request.
    pub async fn cancelled(&self) {
        // Checa antes para não perder um `fire` anterior ao `notified()`.
        if self.is_cancelled() {
            return;
        }
        let n = self.inner.notify.notified();
        tokio::pin!(n);
        // Corre contra `fire` entre o primeiro `is_cancelled` e o `notified()`.
        n.as_mut().enable();
        if self.is_cancelled() {
            return;
        }
        n.await;
    }
    /// Dispara o token. Idempotente.
    pub(crate) fn fire(&self) {
        self.inner.fired.store(true, Ordering::SeqCst);
        self.inner.notify.notify_waiters();
    }
}

/// Declaração da extensão (subconjunto do manifesto, spec protocol §4).
#[derive(Debug, Clone, Default)]
pub struct Declare {
    pub provides: Vec<ProvidesEntry>,
    pub handles: Vec<String>,
    pub intercepts: Vec<atom_protocol::InterceptDecl>,
    pub subscribes: Vec<String>,
    /// (v1.1) `"credit"` = produtor quer `atom.stream.credit`
    /// (`[P-STREAM-3]`); ausente = comportamento v1.0.
    pub flow: Option<String>,
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
    /// Token de cancelamento deste request (`$/cancelRequest`, `[P-CANCEL-1]`).
    pub cancel: CancelToken,
}

impl Invoke {
    /// Prazo absoluto da cadeia em epoch ms, se o core propagou
    /// `$ctx.deadline` neste request (spec §3, `[P-DL-1]`).
    pub fn deadline_ms(&self) -> Option<i64> {
        self.ctx.deadline
    }
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
    /// Token de cancelamento deste request (`$/cancelRequest`, `[P-CANCEL-1]`).
    pub cancel: CancelToken,
}

impl Intercept {
    /// Prazo absoluto da cadeia em epoch ms, se o core propagou
    /// `$ctx.deadline` neste request (spec §3, `[P-DL-1]`).
    pub fn deadline_ms(&self) -> Option<i64> {
        self.ctx.deadline
    }
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
    /// (v1.1) Instância do log de eventos do perfil (`[P-EV-1]`).
    /// Ausente = evento v1.0.
    pub epoch: Option<String>,
    /// (v1.1) Offset global ao perfil, contíguo no `epoch` (`[P-EV-1]`).
    /// Com `from`, a entrega é at-least-once: deduplique por
    /// `(epoch, offset)` (`[P-EV-4]`).
    pub offset: Option<u64>,
    pub ctx: CallerCtx,
}

/// Pacote declarativo p/ `kind.load` (spec protocol §8).
#[derive(Debug, Clone)]
pub struct KindPackage {
    pub manifest: serde_json::Value,
    pub dir: String,
    pub config: serde_json::Value,
}

/// Cursor de retomada de eventos (spec §7, `[P-EV-3]`).
#[derive(Debug, Clone)]
pub struct EventCursor {
    pub epoch: String,
    pub offset: u64,
}

/// Lacuna na entrega de eventos (`atom.events.gap`, spec §7).
///
/// O core envia antes do replay quando o `epoch` diverge ou o offset saiu
/// do ring (`[P-EV-3]`), ou quando o assinante lento estourou a fila
/// (`[P-EV-5]`). `from`/`to` nulos quando desconhecidos.
#[derive(Debug, Clone)]
pub struct EventsGap {
    pub subscription: String,
    pub epoch: String,
    pub from: Option<u64>,
    pub to: Option<u64>,
}

/// Mudança de KV entregue (`storage.changed`, spec §9.1 `[P-KV-6]`).
///
/// Quando `overflow` é `true`, mudanças foram descartadas por fila cheia e
/// o assinante deve reler o que lhe interessa.
#[derive(Debug, Clone)]
pub struct StorageChanged {
    pub subscription: String,
    pub key: Option<String>,
    pub version: Option<String>,
    pub deleted: bool,
    pub overflow: bool,
}

/// Crédito de emissão do core ao produtor em modo crédito
/// (`atom.stream.credit`, spec §5.3 `[P-STREAM-3]`).
///
/// O produtor opta respondendo `"flow": "credit"` no initialize e nunca
/// deve emitir `seq > until`.
#[derive(Debug, Clone)]
pub struct StreamCredit {
    pub stream: String,
    pub until: u64,
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

type Cancels = Arc<std::sync::Mutex<HashMap<String, CancelToken>>>;
type Pending = Arc<std::sync::Mutex<HashMap<String, oneshot::Sender<IncomingResponse>>>>;

/// Normaliza id JSON-RPC do request em tratamento para chave do mapa
/// de cancelamento (`[P-CANCEL-1]`).
fn cancel_key(id: &serde_json::Value) -> String {
    id.to_string()
}

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
    ///
    /// Propaga `$ctx.deadline` do [`CallerCtx`] em tratamento: chame a
    /// partir de `on_invoke`/`on_intercept` passando `inv.ctx`/`it.ctx`
    /// (spec §3, `[P-DL-1]`). Sem deadline no contexto, sem prazo (v1.0).
    // spec: [P-DL-1]
    pub async fn call(
        &self,
        service: &str,
        method: &str,
        params: serde_json::Value,
        target: Option<&str>,
        stream: bool,
    ) -> Result<serde_json::Value, RpcError> {
        self.call_in_ctx(service, method, params, target, stream, None)
            .await
    }

    /// `atom.call` propagando o `$ctx` do request em tratamento
    /// (spec §3, `[P-DL-1]`): repassa `deadline` quando presente, sem tocar
    /// em `tenant`/`user`/`profile`/`caller` (o core sobrescreve).
    // spec: [P-DL-1]
    pub async fn call_in_ctx(
        &self,
        service: &str,
        method: &str,
        params: serde_json::Value,
        target: Option<&str>,
        stream: bool,
        ctx: Option<&CallerCtx>,
    ) -> Result<serde_json::Value, RpcError> {
        let mut p = serde_json::json!({"service": service, "method": method, "params": params, "stream": stream});
        if let Some(t) = target {
            p["target"] = serde_json::json!(t);
        }
        if let Some(d) = ctx.and_then(|c| c.deadline) {
            p["$ctx"] = serde_json::json!({"deadline": d});
        }
        self.request("atom.call", p).await
    }

    /// `atom.call` com `$ctx.deadline` (v1.1, `[P-DL-1]`): prazo absoluto em
    /// epoch ms propagado a toda a cadeia. Sem isto, sem prazo (v1.0).
    // spec: [P-DL-1]
    pub async fn call_with_deadline(
        &self,
        service: &str,
        method: &str,
        params: serde_json::Value,
        target: Option<&str>,
        stream: bool,
        deadline_ms: i64,
    ) -> Result<serde_json::Value, RpcError> {
        let mut p = serde_json::json!({"service": service, "method": method, "params": params, "stream": stream});
        if let Some(t) = target {
            p["target"] = serde_json::json!(t);
        }
        p["$ctx"] = serde_json::json!({"deadline": deadline_ms});
        self.request("atom.call", p).await
    }

    /// `atom.call` com janela de backpressure opt-in (v1.1, `[P-STREAM-2]`):
    /// `1 <= window <= 4096`. O core só envia chunks com
    /// `seq <= último ack + window`; confirme com [`Client::stream_ack`].
    /// Propaga `deadline` de `ctx` como [`Client::call_in_ctx`].
    // spec: [P-STREAM-2]
    // spec: [P-DL-1]
    #[allow(clippy::too_many_arguments)] // espelha `call` + `window`: uma chamada, sem struct nova no fio
    pub async fn call_with_window(
        &self,
        service: &str,
        method: &str,
        params: serde_json::Value,
        target: Option<&str>,
        stream: bool,
        window: u32,
        ctx: Option<&CallerCtx>,
    ) -> Result<serde_json::Value, RpcError> {
        let mut p = serde_json::json!({"service": service, "method": method, "params": params, "stream": stream});
        if let Some(t) = target {
            p["target"] = serde_json::json!(t);
        }
        p["flow"] = serde_json::json!({"window": window});
        if let Some(d) = ctx.and_then(|c| c.deadline) {
            p["$ctx"] = serde_json::json!({"deadline": d});
        }
        self.request("atom.call", p).await
    }

    /// `atom.proceed` p/ interceptors `around` (uso único, spec §6).
    /// Propaga `deadline` de `ctx` como [`Client::call_in_ctx`]
    /// (`[P-DL-1]`).
    // spec: [P-DL-1]
    pub async fn proceed(&self, token: &str) -> Result<serde_json::Value, RpcError> {
        self.request("atom.proceed", serde_json::json!({"token": token}))
            .await
    }

    /// `atom.proceed` propagando o `$ctx.deadline` do `interceptor.invoke`
    /// em tratamento (spec §3, `[P-DL-1]`).
    // spec: [P-DL-1]
    pub async fn proceed_in_ctx(
        &self,
        token: &str,
        ctx: Option<&CallerCtx>,
    ) -> Result<serde_json::Value, RpcError> {
        let mut p = serde_json::json!({"token": token});
        if let Some(d) = ctx.and_then(|c| c.deadline) {
            p["$ctx"] = serde_json::json!({"deadline": d});
        }
        self.request("atom.proceed", p).await
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

    /// `atom.events.subscribe` com retomada (v1.1, `[P-EV-3]`): reentrega
    /// retidos com `offset > from.offset` que casam os tópicos, depois segue
    /// ao vivo. Se o `epoch` diverge ou o offset saiu do ring, o core envia
    /// antes `atom.events.gap` (`Handler::on_events_gap`). Trate gaps
    /// deduplicando por `(epoch, offset)` (`[P-EV-4]`).
    // spec: [P-EV-3]
    pub async fn subscribe_from(
        &self,
        topics: &[&str],
        from: &EventCursor,
    ) -> Result<serde_json::Value, RpcError> {
        self.request(
            "atom.events.subscribe",
            serde_json::json!({"topics": topics, "from": {"epoch": from.epoch, "offset": from.offset}}),
        )
        .await
    }

    /// `atom.events.unsubscribe` (spec §7).
    pub async fn unsubscribe(&self, topics: &[&str]) -> Result<serde_json::Value, RpcError> {
        self.request(
            "atom.events.unsubscribe",
            serde_json::json!({"topics": topics}),
        )
        .await
    }

    /// `atom.storage.watch` (v1.1, `[P-KV-6]`) → `{subscription}`.
    /// Depois, mudanças chegam em `Handler::on_storage_changed` em ordem
    /// por chave; `overflow: true` = fila cheia, releia o que interessa.
    // spec: [P-KV-6]
    pub async fn storage_watch(&self, prefix: &str) -> Result<serde_json::Value, RpcError> {
        self.request("atom.storage.watch", serde_json::json!({"prefix": prefix}))
            .await
    }

    /// `atom.storage.unwatch` (v1.1, `[P-KV-6]`).
    // spec: [P-KV-6]
    pub async fn storage_unwatch(&self, subscription: &str) -> Result<serde_json::Value, RpcError> {
        self.request(
            "atom.storage.unwatch",
            serde_json::json!({"subscription": subscription}),
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

    /// `atom.stream.ack` cumulativo (v1.1, `[P-STREAM-2]`, lado chamador):
    /// libera `seq <= ack + window` no core. Só tem efeito quando a chamada
    /// usou `flow` ([`Client::call_with_window`]); sem `flow`, ignorado.
    /// Em modo crédito, renova `until = ack + window` do produtor.
    // spec: [P-STREAM-2]
    pub fn stream_ack(&self, stream: &str, seq: u64) {
        self.notify(
            "atom.stream.ack",
            serde_json::json!({"stream": stream, "seq": seq}),
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
    /// Crédito de emissão (`atom.stream.credit`, `[P-STREAM-3]`): o core
    /// autoriza `seq <= until`. Em modo crédito (`Declare.flow = "credit"`),
    /// nunca emita acima do último `until` recebido.
    fn on_stream_credit(
        &self,
        _client: &Client,
        _credit: StreamCredit,
    ) -> impl Future<Output = ()> + Send {
        async move {}
    }
    /// Lacuna na entrega de eventos (`atom.events.gap`, `[P-EV-3]`/`[P-EV-5]`).
    fn on_events_gap(&self, _client: &Client, _gap: EventsGap) -> impl Future<Output = ()> + Send {
        async move {}
    }
    /// Mudança de KV (`storage.changed`, `[P-KV-6]`). Com `overflow: true`,
    /// releia o que interessa.
    fn on_storage_changed(
        &self,
        _client: &Client,
        _change: StorageChanged,
    ) -> impl Future<Output = ()> + Send {
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
        deadline: None,
    })
}

/// Loop principal: handshake + dispatch NDJSON até `atom.exit`.
/// Nunca retorna em uso normal (encerra quando o stdin fecha).
pub async fn run<H: Handler>(handler: H) {
    let handler = Arc::new(handler);
    let (tx, mut rx) = mpsc::unbounded_channel::<serde_json::Value>();
    let pending: Pending = Arc::new(std::sync::Mutex::new(HashMap::new()));
    let cancels: Cancels = Arc::new(std::sync::Mutex::new(HashMap::new()));
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
        if !dispatch_line(
            handler.clone(),
            client.clone(),
            pending.clone(),
            cancels.clone(),
            msg,
        )
        .await
        {
            break;
        }
    }
}
/// Retorna `false` em `atom.exit`.
async fn dispatch_line<H: Handler>(
    handler: Arc<H>,
    client: Client,
    pending: Pending,
    cancels: Cancels,
    msg: serde_json::Value,
) -> bool {
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
        return true;
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
                let mut result = serde_json::json!({
                    "protocol": "atom/1",
                    "provides": provides,
                    "handles": d.handles,
                    "intercepts": intercepts,
                    "subscribes": d.subscribes,
                });
                if let Some(flow) = d.flow {
                    result["flow"] = serde_json::json!(flow);
                }
                let _ = client.tx.send(ok(id_ref, result));
            }
        }
        "atom.shutdown" => {
            handler.on_shutdown(&client).await;
            if has_id {
                let _ = client.tx.send(ok(id_ref, serde_json::json!({})));
            }
        }
        "atom.exit" => return false,
        "service.invoke" | "interceptor.invoke" if id.is_none() => {
            // Sem id: comportamento v1.0 (`service.invoke` descartado sem
            // resposta/handler; `interceptor.invoke` executa, `atom.exit` não).
            if method.as_str() == "interceptor.invoke" {
                let (h, c) = (handler.clone(), client.clone());
                let params = params.clone();
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
                        cancel: CancelToken::default(),
                    };
                    let _ = h.on_intercept(&c, it).await;
                });
            }
        }
        "service.invoke" => {
            let (h, c, tx, cancels) = (
                handler.clone(),
                client.clone(),
                client.tx.clone(),
                cancels.clone(),
            );
            let (id, params) = (id.clone(), params.clone());
            let cancel = CancelToken::default();
            if let Some(i) = id.clone() {
                cancels
                    .lock()
                    .ok()
                    .map(|mut m| m.insert(cancel_key(&i), cancel.clone()));
            }
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
                    cancel: cancel.clone(),
                };
                if let Some(i) = id {
                    let out = match h.on_invoke(&c, inv).await {
                        Ok(r) => ok(&i, r),
                        Err(e) => err(&i, &e),
                    };
                    cancels.lock().ok().map(|mut m| m.remove(&cancel_key(&i)));
                    let _ = tx.send(out);
                }
            });
        }
        "interceptor.invoke" => {
            let (h, c, tx, cancels) = (
                handler.clone(),
                client.clone(),
                client.tx.clone(),
                cancels.clone(),
            );
            let (id, params) = (id.clone(), params.clone());
            let cancel = CancelToken::default();
            if let Some(i) = id.clone() {
                cancels
                    .lock()
                    .ok()
                    .map(|mut m| m.insert(cancel_key(&i), cancel.clone()));
            }
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
                    cancel: cancel.clone(),
                };
                let action = h.on_intercept(&c, it).await.into_json();
                if let Some(i) = id {
                    cancels.lock().ok().map(|mut m| m.remove(&cancel_key(&i)));
                    let _ = tx.send(ok(&i, action));
                }
            });
        }
        "$/cancelRequest" => {
            // `[P-CANCEL-1]`: `params.id` conhecido → dispara o token; senão ignora.
            if let Some(target) = params.get("id") {
                let key = cancel_key(target);
                if let Some(tok) = cancels.lock().ok().and_then(|m| m.get(&key).cloned()) {
                    tok.fire();
                }
            }
            if has_id {
                let _ = client.tx.send(ok(id_ref, serde_json::Value::Null));
            }
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
                        epoch: params
                            .get("epoch")
                            .and_then(|e| e.as_str())
                            .map(|s| s.to_string()),
                        offset: params.get("offset").and_then(|o| o.as_u64()),
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
        "atom.stream.credit" => {
            let (h, c) = (handler.clone(), client.clone());
            tokio::spawn(async move {
                h.on_stream_credit(
                    &c,
                    StreamCredit {
                        stream: str_field(&params, "stream"),
                        until: params.get("until").and_then(|u| u.as_u64()).unwrap_or(0),
                    },
                )
                .await;
            });
        }
        "atom.events.gap" => {
            let (h, c) = (handler.clone(), client.clone());
            tokio::spawn(async move {
                h.on_events_gap(
                    &c,
                    EventsGap {
                        subscription: str_field(&params, "subscription"),
                        epoch: str_field(&params, "epoch"),
                        from: params.get("from").and_then(|f| f.as_u64()),
                        to: params.get("to").and_then(|t| t.as_u64()),
                    },
                )
                .await;
            });
        }
        "storage.changed" => {
            let (h, c) = (handler.clone(), client.clone());
            tokio::spawn(async move {
                h.on_storage_changed(
                    &c,
                    StorageChanged {
                        subscription: str_field(&params, "subscription"),
                        key: params
                            .get("key")
                            .and_then(|k| k.as_str())
                            .map(|s| s.to_string()),
                        version: params
                            .get("version")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string()),
                        deleted: params
                            .get("deleted")
                            .and_then(|d| d.as_bool())
                            .unwrap_or(false),
                        overflow: params
                            .get("overflow")
                            .and_then(|o| o.as_bool())
                            .unwrap_or(false),
                    },
                )
                .await;
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
    true
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

    fn test_cancels() -> Cancels {
        Arc::new(std::sync::Mutex::new(HashMap::new()))
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

    #[tokio::test]
    async fn client_notifications_shape() {
        let (c, _pending, mut rx) = test_client();
        c.log("info", "oi");
        c.stream_chunk("st_1", 0, serde_json::json!({"t": "x"}));
        c.stream_end("st_1", serde_json::json!({"ok": true}));
        c.stream_error("st_2", RpcError::new(ErrorCode::INTERNAL_ERROR, "quebrou"));
        c.stream_cancel("st_3");
        // Notificações não têm `id`.
        for method in [
            "atom.log",
            "atom.stream.chunk",
            "atom.stream.end",
            "atom.stream.end",
            "atom.stream.cancel",
        ] {
            let m = rx.recv().await.unwrap();
            assert_eq!(m["method"], serde_json::json!(method));
            assert!(m.get("id").is_none());
        }
        let (c2, _p2, mut rx2) = test_client();
        c2.notify("atom.custom", serde_json::json!({}));
        assert_eq!(
            rx2.recv().await.unwrap()["method"],
            serde_json::json!("atom.custom")
        );
    }

    #[tokio::test]
    async fn client_request_helpers() {
        let (c, pending, rx) = test_client();
        fake_core(rx, pending);
        let r = c.proceed("tok-1").await.unwrap();
        assert_eq!(r["echo"]["token"], serde_json::json!("tok-1"));
        let r = c.publish("a.b", serde_json::json!({"n": 1})).await.unwrap();
        assert_eq!(r["echo"]["topic"], serde_json::json!("a.b"));
        let r = c.subscribe(&["a.*"]).await.unwrap();
        assert_eq!(r["echo"]["topics"], serde_json::json!(["a.*"]));
    }

    #[tokio::test]
    async fn client_request_core_down() {
        let (c, _pending, rx) = test_client();
        drop(rx); // ninguém lê: canal fechado
        let e = c
            .call("s", "m", serde_json::json!({}), None, false)
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::INTERNAL_ERROR);
    }

    #[test]
    fn intercept_all_actions_json() {
        assert_eq!(
            InterceptAction::ModifyParams(serde_json::json!({"x": 1})).into_json(),
            serde_json::json!({"action": "modify", "params": {"x": 1}})
        );
        assert_eq!(
            InterceptAction::ModifyResult(serde_json::json!(1)).into_json()["action"],
            serde_json::json!("modify")
        );
        assert_eq!(
            InterceptAction::Respond(serde_json::json!({"ok": true})).into_json()["action"],
            serde_json::json!("respond")
        );
    }

    /// Handler que grava chamadas p/ `dispatch_line`.
    #[derive(Default)]
    struct Rec {
        calls: std::sync::Mutex<Vec<String>>,
    }

    impl Rec {
        fn said(&self, s: &str) {
            self.calls.lock().unwrap().push(s.to_string());
        }
        fn has(&self, s: &str) -> bool {
            self.calls.lock().unwrap().iter().any(|c| c == s)
        }
    }

    impl Handler for Rec {
        fn declare(&self) -> Declare {
            Declare {
                provides: vec![ProvidesEntry {
                    contract: "t@0".to_string(),
                    version: "0.1".to_string(),
                }],
                handles: vec!["k".to_string()],
                intercepts: vec![atom_protocol::InterceptDecl {
                    match_: "t@0/*".to_string(),
                    phase: "before".to_string(),
                    priority: 1,
                }],
                subscribes: vec!["a.*".to_string()],
                flow: None,
            }
        }
        fn on_invoke(
            &self,
            _client: &Client,
            inv: Invoke,
        ) -> impl std::future::Future<Output = Result<serde_json::Value, RpcError>> + Send {
            let m = inv.method.clone();
            self.said(&format!("invoke:{m}"));
            async move {
                if m == "boom" {
                    Err(RpcError::new(ErrorCode::INTERNAL_ERROR, "bum"))
                } else {
                    Ok(serde_json::json!({"ok": m}))
                }
            }
        }
        fn on_intercept(
            &self,
            _client: &Client,
            _it: Intercept,
        ) -> impl std::future::Future<Output = InterceptAction> + Send {
            self.said("intercept");
            async move { InterceptAction::ModifyResult(serde_json::json!(1)) }
        }
        fn on_event(
            &self,
            _client: &Client,
            _ev: Event,
        ) -> impl std::future::Future<Output = ()> + Send {
            self.said("event");
            async move {}
        }
        fn on_kind_load(
            &self,
            _client: &Client,
            _pkg: KindPackage,
        ) -> impl std::future::Future<Output = Result<(), RpcError>> + Send {
            self.said("kind_load");
            async move { Ok(()) }
        }
        fn on_kind_unload(
            &self,
            _client: &Client,
            _package: &str,
        ) -> impl std::future::Future<Output = ()> + Send {
            self.said("kind_unload");
            async move {}
        }
        fn on_stream_chunk(
            &self,
            _client: &Client,
            _chunk: StreamChunk,
        ) -> impl std::future::Future<Output = ()> + Send {
            self.said("chunk");
            async move {}
        }
        fn on_stream_end(
            &self,
            _client: &Client,
            _end: StreamEnd,
        ) -> impl std::future::Future<Output = ()> + Send {
            self.said("end");
            async move {}
        }
        fn on_stream_cancel(
            &self,
            _client: &Client,
            _stream: &str,
        ) -> impl Future<Output = ()> + Send {
            self.said("cancel");
            async move {}
        }
        fn on_stream_credit(
            &self,
            _client: &Client,
            _credit: StreamCredit,
        ) -> impl Future<Output = ()> + Send {
            self.said("credit");
            async move {}
        }
        fn on_events_gap(
            &self,
            _client: &Client,
            _gap: EventsGap,
        ) -> impl Future<Output = ()> + Send {
            self.said("gap");
            async move {}
        }
        fn on_storage_changed(
            &self,
            _client: &Client,
            _change: StorageChanged,
        ) -> impl Future<Output = ()> + Send {
            self.said("changed");
            async move {}
        }
        fn on_shutdown(&self, _client: &Client) -> impl Future<Output = ()> + Send {
            self.said("shutdown");
            async move {}
        }
    }

    async fn recv_timeout(
        rx: &mut mpsc::UnboundedReceiver<serde_json::Value>,
    ) -> serde_json::Value {
        tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("resposta do dispatch")
            .expect("canal aberto")
    }

    async fn wait_for(rec: &Rec, what: &str) {
        for _ in 0..100 {
            if rec.has(what) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("handler não chamou {what}");
    }

    #[tokio::test]
    async fn dispatch_initialize_shutdown_exit() {
        let h = Arc::new(Rec::default());
        let (c, pending, mut rx) = test_client();
        let cancels = test_cancels();
        let init = serde_json::json!({"jsonrpc": "2.0", "id": "1", "method": "atom.initialize", "params": {}});
        assert!(dispatch_line(h.clone(), c.clone(), pending.clone(), cancels.clone(), init).await);
        let r = recv_timeout(&mut rx).await;
        assert_eq!(r["result"]["protocol"], serde_json::json!("atom/1"));
        assert_eq!(
            r["result"]["provides"][0]["contract"],
            serde_json::json!("t@0")
        );
        // Sem id: sem resposta.
        let n = serde_json::json!({"jsonrpc": "2.0", "method": "atom.initialize", "params": {}});
        assert!(dispatch_line(h.clone(), c.clone(), pending.clone(), cancels.clone(), n).await);
        // shutdown + exit.
        let s = serde_json::json!({"jsonrpc": "2.0", "id": "2", "method": "atom.shutdown", "params": {}});
        assert!(dispatch_line(h.clone(), c.clone(), pending.clone(), cancels.clone(), s).await);
        wait_for(&h, "shutdown").await;
        assert_eq!(recv_timeout(&mut rx).await["id"], serde_json::json!("2"));
        assert!(
            !dispatch_line(
                h.clone(),
                c.clone(),
                pending.clone(),
                cancels.clone(),
                serde_json::json!({"method": "atom.exit"})
            )
            .await
        );
    }

    #[tokio::test]
    async fn dispatch_invoke_variants() {
        let h = Arc::new(Rec::default());
        let (c, pending, mut rx) = test_client();
        let cancels = test_cancels();
        let inv = |id: &str, method: &str| {
            serde_json::json!({"jsonrpc": "2.0", "id": id, "method": "service.invoke",
                "params": {"service": "t@0", "method": method, "params": {}}})
        };
        dispatch_line(
            h.clone(),
            c.clone(),
            pending.clone(),
            cancels.clone(),
            inv("a", "hi"),
        )
        .await;
        wait_for(&h, "invoke:hi").await;
        assert_eq!(
            recv_timeout(&mut rx).await["result"],
            serde_json::json!({"ok": "hi"})
        );
        dispatch_line(
            h.clone(),
            c.clone(),
            pending.clone(),
            cancels.clone(),
            inv("b", "boom"),
        )
        .await;
        wait_for(&h, "invoke:boom").await;
        assert!(recv_timeout(&mut rx).await.get("error").is_some());
        // Sem id: executa sem responder.
        // Sem id: descartado sem responder e sem chamar o handler.
    }

    #[tokio::test]
    async fn dispatch_events_kinds_streams() {
        let h = Arc::new(Rec::default());
        let cancels = test_cancels();
        let (c, pending, mut rx) = test_client();
        let fire = |method: &str, params: serde_json::Value| serde_json::json!({"jsonrpc": "2.0", "method": method, "params": params});
        dispatch_line(
            h.clone(),
            c.clone(),
            pending.clone(),
            cancels.clone(),
            fire(
                "interceptor.invoke",
                serde_json::json!({"id": "i", "phase": "before"}),
            ),
        )
        .await;
        // interceptor.invoke sem id: sem resposta, mas handler roda.
        wait_for(&h, "intercept").await;
        // invoke/kind sem id são descartados (sem resposta e sem handler).
        for (m, p, want) in [
            (
                "event.deliver",
                serde_json::json!({"topic": "a.b", "data": {}, "seq": 1}),
                "event",
            ),
            (
                "atom.stream.chunk",
                serde_json::json!({"stream": "s", "seq": 1}),
                "chunk",
            ),
            ("atom.stream.end", serde_json::json!({"stream": "s"}), "end"),
            (
                "atom.stream.cancel",
                serde_json::json!({"stream": "s"}),
                "cancel",
            ),
        ] {
            dispatch_line(
                h.clone(),
                c.clone(),
                pending.clone(),
                cancels.clone(),
                fire(m, p),
            )
            .await;
            wait_for(&h, want).await;
        }
        // kind.load com id responde.
        dispatch_line(
            h.clone(),
            c.clone(),
            pending.clone(),
            cancels.clone(),
            serde_json::json!({"jsonrpc": "2.0", "id": "k", "method": "kind.load", "params": {}}),
        )
        .await;
        assert_eq!(recv_timeout(&mut rx).await["id"], serde_json::json!("k"));
        // kind.unload com id responde.
        dispatch_line(
            h.clone(),
            c.clone(),
            pending.clone(),
            cancels.clone(),
            serde_json::json!({"jsonrpc": "2.0", "id": "u", "method": "kind.unload", "params": {}}),
        )
        .await;
        assert_eq!(recv_timeout(&mut rx).await["id"], serde_json::json!("u"));
        // Desconhecido com id → erro; sem id → silêncio.
        dispatch_line(
            h.clone(),
            c.clone(),
            pending.clone(),
            cancels.clone(),
            serde_json::json!({"jsonrpc": "2.0", "id": "z", "method": "x.y", "params": {}}),
        )
        .await;
        let r = recv_timeout(&mut rx).await;
        assert_eq!(r["error"]["code"], serde_json::json!(-32601));
        dispatch_line(
            h.clone(),
            c.clone(),
            pending.clone(),
            cancels.clone(),
            serde_json::json!({"method": "x.y", "params": {}}),
        )
        .await;
    }

    #[tokio::test]
    async fn dispatch_completes_pending_request() {
        let h = Arc::new(Rec::default());
        let (c, pending, _rx) = test_client();
        let cancels = test_cancels();
        let (tx, rx) = oneshot::channel();
        pending.lock().unwrap().insert("sdk-9".to_string(), tx);
        dispatch_line(
            h.clone(),
            c.clone(),
            pending.clone(),
            cancels.clone(),
            serde_json::json!({"jsonrpc": "2.0", "id": "sdk-9", "result": {"v": 1}}),
        )
        .await;
        let r = rx.await.unwrap();
        assert_eq!(r.result, Some(serde_json::json!({"v": 1})));
        // Resposta sem pendente: ignora.
        dispatch_line(
            h.clone(),
            c.clone(),
            pending.clone(),
            cancels.clone(),
            serde_json::json!({"jsonrpc": "2.0", "id": "sdk-0", "result": {}}),
        )
        .await;
    }

    /// Deadline/flow do chamador: `call_in_ctx`/`call_with_window` copiam
    /// `deadline` do ctx em tratamento; `call` simples não envia `$ctx`;
    /// `call_with_window` envia `flow.window` (round-trip dos schemas
    /// `atom.call.json`/`ctx.json`, cláusulas `[P-DL-1]`/`[P-STREAM-2]`).
    // spec: [P-DL-1]
    // spec: [P-STREAM-2]
    #[tokio::test]
    async fn client_deadline_and_window_round_trip() {
        let (c, pending, rx) = test_client();
        fake_core(rx, pending);
        let r = c
            .call("s", "m", serde_json::json!({}), None, false)
            .await
            .unwrap();
        assert!(r["echo"].get("$ctx").is_none());
        let ctx = CallerCtx {
            tenant: "local".to_string(),
            user: "local".to_string(),
            profile: "dev".to_string(),
            session: None,
            trace: None,
            caller: None,
            deadline: Some(1_700_000_000_000),
        };
        let r = c
            .call_in_ctx("s", "m", serde_json::json!({}), None, false, Some(&ctx))
            .await
            .unwrap();
        assert_eq!(
            r["echo"]["$ctx"],
            serde_json::json!({"deadline": 1_700_000_000_000_i64})
        );
        let atom_call: atom_protocol::AtomCallParams = serde_json::from_value(serde_json::json!({
            "service": "ai.llm@0", "method": "complete",
            "params": {"messages": [{"role": "user"}]},
            "$ctx": {"tenant": "local", "user": "local", "profile": "dev",
                     "session": "s_1", "trace": "t_1", "deadline": 1_700_000_000_000_i64},
        }))
        .unwrap();
        assert_eq!(
            atom_call.ctx.as_ref().and_then(|x| x.deadline),
            Some(1_700_000_000_000)
        );
        let r = c
            .call_with_window("s", "m", serde_json::json!({}), None, true, 16, Some(&ctx))
            .await
            .unwrap();
        assert_eq!(r["echo"]["flow"], serde_json::json!({"window": 16}));
        assert_eq!(
            r["echo"]["$ctx"],
            serde_json::json!({"deadline": 1_700_000_000_000_i64})
        );
        let (c2, _p2, mut rx2) = test_client();
        c2.stream_ack("st_1", 3);
        assert_eq!(
            rx2.recv().await.unwrap()["params"],
            serde_json::json!({"stream": "st_1", "seq": 3})
        );
        let r = c.proceed_in_ctx("tok-1", Some(&ctx)).await.unwrap();
        assert_eq!(
            r["echo"]["$ctx"],
            serde_json::json!({"deadline": 1_700_000_000_000_i64})
        );
        let inv = Invoke {
            service: "s".to_string(),
            method: "m".to_string(),
            params: serde_json::json!({}),
            stream: None,
            ctx: ctx.clone(),
            cancel: CancelToken::default(),
        };
        assert_eq!(inv.deadline_ms(), Some(1_700_000_000_000));
        let it = Intercept {
            phase: "around".to_string(),
            service: "s".to_string(),
            method: "m".to_string(),
            params: serde_json::json!({}),
            result: None,
            error: None,
            proceed_token: Some("tok-1".to_string()),
            ctx,
            cancel: CancelToken::default(),
        };
        assert_eq!(it.deadline_ms(), Some(1_700_000_000_000));
    }

    /// Produtor em modo crédito: `initialize` responde `"flow": "credit"` e
    /// `atom.stream.credit` chega em `on_stream_credit` sem emitir acima de
    /// `until` (round-trip de `atom.stream.credit.json`, `[P-STREAM-3]`).
    // spec: [P-STREAM-3]
    #[tokio::test]
    async fn dispatch_credit_flow_and_gap_storage() {
        struct Credit;
        impl Handler for Credit {
            fn declare(&self) -> Declare {
                Declare {
                    flow: Some("credit".to_string()),
                    ..Default::default()
                }
            }
            fn on_stream_credit(
                &self,
                client: &Client,
                credit: StreamCredit,
            ) -> impl Future<Output = ()> + Send {
                assert_eq!(credit.until, 16);
                // Nunca emite seq > until: respeita o teto do último crédito.
                let seq = credit.until.min(16);
                client.stream_chunk(&credit.stream, seq, serde_json::json!({}));
                async move {}
            }
        }
        let h = Arc::new(Credit);
        let (c, pending, mut rx) = test_client();
        let cancels = test_cancels();
        let init = serde_json::json!({"jsonrpc": "2.0", "id": "1", "method": "atom.initialize", "params": {}});
        assert!(dispatch_line(h.clone(), c.clone(), pending.clone(), cancels.clone(), init).await);
        assert_eq!(
            recv_timeout(&mut rx).await["result"]["flow"],
            serde_json::json!("credit")
        );
        let credit: atom_protocol::StreamCredit = serde_json::from_value(serde_json::json!({
            "stream": "st_01J9ZQ7NA1MRAP0Y2C8Q3W5E6", "until": 16,
        }))
        .unwrap();
        dispatch_line(
            h.clone(),
            c.clone(),
            pending.clone(),
            cancels.clone(),
            serde_json::json!({"jsonrpc": "2.0", "method": "atom.stream.credit", "params": serde_json::to_value(&credit).unwrap()}),
        )
        .await;
        let chunk = recv_timeout(&mut rx).await;
        assert_eq!(chunk["method"], serde_json::json!("atom.stream.chunk"));
        assert!(chunk["params"]["seq"].as_u64().unwrap() <= 16);
    }

    /// Retomada de eventos + KV watch: `subscribe_from` envia
    /// `from {epoch, offset}`, `atom.events.gap` chega em `on_events_gap`
    /// (dedução por `(epoch, offset)`) e `storage.watch`/`storage.changed`
    /// cobre set/delete/overflow (round-trip de
    /// `atom.events.subscribe.json`/`event.deliver.json`, `[P-EV-3]`/`[P-KV-6]`).
    // spec: [P-EV-3]
    // spec: [P-KV-6]
    #[tokio::test]
    async fn dispatch_subscribe_from_and_storage_watch() {
        let _h = Arc::new(Rec::default());
        let (c, pending, rx) = test_client();
        fake_core(rx, pending);
        let r = c
            .subscribe_from(
                &["atom.test"],
                &EventCursor {
                    epoch: "ep_1".to_string(),
                    offset: 41,
                },
            )
            .await
            .unwrap();
        let params: atom_protocol::EventsSubscribeParams =
            serde_json::from_value(r["echo"].clone()).unwrap();
        let from = params.from.unwrap();
        assert_eq!((from.epoch.as_str(), from.offset), ("ep_1", 41));
        let r = c.storage_watch("a/").await.unwrap();
        assert_eq!(r["echo"]["prefix"], serde_json::json!("a/"));
        let r = c.storage_unwatch("w_1").await.unwrap();
        assert_eq!(r["echo"]["subscription"], serde_json::json!("w_1"));
        let deliver: atom_protocol::EventDeliver = serde_json::from_value(serde_json::json!({
            "topic": "atom.test", "data": {},
            "seq": 1, "epoch": "ep_1", "offset": 42,
            "$ctx": {"tenant": "local", "user": "local", "profile": "conf",
                     "session": "s_1", "trace": "t_1", "caller": "core"},
        }))
        .unwrap();
        let back = serde_json::to_value(&deliver).unwrap();
        assert_eq!(back["offset"], serde_json::json!(42));
        let seen = [(deliver.epoch.clone().unwrap(), deliver.offset.unwrap())];
        assert!(!seen.contains(&("ep_1".to_string(), 41)));
        let h2 = Arc::new(Rec::default());
        let (c2, pending2, _rx2) = test_client();
        let cancels2 = test_cancels();
        let fire = |method: &str, params: serde_json::Value| serde_json::json!({"jsonrpc": "2.0", "method": method, "params": params});
        dispatch_line(
            h2.clone(),
            c2.clone(),
            pending2.clone(),
            cancels2.clone(),
            fire(
                "atom.events.gap",
                serde_json::json!({"subscription": "sub_1", "epoch": "ep_1", "from": 41, "to": 42}),
            ),
        )
        .await;
        wait_for(&h2, "gap").await;
        for (m, p, want) in [
            (
                "storage.changed",
                serde_json::json!({"subscription": "w_1", "key": "a/k", "version": "v_1"}),
                "changed",
            ),
            (
                "storage.changed",
                serde_json::json!({"subscription": "w_1", "key": "a/k", "deleted": true}),
                "changed",
            ),
            (
                "storage.changed",
                serde_json::json!({"subscription": "w_1", "overflow": true}),
                "changed",
            ),
        ] {
            let h3 = Arc::new(Rec::default());
            let (c3, pending3, _rx3) = test_client();
            dispatch_line(
                h3.clone(),
                c3.clone(),
                pending3.clone(),
                test_cancels(),
                fire(m, p),
            )
            .await;
            wait_for(&h3, want).await;
        }
    }

    /// Cancelamento de request em tratamento (spec protocol §5.4 `[P-CANCEL-1]`
    /// + `[P-CANCEL-2]` lado SDK): `service.invoke`/`interceptor.invoke` com id
    /// registram token; `$/cancelRequest` com `params.id` conhecido dispara o
    /// token observado pelo handler; id desconhecido é ignorado; `$/cancelRequest`
    /// como request-com-id responde `null`.
    // spec: [P-CANCEL-1]
    // spec: [P-CANCEL-2]
    #[tokio::test]
    async fn cancel_fires_token() {
        use std::sync::atomic::{AtomicBool, Ordering};
        struct Slow {
            saw: Arc<AtomicBool>,
            it_saw: Arc<AtomicBool>,
        }
        impl Handler for Slow {
            fn declare(&self) -> Declare {
                Declare::default()
            }
            fn on_invoke(
                &self,
                _client: &Client,
                inv: Invoke,
            ) -> impl std::future::Future<Output = Result<serde_json::Value, RpcError>> + Send
            {
                let saw = self.saw.clone();
                let cancel = inv.cancel.clone();
                async move {
                    // Prova que o handler observou o cancel disparado (<2s, sem
                    // depender da cortesia do core): espera o token, não um sleep.
                    tokio::time::timeout(std::time::Duration::from_secs(2), cancel.cancelled())
                        .await
                        .expect("cancel não disparou em 2s");
                    assert!(cancel.is_cancelled());
                    saw.store(true, Ordering::SeqCst);
                    Ok(serde_json::json!({"ok": true}))
                }
            }
            fn on_intercept(
                &self,
                _client: &Client,
                it: Intercept,
            ) -> impl std::future::Future<Output = InterceptAction> + Send {
                let saw = self.it_saw.clone();
                let cancel = it.cancel.clone();
                async move {
                    tokio::time::timeout(std::time::Duration::from_secs(2), cancel.cancelled())
                        .await
                        .expect("cancel do intercept não disparou em 2s");
                    assert!(cancel.is_cancelled());
                    saw.store(true, Ordering::SeqCst);
                    InterceptAction::Continue
                }
            }
        }
        let h = Arc::new(Slow {
            saw: Arc::new(AtomicBool::new(false)),
            it_saw: Arc::new(AtomicBool::new(false)),
        });
        let (c, pending, mut rx) = test_client();
        let cancels = test_cancels();
        // `service.invoke` com id numérico: token registrado sob `to_string()`.
        dispatch_line(
            h.clone(),
            c.clone(),
            pending.clone(),
            cancels.clone(),
            serde_json::json!({"jsonrpc": "2.0", "id": 7, "method": "service.invoke",
                "params": {"service": "t@0", "method": "slow", "params": {}}}),
        )
        .await;
        // Dá um giro para o spawn registrar o token antes do cancel.
        for _ in 0..100 {
            if cancels
                .lock()
                .ok()
                .map(|m| m.contains_key("7"))
                .unwrap_or(false)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(cancels.lock().unwrap().contains_key("7"));
        // `$/cancelRequest` como notificação dispara o token.
        dispatch_line(
            h.clone(),
            c.clone(),
            pending.clone(),
            cancels.clone(),
            serde_json::json!({"jsonrpc": "2.0", "method": "$/cancelRequest", "params": {"id": 7}}),
        )
        .await;
        let out = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("resposta do invoke")
            .expect("canal aberto");
        assert_eq!(out["id"], serde_json::json!(7));
        assert!(h.saw.load(Ordering::SeqCst));
        // Token removido ao concluir.
        assert!(!cancels.lock().unwrap().contains_key("7"));
        // Variante interceptor.invoke (id string).
        dispatch_line(
            h.clone(),
            c.clone(),
            pending.clone(),
            cancels.clone(),
            serde_json::json!({"jsonrpc": "2.0", "id": "it-1", "method": "interceptor.invoke",
                "params": {"phase": "around", "service": "t@0", "method": "m"}}),
        )
        .await;
        for _ in 0..100 {
            if cancels
                .lock()
                .ok()
                .map(|m| m.contains_key("\"it-1\""))
                .unwrap_or(false)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        dispatch_line(
            h.clone(),
            c.clone(),
            pending.clone(),
            cancels.clone(),
            serde_json::json!({"jsonrpc": "2.0", "method": "$/cancelRequest", "params": {"id": "it-1"}}),
        )
        .await;
        let out = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("resposta do intercept")
            .expect("canal aberto");
        assert_eq!(out["id"], serde_json::json!("it-1"));
        assert!(h.it_saw.load(Ordering::SeqCst));
        // Id desconhecido: ignorado, sem resposta.
        dispatch_line(
            h.clone(),
            c.clone(),
            pending.clone(),
            cancels.clone(),
            serde_json::json!({"jsonrpc": "2.0", "method": "$/cancelRequest", "params": {"id": "nope"}}),
        )
        .await;
        // `$/cancelRequest` como request-com-id: responde `null`, mesmo com id desconhecido.
        dispatch_line(
            h.clone(),
            c.clone(),
            pending.clone(),
            cancels.clone(),
            serde_json::json!({"jsonrpc": "2.0", "id": "c1", "method": "$/cancelRequest", "params": {"id": "nope"}}),
        )
        .await;
        let out = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("resposta do cancel-request")
            .expect("canal aberto");
        assert_eq!(out["id"], serde_json::json!("c1"));
        assert_eq!(out["result"], serde_json::Value::Null);
    }
}
