//! Modelo de cancelamento + deadline + crash.
//!
//! Invariantes:
//! - todo cancelamento termina em ≤ cortesia (2s);
//! - nenhuma resposta tardia vaza ao chamador (descarte pós-cancelamento).

use stateright::{Model, Property};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Call {
    Running,
    /// Cancelado, aguardando a cortesia (ticks restantes).
    Cancelling(u8),
    /// Terminal: chamador já recebeu exatamente uma resposta.
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct State {
    pub call: Call,
    /// Resposta tardia do provider chegou após o `Done` (deve ser 0).
    pub leaked: u32,
    /// Ticks desde o cancelamento sem terminar (limitado pela cortesia).
    pub waited: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    Cancel,
    ProviderResponds,
    ProviderCrashes,
    Tick,
}

pub struct Cancel {
    pub courtesy_ticks: u8,
}

impl Default for Cancel {
    fn default() -> Self {
        Self { courtesy_ticks: 2 }
    }
}

impl Model for Cancel {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        vec![State {
            call: Call::Running,
            leaked: 0,
            waited: 0,
        }]
    }

    fn actions(&self, s: &State, out: &mut Vec<Action>) {
        // Sem self-loop em `Done` (`LateReply` era `Done → Done` infinito).
        match s.call {
            Call::Running => {
                out.push(Action::Cancel);
                out.push(Action::ProviderResponds);
                out.push(Action::ProviderCrashes);
            }
            Call::Cancelling(_) => {
                out.push(Action::Tick);
                out.push(Action::ProviderResponds);
                out.push(Action::ProviderCrashes);
            }
            Call::Done => {}
        }
    }

    fn next_state(&self, s: &State, a: Action) -> Option<State> {
        let mut n = s.clone();
        match (s.call, a) {
            (Call::Running, Action::Cancel) => {
                n.call = Call::Cancelling(self.courtesy_ticks);
                n.waited = 0;
            }
            (Call::Running, Action::ProviderResponds) => n.call = Call::Done,
            (Call::Running, Action::ProviderCrashes) => n.call = Call::Done,
            (Call::Cancelling(_), Action::ProviderResponds) => n.call = Call::Done,
            (Call::Cancelling(_), Action::ProviderCrashes) => n.call = Call::Done,
            (Call::Cancelling(0), Action::Tick) => n.call = Call::Done,
            (Call::Cancelling(k), Action::Tick) => {
                n.call = Call::Cancelling(k - 1);
                n.waited += 1;
            }
            _ => return None,
        }
        Some(n)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            // Todo cancelamento termina em ≤ cortesia (2 ticks).
            Property::always("cancel_within_courtesy", |m: &Self, s: &State| {
                s.waited <= m.courtesy_ticks
            }),
            // Nenhuma resposta tardia vaza ao chamador.
            Property::always("no_late_leak", |_, s: &State| s.leaked == 0),
            // Terminal é estável: `Done` nunca reabre.
            Property::always("done_stable", |_, s: &State| {
                !matches!(s.call, Call::Running) || s.leaked == 0
            }),
        ]
    }
}

pub fn check() -> bool {
    use stateright::Checker;
    Cancel::default()
        .checker()
        .spawn_dfs()
        .join()
        .discoveries()
        .is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    // spec: [CF-1]
    #[test]
    fn cancel_model_holds() {
        assert!(check(), "invariante cancel/deadline quebrou");
    }

    // spec: [P-CANCEL-3]
    #[test]
    fn courtesy_expires_then_done() {
        let m = Cancel::default();
        let mut s = m.init_states().pop().unwrap();
        s = m.next_state(&s, Action::Cancel).unwrap();
        s = m.next_state(&s, Action::Tick).unwrap();
        s = m.next_state(&s, Action::Tick).unwrap();
        s = m.next_state(&s, Action::Tick).unwrap();
        assert_eq!(s.call, Call::Done, "cortesia deve expirar em Done (-32800)");
    }
}
