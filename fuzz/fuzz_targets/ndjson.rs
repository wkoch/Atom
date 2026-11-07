//! Fuzz do framing NDJSON: nenhuma entrada panica o parse/seriação.
// spec: [CF-4]
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Linha NDJSON: deve ser UTF-8 válido + JSON válido ou rejeição limpa.
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let line = text.lines().next().unwrap_or("");
    if line.len() > 16 * 1024 * 1024 {
        return; // acima do limite: o transporte rejeita sem panicar.
    }
    if line.trim().is_empty() {
        return; // linha vazia = keepalive ignorado.
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
        return; // JSON inválido = ignorado, sem panic.
    };
    // Round-trip de seriação nunca panica.
    let _ = serde_json::to_string(&v);
    // Envelope RPC: parse tipado ou rejeição limpa.
    let _ = serde_json::from_value::<atom_protocol::RpcRequest>(v);
});
