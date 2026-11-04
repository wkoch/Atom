#![forbid(unsafe_code)]
//! Aceite v0.3: guard-shell bloqueia/permite sem mudar tools-shell.
//! + eventos pub/sub + validação de contrato (-32013).

use std::path::{Path, PathBuf};

use atom_core::{bus::Core, pkg};
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
    let ep = m.entrypoint.unwrap();
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
    // Testes = CLI não-TTY: concede o declarado (mesma regra do `approve_on_install`).
    let missing = core.missing_grants(pkg_name);
    if !missing.is_empty() {
        core.approve_grants(pkg_name, &missing).unwrap();
        core.refresh().unwrap();
    }
}

/// Instala pacote declarativo (contrato): sem binário, só lock + refresh.
fn install_contract(core: &mut Core, src: &str, pkg_name: &str) {
    let full = workspace_root().join(src);
    let id = pkg::install_dev(&core.paths, &core.data_store, &full).unwrap();
    assert!(id.starts_with(pkg_name), "id {id} != {pkg_name}@");
    core.refresh().unwrap();
}

#[tokio::test]
async fn guard_blocks_and_permits() {
    let home = tempfile::tempdir().unwrap();
    let mut core = Core::open_with("default", Some(home.path())).await.unwrap();
    install(
        &mut core,
        "extensions/tools-shell",
        "atom/tools-shell",
        "tools-shell",
        "atom-tools-shell",
    )
    .await;
    install(
        &mut core,
        "extensions/guard-shell",
        "atom/guard-shell",
        "guard-shell",
        "atom-guard-shell",
    )
    .await;
    install(
        &mut core,
        "testdata/prompt-fake",
        "test/prompt-fake",
        "prompt-fake",
        "test-prompt-fake",
    )
    .await;

    // Permite: detalhe sem marcador de bloqueio → prompt aprova.
    let v = core
        .call(
            "ai.tools@0",
            "execute",
            json!({"name": "shell_exec", "input": {"command": "echo hi"}}),
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(v.get("is_error"), Some(&json!(false)));

    // Bloqueia: detalhe com BLOCKME → prompt nega → -32020, sem tocar no provider.
    let e = core
        .call(
            "ai.tools@0",
            "execute",
            json!({"name": "shell_exec", "input": {"command": "echo BLOCKME"}}),
            None,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(e.code(), -32020, "esperava Rejected, deu {e}");
    core.shutdown().await;
}

#[tokio::test]
async fn contract_validation_rejects_invalid_params() {
    let home = tempfile::tempdir().unwrap();
    let mut core = Core::open_with("default", Some(home.path())).await.unwrap();
    install(
        &mut core,
        "extensions/tools-shell",
        "atom/tools-shell",
        "tools-shell",
        "atom-tools-shell",
    )
    .await;
    install_contract(
        &mut core,
        "extensions/contracts/ai-tools",
        "atom/contract-ai-tools",
    );
    core.config.validation.contracts = true;

    // Params válidos passam mesmo com validação ligada.
    let v = core
        .call("ai.tools@0", "list", json!({}), None, None)
        .await
        .unwrap();
    assert!(v.is_array());

    // Params inválidos (sem name/input) → -32013.
    let e = core
        .call("ai.tools@0", "execute", json!({}), None, None)
        .await
        .unwrap_err();
    assert_eq!(
        e.code(),
        -32013,
        "esperava ContractValidationFailed, deu {e}"
    );
    core.shutdown().await;
}

#[tokio::test]
async fn events_publish_subscribe_roundtrip() {
    let home = tempfile::tempdir().unwrap();
    let mut core = Core::open_with("default", Some(home.path())).await.unwrap();
    // Sem assinantes = no-op, sem erro.
    let ctx = atom_protocol::CallerCtx::local("default", "core");
    core.publish_event("atom.test.ping", json!({"n": 1}), &ctx)
        .await
        .unwrap();
    // Registro temporário via API (cobre atom.register).
    core.registry.add_provider("test.echo@0", "test/echo");
    core.registry
        .add_subscription("test/echo", "atom.test.*")
        .unwrap();
    core.publish_event("atom.test.ping", json!({"n": 2}), &ctx)
        .await
        .unwrap();
    core.shutdown().await;
}
