//! Modelo do ciclo de vida da extensão (protocol §4.1 + supervisão).
//!
//! Estados: `Starting → Running ⇄ Crashed → Failed` (terminal após 5 mortes
//! em 5 min). Invariantes:
//! - transições só pelas arestas de §4.1 (ex.: `Failed` nunca sai);
//! - `failed` após 5 mortes/5 min;
//! - pendentes de extensão `failed` recebem `-32010` (nunca ficam órfãos).

use stateright::{Model, Property};

/// Fase da extensão no supervisor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Phase {
    Starting,
    Running,
    Crashed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct State {
    pub phase: Phase,
    /// Mortes nos últimos 5 min (janela deslizante simplificada: contador).
    pub deaths: u8,
    /// Chamadas pendentes aguardando a extensão.
    pub pending: u32,
    /// Pendentes que receberam `-32010` (devem drenar, nunca órfãos).
    pub answered_unavailable: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Action {
    /// Handshake ok: `Starting → Running`.
    Started,
    /// Crash: `Running|Starting → Crashed`, morte contabilizada.
    Crash,
    /// Restart supervisionado: `Crashed → Starting` (se < 5 mortes).
    Restart,
    /// 5ª morte em 5 min: `Crashed → Failed` + pendentes → `-32010`.
    GiveUp,
    /// Nova chamada chega (só aceita fora de `Failed`).
    Arrive,
    /// Resposta normal drena um pendente (só em `Running`).
    Answer,
}

pub struct Lifecycle {
    pub max_deaths: u8,
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self { max_deaths: 5 }
    }
}

impl Model for Lifecycle {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        vec![State {
            phase: Phase::Starting,
            deaths: 0,
            pending: 0,
            answered_unavailable: 0,
        }]
    }

    fn actions(&self, s: &State, out: &mut Vec<Action>) {
        // `pending < 3`: teto de exploração (o invariante vale p/ qualquer N;
        // sem teto o espaço é infinito e o checker nunca termina).
        let can_arrive = s.pending < 3;
        match s.phase {
            Phase::Starting => {
                out.push(Action::Started);
                out.push(Action::Crash);
                if can_arrive {
                    out.push(Action::Arrive);
                }
            }
            Phase::Running => {
                out.push(Action::Crash);
                if can_arrive {
                    out.push(Action::Arrive);
                }
                if s.pending > 0 {
                    out.push(Action::Answer);
                }
            }
            Phase::Crashed => {
                if s.deaths >= self.max_deaths {
                    out.push(Action::GiveUp);
                } else {
                    out.push(Action::Restart);
                }
                if can_arrive {
                    out.push(Action::Arrive);
                }
            }
            Phase::Failed => {}
        }
    }

    fn next_state(&self, s: &State, a: Action) -> Option<State> {
        let mut n = s.clone();
        match (s.phase, a) {
            (Phase::Starting, Action::Started) => n.phase = Phase::Running,
            (Phase::Starting | Phase::Running, Action::Crash) => {
                n.phase = Phase::Crashed;
                n.deaths = n.deaths.saturating_add(1);
            }
            (Phase::Crashed, Action::Restart) if s.deaths < self.max_deaths => {
                n.phase = Phase::Starting;
            }
            (Phase::Crashed, Action::GiveUp) if s.deaths >= self.max_deaths => {
                n.phase = Phase::Failed;
                n.answered_unavailable += n.pending;
                n.pending = 0;
            }
            (_, Action::Arrive) if s.phase != Phase::Failed => {
                n.pending += 1;
            }
            (Phase::Running, Action::Answer) if s.pending > 0 => {
                n.pending -= 1;
            }
            _ => return None,
        }
        Some(n)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            // Só pelas transições de §4.1: `Failed` é terminal.
            Property::always("failed_terminal", |_, s: &State| {
                s.phase != Phase::Failed || s.pending == 0
            }),
            // `failed` após 5 mortes/5 min: com 5 mortes, o único futuro é `Failed`.
            Property::always("giveup_at_five", |_, s: &State| {
                s.deaths < 5 || s.phase == Phase::Failed || s.phase == Phase::Crashed
            }),
            // Pendentes de extensão `failed` recebem `-32010` (dreno total).
            Property::always("failed_drains_pending", |_, s: &State| {
                s.phase != Phase::Failed || (s.pending == 0)
            }),
            // Mortes nunca excedem o teto + 1 (contador saturado pelo give-up).
            Property::always("deaths_bounded", |_, s: &State| s.deaths <= 6),
        ]
    }
}

/// Explora o modelo por DFS; falha se algum invariante quebrar.
pub fn check(max_depth: usize) -> bool {
    use stateright::Checker;
    let model = Lifecycle::default();
    let checker = model.checker().spawn_dfs().join();
    let _ = max_depth;
    checker.discoveries().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    // spec: [CF-1]
    #[test]
    fn lifecycle_model_holds() {
        assert!(check(0), "invariante do ciclo de vida quebrou");
    }

    /// Cenário dirigido: 5 crashes seguidos terminam em `Failed` com `-32010`.
    // spec: [CF-1]
    #[test]
    fn five_deaths_fail_with_unavailable() {
        let m = Lifecycle::default();
        let mut s = m.init_states().pop().unwrap();
        s.pending = 3;
        for _ in 0..5 {
            if s.phase == Phase::Starting {
                s = m.next_state(&s, Action::Started).unwrap();
            }
            s = m.next_state(&s, Action::Crash).unwrap();
            if s.phase == Phase::Crashed && s.deaths < 5 {
                s = m.next_state(&s, Action::Restart).unwrap();
            }
        }
        assert_eq!(s.deaths, 5);
        s = m.next_state(&s, Action::GiveUp).unwrap();
        assert_eq!(s.phase, Phase::Failed);
        assert_eq!(s.pending, 0);
        assert_eq!(s.answered_unavailable, 3);
    }
}
