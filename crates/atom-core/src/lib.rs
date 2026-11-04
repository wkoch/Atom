#![forbid(unsafe_code)]
//! Kernel do Atom: roteador de mensagens, supervisão de extensões,
//! registro de serviços, perfis, armazenamento e pacotes.

pub mod bus;
pub mod daemon;
pub mod error;
pub mod host;
pub mod journal;
pub mod pkg;
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
