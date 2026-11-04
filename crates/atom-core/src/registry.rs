#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::error::CoreError;

/// Registro de providers, bindings, interceptors, assinaturas e kind handlers.
#[derive(Debug, Default)]
pub struct Registry {
    /// contrato@major -> provedores (nomes `publisher/nome`)
    providers: HashMap<String, Vec<String>>,
    bindings: HashMap<String, String>,
    intercepts: Vec<InterceptRule>,
    subscribes: Vec<SubscribeRule>,
    /// kind -> handlers (nomes `publisher/nome`, ordem de instalação).
    handlers: HashMap<String, Vec<String>>,
}

/// Regra de intercept (`[[intercepts]]` do manifesto): glob sobre `<contrato>/<método>`.
#[derive(Debug, Clone)]
pub struct InterceptRule {
    pub provider: String,
    pub glob: globset::Glob,
    pub phase: String,
    pub priority: i32,
}

/// Assinatura declarada (`subscribes` do manifesto ou `atom.register`).
#[derive(Debug, Clone)]
pub struct SubscribeRule {
    pub subscriber: String,
    pub glob: globset::Glob,
}

impl Registry {
    pub fn new(bindings: HashMap<String, String>) -> Self {
        Self {
            providers: HashMap::new(),
            bindings,
            intercepts: vec![],
            subscribes: vec![],
            handlers: HashMap::new(),
        }
    }

    pub fn add_provider(&mut self, contract: &str, provider: &str) {
        let list = self.providers.entry(contract.to_string()).or_default();
        if !list.iter().any(|p| p == provider) {
            list.push(provider.to_string());
        }
    }

    pub fn providers_of(&self, contract: &str) -> Vec<String> {
        self.providers.get(contract).cloned().unwrap_or_default()
    }

    pub fn set_binding(&mut self, contract: &str, provider: &str) {
        self.bindings
            .insert(contract.to_string(), provider.to_string());
    }

    pub fn remove_binding(&mut self, contract: &str) {
        self.bindings.remove(contract);
    }

    pub fn bindings(&self) -> &HashMap<String, String> {
        &self.bindings
    }

    pub fn add_intercept(
        &mut self,
        provider: &str,
        match_: &str,
        phase: &str,
        priority: i32,
    ) -> Result<(), String> {
        let glob = globset::Glob::new(match_).map_err(|e| format!("intercept {match_:?}: {e}"))?;
        if !matches!(phase, "before" | "after" | "around") {
            return Err(format!("phase inválida: {phase:?}"));
        }
        self.intercepts.push(InterceptRule {
            provider: provider.to_string(),
            glob,
            phase: phase.to_string(),
            priority,
        });
        Ok(())
    }

    /// Interceptors de uma fase, por prioridade crescente. Casa glob sobre
    /// `"<contrato>/<método>"`; `after` o chamador inverte a ordem.
    pub fn intercepts_for(&self, service: &str, method: &str, phase: &str) -> Vec<(String, i32)> {
        let target = format!("{service}/{method}");
        let mut out: Vec<(String, i32)> = self
            .intercepts
            .iter()
            .filter(|r| r.phase == phase && r.glob.compile_matcher().is_match(&target))
            .map(|r| (r.provider.clone(), r.priority))
            .collect();
        out.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        out
    }

    pub fn add_subscription(&mut self, subscriber: &str, topic: &str) -> Result<(), String> {
        let glob = globset::Glob::new(topic).map_err(|e| format!("subscribe {topic:?}: {e}"))?;
        if !self
            .subscribes
            .iter()
            .any(|s| s.subscriber == subscriber && s.glob.glob() == topic)
        {
            self.subscribes.push(SubscribeRule {
                subscriber: subscriber.to_string(),
                glob,
            });
        }
        Ok(())
    }

    /// Remove assinatura exata (para `atom.events.unsubscribe`).
    pub fn remove_subscription(&mut self, subscriber: &str, topic: &str) {
        self.subscribes
            .retain(|s| !(s.subscriber == subscriber && s.glob.glob() == topic));
    }

    /// Remove todos os intercepts de um provedor (para `atom.register` reverso).
    pub fn remove_intercepts_of(&mut self, provider: &str) {
        self.intercepts.retain(|r| r.provider != provider);
    }

    /// Registra handler de kind (`handles` do manifesto).
    pub fn add_handler(&mut self, kind: &str, provider: &str) {
        let list = self.handlers.entry(kind.to_string()).or_default();
        if !list.iter().any(|p| p == provider) {
            list.push(provider.to_string());
        }
    }

    /// Handlers de um kind (ordem de instalação).
    pub fn handlers_of(&self, kind: &str) -> Vec<String> {
        self.handlers.get(kind).cloned().unwrap_or_default()
    }

