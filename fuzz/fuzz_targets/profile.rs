//! Fuzz de `profile.toml`: config malformada nunca panica.
// spec: [CF-4]
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    if let Ok(v) = toml::from_str::<toml::Value>(text) {
        let _ = toml::to_string(&v);
        // Bindings malformados: parse tolerante, nunca panic.
        if let Some(bindings) = v.get("bindings").and_then(|b| b.as_table()) {
            for (k, _) in bindings {
                let _ = atom_protocol::valid_profile_name(k);
            }
        }
    }
});
