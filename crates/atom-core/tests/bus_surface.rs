//! Superfície do `Core` sem extensões: `pump_one` sintético cobre os
//! braços de `route_incoming`, e chamadas diretas cobrem acessores/grants.

use std::path::{Path, PathBuf};

use atom_core::{bus::Core, host::Incoming};
use serde_json::json;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

async fn fresh_core() -> (tempfile::TempDir, Core) {
    let home = tempfile::tempdir().unwrap();
    let core = Core::open_with("default", Some(home.path())).await.unwrap();
    // Toca o workspace_root p/ clippy não reclamar de fn morta em outro módulo.
    let _ = workspace_root();
    (home, core)
}

fn inc(ext: &str, method: &str, params: serde_json::Value) -> Incoming {
    Incoming {
        ext: ext.to_string(),
        id: None,
        method: method.to_string(),
        params,
    }
}

#[tokio::test]
async fn core_accessors_without_extensions() {
    let (_home, mut core) = fresh_core().await;
    // Grants vazios: aprovar + negar + ler.
    core.approve_grants("pkg/a", &["service:test.echo@0".to_string()])
        .unwrap();
    assert_eq!(
        core.granted_of("pkg/a"),
        vec!["service:test.echo@0".to_string()]
    );
    assert!(core.missing_grants("pkg/a").is_empty());
    assert!(core.pending_grants().is_empty());
    core.deny_grants("pkg/a", &["service:test.echo@0".to_string()])
        .unwrap();
    assert!(core.granted_of("pkg/a").is_empty());
    // Sem manifestos/comandos.
    assert!(core.manifests_info().is_empty());
    assert!(core.bindings().is_empty());
    assert!(core.find_command("chat").is_none());
    assert!(core.kind_state("skill").is_none());
    assert!(core.ws_url().is_none());
    assert!(core.ui_url().is_none());
    core.set_ws_info("ws://x".to_string(), "t".to_string());
    assert!(core.ws_url().is_some());
    // KV por extensão.
    core.kv("e/x").set("k", json!({"v": 1})).unwrap();
    assert_eq!(core.kv("e/x").get("k").unwrap(), Some(json!({"v": 1})));
    // Evento sem assinantes: ok.
    let ctx = atom_protocol::CallerCtx::local("default", "core");
    core.publish_event("atom.test", json!({}), &ctx)
        .await
        .unwrap();
    // Operações em pacote inexistente.
    assert!(core.start_package("nope/x").await.is_err());
    core.stop_package("nope/x").await;
    assert!(!core.remove_package("nope/x").await.unwrap());
    core.reap_dead().await;
    core.reload_package("nope/x").await.unwrap_err();
    core.unload_package_kind("nope/x").await;
    assert!(core.load_package_kind("nope/x").await.is_err());
    core.sync_kinds().await.unwrap();
    core.ensure_kinds().await.unwrap();
    core.reload_profile().await.unwrap();
    core.shutdown().await;
}

