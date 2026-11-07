//! Fuzz do índice `store/v1`: entrada malformada nunca panica.
// spec: [CF-4]
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Índice é JSON: parse tolerante + acesso defensivo, nunca panic.
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(data) else {
        return;
    };
    let _ = serde_json::to_string(&v);
    if let Some(pkgs) = v.get("packages").and_then(|p| p.as_array()) {
        for p in pkgs {
            let _ = p.get("name").and_then(|n| n.as_str());
            let _ = p.get("version").and_then(|n| n.as_str());
        }
    }
});
