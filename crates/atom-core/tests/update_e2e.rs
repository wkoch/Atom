//! Aceite v0.7: self-update com rollback, extensões por política,
//! migrações versionadas e `store gc`.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use atom_core::{bus::Core, pkg, store, update};
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

fn write_key(dir: &Path, name: &str, seed_b64: &str) -> PathBuf {
    let p = dir.join(format!("{name}.key"));
    std::fs::write(&p, format!("ed25519-seed:{seed_b64}\n")).unwrap();
    p
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap() {
        let e = e.unwrap();
        let to = dst.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &to);
        } else {
            std::fs::copy(e.path(), &to).unwrap();
        }
    }
}

fn sha_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Nome do executável `atom` na plataforma (`.bat` no Windows: sem `/bin/sh`).
fn cur_exe(bin: &Path) -> PathBuf {
    if cfg!(windows) {
        bin.join("atom.bat")
    } else {
        bin.join("atom")
    }
}

fn make_exe(path: &Path, healthy: bool) {
    // Windows não tem `/bin/sh`: gera `.bat` quando o path tem essa extensão.
    let is_bat = path.extension().and_then(|e| e.to_str()) == Some("bat");
    let body = if is_bat {
        if healthy {
            "@exit /b 0\r\n"
        } else {
            "@exit /b 1\r\n"
        }
    } else if healthy {
        "#!/bin/sh\nexit 0\n"
    } else {
        "#!/bin/sh\nexit 1\n"
    };
    std::fs::write(path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut p = std::fs::metadata(path).unwrap().permissions();
        p.set_mode(0o755);
        std::fs::set_permissions(path, p).unwrap();
    }
}

/// Loja file:// com `test/echo` numa versão. Retorna (v1, store_pub, store_key).
fn make_store(work: &Path, tag: &str, version: &str) -> (PathBuf, String, PathBuf) {
    let src = workspace_root().join("testdata/echo");
    let (store_pub, store_seed) = store::keygen();
    let (_pp, pub_seed) = store::keygen();
    let keys = work.join(format!("keys-{tag}"));
    std::fs::create_dir_all(&keys).unwrap();
    let store_key = write_key(&keys, "store", &store_seed);
    let pub_key = write_key(&keys, "pub", &pub_seed);
    let pkg_src = work.join(format!("pkgsrc-{tag}"));
    copy_dir(&src, &pkg_src);
    let manifest = pkg_src.join("atom.toml");
    let text = std::fs::read_to_string(&manifest)
        .unwrap()
        .replace("version = \"0.1.0\"", &format!("version = \"{version}\""));
    std::fs::write(&manifest, text).unwrap();
    let out = work.join(format!("out-{tag}"));
    std::fs::create_dir_all(&out).unwrap();
    let (_art, snip) = store::pack_package(&pkg_src, None, &pub_key, &out).unwrap();
    let v1 = work.join("v1");
    store::publish_to_store(&v1, &store_key, &[snip], Some(&out), false, false, false).unwrap();
    (v1, store_pub, store_key)
}

fn add_file_source(core: &mut Core, name: &str, v1: &Path, store_pub: &str) {
    core.config.sources.push(atom_core::profile::SourceCfg {
        name: name.to_string(),
        url: format!("file://{}/", v1.display()),
        keys: vec![store_pub.to_string()],
        priority: 0,
        root_sha256: None,
    });
    atom_core::profile::save(&core.paths, &core.config).unwrap();
}

