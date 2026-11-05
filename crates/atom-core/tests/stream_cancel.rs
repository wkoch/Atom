#![forbid(unsafe_code)]
//! Aceite v0.2 (parcial, sem rede/LLM): stream com cancelamento fim a fim.
//! `fake-llm` com `SLOW` dorme antes de responder; o cancelamento deve
//! abortar o `drive` com `-32800` sem esperar o produtor.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use atom_core::bus::{Core, StreamCall};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn fake_llm_bin() -> PathBuf {
    let b = workspace_root().join("target/debug/fake-llm");
    if !b.exists() {
        let st = std::process::Command::new("cargo")
            .args(["build", "-p", "test-fake-llm"])
            .status()
            .expect("cargo build");
        assert!(st.success(), "cargo build -p test-fake-llm falhou");
    }
    b
}

fn install_fake(core: &mut Core) {
    let src = workspace_root().join("testdata/fake-llm");
    let id = atom_core::pkg::install_dev(&core.paths, &core.data_store, &src).unwrap();
    assert_eq!(id, "test/fake-llm@0.2.0");
    let lock = atom_core::pkg::load_lock(&core.paths).unwrap();
    let lp = lock.find("test/fake-llm").unwrap();
    let dir = atom_core::pkg::package_dir(&core.data_store, lp).unwrap();
    let m = atom_core::pkg::read_manifest(&dir).unwrap();
    let dst = dir.join(m.entrypoint.unwrap());
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
    std::fs::copy(fake_llm_bin(), &dst).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut p = std::fs::metadata(&dst).unwrap().permissions();
        p.set_mode(0o755);
        std::fs::set_permissions(&dst, p).unwrap();
    }
    core.refresh().unwrap();
}

#[tokio::test]
async fn stream_cancel_mid_sleep() {
    // spec: [P-CANCEL-3]
    // spec: [P-CANCEL-4]
    // spec: [P-STREAM-1]
    // spec: [P-LIM-1]
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    // binding ai.llm@0 -> test/fake-llm antes de abrir
    {
        let a = atom_core::profile::AtomPaths::resolve_with(Some(home));
        a.ensure().unwrap();
        let p = atom_core::profile::ProfilePaths::new(&a, "cancel").unwrap();
        let mut cfg = atom_core::profile::load_or_create(&p).unwrap();
        cfg.bindings
            .insert("ai.llm@0".to_string(), "test/fake-llm".to_string());
        atom_core::profile::save(&p, &cfg).unwrap();
    }
    let mut core = Core::open_with("cancel", Some(home)).await.unwrap();
    install_fake(&mut core);

    let (tx, mut rx) = tokio::sync::mpsc::channel(256);
    let cancel = std::sync::Arc::new(tokio::sync::Notify::new());
    let cancel2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        cancel2.notify_one();
    });

    let start = Instant::now();
    let r = core
        .call_stream_chan(
            StreamCall {
                id: atom_protocol::new_stream_id(),
                service: "ai.llm@0".to_string(),
                method: "complete".to_string(),
                params: serde_json::json!({"messages": [{"role": "user", "content": [{"type": "text", "text": "SLOW 20000 :: hi"}]}]}),
                target: None,
                ctx: None,
                flow: None,
            },
            tx,
            cancel,
        )
        .await;
    let dt = start.elapsed();
    assert!(dt < Duration::from_secs(10), "cancel demorou {dt:?}");
    let err = r.expect_err("stream devia ser cancelado");
    let rpc = err.rpc("t");
    assert_eq!(
        rpc.code,
        atom_protocol::ErrorCode::REQUEST_CANCELLED,
        "código: {rpc:?}"
    );
    // drain esvazia (fim ou nada) sem travar
    rx.close();
    core.shutdown().await;
}

#[tokio::test]
async fn stream_complete_without_cancel() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    {
        let a = atom_core::profile::AtomPaths::resolve_with(Some(home));
        a.ensure().unwrap();
        let p = atom_core::profile::ProfilePaths::new(&a, "nocancel").unwrap();
        let mut cfg = atom_core::profile::load_or_create(&p).unwrap();
        cfg.bindings
            .insert("ai.llm@0".to_string(), "test/fake-llm".to_string());
        atom_core::profile::save(&p, &cfg).unwrap();
    }
    let mut core = Core::open_with("nocancel", Some(home)).await.unwrap();
    install_fake(&mut core);

    let (tx, mut rx) = tokio::sync::mpsc::channel(256);
    let cancel = std::sync::Arc::new(tokio::sync::Notify::new());
    let r = core
        .call_stream_chan(
            StreamCall {
                id: atom_protocol::new_stream_id(),
                service: "ai.llm@0".to_string(),
                method: "complete".to_string(),
                params: serde_json::json!({"messages": [{"role": "user", "content": [{"type": "text", "text": "hi"}]}]}),
                target: None,
                ctx: None,
                flow: None,
            },
            tx,
            cancel,
        )
        .await
        .expect("stream devia completar");
    assert_eq!(r["stop_reason"], serde_json::json!("stop"));
    let mut chunks = 0;
    let mut saw_end = false;
    while let Some(ev) = rx.recv().await {
        if ev.method == "atom.stream.chunk" {
            chunks += 1;
        } else {
            assert_eq!(ev.method, "atom.stream.end");
            saw_end = true;
        }
    }
    assert!(chunks >= 1, "sem chunks");
    assert!(saw_end, "sem end");
    core.shutdown().await;
}

