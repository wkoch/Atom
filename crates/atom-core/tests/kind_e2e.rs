//! Aceite v0.4: kinds, skill, mcp (stdio+http), runtime-python, dormant.
//! Declarativos sem handler ficam `dormant` sem erro.

use std::path::{Path, PathBuf};

use atom_core::{bus::Core, pkg};
use serde_json::{Value, json};

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

/// Copia binário auxiliar para caminho relativo do pacote (ex.: MCP stdio).
fn link_file(core: &Core, package: &str, rel: &str, src_bin: &Path) {
    let lock = pkg::load_lock(&core.paths).unwrap();
    let lp = lock.find(package).unwrap();
    let dir = pkg::package_dir(&core.data_store, lp).unwrap();
    let dst = dir.join(rel);
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

async fn install_exec(
    core: &mut Core,
    src: &str,
    pkg_name: &str,
    bin_name: &str,
    crate_name: &str,
) {
    let full = workspace_root().join(src);
    let id = pkg::install_dev(&core.paths, &core.data_store, &full).unwrap();
    assert!(id.starts_with(pkg_name), "id {id} != {pkg_name}@");
    link_bin(core, pkg_name, &bin(bin_name, crate_name));
    core.refresh().unwrap();
}

fn install_data(core: &mut Core, src: &str, pkg_name: &str) {
    let full = workspace_root().join(src);
    let id = pkg::install_dev(&core.paths, &core.data_store, &full).unwrap();
    assert!(id.starts_with(pkg_name), "id {id} != {pkg_name}@");
    core.refresh().unwrap();
}

#[tokio::test]
async fn dormant_without_handler_is_not_an_error() {
    let home = tempfile::tempdir().unwrap();
    let mut core = Core::open_with("default", Some(home.path())).await.unwrap();
    install_data(&mut core, "testdata/kind-xyz", "test/kind-xyz");
    core.ensure_kinds().await.unwrap();
    assert_eq!(core.kind_state("test/kind-xyz").as_deref(), Some("dormant"));
    core.shutdown().await;
}

#[tokio::test]
async fn bootstrap_kinds_load_in_core() {
    let home = tempfile::tempdir().unwrap();
    let mut core = Core::open_with("default", Some(home.path())).await.unwrap();
    install_data(
        &mut core,
        "extensions/contracts/ai-tools",
        "atom/contract-ai-tools",
    );
    install_data(
        &mut core,
        "extensions/runtime-python",
        "atom/runtime-python",
    );
    core.ensure_kinds().await.unwrap();
    assert_eq!(
        core.kind_state("atom/contract-ai-tools").as_deref(),
        Some("core")
    );
    assert_eq!(
        core.kind_state("atom/runtime-python").as_deref(),
        Some("core")
    );
    core.shutdown().await;
}

#[tokio::test]
async fn skill_loads_and_collects_in_chat_context() {
    let home = tempfile::tempdir().unwrap();
    let mut core = Core::open_with("default", Some(home.path())).await.unwrap();
    install_data(
        &mut core,
        "extensions/contracts/ai-context",
        "atom/contract-ai-context",
    );
    install_exec(
        &mut core,
        "extensions/skill-loader",
        "atom/skill-loader",
        "skill-loader",
        "atom-skill-loader",
    )
    .await;
    install_data(&mut core, "testdata/skill-fake", "test/skill-demo");
    core.ensure_kinds().await.unwrap();
    assert!(
        core.kind_state("test/skill-demo")
            .unwrap_or_default()
            .starts_with("loaded:")
    );
    core.config.validation.contracts = true;
    let v: Value = core
        .call(
            "ai.context@0",
            "collect",
            json!({"session": "s", "input": "oi"}),
            None,
            None,
        )
        .await
        .unwrap();
    let text = serde_json::to_string(&v).unwrap();
    assert!(
        text.contains("demo"),
        "skill não apareceu no contexto: {text}"
    );
    core.shutdown().await;
}

#[tokio::test]
async fn mcp_stdio_lists_and_executes() {
    let home = tempfile::tempdir().unwrap();
    let mut core = Core::open_with("default", Some(home.path())).await.unwrap();
    install_data(
        &mut core,
        "extensions/contracts/ai-tools",
        "atom/contract-ai-tools",
    );
    install_exec(
        &mut core,
        "extensions/mcp-bridge",
        "atom/mcp-bridge",
        "mcp-bridge",
        "atom-mcp-bridge",
    )
    .await;
    install_data(&mut core, "testdata/mcp-fake", "test/mcp-fake");
    // Binário do servidor MCP falso vai junto do pacote declarativo.
    link_file(
        &core,
        "test/mcp-fake",
        "bin/mcp-fake",
        &bin("mcp-fake", "test-mcp-fake"),
    );
    core.refresh().unwrap();
    core.ensure_kinds().await.unwrap();
    assert!(
        core.kind_state("test/mcp-fake")
            .unwrap_or_default()
            .starts_with("loaded:")
    );
    let tools: Value = core
        .call("ai.tools@0", "list", json!({}), None, None)
        .await
        .unwrap();
    let names: Vec<String> = tools
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| {
            t.get("name")
                .and_then(|n| n.as_str())
                .map(|s| s.to_string())
        })
        .collect();
    assert!(
        names.iter().any(|n| n.ends_with("__hello")),
        "tools: {names:?}"
    );
    let tool = names
        .iter()
        .find(|n| n.ends_with("__hello"))
        .unwrap()
        .clone();
    let r: Value = core
        .call(
            "ai.tools@0",
            "execute",
            json!({"name": tool, "input": {"name": " Ada "}}),
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(r.get("is_error"), Some(&json!(false)));
    assert!(serde_json::to_string(&r).unwrap().contains("Ada"));
    core.shutdown().await;
}

#[tokio::test]
async fn mcp_http_lists_and_executes() {
    let home = tempfile::tempdir().unwrap();
    let mut core = Core::open_with("default", Some(home.path())).await.unwrap();
    install_data(
        &mut core,
        "extensions/contracts/ai-tools",
        "atom/contract-ai-tools",
    );
    install_exec(
        &mut core,
        "extensions/mcp-bridge",
        "atom/mcp-bridge",
        "mcp-bridge",
        "atom-mcp-bridge",
    )
    .await;
    // Sobe MCP falso em HTTP (porta efêmera via SO_REUSE? fixa alta p/ teste).
    let port = 18377u16;
    let mut child = std::process::Command::new(bin("mcp-fake", "test-mcp-fake"))
        .args(["--http", &port.to_string()])
        .spawn()
        .expect("spawn mcp-fake http");
    // Pacote MCP gerado apontando ao servidor local.
    let pkgdir = tempfile::tempdir().unwrap();
    std::fs::write(
        pkgdir.path().join("atom.toml"),
        format!("manifest = 1\nname = \"test/mcp-http\"\nversion = \"0.1.0\"\nkind = \"mcp\"\ndescription = \"MCP http de teste\"\nlicense = \"MIT\"\nengine = \">=0.1.0\"\n\n[contributes.mcp]\ntransport = \"http\"\nurl = \"http://127.0.0.1:{port}/\"\n"),
    )
    .unwrap();
    let id = pkg::install_dev(&core.paths, &core.data_store, pkgdir.path()).unwrap();
    assert!(id.starts_with("test/mcp-http"));
    core.refresh().unwrap();
    core.ensure_kinds().await.unwrap();
    // Espera servidor subir (poll curto).
    let mut tools: Value = json!([]);
    for _ in 0..50 {
        match core.call("ai.tools@0", "list", json!({}), None, None).await {
            Ok(v) => {
                tools = v;
                break;
            }
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
        }
    }
    let names: Vec<String> = tools
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| {
            t.get("name")
                .and_then(|n| n.as_str())
                .map(|s| s.to_string())
        })
        .collect();
    assert!(
        names.iter().any(|n| n.ends_with("__hello")),
        "http tools: {names:?}"
    );
    let _ = child.kill();
    let _ = child.wait();
    core.shutdown().await;
}

#[tokio::test]
async fn runtime_python_runs_extension() {
    let home = tempfile::tempdir().unwrap();
    let mut core = Core::open_with("default", Some(home.path())).await.unwrap();
    install_data(
        &mut core,
        "extensions/runtime-python",
        "atom/runtime-python",
    );
    install_data(&mut core, "testdata/py-echo", "test/py-echo");
    core.ensure_kinds().await.unwrap();
    let v: Value = core
        .call("test.pyecho@0", "echo", json!({"x": 42}), None, None)
        .await
        .unwrap();
    assert_eq!(v, json!({"x": 42}));
    core.shutdown().await;
}
