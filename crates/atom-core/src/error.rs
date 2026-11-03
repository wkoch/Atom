#![forbid(unsafe_code)]

use atom_protocol::RpcError;

/// Erros do núcleo, mapeados para códigos de protocolo no bus.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("pacote: {0}")]
    Package(String),
    #[error("perfil: {0}")]
    Profile(String),
    #[error("sem provedor para {0}")]
    NoProvider(String),
    #[error("provedores ambíguos para {0}: {1}")]
    AmbiguousProvider(String, String),
    #[error("extensão indisponível: {0}")]
    ExtensionUnavailable(String),
    #[error("armazenamento: {0}")]
    Storage(String),
    #[error("e/s: {0}")]
    Io(String),
    #[error("protocolo: {0}")]
    Protocol(#[from] RpcError),
    #[error("interno: {0}")]
    Internal(String),
}

impl CoreError {
    pub fn code(&self) -> i32 {
        match self {
            Self::Package(_) => atom_protocol::ErrorCode::PACKAGE_ERROR,
            Self::Profile(_) => atom_protocol::ErrorCode::INVALID_CONTEXT,
            Self::NoProvider(_) => atom_protocol::ErrorCode::NO_PROVIDER,
            Self::AmbiguousProvider(_, _) => atom_protocol::ErrorCode::AMBIGUOUS_PROVIDER,
            Self::ExtensionUnavailable(_) => atom_protocol::ErrorCode::EXTENSION_UNAVAILABLE,
            Self::Io(_) => atom_protocol::ErrorCode::STORAGE_ERROR,
            Self::Storage(_) => atom_protocol::ErrorCode::STORAGE_ERROR,
            Self::Protocol(e) => e.code,
            Self::Internal(_) => atom_protocol::ErrorCode::INTERNAL_ERROR,
        }
    }

    pub fn rpc(&self, trace: &str) -> RpcError {
        RpcError::new(self.code(), self.to_string()).with_trace(trace)
    }
}

impl From<std::io::Error> for CoreError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}
