#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

use crate::ctx::CallerCtx;

/// Envelope JSON-RPC 2.0 (request ou notificação).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcRequest {
    pub jsonrpc: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<serde_json::Value>,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcResponse {
    pub jsonrpc: String,
    pub id: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<crate::errors::RpcError>,
}

impl RpcResponse {
    pub fn ok(id: serde_json::Value, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: serde_json::Value, error: crate::errors::RpcError) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(error),
        }
    }
}

/// `atom.initialize` (núcleo → extensão, spec protocol §4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeParams {
    pub protocol: String,
    pub core: CoreInfo,
    pub extension: ExtensionInfo,
    #[serde(default)]
    pub config: serde_json::Value,
    #[serde(default)]
    pub grants: Vec<String>,
    #[serde(rename = "$ctx")]
    pub ctx: CallerCtx,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreInfo {
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvidesEntry {
    pub contract: String,
    pub version: String,
}

/// Resposta de `atom.initialize` (extensão → núcleo).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InitializeResult {
    pub protocol: String,
    #[serde(default)]
    pub provides: Vec<ProvidesEntry>,
    #[serde(default)]
    pub handles: Vec<String>,
    #[serde(default)]
    pub subscribes: Vec<String>,
    /// (v1.1) `"credit"` = produtor quer `atom.stream.credit` (`[P-STREAM-3]`).
    /// Ausente = comportamento v1.0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow: Option<String>,
}

/// Controle de fluxo opt-in do consumidor (v1.1, `[P-STREAM-2]`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowOpts {
    /// Janela inicial em chunks (1..=4096).
    pub window: u32,
}

/// `atom.call` (chamador → núcleo, spec protocol §5.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtomCallParams {
    pub service: String,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub stream: bool,
    /// (v1.1) Opt-in de backpressure do consumidor (`[P-STREAM-2]`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow: Option<FlowOpts>,
    #[serde(rename = "$ctx", default, skip_serializing_if = "Option::is_none")]
    pub ctx: Option<CallerCtx>,
}

/// `service.invoke` (núcleo → provedor, spec protocol §5.2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceInvokeParams {
    pub service: String,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
    #[serde(default)]
    pub stream: Option<String>,
    #[serde(rename = "$ctx")]
    pub ctx: CallerCtx,
}

/// `event.deliver` (núcleo → assinante, spec protocol §7).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventDeliver {
    pub topic: String,
    pub data: serde_json::Value,
    pub seq: u64,
    /// (v1.1) Cursor de retomada (`[P-EV-1]`). Ausente = evento v1.0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epoch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u64>,
    #[serde(rename = "$ctx")]
    pub ctx: CallerCtx,
}

/// `interceptor.invoke` (núcleo → interceptor, spec protocol §6).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterceptorInvokeParams {
    pub phase: String,
    pub service: String,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<serde_json::Value>,
    #[serde(rename = "$proceed", default, skip_serializing_if = "Option::is_none")]
    pub proceed_token: Option<String>,
    #[serde(rename = "$ctx")]
    pub ctx: CallerCtx,
}

/// Resposta de `interceptor.invoke` (spec protocol §6).
/// Formas do fio: `{"action":"continue"}`, `{"action":"modify","params"|"result"}`,
/// `{"action":"respond","result"}`, `{"action":"reject","error"}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action")]
pub enum InterceptorResult {
    #[serde(rename = "continue")]
    Continue,
    #[serde(rename = "modify")]
    Modify {
        params: Option<serde_json::Value>,
        result: Option<serde_json::Value>,
    },
    #[serde(rename = "respond")]
    Respond { result: serde_json::Value },
    #[serde(rename = "reject")]
    Reject { error: crate::errors::RpcError },
}

/// Chunk `atom.stream.chunk` (spec protocol §5.3, `[P-STREAM-4]`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamChunk {
    pub stream: String,
    pub seq: u64,
    pub data: serde_json::Value,
}

/// Fim `atom.stream.end` (spec protocol §5.3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamEnd {
    pub stream: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<crate::errors::RpcError>,
}

/// `atom.stream.ack` cumulativo do consumidor (`[P-STREAM-2]`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamAck {
    pub stream: String,
    pub seq: u64,
}

/// `atom.stream.cancel` do consumidor (spec §5.4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamCancel {
    pub stream: String,
}

/// `atom.stream.credit` do núcleo ao produtor (`[P-STREAM-3]`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamCredit {
    pub stream: String,
    pub until: u64,
}

/// `atom.proceed` do interceptor (spec §6; token de uso único).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProceedParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(rename = "$proceed", default, skip_serializing_if = "Option::is_none")]
    pub proceed_alias: Option<String>,
}

/// `atom.events.subscribe` (spec §7; `[P-EV-3]` com `from`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventsSubscribeParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topics: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<EventsFrom>,
}

/// Cursor de retomada de `atom.events.subscribe` (`[P-EV-3]`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventsFrom {
    pub epoch: String,
    pub offset: u64,
}

/// `$/cancelRequest` estilo LSP (spec §5.4, `[P-CANCEL-1/2]`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelRequestParams {
    pub id: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_envelopes() {
        let ok = RpcResponse::ok(serde_json::json!(1), serde_json::json!({"x": 1}));
        assert_eq!(ok.jsonrpc, "2.0");
        assert!(ok.result.is_some() && ok.error.is_none());
        let err = RpcResponse::err(
            serde_json::json!(1),
            crate::errors::RpcError::new(crate::errors::ErrorCode::NO_PROVIDER, "sem provedor"),
        );
        assert!(err.result.is_none() && err.error.is_some());
    }

    #[test]
    fn call_params_defaults() {
        let p: AtomCallParams = serde_json::from_value(serde_json::json!({
            "service": "test.echo@0", "method": "echo"
        }))
        .unwrap();
        assert!(!p.stream && p.target.is_none() && p.ctx.is_none());
    }
}
