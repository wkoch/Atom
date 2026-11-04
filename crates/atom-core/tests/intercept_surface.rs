//! Interceptors fim a fim com `test/intercept-fake` (ACTION por fase):
//! continue/modify/respond/reject/bad + around + bypass sem grant +
//! chamadas aninhadas via `test/caller` (nested before/stream/proceed).

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

fn install(core: &Core, dir: &str, pkg: &str, cargo_pkg: &str, bin: &str, entry: &str) {
    let src = workspace_root().join(dir);
    let id = pkg::install_dev(&core.paths, &core.data_store, &src).unwrap();
    assert!(id.starts_with(pkg));
    let lock = pkg::load_lock(&core.paths).unwrap();
    let lp = lock.find(pkg).unwrap();
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

fn action_file(core: &Core) -> PathBuf {
    let lock = pkg::load_lock(&core.paths).unwrap();
    let lp = lock.find("test/intercept-fake").unwrap();
    pkg::package_dir(&core.data_store, lp)
        .unwrap()
        .join("action")
}

fn set_action(core: &Core, action: &str) {
    // O fake lê `./action` (cwd = raiz do pacote) a cada mensagem.
    std::fs::write(action_file(core), action).unwrap();
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
async fn interceptor_phases() {
    let home = tempfile::tempdir().unwrap();
    let mut core = Core::open_with("default", Some(home.path())).await.unwrap();
    install(
        &core,
        "testdata/echo",
        "test/echo",
        "test-echo",
        "echo",
        "bin/echo",
    );
    install(
        &core,
        "testdata/intercept-fake",
        "test/intercept-fake",
        "test-intercept-fake",
        "intercept-fake",
        "bin/intercept-fake",
    );
    core.refresh().unwrap();
    core.approve_grants(
        "test/intercept-fake",
        &["intercept:test.echo@0".to_string()],
    )
    .unwrap();

    // continue: passa direto.
    set_action(&core, "continue");
    let v = core
        .call("test.echo@0", "echo", json!({"x": 1}), None, None)
        .await
        .unwrap();
    assert_eq!(v, json!({"x": 1}));
    // modify: params reescritos pelo interceptor.
    set_action(&core, "modify");
    let v = core
        .call("test.echo@0", "echo", json!({"x": 1}), None, None)
        .await
        .unwrap();
    assert_eq!(v, json!({"x": 99}));
    // respond: curto-circuito, provider nem roda.
    set_action(&core, "respond");
    let v = core
        .call("test.echo@0", "echo", json!({"x": 1}), None, None)
        .await
        .unwrap();
    assert_eq!(v, json!({"fake": true}));
    // reject: erro -32020 do interceptor.
    set_action(&core, "reject");
    let e = core
        .call("test.echo@0", "echo", json!({"x": 1}), None, None)
        .await
        .unwrap_err();
    assert_eq!(e.code(), -32020);
    // bad: ação inválida → erro de params.
    set_action(&core, "bad");
    assert!(
        core.call("test.echo@0", "echo", json!({"x": 1}), None, None)
            .await
            .is_err()
    );

    // around via registro temporário: continue executa o resto (auto-proceed).
    set_action(&core, "continue");
    core.pump_one(inc(
        "test/intercept-fake",
        "atom.register",
        json!({"intercepts": [
            {"match": "test.echo@0/*", "phase": "around", "priority": 5},
            {"match": "test.echo@0/*", "phase": "after", "priority": 5},
        ]}),
    ))
    .await;
    let v = core
        .call("test.echo@0", "echo", json!({"x": 2}), None, None)
        .await
        .unwrap();
    assert_eq!(v, json!({"x": 2}));
    // around + reject: bloqueia.
    set_action(&core, "reject");
    let e = core
        .call("test.echo@0", "echo", json!({"x": 2}), None, None)
        .await
        .unwrap_err();
    assert_eq!(e.code(), -32020);
    // target explícito + around: provider fixo (run_arounds_with_provider).
    set_action(&core, "continue");
    let v = core
        .call(
            "test.echo@0",
            "echo",
            json!({"x": 2}),
            Some("test/echo"),
            None,
        )
        .await
        .unwrap();
    assert_eq!(v, json!({"x": 2}));
    // via-proceed: interceptor chama atom.proceed, inner executa.
    set_action(&core, "via-proceed");
    let v = core
        .call("test.echo@0", "echo", json!({"x": 4}), None, None)
        .await
        .unwrap();
    assert_eq!(v, json!({"x": 4}));

    // Sem grant, interceptor é pulado (warn) e a chamada passa.
    set_action(&core, "reject");
    core.deny_grants(
        "test/intercept-fake",
        &["intercept:test.echo@0".to_string()],
    )
    .unwrap();
    let v = core
        .call("test.echo@0", "echo", json!({"x": 3}), None, None)
        .await
        .unwrap();
    assert_eq!(v, json!({"x": 3}));
    core.shutdown().await;
}

#[tokio::test]
async fn nested_calls_via_caller() {
    let home = tempfile::tempdir().unwrap();
    let mut core = Core::open_with("default", Some(home.path())).await.unwrap();
    install(
        &core,
        "testdata/echo",
        "test/echo",
        "test-echo",
        "echo",
        "bin/echo",
    );
    install(
        &core,
        "testdata/caller",
        "test/caller",
        "test-caller",
        "caller",
        "bin/caller",
    );
    install(
        &core,
        "testdata/fake-llm",
        "test/fake-llm",
        "test-fake-llm",
        "fake-llm",
        "bin/fake-llm",
    );
    install(
        &core,
        "testdata/intercept-fake",
        "test/intercept-fake",
        "test-intercept-fake",
        "intercept-fake",
        "bin/intercept-fake",
    );
    core.refresh().unwrap();
    core.approve_grants(
        "test/caller",
        &[
            "service:test.echo@0".to_string(),
            "service:ai.llm@0".to_string(),
        ],
    )
    .unwrap();
    core.approve_grants(
        "test/intercept-fake",
        &["intercept:test.echo@0".to_string()],
    )
    .unwrap();
    let relay = |method: &str, params: serde_json::Value| json!({"service": "test.echo@0", "method": method, "params": params});
    // before/modify no nested: ecoa params reescritos.
    set_action(&core, "modify");
    let v = core
        .call(
            "test.caller@0",
            "call",
            relay("echo", json!({"x": 1})),
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(v, json!({"x": 99}));
    // before/respond no nested: resposta imediata de stream rejeitado.
    set_action(&core, "respond");
    let v = core
        .call(
            "test.caller@0",
            "call",
            relay("echo", json!({})),
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(v["stream"], json!("rejected"));
    // before/reject no nested: erro propaga.
    set_action(&core, "reject");
    assert!(
        core.call(
            "test.caller@0",
            "call",
            relay("echo", json!({})),
            None,
            None
        )
        .await
        .is_err()
    );
    // Stream aninhado (provedor com streaming: dirige até o `end`).
    set_action(&core, "continue");
    core.pump_one(Incoming {
        ext: "test/caller".to_string(),
        id: None,
        method: "atom.call".to_string(),
        params: json!({"service": "ai.llm@0", "method": "complete", "params": {"messages": []}, "stream": true}),
    })
    .await;
    // Proceed com provider que falha: erro do inner volta ao interceptor.
    core.pump_one(inc(
        "test/intercept-fake",
        "atom.register",
        json!({"intercepts": [{"match": "test.echo@0/*", "phase": "around", "priority": 5}]}),
    ))
    .await;
    set_action(&core, "via-proceed");
    let e = core
        .call("test.echo@0", "echo", json!({}), Some("nope/x"), None)
        .await
        .unwrap_err();
    assert!(
        matches!(
            e,
            atom_core::error::CoreError::ExtensionUnavailable(_)
                | atom_core::error::CoreError::NoProvider(_)
        ),
        "{e:?}"
    );
    core.shutdown().await;
}
