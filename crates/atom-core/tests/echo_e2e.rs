#![forbid(unsafe_code)]
//! Aceite v0.1: install --dev + call + isolamento por perfil (usa testdata/echo).

use std::path::{Path, PathBuf};

use atom_core::{bus::Core, pkg, profile};
use serde_json::json;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn echo_bin() -> PathBuf {
    let b = {
        let p = workspace_root().join("target/debug/echo");
        if cfg!(windows) {
            p.with_extension("exe")
        } else {
            p
        }
    };
    if !b.exists() {
        let st = std::process::Command::new("cargo")
            .args(["build", "-p", "test-echo"])
            .status()
            .expect("cargo build");
        assert!(st.success(), "cargo build -p test-echo falhou");
    }
    b
}

#[tokio::test]
async fn dev_install_call_isolated_per_profile() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    let src = workspace_root().join("testdata/echo");

    // Perfil default: instala e chama.
    let mut core = Core::open_with("default", Some(home)).await.unwrap();
    let id = pkg::install_dev(&core.paths, &core.data_store, &src).unwrap();
    assert_eq!(id, "test/echo@0.1.0");

    // Leva o binário compilado ao entrypoint da store (o que o CLI faz).
    let lock = pkg::load_lock(&core.paths).unwrap();
    let lp = lock.find("test/echo").unwrap();
    let dir = pkg::package_dir(&core.data_store, lp).unwrap();
    let m = pkg::read_manifest(&dir).unwrap();
    let ep = m.entrypoint.unwrap();
    assert!(!dir.join(&ep).exists());
    let dst = dir.join(&ep);
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
    std::fs::copy(echo_bin(), &dst).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut p = std::fs::metadata(&dst).unwrap().permissions();
        p.set_mode(0o755);
        std::fs::set_permissions(&dst, p).unwrap();
    }
    core.refresh().unwrap();

    let v = core
        .call("test.echo@0", "echo", json!({"x": 1}), None, None)
        .await
        .unwrap();
    assert_eq!(v, json!({"x": 1}));
    core.shutdown().await;

    // Perfil outro: lock vazio (isolado), mesmo ATOM_HOME.
    let other = Core::open_with("outro", Some(home)).await.unwrap();
    let lock = pkg::load_lock(&other.paths).unwrap();
    assert!(lock.package.is_empty());
    assert!(profile::load_or_create(&other.paths).is_ok());
}