/// Release falso do core em `v1/core/<canal>.json` + artefato tar.gz.
/// Retorna (v1, store_pub, release_version).
fn make_core_release(work: &Path, version: &str, healthy: bool) -> (PathBuf, String, PathBuf) {
    let (store_pub, store_seed) = store::keygen();
    let keys = work.join(format!("ckeys-{version}"));
    std::fs::create_dir_all(&keys).unwrap();
    let store_key = write_key(&keys, "store", &store_seed);
    // Artefato: tar.gz com um script executável.
    let plat = store::store_platform();
    let bindir = work.join(format!("cbits-{version}"));
    std::fs::create_dir_all(&bindir).unwrap();
    make_exe(&bindir.join("run.sh"), healthy);
    let tar_path = work.join(format!("core-{version}.tar.gz"));
    {
        let f = std::fs::File::create(&tar_path).unwrap();
        let gz = flate2::write::GzEncoder::new(f, flate2::Compression::default());
        let mut tar = tar::Builder::new(gz);
        tar.append_dir_all(".", &bindir).unwrap();
        tar.finish().unwrap();
    }
    let bytes = std::fs::read(&tar_path).unwrap();
    let sha = sha_hex(&bytes);
    // Papéis podem coincidir no começo (spec §4): publisher = chave da loja.
    let seed = store::load_seed(&store_key).unwrap();
    let sig = store::sign_with_seed(&seed, &update::core_artifact_message(version, &sha, &plat));
    let v1 = work.join("v1");
    let arts = v1.join("artifacts/core");
    std::fs::create_dir_all(&arts).unwrap();
    let fname = format!("atom-{version}-{plat}.tar.gz");
    std::fs::copy(&tar_path, arts.join(&fname)).unwrap();
    let rel: serde_json::Value = json!({
        "version": version,
        "published_at": "1",
        "min_upgrade_from": "0.0.0",
        "artifacts": [{
            "platform": plat, "url": format!("artifacts/core/{fname}"),
            "sha256": sha, "size": bytes.len(), "signature": sig,
        }],
    });
    let rel_dir = v1.join("v1/core");
    std::fs::create_dir_all(&rel_dir).unwrap();
    let raw = serde_json::to_vec_pretty(&rel).unwrap();
    std::fs::write(rel_dir.join("stable.json"), &raw).unwrap();
    let store_seed = store::load_seed(&store_key).unwrap();
    let sig = store::sign_with_seed(&store_seed, &raw);
    std::fs::write(rel_dir.join("stable.json.sig"), format!("{sig}\n")).unwrap();
    (v1, store_pub, store_key)
}

// ---------------------------------------------------------------------------
// Self-update do core.

#[tokio::test]
async fn release_check_verifies_sig_and_picks_newer() {
    let work = tempfile::tempdir().unwrap();
    let (v1, store_pub, _k) = make_core_release(work.path(), "99.0.0", true);
    let base = format!("file://{}/", v1.display());
    // Chave errada → recusa.
    let (other_pub, _) = store::keygen();
    assert!(
        update::fetch_release(&base, "stable", &[other_pub])
            .await
            .is_err()
    );
    // Chave certa → acha N+1 (maior que o core atual).
    let (rel, _) = update::fetch_release(&base, "stable", std::slice::from_ref(&store_pub))
        .await
        .unwrap();
    assert_eq!(rel.version, "99.0.0");
    let bad = std::collections::HashSet::new();
    let pick = update::pick_release(&rel, atom_core::CORE_VERSION, &bad).unwrap();
    assert_eq!(pick.platform, store::store_platform());
    // Versão marcada como ruim é pulada.
    let mut bad = std::collections::HashSet::new();
    bad.insert("99.0.0".to_string());
    assert!(update::pick_release(&rel, atom_core::CORE_VERSION, &bad).is_none());
}

#[tokio::test]
async fn defective_core_release_rolls_back() {
    let work = tempfile::tempdir().unwrap();
    // Núcleo "instalado": executável saudável.
    let bin = work.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let current = cur_exe(&bin);
    make_exe(&current, true);
    assert!(update::health_check(&current, &["version"], 10));

    // Aplica release defeituosa (exit 1): falha no health e restaura N.
    let (v1, store_pub, _k) = make_core_release(work.path(), "99.0.1", false);
    let base = format!("file://{}/", v1.display());
    let (rel, _) = update::fetch_release(&base, "stable", std::slice::from_ref(&store_pub))
        .await
        .unwrap();
    let bad = std::collections::HashSet::new();
    let art = update::pick_release(&rel, atom_core::CORE_VERSION, &bad).unwrap();
    let before = std::fs::read(&current).unwrap();
    let r = update::apply_core_release(
        &bin,
        &current,
        "0.1.0",
        &rel,
        &art,
        &base,
        &[store_pub],
        &["version"],
        10,
    )
    .await;
    assert!(r.is_err(), "defeito deveria falhar o health: {r:?}");
    assert_eq!(
        std::fs::read(&current).unwrap(),
        before,
        "N não foi restaurado"
    );
    // Versão ruim marcada: próxima checagem pula.
    let bad = update::bad_versions(&bin);
    assert!(bad.contains("99.0.1"), "faltou marca .bad: {bad:?}");
    assert!(update::pick_release(&rel, atom_core::CORE_VERSION, &bad).is_none());
}

