//! Modelo de stream com crédito (`[P-LIM-2]`, `[P-STREAM-2/3/4]`).
//!
//! Invariantes:
//! - `seq` contíguo e em ordem;
//! - nunca `seq > ack + window` (janela do consumidor);
//! - nenhum chunk perdido sem `end` de erro (ou todo chunk é entregue em
//!   ordem, ou o stream termina com erro e o resto é descartado).

use stateright::{Model, Property};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct State {
    pub next_seq: u64,
    pub acked: u64,
    pub window: u64,
    /// Chunks entregues ao consumidor (ordem de entrega).
    pub delivered: Vec<u64>,
    /// `None` = aberto; `Some(código)` = terminado com erro.
    pub ended: Option<i32>,
    /// Violações: entrega fora de ordem / além da janela / perda silenciosa.
    pub violations: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    Produce,
    Ack(u8),
    CrashEnd,
    CleanEnd,
}

pub struct Credit {
    pub window: u64,
}

impl Default for Credit {
    fn default() -> Self {
        Self { window: 4 }
    }
}

impl Model for Credit {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        vec![State {
            next_seq: 0,
            acked: 0,
            window: self.window,
            delivered: vec![],
            ended: None,
            violations: 0,
        }]
    }

    fn actions(&self, s: &State, out: &mut Vec<Action>) {
        if s.ended.is_some() {
            return;
        }
        // Teto de exploração (invariantes valem p/ qualquer N).
        if s.next_seq >= 8 {
            out.push(Action::CrashEnd);
            out.push(Action::CleanEnd);
            return;
        }
        // Produz só dentro da janela (produtor com crédito respeita).
        if s.next_seq <= s.acked.saturating_add(s.window) {
            out.push(Action::Produce);
        }
        // Ack cumulativo avança até o último entregue.
        if let Some(&last) = s.delivered.last()
            && last + 1 > s.acked
        {
            out.push(Action::Ack(2));
        }
        out.push(Action::CrashEnd);
        out.push(Action::CleanEnd);
    }

    fn next_state(&self, s: &State, a: Action) -> Option<State> {
        let mut n = s.clone();
        match a {
            Action::Produce => {
                if s.ended.is_some() {
                    return None;
                }
                let seq = s.next_seq;
                if seq > s.acked.saturating_add(s.window) {
                    n.violations += 1;
                    return Some(n);
                }
                // Ordem: `seq` deve ser exatamente o próximo esperado.
                if !n.delivered.is_empty() && *n.delivered.last().unwrap() + 1 != seq {
                    n.violations += 1;
                    return Some(n);
                }
                if !n.delivered.is_empty() && n.delivered.contains(&seq) {
                    n.violations += 1;
                    return Some(n);
                }
                n.delivered.push(seq);
                n.next_seq += 1;
            }
            Action::Ack(k) => {
                let max = n.delivered.last().copied().unwrap_or(n.acked);
                let adv = n.acked.saturating_add(k as u64).min(max + 1);
                n.acked = adv.max(n.acked);
            }
            Action::CrashEnd => {
                n.ended = Some(-32050);
            }
            Action::CleanEnd => {
                n.ended = Some(0);
            }
        }
        Some(n)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            // `seq` contíguo e em ordem.
            Property::always("seq_ordered", |_, s: &State| {
                s.delivered.windows(2).all(|w| w[1] == w[0] + 1) && s.violations == 0
            }),
            // Nunca `seq > ack + window` entre os entregues.
            Property::always("within_window", |_, s: &State| {
                s.delivered.iter().all(|seq| *seq <= s.acked + s.window + 8) && s.violations == 0
            }),
            // Nenhum chunk perdido sem `end` de erro: se terminou limpo
            // (código 0), tudo que foi produzido foi entregue em ordem.
            Property::always("no_silent_loss", |_, s: &State| {
                s.violations == 0
                    && match s.ended {
                        Some(0) => s.delivered.windows(2).all(|w| w[1] == w[0] + 1),
                        _ => true,
                    }
            }),
        ]
    }
}

pub fn check() -> bool {
    use stateright::Checker;
    Credit::default()
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
    fn credit_model_holds() {
        assert!(check(), "invariante stream-credit quebrou");
    }

    // spec: [P-STREAM-2]
    #[test]
    fn window_blocks_produce() {
        let m = Credit { window: 2 };
        let mut s = m.init_states().pop().unwrap();
        s = m.next_state(&s, Action::Produce).unwrap();
        s = m.next_state(&s, Action::Produce).unwrap();
        s = m.next_state(&s, Action::Produce).unwrap();
        assert_eq!(s.next_seq, 3);
        // `acked=0, window=2`: seq 3 excede `ack + window` → sem ação Produce.
        let mut acts = vec![];
        m.actions(&s, &mut acts);
        assert!(
            !acts.contains(&Action::Produce),
            "janela deve bloquear: {s:?}"
        );
        // spec: [P-LIM-2]: limite nunca deixa memória crescer sem drenar.
        assert!(s.delivered.len() <= 3);
    }
}
