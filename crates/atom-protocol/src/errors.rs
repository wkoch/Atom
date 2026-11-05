#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

/// Códigos de erro do protocolo (spec protocol §11).
pub struct ErrorCode;

impl ErrorCode {
    pub const PARSE_ERROR: i32 = -32700;
    pub const INVALID_REQUEST: i32 = -32600;
    pub const METHOD_NOT_FOUND: i32 = -32601;
    pub const INVALID_PARAMS: i32 = -32602;
    pub const INTERNAL_ERROR: i32 = -32603;
    pub const REQUEST_CANCELLED: i32 = -32800;
    pub const PROTOCOL_MISMATCH: i32 = -32001;
    pub const NOT_INITIALIZED: i32 = -32002;
    pub const CAPABILITY_DENIED: i32 = -32003;
    pub const INVALID_CONTEXT: i32 = -32004;
    pub const UNTRUSTED_PEER: i32 = -32005;
    // spec: [P-CONN-2] (v1.1, ADR-019).
    pub const EXTENSION_UNAVAILABLE: i32 = -32010;
    pub const NO_PROVIDER: i32 = -32011;
    pub const AMBIGUOUS_PROVIDER: i32 = -32012;
    pub const CONTRACT_VALIDATION_FAILED: i32 = -32013;
    pub const REJECTED: i32 = -32020;
    pub const INTERCEPTOR_TIMEOUT: i32 = -32021;
    pub const DEADLINE_EXCEEDED: i32 = -32022;
    // spec: [P-DL-3] (v1.1, ADR-021).
    pub const PACKAGE_ERROR: i32 = -32030;
    pub const SIGNATURE_INVALID: i32 = -32031;
    pub const STORAGE_ERROR: i32 = -32040;
    pub const VERSION_CONFLICT: i32 = -32041;
    // spec: [P-KV-4] (v1.1, ADR-023).
    pub const OVERLOADED: i32 = -32050;
    // spec: [P-LIM-1] (v1.1, ADR-020).
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl RpcError {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn with_trace(mut self, trace: &str) -> Self {
        let mut data = self.data.unwrap_or_else(|| serde_json::json!({}));
        if let Some(obj) = data.as_object_mut() {
            obj.insert("trace".to_string(), serde_json::json!(trace));
        }
        self.data = Some(data);
        self
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

impl std::error::Error for RpcError {}

/// Erro do núcleo com código de protocolo.
pub fn core_error(code: i32, message: impl Into<String>) -> RpcError {
    RpcError::new(code, message)
}

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("framing: {0}")]
    Framing(String),
    #[error("rpc: {0}")]
    Rpc(#[from] RpcError),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_and_display() {
        let e = RpcError::new(ErrorCode::NO_PROVIDER, "sem provedor").with_trace("t_1");
        assert_eq!(e.to_string(), "sem provedor (-32011)");
        assert_eq!(e.data.unwrap()["trace"], serde_json::json!("t_1"));
        assert_eq!(
            core_error(ErrorCode::REJECTED, "não").code,
            ErrorCode::REJECTED
        );
        let p: ProtocolError = RpcError::new(ErrorCode::REJECTED, "não").into();
        assert!(p.to_string().starts_with("rpc: "));
    }
}
