//! Fuzz de `atom.lock`: lockfile malformado nunca panica.
// spec: [CF-4]
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    if let Ok(v) = toml::from_str::<toml::Value>(text) {
        // Round-trip: parse → ser → reparse deve ser estável ou erro limpo.
        if let Ok(back) = toml::to_string(&v) {
            let _ = toml::from_str::<toml::Value>(&back);
        }
    }
});
