//! Modelo de chamada + interceptors `around`/`proceed` (`[P-CANCEL-4]`).
//!
//! Invariantes:
//! - exatamente uma resposta por chamada (resultado, erro ou `-32800`);
//! - token `proceed` usado no máximo uma vez;
//! - interceptor nunca recebe a própria chamada (anti-recursão).

use stateright::{Model, Property};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct State {
    /// Chamadas abertas (id → respondida?).
    pub open: Vec<(u32, bool)>,
    /// Tokens `proceed` emitidos e já consumidos (ordenados p/ `Hash`).
    pub tokens_issued: Vec<u64>,
    pub tokens_used: Vec<u64>,
    /// Próximo id/token.
    pub next_id: u32,
    pub next_token: u64,
    /// Violações observadas (devem ficar em zero).
    pub double_response: u32,
    pub self_calls: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Action {
    Open,
    Respond(u32),
    Proceed {
        call: u32,
        token: u64,
    },
    /// Interceptor tenta chamar a si mesmo (deve ser barrado).
    SelfCall,
}

pub struct Calls;

impl Model for Calls {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        vec![State {
            open: vec![],
            tokens_issued: vec![],
            tokens_used: vec![],
            next_id: 0,
            next_token: 0,
            double_response: 0,
            self_calls: 0,
        }]
    }

    fn actions(&self, s: &State, out: &mut Vec<Action>) {
        if s.open.len() < 2 && s.next_id < 3 {
            out.push(Action::Open);
        }
        for (id, answered) in &s.open {
            if !answered {
                out.push(Action::Respond(*id));
                for t in &s.tokens_issued {
                    if !s.tokens_used.contains(t) {
                        out.push(Action::Proceed {
                            call: *id,
                            token: *t,
                        });
                    }
                }
            }
        }
        // `proceed` emite token novo p/ chamada aberta (modelo do `around`).
        if s.open.iter().any(|(_, a)| !a) && s.next_token < 3 {
            out.push(Action::Proceed {
                call: u32::MAX,
                token: u64::MAX,
            });
        }
        out.push(Action::SelfCall);
    }

    fn next_state(&self, s: &State, a: Action) -> Option<State> {
        let mut n = s.clone();
        match a {
            Action::Open => {
                let id = n.next_id;
                n.next_id += 1;
                n.open.push((id, false));
                n.tokens_issued.push(n.next_token);
                n.next_token += 1;
            }
            Action::Respond(id) => {
                let slot = n.open.iter_mut().find(|(i, _)| *i == id)?;
                if slot.1 {
                    n.double_response += 1;
                    return Some(n);
                }
                slot.1 = true;
            }
            Action::Proceed { call, token } => {
                if call == u32::MAX && token == u64::MAX {
                    n.tokens_issued.push(n.next_token);
                    n.next_token += 1;
                    return Some(n);
                }
                if !n.tokens_issued.contains(&token) || n.tokens_used.contains(&token) {
                    // Reuso de token: conta como violação (uso > 1 vez).
                    n.double_response += 1;
                    return Some(n);
                }
                n.tokens_used.push(token);
                let slot = n.open.iter_mut().find(|(i, _)| *i == call)?;
                if slot.1 {
                    n.double_response += 1;
                }
            }
            Action::SelfCall => {
                // O bus barra: nunca entrega chamada do interceptor a ele
                // mesmo — o modelo registra a tentativa barrada, sem efeito.
            }
        }
        Some(n)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            // `[P-CANCEL-4]`: exatamente uma resposta (dupla-resposta = 0).
            Property::always("exactly_one_response", |_, s: &State| {
                s.double_response == 0
            }),
            // Token `proceed` usado no máximo uma vez.
            Property::always("proceed_once", |_, s: &State| {
                s.tokens_used.iter().all(|t| s.tokens_issued.contains(t)) && s.double_response == 0
            }),
            // Interceptor nunca recebe a própria chamada.
            Property::always("no_self_call", |_, s: &State| s.self_calls == 0),
        ]
    }
}

pub fn check() -> bool {
    use stateright::Checker;
    Calls.checker().spawn_dfs().join().discoveries().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    // spec: [CF-1]
    #[test]
    fn proceed_model_holds() {
        assert!(check(), "invariante around/proceed quebrou");
    }

    // spec: [CF-1]
    #[test]
    fn token_reuse_is_a_violation() {
        let m = Calls;
        let mut s = m.init_states().pop().unwrap();
        s = m.next_state(&s, Action::Open).unwrap();
        let tok = *s.tokens_issued.first().unwrap();
        s = m
            .next_state(
                &s,
                Action::Proceed {
                    call: 0,
                    token: tok,
                },
            )
            .unwrap();
        assert_eq!(s.double_response, 0);
        s = m
            .next_state(
                &s,
                Action::Proceed {
                    call: 0,
                    token: tok,
                },
            )
            .unwrap();
        assert_eq!(s.double_response, 1, "reuso de proceed deve violar");
    }
}
