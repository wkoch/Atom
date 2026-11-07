#![deny(unsafe_code)]
//! Kernel do Atom: roteador de mensagens, supervisão de extensões,
//! registro de serviços, perfis, armazenamento e pacotes.
//!
//! `deny(unsafe_code)` no crate (não `forbid`): o módulo [`platform`] é a
//! exceção isolada da ADR-019 (syscalls `SO_PEERCRED`/`prctl`/`LOCAL_PEERPID`)
//! com `#[allow(unsafe_code)]` justificado no topo do arquivo. Todos os
//! outros módulos mantêm `#![forbid(unsafe_code)]` no próprio arquivo.

pub mod bus;
pub mod daemon;
pub mod error;
pub mod host;
pub mod journal;
pub mod pkg;
/// Identidade de plataforma (`[P-CONN-2]`, ADR-019): ÚNICO módulo com `unsafe`
/// (syscalls `SO_PEERCRED`/`prctl`/`LOCAL_PEERPID`/Job — justificativa no topo
/// do arquivo). Todo o resto do kernel mantém `#![forbid(unsafe_code)]`.
pub mod platform;
pub mod profile;
pub mod registry;
pub mod secrets;
pub mod storage;
pub mod store;
pub mod transport;
pub mod update;

pub use error::CoreError;

pub const CORE_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const PROTOCOL: &str = "atom/1";