    /// Remove todos os handlers/intercepts de um provedor (desinstalação).
    pub fn remove_provider_all(&mut self, provider: &str) {
        for list in self.providers.values_mut() {
            list.retain(|p| p != provider);
        }
        self.remove_intercepts_of(provider);
        self.subscribes.retain(|s| s.subscriber != provider);
        for list in self.handlers.values_mut() {
            list.retain(|p| p != provider);
        }
    }

    /// Remove todas as assinaturas de um assinante.
    pub fn subscribes_retain(&mut self, subscriber: &str) {
        self.subscribes.retain(|s| s.subscriber != subscriber);
    }

    /// Assinantes declarados cujo glob casa o tópico.
    pub fn subscribers_of(&self, topic: &str) -> Vec<String> {
        self.subscribes
            .iter()
            .filter(|s| s.glob.compile_matcher().is_match(topic))
            .map(|s| s.subscriber.clone())
            .collect()
    }

    /// Foto `(contrato, provedor)` para `atom.registry.list`.
    pub fn providers_snapshot(&self) -> Vec<(String, String)> {
        let mut out = vec![];
        for (c, ps) in &self.providers {
            for p in ps {
                out.push((c.clone(), p.clone()));
            }
        }
        out
    }

    /// Resolução: target explícito → binding → único provider (spec protocol §5.1).
    pub fn resolve(&self, service: &str, target: Option<&str>) -> Result<String, CoreError> {
        if let Some(t) = target {
            let list = self.providers_of(service);
            if list.iter().any(|p| p == t) {
                return Ok(t.to_string());
            }
            return Err(CoreError::NoProvider(format!("{service} via {t}")));
        }
        if let Some(b) = self.bindings.get(service) {
            let list = self.providers_of(service);
            if list.iter().any(|p| p == b) {
                return Ok(b.clone());
            }
            return Err(CoreError::NoProvider(format!(
                "{service} (binding {b} ausente)"
            )));
        }
        match self.providers_of(service).as_slice() {
            [] => Err(CoreError::NoProvider(service.to_string())),
            [only] => Ok(only.clone()),
            many => Err(CoreError::AmbiguousProvider(
                service.to_string(),
                many.join(", "),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_order() {
        let mut r = Registry::new(HashMap::new());
        assert!(r.resolve("s@0", None).is_err());
        r.add_provider("s@0", "a/x");
        assert_eq!(r.resolve("s@0", None).unwrap(), "a/x");
        r.add_provider("s@0", "a/y");
        assert!(r.resolve("s@0", None).is_err());
        r.set_binding("s@0", "a/y");
        assert_eq!(r.resolve("s@0", None).unwrap(), "a/y");
        assert_eq!(r.resolve("s@0", Some("a/x")).unwrap(), "a/x");
    }

    #[test]
    fn intercepts_sort_and_filter() {
        let mut r = Registry::new(HashMap::new());
        r.add_intercept("a/g", "ai.tools@0/*", "before", 10)
            .unwrap();
        r.add_intercept("a/h", "ai.tools@0/execute", "before", 1)
            .unwrap();
        r.add_intercept("a/z", "ai.tools@0/execute", "after", 0)
            .unwrap();
        assert_eq!(
            r.intercepts_for("ai.tools@0", "execute", "before"),
            vec![("a/h".to_string(), 1), ("a/g".to_string(), 10)]
        );
        assert!(r.intercepts_for("ai.llm@0", "chat", "before").is_empty());
        assert!(r.add_intercept("a/b", "x", "middle", 0).is_err());
        assert!(r.add_intercept("a/b", "[[", "before", 0).is_err());
    }

    #[test]
    fn subscriptions_and_handlers_cleanup() {
        let mut r = Registry::new(HashMap::new());
        r.add_subscription("a/cli", "atom.*").unwrap();
        r.add_subscription("a/cli", "atom.*").unwrap(); // idempotente
        r.add_subscription("a/other", "atom.config.*").unwrap();
        assert_eq!(r.subscribers_of("atom.config.changed").len(), 2);
        r.remove_subscription("a/cli", "atom.*");
        assert_eq!(
            r.subscribers_of("atom.config.changed"),
            vec!["a/other".to_string()]
        );
        r.add_provider("c@0", "a/p");
        r.add_handler("skill", "a/p");
        r.add_handler("skill", "a/p"); // idempotente
        r.remove_provider_all("a/p");
        assert!(r.providers_of("c@0").is_empty());
        assert!(r.handlers_of("skill").is_empty());
        // Assinatura de outro provedor sobrevive.
        assert_eq!(
            r.subscribers_of("atom.config.changed"),
            vec!["a/other".to_string()]
        );
    }
}
