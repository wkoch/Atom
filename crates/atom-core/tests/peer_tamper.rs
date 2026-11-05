//! Tamper de grants/config + gate de peer descendente (ADR-019, v1.1).
//!
//! - `[P-CONN-2]`/`[C-AMB-1]`: peer descendente do daemon não é cliente confiável.
//! - `[C-AMB-2]`: `grants.toml` externo divergente é ignorado (vale `.core`) + flag p/ `atom.grants.tampered`.
//! - `[C-AMB-3]`: campos de segurança de `profile.toml` com diff detectável p/ `atom.config.tampered`.
//!
//! O bus publica os eventos a partir das flags retornadas aqui (este teste cobre a
//! detecção; a publicação é do dono do `bus.rs`).

use atom_core::profile::{
    GrantEntry, GrantsFile, ProfilePaths, load_grants, load_grants_checked,
    reload_grants_from_disk, save_grants, security_fields_changed, security_fields_diff,
};

fn test_paths(t: &tempfile::TempDir) -> ProfilePaths {
    ProfilePaths {
        name: "dev".to_string(),
        dir: t.path().join("cfg"),
        profile_toml: t.path().join("cfg/profile.toml"),
        lock: t.path().join("cfg/atom.lock"),
        lock_prev: t.path().join("cfg/prev"),
        grants: t.path().join("cfg/grants.toml"),
        data_dir: t.path().join("data"),
        kv_dir: t.path().join("data/kv"),
        socket: t.path().join("atom-dev.sock"),
        pid_file: t.path().join("atom-dev.pid"),
        serve_token: t.path().join("data/serve.token"),
    }
}

fn sample_grants() -> GrantsFile {
    let mut g = GrantsFile::new();
    g.insert(
        "test/echo".to_string(),
        GrantEntry {
            granted: vec!["service:test.echo@0".to_string()],
            denied: vec![],
            approved_at: "1".to_string(),
        },
    );
    g
}

// spec: [P-CONN-2]
#[test]
fn descendant_walk_with_synthetic_pids() {
    // Árvore sintética: 1 <- 100 (daemon) <- 200 <- 300 (peer); 400 é alheio.
    let table = std::collections::HashMap::from([
        (100u32, "x (d) S 1 ".to_string()),
        (200u32, "y (w) S 100 ".to_string()),
        (300u32, "zzz (a b) (c) S 200 ".to_string()),
        (400u32, "q (q) S 1 ".to_string()),
    ]);
    let read = |p: u32| table.get(&p).cloned();
    assert!(atom_core::daemon::is_descendant_with(100, 100, read));
    assert!(atom_core::daemon::is_descendant_with(200, 100, read));
    // `comm` com parênteses/espaços não quebra o parse do ppid.
    assert!(atom_core::daemon::is_descendant_with(300, 100, read));
    assert!(!atom_core::daemon::is_descendant_with(400, 100, read));
    // Pid desconhecido no meio do caminho = sem info = confiável (false).
    assert!(!atom_core::daemon::is_descendant_with(999, 100, read));
}

// spec: [P-CONN-2]
#[test]
fn untrusted_peer_gate_semantics() {
    // Sem info de peer (`None` = sem SO_PEERCRED) → confiável.
    assert!(!atom_core::daemon::untrusted_peer(1234, None));
    // Peer = próprio daemon → descendente (recusa).
    assert!(atom_core::daemon::untrusted_peer(1234, Some(1234)));
    // Sem info nunca recusa, mesmo com pid real.
    assert!(atom_core::daemon::peer_pid_fallback().is_none());
}

// spec: [C-AMB-2]
#[test]
fn grants_mirror_and_external_edit_ignored() {
    let t = tempfile::tempdir().unwrap();
    let p = test_paths(&t);
    save_grants(&p, &sample_grants()).unwrap();
    // Espelho `.core` existe com header sha256.
    let core = std::fs::read_to_string(p.grants_core()).unwrap();
    assert!(core.starts_with("# sha256:"), "{core}");
    // Edição externa: injeta grant inexistente direto no arquivo.
    std::fs::write(
        &p.grants,
        "[evil]\ngranted = [\"service:*@0\"]\ndenied = []\napproved_at = \"9\"\n",
    )
    .unwrap();
    let checked = load_grants_checked(&p);
    // spec: [C-AMB-2] — externo ignorado (vale o `.core`), flag p/ `atom.grants.tampered`.
    assert!(checked.tampered);
    assert!(!checked.grants.contains_key("evil"));
    assert!(checked.grants.contains_key("test/echo"));
    // Compat: `load_grants` retorna os grants efetivos (do `.core`).
    assert_eq!(load_grants(&p), checked.grants);
    // Reload por cliente confiável adota o externo e limpa a flag.
    let adopted = reload_grants_from_disk(&p).unwrap();
    assert!(adopted.contains_key("evil"));
    assert!(!load_grants_checked(&p).tampered);
}

// spec: [C-AMB-2]
#[test]
fn grants_deleted_or_missing_core() {
    let t = tempfile::tempdir().unwrap();
    let p = test_paths(&t);
    // Perfil antigo sem `.core`: confia no externo (migração), sem flag.
    std::fs::create_dir_all(p.grants.parent().unwrap()).unwrap();
    std::fs::write(
        &p.grants,
        "[a]\ngranted = []\ndenied = []\napproved_at = \"1\"\n",
    )
    .unwrap();
    let checked = load_grants_checked(&p);
    assert!(!checked.tampered);
    assert!(checked.grants.contains_key("a"));
    // grants.toml apagado com `.core` presente: vale o core, sem flag de tamper.
    save_grants(&p, &sample_grants()).unwrap();
    std::fs::remove_file(&p.grants).unwrap();
    let checked = load_grants_checked(&p);
    assert!(!checked.tampered);
    assert!(checked.grants.contains_key("test/echo"));
}

// spec: [C-AMB-3]
#[test]
fn security_fields_change_detection() {
    use atom_core::profile::ProfileConfig;
    let old = ProfileConfig::default();
    // Só `[config.*]` mudou → sem evento.
    let mut only_cfg = ProfileConfig::default();
    only_cfg
        .config
        .insert("k".to_string(), toml::Value::from("v"));
    assert!(!security_fields_changed(&old, &only_cfg));
    assert!(security_fields_diff(&old, &only_cfg).is_empty());
    // `bindings`/`update` mudaram → evento com os campos.
    let mut evil = ProfileConfig::default();
    evil.bindings.insert("c".to_string(), "s".to_string());
    evil.update.core = "auto".to_string();
    assert!(security_fields_changed(&old, &evil));
    let fields = security_fields_diff(&old, &evil);
    assert!(fields.contains(&"bindings".to_string()));
    assert!(fields.contains(&"update".to_string()));
    assert!(!fields.contains(&"sources".to_string()));
}