fn ctx_with_deadline(ms_from_now: i64) -> atom_protocol::CallerCtx {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    atom_protocol::CallerCtx {
        tenant: "local".to_string(),
        user: "local".to_string(),
        profile: "cancel".to_string(),
        session: None,
        trace: None,
        caller: None,
        deadline: Some(now + ms_from_now),
    }
}

/// `[P-DL-3]`: prazo já estourado recusa sem invocar o provider.
// spec: [P-DL-3]
#[tokio::test]
async fn deadline_expired_rejects_without_invoke() {
    // spec: [P-DL-1]
    // spec: [P-DL-2]
    // spec: [P-DL-3]
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    {
        let a = atom_core::profile::AtomPaths::resolve_with(Some(home));
        a.ensure().unwrap();
        let p = atom_core::profile::ProfilePaths::new(&a, "cancel").unwrap();
        let mut cfg = atom_core::profile::load_or_create(&p).unwrap();
        cfg.bindings
            .insert("ai.llm@0".to_string(), "test/fake-llm".to_string());
        atom_core::profile::save(&p, &cfg).unwrap();
    }
    let mut core = Core::open_with("cancel", Some(home)).await.unwrap();
    install_fake(&mut core);
    let forged = ctx_with_deadline(-1000);
    let e = core
        .call(
            "ai.llm@0",
            "complete",
            serde_json::json!({"messages": []}),
            None,
            Some(forged),
        )
        .await
        .unwrap_err();
    assert_eq!(
        e.code(),
        atom_protocol::ErrorCode::DEADLINE_EXCEEDED,
        "esperava -32022, deu {e}"
    );
    core.shutdown().await;
}

/// `[P-DL-3]`: provider lento (`SLOW`) com prazo curto perde com `-32022`.
// spec: [P-DL-3]
#[tokio::test]
async fn deadline_fires_against_slow_provider() {
    // spec: [P-DL-1]
    // spec: [P-DL-3]
    // spec: [P-DL-4]
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    {
        let a = atom_core::profile::AtomPaths::resolve_with(Some(home));
        a.ensure().unwrap();
        let p = atom_core::profile::ProfilePaths::new(&a, "cancel").unwrap();
        let mut cfg = atom_core::profile::load_or_create(&p).unwrap();
        cfg.bindings
            .insert("ai.llm@0".to_string(), "test/fake-llm".to_string());
        atom_core::profile::save(&p, &cfg).unwrap();
    }
    let mut core = Core::open_with("cancel", Some(home)).await.unwrap();
    install_fake(&mut core);
    let forged = ctx_with_deadline(500);
    let start = Instant::now();
    let e = core
        .call(
            "ai.llm@0",
            "complete",
            serde_json::json!({"messages": [{"role": "user", "content": [{"type": "text", "text": "SLOW 20000 :: hi"}]}]}),
            None,
            Some(forged),
        )
        .await
        .unwrap_err();
    assert_eq!(
        e.code(),
        atom_protocol::ErrorCode::DEADLINE_EXCEEDED,
        "esperava -32022, deu {e}"
    );
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "deadline não cortou o provider lento"
    );
    core.shutdown().await;
}

/// `[P-DL-2]`: `default_deadline_ms` do perfil aplica prazo sem `$ctx`.
// spec: [P-DL-2]
#[tokio::test]
async fn default_deadline_applies_without_ctx() {
    // spec: [P-DL-2]
    // spec: [P-DL-3]
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    {
        let a = atom_core::profile::AtomPaths::resolve_with(Some(home));
        a.ensure().unwrap();
        let p = atom_core::profile::ProfilePaths::new(&a, "cancel").unwrap();
        let mut cfg = atom_core::profile::load_or_create(&p).unwrap();
        cfg.bindings
            .insert("ai.llm@0".to_string(), "test/fake-llm".to_string());
        cfg.bus.default_deadline_ms = 500;
        atom_core::profile::save(&p, &cfg).unwrap();
    }
    let mut core = Core::open_with("cancel", Some(home)).await.unwrap();
    install_fake(&mut core);
    let e = core
        .call(
            "ai.llm@0",
            "complete",
            serde_json::json!({"messages": [{"role": "user", "content": [{"type": "text", "text": "SLOW 20000 :: hi"}]}]}),
            None,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(
        e.code(),
        atom_protocol::ErrorCode::DEADLINE_EXCEEDED,
        "esperava -32022 via default_deadline_ms, deu {e}"
    );
    core.shutdown().await;
}
