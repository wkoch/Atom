//! Fuzz de `atom.toml`: manifesto malformado nunca panica o parse.
// spec: [CF-4]
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    // Parse TOML + validação de manifesto: ok ou erro tipado, nunca panic.
    if let Ok(v) = toml::from_str::<toml::Value>(text) {
        let _ = serde_json::to_string(&v);
        let _ = atom_protocol::valid_profile_name(
            v.get("name").and_then(|n| n.as_str()).unwrap_or(""),
        );
    }
});
