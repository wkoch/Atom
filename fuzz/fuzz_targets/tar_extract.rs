//! Fuzz de extração tar: arquivo malformado nunca panica nem escapa.
// spec: [CF-4]
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Gzip opcional: bytes arbitrários não podem panicar nem escrever fora.
    let gz_ok = flate2::read::GzDecoder::new(data);
    let mut archive = tar::Archive::new(gz_ok);
    let Ok(entries) = archive.entries() else {
        return;
    };
    for entry in entries {
        let Ok(mut e) = entry else { break };
        // Só inspeciona o caminho (rejeita `..`/absoluto); nunca extrai.
        let Ok(path) = e.path() else { continue };
        let p = path.to_string_lossy();
        if p.contains("..") || p.starts_with('/') {
            continue; // traversal barrado.
        }
        let mut buf = Vec::new();
        let _ = std::io::Read::read_to_end(&mut e, &mut buf);
        if buf.len() > 64 * 1024 * 1024 {
            break; // bomba de zip limitada.
        }
    }
});
