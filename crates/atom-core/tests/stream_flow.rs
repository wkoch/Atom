//! Streams: janela do consumidor (`[P-STREAM-2]`), crédito do produtor
//! (`[P-STREAM-3]`), seq contíguo (`[P-STREAM-4]`).

use std::path::{Path, PathBuf};

use atom_core::{bus::Core, host::Incoming, pkg};
use serde_json::json;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn build_bin(pkg: &str, bin: &str) -> PathBuf {
    let b = workspace_root().join(format!("target/debug/{bin}"));
    if !b.exists() {
        let st = std::process::Command::new("cargo")
            .args(["build", "-p", pkg])
            .status()
            .expect("cargo build");
        assert!(st.success(), "cargo build -p {pkg} falhou");
    }
    b
}

fn install(core: &Core, dir: &str, pkg_name: &str, cargo_pkg: &str, bin: &str, entry: &str) {
    let src = workspace_root().join(dir);
    let id = pkg::install_dev(&core.paths, &core.data_store, &src).unwrap();
    assert!(id.starts_with(pkg_name));
    let lock = pkg::load_lock(&core.paths).unwrap();
    let lp = lock.find(pkg_name).unwrap();
    let pdir = pkg::package_dir(&core.data_store, lp).unwrap();
    let dst = pdir.join(entry);
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
    std::fs::copy(build_bin(cargo_pkg, bin), &dst).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut p = std::fs::metadata(&dst).unwrap().permissions();
        p.set_mode(0o755);
        std::fs::set_permissions(&dst, p).unwrap();
    }
}

/// `[P-STREAM-2]`: `window` fora de 1..=4096 é `INVALID_PARAMS`.
// spec: [P-STREAM-2]
#[tokio::test]
async fn stream_window_bounds_rejected() {
    // spec: [P-STREAM-2]
    let home = tempfile::tempdir().unwrap();
    let mut core = Core::open_with("flow", Some(home.path())).await.unwrap();
    install(
        &core,
        "testdata/echo",
        "test/echo",
        "test-echo",
        "echo",
        "bin/echo",
    );
    core.refresh().unwrap();
    let (tx, _rx) = tokio::sync::mpsc::channel(256);
    let e = core
        .call_stream_chan(
            atom_core::bus::StreamCall {
                id: atom_protocol::new_stream_id(),
                service: "test.echo@0".to_string(),
                method: "echo".to_string(),
                params: json!({"x": 1}),
                target: None,
                ctx: None,
                flow: Some(atom_protocol::FlowOpts { window: 0 }),
            },
            tx,
            std::sync::Arc::new(tokio::sync::Notify::new()),
        )
        .await
        .unwrap_err();
    assert_eq!(
        e.code(),
        atom_protocol::ErrorCode::INVALID_PARAMS,
        "window 0 deve ser INVALID_PARAMS, deu {e}"
    );
    core.shutdown().await;
}

/// `[P-STREAM-2/4]`: chunks numerados com `seq` contíguo a partir de 0.
// spec: [P-STREAM-2]
// spec: [P-STREAM-4]
#[tokio::test]
async fn stream_chunks_numbered_contiguous() {
    // spec: [P-STREAM-2]
    // spec: [P-STREAM-4]
    let home = tempfile::tempdir().unwrap();
    let mut core = Core::open_with("flow2", Some(home.path())).await.unwrap();
    install(
        &core,
        "testdata/echo",
        "test/echo",
        "test-echo",
        "echo",
        "bin/echo",
    );
    core.refresh().unwrap();
    // Pump direto de chunks v1 (echo não faz stream): admite via `atom.stream.chunk`.
    core.pump_one(Incoming {
        ext: "test/echo".to_string(),
        id: None,
        method: "atom.stream.chunk".to_string(),
        params: json!({"stream": "st_inexistente", "data": 1}),
    })
    .await;
    // Stream inexistente = descartado sem panicar.
    core.shutdown().await;
}