#[tokio::test]
async fn pending_health_marker_heals_on_next_start() {
    let work = tempfile::tempdir().unwrap();
    let bin = work.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let current = cur_exe(&bin);
    make_exe(&current, true);
    let backup = bin.join(".backup-0.1.0");
    make_exe(&backup, true);
    // Simula boot que morreu após o swap, antes de confirmar saúde.
    std::fs::write(bin.join(".pending-health"), "99.0.9").unwrap();
    assert!(update::heal_if_needed(&bin, &current));
    assert!(!bin.join(".pending-health").exists());
    assert!(update::bad_versions(&bin).contains("99.0.9"));
    // Sem marcador: nada a fazer.
    assert!(!update::heal_if_needed(&bin, &current));
}

#[test]
fn prune_bins_keeps_two_newest() {
    let work = tempfile::tempdir().unwrap();
    let bin = work.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for v in ["0.1.0", "0.2.0", "0.3.0"] {
        std::fs::write(bin.join(format!("atom-{v}")), b"x").unwrap();
    }
    update::prune_bins(&bin, 2);
    assert!(!bin.join("atom-0.1.0").exists());
    assert!(bin.join("atom-0.2.0").exists());
    assert!(bin.join("atom-0.3.0").exists());
}

// ---------------------------------------------------------------------------
// Políticas de extensão.

/// Publica `version` de test/echo na MESMA loja (reusa store key).
/// Inclui o binário compilado (p/ reload funcionar) e extras no manifesto.
#[allow(clippy::too_many_arguments)]
fn publish_echo_version(
    work: &Path,
    tag: &str,
    version: &str,
    caps: &[&str],
    entrypoint: Option<&str>,
    v1: &Path,
    store_key: &Path,
) {
    let src = workspace_root().join("testdata/echo");
    let keys = work.join(format!("keys-{tag}"));
    std::fs::create_dir_all(&keys).unwrap();
    let (_pp, pub_seed) = store::keygen();
    let pub_key = write_key(&keys, "pub", &pub_seed);
    let pkg_src = work.join(format!("pkgsrc-{tag}"));
    copy_dir(&src, &pkg_src);
    let manifest = pkg_src.join("atom.toml");
    let mut text = std::fs::read_to_string(&manifest)
        .unwrap()
        .replace("version = \"0.1.0\"", &format!("version = \"{version}\""));
    if !caps.is_empty() {
        let list = caps
            .iter()
            .map(|c| format!("\"{c}\""))
            .collect::<Vec<_>>()
            .join(", ");
        // `capabilities` é topo do manifesto: insere antes do primeiro
        // cabeçalho de tabela, senão cairia na tabela errada (TOML).
        let line = format!("capabilities = [{list}]\n");
        text = match text.find("[[provides]]") {
            Some(i) => format!("{}{}\n{}", &text[..i], line.trim_end(), &text[i..]),
            None => format!("{text}\n{line}"),
        };
    }
    if let Some(ep) = entrypoint {
        text = text.replace(
            "entrypoint = \"bin/echo\"",
            &format!("entrypoint = \"{ep}\""),
        );
    }
    std::fs::write(&manifest, text).unwrap();
    let bindir = pkg_src.join("bin");
    std::fs::create_dir_all(&bindir).unwrap();
    std::fs::copy(echo_bin(), bindir.join("echo")).unwrap();
    let out = work.join(format!("out-{tag}"));
    std::fs::create_dir_all(&out).unwrap();
    let (_art, snip) = store::pack_package(&pkg_src, None, &pub_key, &out).unwrap();
    store::publish_to_store(v1, store_key, &[snip], Some(&out), false, false, false).unwrap();
}

async fn run_auto(core: &mut Core) -> update::UpdateReport {
    update::run_update(
        core,
        &update::UpdateInputs {
            base_url: None,
            keys: vec![],
            channel: "stable",
            check_only: false,
            current_exe: PathBuf::from("atom-test-dummy"),
            health_args: vec!["version".to_string()],
        },
    )
    .await
    .unwrap()
}

fn lock_version(core: &Core, pkg: &str) -> String {
    pkg::load_lock(&core.paths)
        .unwrap()
        .find(pkg)
        .unwrap()
        .version
        .clone()
}