#[tokio::test]
async fn route_incoming_arms() {
    let (_home, mut core) = fresh_core().await;
    // Secrets via bus.
    core.pump_one(inc(
        "e/x",
        "atom.secrets.set",
        json!({"name": "t", "value": "s"}),
    ))
    .await;
    assert_eq!(core.secrets.get("t").unwrap(), Some("s".to_string()));
    core.pump_one(inc("e/x", "atom.secrets.get", json!({})))
        .await;
    core.pump_one(inc("e/x", "atom.secrets.set", json!({"name": "t"})))
        .await;
    // Registry/profile/packages: sem params ruins.
    core.pump_one(inc("e/x", "atom.registry.list", json!({})))
        .await;
    core.pump_one(inc("e/x", "atom.profile.info", json!({})))
        .await;
    core.pump_one(inc("e/x", "atom.packages.mods", json!({})))
        .await;
    core.pump_one(inc("e/x", "atom.packages.asset", json!({})))
        .await;
    core.pump_one(inc(
        "e/x",
        "atom.packages.asset",
        json!({"package": "nope", "path": "../evil"}),
    ))
    .await;
    core.pump_one(inc("e/x", "atom.ui.serve", json!({}))).await;
    // (v1.1, ADR-026) `atom.ui.serve`/`atom.ui.session` exigem `ui.serve`:
    // sem grant, `serve` é negado e `ui_url` continua `None`.
    assert!(core.ui_url().is_none());
    core.pump_one(inc(
        "e/x",
        "atom.ui.serve",
        json!({"url": "http://127.0.0.1:1"}),
    ))
    .await;
    assert!(core.ui_url().is_none());
    core.approve_grants("e/x", &["ui.serve".to_string()])
        .unwrap();
    core.pump_one(inc(
        "e/x",
        "atom.ui.serve",
        json!({"url": "http://127.0.0.1:1"}),
    ))
    .await;
    assert_eq!(core.ui_url().as_deref(), Some("http://127.0.0.1:1"));
    core.pump_one(inc("e/x", "atom.ui.session", json!({})))
        .await;
    // Events: tópico vazio, sem capability, tópico atom.* (permitido).
    core.pump_one(inc("e/x", "atom.events.publish", json!({})))
        .await;
    core.pump_one(inc(
        "e/x",
        "atom.events.publish",
        json!({"topic": "a.b", "data": {}}),
    ))
    .await;
    core.pump_one(inc(
        "e/x",
        "atom.events.publish",
        json!({"topic": "atom.test", "data": {"n": 1}}),
    ))
    .await;
    assert_eq!(core.pending_grants().len(), 0);
    core.pump_one(inc("e/x", "atom.events.subscribe", json!({})))
        .await;
    core.pump_one(inc(
        "e/x",
        "atom.events.subscribe",
        json!({"topics": ["atom.test"]}),
    ))
    .await;
    assert_eq!(
        core.registry.subscribers_of("atom.test"),
        vec!["e/x".to_string()]
    );
    core.pump_one(inc(
        "e/x",
        "atom.events.subscribe",
        json!({"topic": "priv.b"}),
    ))
    .await;
    core.pump_one(inc("e/x", "atom.events.unsubscribe", json!({})))
        .await;
    assert!(core.registry.subscribers_of("atom.test").is_empty());
    // Register temporário.
    core.pump_one(inc(
        "e/x",
        "atom.register",
        json!({
            "provides": [{"contract": "test.tmp@0"}],
            "intercepts": [{"match": "test.tmp@0/*", "phase": "before"}],
            "subscribes": ["atom.test"],
        }),
    ))
    .await;
    assert_eq!(
        core.registry.providers_of("test.tmp@0"),
        vec!["e/x".to_string()]
    );
    // Grants request: vazio (erro) e válido (pendente).
    core.pump_one(inc("e/x", "atom.grants.request", json!({})))
        .await;
    core.pump_one(inc(
        "e/x",
        "atom.grants.request",
        json!({"capabilities": ["service:test.tmp@0"]}),
    ))
    .await;
    assert_eq!(
        core.pending_grants()
            .get("e/x")
            .cloned()
            .unwrap_or_default(),
        vec!["service:test.tmp@0".to_string()]
    );
    // Proceed com token inválido; stream msgs sem stream; método desconhecido.
    core.pump_one(inc("e/x", "atom.proceed", json!({"token": "nope"})))
        .await;
    core.pump_one(inc("e/x", "atom.stream.chunk", json!({"stream": "st_0"})))
        .await;
    core.pump_one(inc("e/x", "atom.stream.end", json!({"stream": "st_0"})))
        .await;
    core.pump_one(inc("e/x", "atom.stream.cancel", json!({})))
        .await;
    core.pump_one(inc("e/x", "atom.stream.ack", json!({})))
        .await;
    core.pump_one(inc("e/x", "atom.bogus", json!({}))).await;
    // nested_call: params inválidos, sem provider, profundidade.
    core.pump_one(inc("e/x", "atom.call", json!([]))).await;
    core.pump_one(inc(
        "e/x",
        "atom.call",
        json!({"service": "test.tmp@0", "method": "m", "params": {}}),
    ))
    .await;
    // Supervisão: exit/restart de extensão desconhecida não quebra.
    core.pump_one(inc("nope/x", "__ext_exit", json!({}))).await;
    core.pump_one(inc("nope/x", "__ext_restart", json!({})))
        .await;
    core.shutdown().await;
}

#[tokio::test]
async fn core_call_system_errors() {
    let (_home, mut core) = fresh_core().await;
    // Sem provider: NoProvider; com target inexistente idem.
    let e = core
        .call("test.nada@0", "m", json!({}), None, None)
        .await
        .unwrap_err();
    assert!(matches!(e, atom_core::error::CoreError::NoProvider(_)));
    // Forged ctx é sobrescrito (tenant volta a local) — cobre authorize no call.
    let mut forged = atom_protocol::CallerCtx::local("evil", "evil");
    forged.tenant = "evil".to_string();
    let e = core
        .call("test.nada@0", "m", json!({}), None, Some(forged))
        .await
        .unwrap_err();
    assert!(matches!(e, atom_core::error::CoreError::NoProvider(_)));
    core.shutdown().await;
}
