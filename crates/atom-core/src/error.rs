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
    #[error("assinatura: {0}")]
    SignatureInvalid(String),
    #[error("armazenamento: {0}")]
    Storage(String),
    #[error("conflito de versão: {0}")]
    VersionConflict(String),
    #[error("sobrecarregado: {0}")]
    Overloaded(String),
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
            Self::SignatureInvalid(_) => atom_protocol::ErrorCode::SIGNATURE_INVALID,
            Self::Io(_) => atom_protocol::ErrorCode::STORAGE_ERROR,
            Self::Storage(_) => atom_protocol::ErrorCode::STORAGE_ERROR,
            Self::VersionConflict(_) => atom_protocol::ErrorCode::VERSION_CONFLICT,
            Self::Overloaded(_) => atom_protocol::ErrorCode::OVERLOADED,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_follow_protocol_spec() {
        use atom_protocol::ErrorCode as C;
        for (e, code) in [
            (CoreError::Package("x".into()), C::PACKAGE_ERROR),
            (CoreError::Profile("x".into()), C::INVALID_CONTEXT),
            (CoreError::NoProvider("x".into()), C::NO_PROVIDER),
            (
                CoreError::AmbiguousProvider("a".into(), "b".into()),
                C::AMBIGUOUS_PROVIDER,
            ),
            (
                CoreError::ExtensionUnavailable("x".into()),
                C::EXTENSION_UNAVAILABLE,
            ),
            (
                CoreError::SignatureInvalid("x".into()),
                C::SIGNATURE_INVALID,
            ),
            (CoreError::Storage("x".into()), C::STORAGE_ERROR),
            (CoreError::Io("x".into()), C::STORAGE_ERROR),
            (CoreError::Internal("x".into()), C::INTERNAL_ERROR),
        ] {
            assert_eq!(e.code(), code, "{e:?}");
            let rpc = e.rpc("t_1");
            assert_eq!(rpc.code, code);
            assert_eq!(rpc.data.unwrap()["trace"], serde_json::json!("t_1"));
        }
        let proto: CoreError = RpcError::new(C::CAPABILITY_DENIED, "negado").into();
        assert_eq!(proto.code(), C::CAPABILITY_DENIED);
    }

    #[test]
    fn io_converts() {
        let e: CoreError = std::io::Error::new(std::io::ErrorKind::NotFound, "sumiu").into();
        assert!(matches!(e, CoreError::Io(_)));
        assert_eq!(e.code(), atom_protocol::ErrorCode::STORAGE_ERROR);
    }
}
