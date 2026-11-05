//! Ciclo de vida do Core com echo + validação de contratos (`@0`).

use std::path::{Path, PathBuf};

use atom_core::{bus::Core, pkg};
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

fn install(core: &Core, dir: &Path, name: &str, bin: Option<(&str, &str, &str)>) {
    let id = pkg::install_dev(&core.paths, &core.data_store, dir).unwrap();
    assert!(id.starts_with(name));
    if let Some((cargo_pkg, bin_name, entry)) = bin {
        let lock = pkg::load_lock(&core.paths).unwrap();
        let lp = lock.find(name).unwrap();
        let pdir = pkg::package_dir(&core.data_store, lp).unwrap();
        let dst = pdir.join(entry);
        std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
        std::fs::copy(build_bin(cargo_pkg, bin_name), &dst).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut p = std::fs::metadata(&dst).unwrap().permissions();
            p.set_mode(0o755);
            std::fs::set_permissions(&dst, p).unwrap();
        }
    }
}

#[tokio::test]
async fn invalid_profile_and_lifecycle() {
    let home = tempfile::tempdir().unwrap();
    assert!(
        Core::open_with("NOME INVALIDO!", Some(home.path()))
            .await
            .is_err()
    );
    let mut core = Core::open_with("default", Some(home.path())).await.unwrap();
    install(
        &core,
        &workspace_root().join("testdata/echo"),
        "test/echo",
        Some(("test-echo", "echo", "bin/echo")),
    );
    core.refresh().unwrap();
    let started = core.boot().await.unwrap();
    assert!(started.is_empty());
    core.start_package("test/echo").await.unwrap();
    // Já rodando: idempotente.
    core.start_package("test/echo").await.unwrap();
    let v = core
        .call("test.echo@0", "echo", json!({"x": 1}), None, None)
        .await
        .unwrap();
    assert_eq!(v, json!({"x": 1}));
    core.stop_package("test/echo").await;
    core.reload_package("test/echo").await.unwrap();
    let v = core
        .call("test.echo@0", "echo", json!({"x": 2}), None, None)
        .await
        .unwrap();
    assert_eq!(v, json!({"x": 2}));
    assert!(core.remove_package("test/echo").await.unwrap());
    assert!(!core.remove_package("test/echo").await.unwrap());
    assert!(
        core.call("test.echo@0", "echo", json!({}), None, None)
            .await
            .is_err()
    );
    core.shutdown().await;
}

#[tokio::test]
async fn stream_success_collects_chunks() {
    use atom_core::bus::StreamCall;

    let home = tempfile::tempdir().unwrap();
    let mut core = Core::open_with("default", Some(home.path())).await.unwrap();
    install(
        &core,
        &workspace_root().join("testdata/fake-llm"),
        "test/fake-llm",
        Some(("test-fake-llm", "fake-llm", "bin/fake-llm")),
    );
    core.refresh().unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(256);
    let cancel = std::sync::Arc::new(tokio::sync::Notify::new());
    let r = core
        .call_stream_chan(
            StreamCall {
                id: atom_protocol::new_stream_id(),
                service: "ai.llm@0".to_string(),
                method: "complete".to_string(),
                params: json!({"messages": [{"role": "user", "content": "oi"}]}),
                target: None,
                ctx: None,
                flow: None,
            },
            tx,
            cancel,
        )
        .await
        .unwrap();
    assert!(r.get("message").is_some());
    let mut chunks = 0;
    let mut saw_end = false;
    while let Ok(ev) = rx.try_recv() {
        match ev.method.as_str() {
            "atom.stream.chunk" => chunks += 1,
            "atom.stream.end" => saw_end = true,
            _ => {}
        }
    }
    assert!(chunks >= 1 && saw_end);
    core.shutdown().await;
}

#[tokio::test]
async fn contract_validation() {
    let home = tempfile::tempdir().unwrap();
    let mut core = Core::open_with("default", Some(home.path())).await.unwrap();
    install(
        &core,
        &workspace_root().join("testdata/echo"),
        "test/echo",
        Some(("test-echo", "echo", "bin/echo")),
    );
    // Contrato fake p/ test.echo@0: params exigem x numérico; result exige y.
    let cdir = home.path().join("contract");
    std::fs::create_dir_all(cdir.join("schemas")).unwrap();
    std::fs::write(
        cdir.join("atom.toml"),
        "manifest = 1\nname = \"test/contract-echo\"\nversion = \"0.1.0\"\nkind = \"contract\"\nengine = \">=0.1.0\"\nprotocol = \"atom/1\"\n[contributes.contract]\nid = \"test.echo@0\"\nversion = \"0.1\"\nschema = \"schemas/echo.json\"\n",
    )
    .unwrap();
    std::fs::write(
        cdir.join("schemas/echo.json"),
        json!({"methods": {"echo": {
            "params": {"type": "object", "properties": {"x": {"type": "number"}}, "required": ["x"]},
            "result": {"type": "object", "required": ["y"]},
        }}}).to_string(),
    )
    .unwrap();
    install(&core, &cdir, "test/contract-echo", None);
    core.refresh().unwrap();
    // Sem flag: sem validação.
    core.call("test.echo@0", "echo", json!({"o que for": 1}), None, None)
        .await
        .unwrap();
    core.config.validation.contracts = true;
    // Params válidos, result sem `y` → falha no result.
    let e = core
        .call("test.echo@0", "echo", json!({"x": 1}), None, None)
        .await
        .unwrap_err();
    assert_eq!(
        e.code(),
        atom_protocol::ErrorCode::CONTRACT_VALIDATION_FAILED
    );
    // Params inválidos → falha nos params.
    let e = core
        .call("test.echo@0", "echo", json!({"x": "texto"}), None, None)
        .await
        .unwrap_err();
    assert_eq!(
        e.code(),
        atom_protocol::ErrorCode::CONTRACT_VALIDATION_FAILED
    );
    // atom.* pula validação mesmo com flag.
    core.pump_one(atom_core::host::Incoming {
        ext: "e/x".to_string(),
        id: None,
        method: "atom.registry.list".to_string(),
        params: json!({}),
    })
    .await;
    core.shutdown().await;
}
