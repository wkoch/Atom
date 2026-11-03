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
    #[serde(rename = "$ctx")]
    pub ctx: CallerCtx,
}