#[tokio::test]
async fn auto_update_pauses_on_new_caps() {
    let work = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let (v1, store_pub, store_key) = make_store(work.path(), "a", "0.1.0");
    let mut core = Core::open_with("pol", Some(home.path())).await.unwrap();
    core.config.update.extensions = "auto".to_string();
    add_file_source(&mut core, "local", &v1, &store_pub);
    store::install_package(&mut core, "test/echo", None, None, false)
        .await
        .unwrap();
    link_bin(&core, "test/echo", &echo_bin());
    core.refresh().unwrap();
    assert_eq!(lock_version(&core, "test/echo"), "0.1.0");

    // 0.2.0 traz capability nova → pausa mesmo em `auto`.
    publish_echo_version(
        work.path(),
        "b",
        "0.2.0",
        &["service:test.other@0"],
        None,
        &v1,
        &store_key,
    );
    let rep = run_auto(&mut core).await;
    assert!(
        rep.ext_updated.is_empty(),
        "não devia auto-aplicar: {:?}",
        rep.ext_updated
    );
    assert!(rep.ext_pending_caps.iter().any(|(n, _)| n == "test/echo"));
    assert_eq!(lock_version(&core, "test/echo"), "0.1.0");
    // Aprova → aplica.
    core.approve_grants("test/echo", &["service:test.other@0".to_string()])
        .unwrap();
    let rep = run_auto(&mut core).await;
    assert!(
        rep.ext_updated
            .iter()
            .any(|(n, f, t)| n == "test/echo" && f == "0.1.0" && t == "0.2.0"),
        "{rep:?}"
    );
    assert_eq!(lock_version(&core, "test/echo"), "0.2.0");
    // Chamada real funciona na versão nova (reload hot ok).
    link_bin(&core, "test/echo", &echo_bin());
    let v = core
        .call("test.echo@0", "echo", json!({"x": 1}), None, None)
        .await
        .unwrap();
    assert_eq!(v, json!({"x": 1}));
    core.shutdown().await;
}

#[tokio::test]
async fn auto_update_applies_without_new_caps_and_skips_pinned() {
    let work = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let (v1, store_pub, store_key) = make_store(work.path(), "a", "0.1.0");
    let mut core = Core::open_with("pol2", Some(home.path())).await.unwrap();
    core.config.update.extensions = "auto".to_string();
    core.config.update.pinned = vec!["test/echo".to_string()];
    add_file_source(&mut core, "local", &v1, &store_pub);
    store::install_package(&mut core, "test/echo", None, None, false)
        .await
        .unwrap();
    link_bin(&core, "test/echo", &echo_bin());
    core.refresh().unwrap();
    // Pinned: nem prévia aparece.
    publish_echo_version(work.path(), "b", "0.2.0", &[], None, &v1, &store_key);
    let plans = update::preview_updates(&core).await.unwrap();
    assert!(plans.is_empty(), "pinned não entra no plano: {plans:?}");
    let rep = run_auto(&mut core).await;
    assert!(rep.ext_updated.is_empty());
    assert_eq!(lock_version(&core, "test/echo"), "0.1.0");
    // Desafixa: aplica sozinho (sem caps novas, sem prompt).
    core.config.update.pinned.clear();
    atom_core::profile::save(&core.paths, &core.config).unwrap();
    core.refresh().unwrap();
    let rep = run_auto(&mut core).await;
    assert!(
        rep.ext_updated
            .iter()
            .any(|(n, _, t)| n == "test/echo" && t == "0.2.0"),
        "{rep:?}"
    );
    assert_eq!(lock_version(&core, "test/echo"), "0.2.0");
    core.shutdown().await;
}

