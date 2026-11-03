#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

/// Contexto autoritativo `$ctx` de toda mensagem roteada (spec protocol §3).
///
/// O núcleo sobrescreve `tenant`, `user`, `profile` e `caller` sempre;
/// `session` e `trace` são propagados quando presentes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallerCtx {
    pub tenant: String,
    pub user: String,
    pub profile: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller: Option<String>,
}

impl CallerCtx {
    pub fn local(profile: &str, caller: &str) -> Self {
        Self {
            tenant: "local".to_string(),
            user: "local".to_string(),
            profile: profile.to_string(),
            session: None,
            trace: Some(new_trace_id()),
            caller: Some(caller.to_string()),
        }
    }

    /// Aplica autoridade: troca os campos forjáveis pelos valores do núcleo.
    pub fn authorize(&mut self, profile: &str, caller: &str) {
        self.tenant = "local".to_string();
        self.user = "local".to_string();
        self.profile = profile.to_string();
        self.caller = Some(caller.to_string());
        if self.trace.is_none() {
            self.trace = Some(new_trace_id());
        }
    }
}

/// Extrai `$ctx` de `params` (objeto JSON), se presente.
pub fn ctx_from_params(params: &serde_json::Value) -> Option<CallerCtx> {
    params
        .get("$ctx")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
}

pub fn new_trace_id() -> String {
    format!("t_{}", ulid::Ulid::new())
}

/// Novo id de stream (`st_...`, spec protocol §5.3).
pub fn new_stream_id() -> String {
    format!("st_{}", ulid::Ulid::new())
}

pub fn new_session_id() -> String {
    format!("s_{}", ulid::Ulid::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorize_overrides_forged_fields() {
        let mut ctx = CallerCtx {
            tenant: "evil".to_string(),
            user: "evil".to_string(),
            profile: "evil".to_string(),
            session: Some("s_x".to_string()),
            trace: None,
            caller: Some("evil/caller".to_string()),
        };
        ctx.authorize("dev", "core");
        assert_eq!(ctx.tenant, "local");
        assert_eq!(ctx.user, "local");
        assert_eq!(ctx.profile, "dev");
        assert_eq!(ctx.caller.as_deref(), Some("core"));
        assert_eq!(ctx.session.as_deref(), Some("s_x"));
        assert!(ctx.trace.is_some());
    }
}
