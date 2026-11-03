#![forbid(unsafe_code)]
//! Tipos serde do protocolo `atom/1`.
//!
//! Reutilizado pelo núcleo e por extensões (base do futuro SDK).

mod ctx;
mod errors;
mod lockfile;
mod manifest;
mod rpc;

pub use ctx::{CallerCtx, ctx_from_params, new_session_id, new_stream_id, new_trace_id};
pub use errors::{ErrorCode, ProtocolError, RpcError, core_error};
pub use lockfile::{LockPackage, Lockfile};
pub use manifest::{
    InterceptDecl, MANIFEST_VERSION, Manifest, NAME_RE, PROFILE_RE, ProvidesDecl, Requires,
    valid_profile_name,
};
pub use rpc::{
    AtomCallParams, CoreInfo, EventDeliver, ExtensionInfo, InitializeParams, InitializeResult,
    ProvidesEntry, RpcRequest, RpcResponse, ServiceInvokeParams,
};