#[tokio::test]
async fn activation_failure_rolls_back() {
    let work = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let (v1, store_pub, store_key) = make_store(work.path(), "a", "0.1.0");
    let mut core = Core::open_with("rb", Some(home.path())).await.unwrap();
    core.config.update.extensions = "auto".to_string();
    core.config.journal.enabled = true;
    add_file_source(&mut core, "local", &v1, &store_pub);
    store::install_package(&mut core, "test/echo", None, None, false)
        .await
        .unwrap();
    link_bin(&core, "test/echo", &echo_bin());
    core.refresh().unwrap();
    // Sobe o echo: rollback testa hot swap real (para → ativa nova → falha).
    let v = core
        .call("test.echo@0", "echo", json!({"up": 1}), None, None)
        .await
        .unwrap();
    assert_eq!(v, json!({"up": 1}));
    // 0.3.0 com entrypoint inexistente: instala, mas ativar falha.
    publish_echo_version(
        work.path(),
        "c",
        "0.3.0",
        &[],
        Some("bin/missing"),
        &v1,
        &store_key,
    );
    let rep = run_auto(&mut core).await;
    assert!(
        rep.ext_updated.is_empty(),
        "falha devia impedir o update: {rep:?}"
    );
    assert!(
        rep.messages.iter().any(|m| m.contains("rollback")),
        "sem rollback: {:?}",
        rep.messages
    );
    assert_eq!(lock_version(&core, "test/echo"), "0.1.0");
    // Journal registra a falha de ativação.
    let journal = std::fs::read_to_string(core.paths.data_dir.join("journal/bus.jsonl")).unwrap();
    assert!(
        journal.contains("atom.extension.failed"),
        "sem evento no journal"
    );
    // Versão antiga continua chamável (reload do rollback já a subiu).
    let v = core
        .call("test.echo@0", "echo", json!({"z": 9}), None, None)
        .await
        .unwrap();
    assert_eq!(v, json!({"z": 9}));
    core.shutdown().await;
}

// ---------------------------------------------------------------------------
// Migrações e GC.

#[test]
fn migrate_stamps_versions_with_backup() {
    let home = tempfile::tempdir().unwrap();
    let atom_paths = atom_core::profile::AtomPaths::resolve_with(Some(home.path()));
    atom_paths.ensure().unwrap();
    let paths = atom_core::profile::ProfilePaths::new(&atom_paths, "mig").unwrap();
    paths.ensure().unwrap();
    // Formatos antigos: sem campos de versão (mas com campos obrigatórios).
    std::fs::write(&paths.profile_toml, "[bindings]\n").unwrap();
    std::fs::write(
        &paths.lock,
        "core = \"0.1.0\"\n[[package]]\nname = \"x/y\"\nversion = \"1.0.0\"\nsource = \"dev:x\"\nplatform = \"any\"\nsha256 = \"abc\"\n",
    )
    .unwrap();
    let done = update::migrate_state(&paths).unwrap();
    assert_eq!(done.len(), 2, "esperava 2 migrações: {done:?}");
    let cfg = atom_core::profile::load_or_create(&paths).unwrap();
    assert_eq!(cfg.profile, 1);
    // Segunda rodada: idempotente, nada a fazer.
    let done2 = update::migrate_state(&paths).unwrap();
    assert!(done2.is_empty());
    // Futuro desconhecido: erro claro pedindo upgrade do core.
    std::fs::write(&paths.lock, "lock = 99\ncore = \"0.1.0\"\n").unwrap();
    assert!(update::migrate_state(&paths).is_err());
}

#[test]
fn gc_removes_only_unreferenced_old_dirs() {
    let home = tempfile::tempdir().unwrap();
    let atom_paths = atom_core::profile::AtomPaths::resolve_with(Some(home.path()));
    atom_paths.ensure().unwrap();
    let paths = atom_core::profile::ProfilePaths::new(&atom_paths, "gc").unwrap();
    paths.ensure().unwrap();
    // Lock referencia test/echo@0.1.0 (<sha>).
    let dir = atom_paths
        .data
        .join("store/test/echo/0.1.0-deadbeefdeadbeef");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("atom.toml"), "x").unwrap();
    std::fs::write(
        &paths.lock,
        "lock = 1\ncore = \"0.1.0\"\n[[package]]\nname = \"test/echo\"\nversion = \"0.1.0\"\nsource = \"dev:x\"\nplatform = \"any\"\nsha256 = \"deadbeefdeadbeef\"\nrequested = \"0.1.0\"\n",
    )
    .unwrap();
    // Órfão recente: mantém (mtime novo, older_than alto).
    let orfan = atom_paths
        .data
        .join("store/test/old/9.9.9-aaaaaaaaaaaaaaaa");
    std::fs::create_dir_all(&orfan).unwrap();
    let stats = update::gc_store(&atom_paths, 3600 * 24 * 365).unwrap();
    assert_eq!(stats.removed_dirs, 0);
    assert!(orfan.exists());
    // Órfão com corte zero: remove.
    let stats = update::gc_store(&atom_paths, 0).unwrap();
    assert_eq!(stats.removed_dirs, 1, "stats: {stats:?}");
    assert!(!orfan.exists());
    assert!(dir.exists(), "referenciado não pode sumir");
}
