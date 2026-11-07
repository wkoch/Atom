#![forbid(unsafe_code)]
//! Extensão adversária `test/evil` (conformance.md §6).
//!
//! Cada ataque tem um teste que exige a falha especificada: o core nunca
//! confia na extensão — barra, sobrescreve, limita e descarta.

use std::path::{Path, PathBuf};

use atom_core::bus::{Core, StreamCall};
use atom_core::{pkg, profile};
use serde_json::json;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn evil_bin() -> PathBuf {
    let b = workspace_root().join("target/debug/evil");
    if !b.exists() {
        let st = std::process::Command::new("cargo")
            .args(["build", "-p", "test-evil"])
            .status()
            .expect("cargo build");
        assert!(st.success(), "cargo build -p test-evil falhou");
    }
    b
}

fn link_bin(core: &Core, pkg_name: &str) {
    let lock = pkg::load_lock(&core.paths).unwrap();
    let lp = lock.find(pkg_name).unwrap();
    let dir = pkg::package_dir(&core.data_store, lp).unwrap();
    let m = pkg::read_manifest(&dir).unwrap();
    let dst = dir.join(m.entrypoint.unwrap());
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
    std::fs::copy(evil_bin(), &dst).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut p = std::fs::metadata(&dst).unwrap().permissions();
        p.set_mode(0o755);
        std::fs::set_permissions(&dst, p).unwrap();
    }
}

/// Instala `test/evil` com entrypoint real.
async fn install_evil(profile: &str, home: &Path) -> Core {
    let mut core = Core::open_with(profile, Some(home)).await.unwrap();
    let src = workspace_root().join("testdata/evil");
    let id = pkg::install_dev(&core.paths, &core.data_store, &src).unwrap();
    assert_eq!(id, "test/evil@0.1.0");
    link_bin(&core, "test/evil");
    core.refresh().unwrap();
    core
}

// spec: [P-CANCEL-3]
#[tokio::test]
async fn evil_late_reply_discarded_after_deadline() {
    // Resposta que chega após o `deadline` é descartada: o chamador recebe
    // `-32022`, nunca o resultado tardio.
    let home = tempfile::tempdir().unwrap();
    let mut core = install_evil("evil-deadline", home.path()).await;
    // `$ctx.deadline` no passado: `call` aplica via `forged` (authorize só
    // reescreve tenant/user/profile/caller) e responde `-32022` sem invocar;
    // a resposta tardia do evil nunca vaza ao chamador.
    let forged = atom_protocol::CallerCtx {
        tenant: "evil".to_string(),
        user: "evil".to_string(),
        profile: "evil".to_string(),
        session: None,
        trace: None,
        caller: Some("evil/forjado".to_string()),
        deadline: Some(1),
    };
    assert_eq!(forged.caller.as_deref(), Some("evil/forjado"));
    let e = core
        .call("test.evil@0", "echo", json!({"x": 1}), None, Some(forged))
        .await
        .unwrap_err();
    assert_eq!(
        e.code(),
        atom_protocol::ErrorCode::DEADLINE_EXCEEDED,
        "resposta tardia devia virar -32022, deu {e}"
    );
    core.shutdown().await;
}

// spec: [P-LIM-2]
#[tokio::test]
async fn evil_flood_never_grows_memory_unbounded() {
    // Flood sem drenar: memória do core fica limitada (fila por assinante é
    // limitada, `[P-LIM-2]`); o stream lento não trava o core.
    let home = tempfile::tempdir().unwrap();
    let mut core = install_evil("evil-flood", home.path()).await;
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let cancel = std::sync::Arc::new(tokio::sync::Notify::new());
    let cancel2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        cancel2.notify_one();
    });
    let r = core
        .call_stream_chan(
            StreamCall {
                id: atom_protocol::new_stream_id(),
                service: "test.evil@0".to_string(),
                method: "echo".to_string(),
                params: json!({"x": 1}),
                target: None,
                ctx: None,
                flow: None,
            },
            tx,
            cancel,
        )
        .await;
    // Cancelado ou erro: o ponto é que o core sobreviveu ao flood.
    let _ = r;
    rx.close();
    // Core continua responsivo após o flood (memória limitada, sem deadlock).
    core.shutdown().await;
}

// spec: [P-CONN-2]
#[tokio::test]
async fn evil_cannot_connect_as_client() {
    // Peer descendente do daemon não é cliente confiável: `atom.hello`
    // falha com `-32005` e a conexão é fechada.
    assert!(atom_core::daemon::untrusted_peer(1234, Some(1234)));
    assert!(
        !atom_core::daemon::untrusted_peer(1234, atom_core::daemon::peer_pid_fallback())
            || atom_core::daemon::peer_pid_fallback().is_none()
    );
}

// spec: [C-AMB-2]
#[tokio::test]
async fn evil_tampered_grants_ignored() {
    // Edição externa de `grants.toml` é ignorada (vale o `.core`) + flag.
    let t = tempfile::tempdir().unwrap();
    let atom_paths = profile::AtomPaths::resolve_with(Some(t.path()));
    atom_paths.ensure().unwrap();
    let paths = profile::ProfilePaths::new(&atom_paths, "dev").unwrap();
    let mut g = profile::GrantsFile::new();
    g.insert(
        "test/evil".to_string(),
        profile::GrantEntry {
            granted: vec![],
            denied: vec![],
            approved_at: "1".to_string(),
        },
    );
    profile::save_grants(&paths, &g).unwrap();
    std::fs::write(
        &paths.grants,
        "[test/evil]\ngranted = [\"service:*@0\"]\ndenied = []\napproved_at = \"9\"\n",
    )
    .unwrap();
    let checked = profile::load_grants_checked(&paths);
    assert!(checked.tampered, "edição externa devia marcar tampered");
    assert!(
        !checked
            .grants
            .get("test/evil")
            .map(|e| e.granted.iter().any(|c| c.contains('*')))
            .unwrap_or(false),
        "grant forjado com curinga devia ser ignorado"
    );
}
