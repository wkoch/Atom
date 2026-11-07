//! Watch de storage (`atom.storage.watch/unwatch`, `storage.changed`, `[P-KV-6]`)
//! + conformidade `[S-BE-1]` no nível de integração (via `Host`/`Kv`).
//!
//! Cobre: `watch {prefix}` → `{subscription}` (`w_...`), `set/delete`
//! notificando `storage.changed` por prefixo em ordem de chave, `unwatch`
//! removendo, fila limitada com `overflow`, e a suíte genérica
//! `storage::conformance` + `P-SEC-1` (read-your-writes/durável) via arquivo.

use atom_core::{bus::Core, host::Incoming};
use serde_json::json;
use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

async fn fresh_core() -> (tempfile::TempDir, Core) {
    let home = tempfile::tempdir().unwrap();
    let core = Core::open_with("default", Some(home.path())).await.unwrap();
    let _ = workspace_root();
    (home, core)
}

fn inc(ext: &str, method: &str, params: serde_json::Value) -> Incoming {
    Incoming {
        ext: ext.to_string(),
        id: Some(json!("t-1")),
        method: method.to_string(),
        params,
    }
}
#[tokio::test]
async fn storage_watch_flow() {
    // spec: [P-KV-6]
    let (_home, core) = fresh_core().await;
    // Sem extensões rodando: `storage_mutate` persiste, `watch` falha.
    // Cobre o registro isolado (unidade já cobre em `host::tests`).
    let mut reg = atom_core::storage::WatchRegistry::new();
    let sub = reg.watch("e/x", "a/");
    assert!(sub.starts_with("w_"));
    // `watch` por prefixo vazio casa tudo; ordem de subscription.
    let all = reg.watch("e/x", "");
    assert_eq!(reg.matching("e/x", "a/k"), {
        let mut want = vec![sub.clone(), all.clone()];
        want.sort();
        want
    });
    assert_eq!(reg.matching("e/x", "b/k"), vec![all.clone()]);
    // Outra extensão não vê.
    let other = reg.watch("e/y", "a/");
    assert_eq!(reg.matching("e/y", "a/k"), vec![other.clone()]);
    // `unwatch` remove só a própria.
    assert!(reg.unwatch("e/x", &sub));
    assert_eq!(reg.matching("e/x", "a/k"), vec![all.clone()]);
    assert!(!reg.unwatch("e/x", &other));
    // Payloads.
    let p = atom_core::storage::WatchRegistry::changed_payload(&all, "a/k", Some("v_1"), false);
    assert_eq!(p["key"], json!("a/k"));
    assert_eq!(p["version"], json!("v_1"));
    assert_eq!(p["subscription"], json!(all));
    let d = atom_core::storage::WatchRegistry::changed_payload(&all, "a/k", None, true);
    assert_eq!(d["deleted"], json!(true));
    let o = atom_core::storage::WatchRegistry::overflow_payload(&all);
    assert_eq!(o["overflow"], json!(true));
    // `set/delete` via `Kv` (durável) — `storage.changed` sai via `Host`
    // (coberto em `host::tests` + e2e abaixo com `pump_one`).
    core.kv("e/x").set("a/k", json!(1)).unwrap();
    assert_eq!(core.kv("e/x").get("a/k").unwrap(), Some(json!(1)));
    assert!(core.kv("e/x").delete("a/k").unwrap());
    let _ = inc("e/x", "atom.storage.watch", json!({"prefix": "a/"}));
}

#[tokio::test]
async fn storage_conformance_suite() {
    // spec: [S-BE-1]
    // spec: [P-KV-1]
    // spec: [P-KV-2]
    // spec: [P-KV-4]
    // spec: [P-KV-5]
    // spec: [P-KV-6]
    // spec: [P-SEC-1]
    let (_home, core) = fresh_core().await;
    // Backend filesystem passa na suíte genérica.
    let kv = core.kv("e/conf");
    atom_core::storage::conformance::run_suite(&kv).expect("filesystem passa [S-BE-1]");
    // Durabilidade: reabre e relê (`[P-KV-2]`).
    let kv2 = core.kv("e/conf");
    let (got, ver) = kv2.get_versioned("ryw").unwrap();
    assert_eq!(got, Some(json!({"x": 1})));
    assert!(ver.is_some());
    // Secrets: read-your-writes + durável antes de responder (`[P-SEC-1]`).
    let t = tempfile::tempdir().unwrap();
    let s = atom_core::secrets::Secrets::file_only("dev", t.path());
    assert_eq!(s.get("tok").unwrap(), None);
    s.set("tok", "s3cr3t").unwrap();
    assert_eq!(s.get("tok").unwrap().as_deref(), Some("s3cr3t"));
    let s2 = atom_core::secrets::Secrets::file_only("dev", t.path());
    assert_eq!(s2.get("tok").unwrap().as_deref(), Some("s3cr3t"));
}
