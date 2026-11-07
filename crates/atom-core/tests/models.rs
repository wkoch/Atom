#![forbid(unsafe_code)]
//! Modelos de estado `stateright` (conformance.md §4).
//!
//! Abstrações executáveis das regras de protocolo; cada invariante espelha a
//! cláusula citada. O comportamento real é coberto pelos testes de integração.
#[path = "models/cancel_deadline.rs"]
mod cancel_deadline;
#[path = "models/lifecycle.rs"]
mod lifecycle;
#[path = "models/proceed.rs"]
mod proceed;
#[path = "models/stream_credit.rs"]
mod stream_credit;

// spec: [CF-1]
#[test]
fn models_hold() {
    assert!(lifecycle::check(0), "lifecycle quebrou");
    assert!(proceed::check(), "around/proceed quebrou");
    assert!(cancel_deadline::check(), "cancel/deadline quebrou");
    assert!(stream_credit::check(), "stream-credit quebrou");
}
