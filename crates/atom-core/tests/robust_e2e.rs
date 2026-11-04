//! Aceite v0.6: grants (negação → `-32003`), supervisão com backoff
//! (`test/crash` reinicia e não derruba o core) e journal do bus.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use atom_core::{bus::Core, host::ExtState, pkg};
use serde_json::json;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn bin(name: &str, pkg: &str) -> PathBuf {
    let b = workspace_root().join(format!("target/debug/{name}"));
    if !b.exists() {
        let st = std::process::Command::new("cargo")
            .args(["build", "-p", pkg])
            .status()
            .expect("cargo build");
        assert!(st.success(), "cargo build -p {pkg} falhou");
    }
    b
}

fn link_bin(core: &Core, package: &str, src_bin: &Path) {
    let lock = pkg::load_lock(&core.paths).unwrap();
    let lp = lock.find(package).unwrap();
    let dir = pkg::package_dir(&core.data_store, lp).unwrap();
    let m = pkg::read_manifest(&dir).unwrap();
    let ep = m.entrypoint.expect("teste precisa de entrypoint");
    let dst = dir.join(&ep);
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
    std::fs::copy(src_bin, &dst).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut p = std::fs::metadata(&dst).unwrap().permissions();
        p.set_mode(0o755);
        std::fs::set_permissions(&dst, p).unwrap();
    }
}

async fn install(core: &mut Core, src: &str, pkg_name: &str, bin_name: &str, crate_name: &str) {
    let full = workspace_root().join(src);
    let id = pkg::install_dev(&core.paths, &core.data_store, &full).unwrap();
    assert!(id.starts_with(pkg_name), "id {id} != {pkg_name}@");
    link_bin(core, pkg_name, &bin(bin_name, crate_name));
    core.refresh().unwrap();
}

/// Chamada sem capability → `-32003`; com grant → passa.
#[tokio::test]
async fn call_without_capability_is_denied() {
    let home = tempfile::tempdir().unwrap();
    let mut core = Core::open_with("caps", Some(home.path())).await.unwrap();
    install(&mut core, "testdata/echo", "test/echo", "echo", "test-echo").await;
    install(
        &mut core,
        "testdata/caller",
        "test/caller",
        "caller",
        "test-caller",
    )
    .await;

    // Sem grant: caller tenta echo → -32003 (bus barra na origem).
    let e = core
        .call(
            "test.caller@0",
            "call",
            json!({"service": "test.echo@0", "method": "echo", "params": {"x": 1}}),
            None,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(e.code(), -32003, "esperava CapabilityDenied, deu {e}");

    // Com grant: passa.
    core.approve_grants("test/caller", &["service:test.echo@0".to_string()])
        .unwrap();
    let v = core
        .call(
            "test.caller@0",
            "call",
            json!({"service": "test.echo@0", "method": "echo", "params": {"x": 1}}),
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(v, json!({"x": 1}));

    // `atom.grants.request` via bus + aprovação via API.
    let v = core
        .call(
            "test.caller@0",
            "ask",
            json!({"capabilities": ["service:test.echo@0"]}),
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(v, json!({"status": "pending"}));
    assert!(core.pending_grants().get("test/caller").is_some());

    // Revogação vale na hora: volta a negar.
    core.deny_grants("test/caller", &["service:test.echo@0".to_string()])
        .unwrap();
    let e = core
        .call(
            "test.caller@0",
            "call",
            json!({"service": "test.echo@0", "method": "echo", "params": {"x": 1}}),
            None,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(
        e.code(),
        -32003,
        "esperava CapabilityDenied após deny, deu {e}"
    );
    core.shutdown().await;
}

/// `test/crash` morre em loop: 5 mortes → `failed`, core segue vivo.
#[tokio::test]
async fn crash_restarts_with_backoff_then_fails() {
    let home = tempfile::tempdir().unwrap();
    let mut core = Core::open_with("crash", Some(home.path())).await.unwrap();
    install(&mut core, "testdata/echo", "test/echo", "echo", "test-echo").await;
    install(
        &mut core,
        "testdata/crash",
        "test/crash",
        "crash",
        "test-crash",
    )
    .await;

    // Dispara o crash (primeira chamada pode responder antes da morte).
    let _ = core
        .call("test.crash@0", "ping", json!({}), None, None)
        .await;

    // Espera `failed` (backoffs 1+2+4+8s + folga).
    let mut failed = false;
    for _ in 0..60 {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        // Bomba a fila p/ processar __ext_exit/__ext_restart parados.
        while core.host.try_incoming().is_some() {}
        // Reap manual: garante supervisão mesmo sem chamadas no meio.
        core.reap_dead().await;
        if let Some((st, _)) = core.host.states().get("test/crash")
            && *st == ExtState::Failed("5 mortes em 5 min".to_string())
        {
            failed = true;
            break;
        }
    }
    assert!(failed, "crash não chegou a failed");

    // Core vivo: echo responde.
    let v = core
        .call("test.echo@0", "echo", json!({"y": 2}), None, None)
        .await
        .unwrap();
    assert_eq!(v, json!({"y": 2}));
    core.shutdown().await;
}

/// Journal registra chamadas quando habilitado.
#[tokio::test]
async fn journal_records_calls() {
    let home = tempfile::tempdir().unwrap();
    let mut core = Core::open_with("journal", Some(home.path())).await.unwrap();
    core.config.journal.enabled = true;
    atom_core::profile::save(&core.paths, &core.config).unwrap();
    core.refresh().unwrap();
    install(&mut core, "testdata/echo", "test/echo", "echo", "test-echo").await;
    let v = core
        .call("test.echo@0", "echo", json!({"z": 3}), None, None)
        .await
        .unwrap();
    assert_eq!(v, json!({"z": 3}));
    let journal = core.paths.data_dir.join("journal/bus.jsonl");
    let text = std::fs::read_to_string(&journal).unwrap();
    assert!(
        text.contains("test.echo@0"),
        "journal sem a chamada: {text}"
    );
    core.shutdown().await;
}
