//! Aceite v0.5: publica na loja (file://), instala em perfil limpo,
//! adultera 1 byte → instalação recusada. + search/info/update/rollback
//! + verificação em 4 passos (sig índice, sha, sig publisher, manifesto).

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use atom_core::{bus::Core, pkg, profile, store};
use serde_json::json;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn echo_bin() -> PathBuf {
    let b = workspace_root().join("target/debug/echo");
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

/// Sobe loja file:// mínima com `test/echo` publicado. Retorna (v1dir, store_pub).
fn make_store(work: &Path, src_dir: &Path, version: &str) -> (PathBuf, String, PathBuf) {
    let (store_pub, store_seed) = store::keygen();
    let (pub_pub, pub_seed) = store::keygen();
    let _ = pub_pub;
    let keys = work.join("keys");
    std::fs::create_dir_all(&keys).unwrap();
    let store_key = write_key(&keys, "store", &store_seed);
    let pub_key = write_key(&keys, "pub", &pub_seed);
    // Copia pacote com versão desejada.
    let pkg_src = work.join(format!("pkgsrc-{}", version.replace('.', "_")));
    copy_dir(src_dir, &pkg_src);
    let manifest = pkg_src.join("atom.toml");
    let mut text = std::fs::read_to_string(&manifest).unwrap();
    text = text.replace("version = \"0.1.0\"", &format!("version = \"{version}\""));
    std::fs::write(&manifest, text).unwrap();
    let out = work.join(format!("out-{}", version.replace('.', "_")));
    std::fs::create_dir_all(&out).unwrap();
    let (_art, snip) = store::pack_package(&pkg_src, None, &pub_key, &out).unwrap();
    let v1 = work.join("v1");
    store::publish_to_store(&v1, &store_key, &[snip], Some(&out), false, false, false).unwrap();
    (v1, store_pub, store_key)
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

fn add_file_source(core: &mut Core, name: &str, v1: &Path, store_pub: &str) {
    core.config.sources.push(profile::SourceCfg {
        name: name.to_string(),
        url: format!("file://{}/", v1.display()),
        keys: vec![store_pub.to_string()],
        priority: 0,
        root_sha256: None,
    });
    profile::save(&core.paths, &core.config).unwrap();
}

#[tokio::test]
async fn publish_install_and_call() {
    let work = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let (v1, store_pub, _store_key) = make_store(
        work.path(),
        &workspace_root().join("testdata/echo"),
        "0.1.0",
    );
    let mut core = Core::open_with("clean", Some(home.path())).await.unwrap();
    add_file_source(&mut core, "local", &v1, &store_pub);
    let id = store::install_package(&mut core, "test/echo", None, None, false)
        .await
        .unwrap();
    assert!(id.starts_with("test/echo@0.1.0"));
    link_bin(&core, "test/echo", &echo_bin());
    core.refresh().unwrap();
    let v = core
        .call("test.echo@0", "echo", json!({"x": 1}), None, None)
        .await
        .unwrap();
    assert_eq!(v, json!({"x": 1}));
    // search + info acham o pacote.
    let hits = store::search(&core, "echo", None).await.unwrap();
    assert!(hits.iter().any(|(_, e)| e.name == "test/echo"));
    let infos = store::info(&core, "test/echo").await.unwrap();
    assert!(
        infos
            .iter()
            .any(|(_, i)| i.versions.iter().any(|v| v.version == "0.1.0"))
    );
    core.shutdown().await;
}

#[tokio::test]
async fn tampered_byte_is_refused() {
    let work = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let (v1, store_pub, _k) = make_store(
        work.path(),
        &workspace_root().join("testdata/echo"),
        "0.1.0",
    );
    // Adultera 1 byte do artefato publicado.
    let arts: Vec<PathBuf> = walk(&v1.join("artifacts"));
    assert!(!arts.is_empty(), "sem artefatos publicados");
    let mut bytes = std::fs::read(&arts[0]).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0x01;
    std::fs::write(&arts[0], &bytes).unwrap();
    let mut core = Core::open_with("outra", Some(home.path())).await.unwrap();
    add_file_source(&mut core, "local", &v1, &store_pub);
    let e = store::install_package(&mut core, "test/echo", None, None, false)
        .await
        .unwrap_err();
    let msg = e.to_string();
    assert!(
        msg.contains("sha256") || msg.contains("assinatura"),
        "erro inesperado: {msg}"
    );
    core.shutdown().await;
}

#[tokio::test]
async fn wrong_publisher_sig_is_refused() {
    let work = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let (v1, store_pub, store_key) = make_store(
        work.path(),
        &workspace_root().join("testdata/echo"),
        "0.1.0",
    );
    // Troca o sha registrado pelo sha dos bytes adulterados, mantendo a sig
    // do publisher (passa no passo 2, falha no passo 3).
    let arts: Vec<PathBuf> = walk(&v1.join("artifacts"));
    let mut bytes = std::fs::read(&arts[0]).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0x01;
    std::fs::write(&arts[0], &bytes).unwrap();
    let (idx_path, mut idx) = load_index(&v1, "test/echo");
    let hex = sha_hex(&bytes);
    idx.versions[0].artifacts[0].sha256 = hex;
    let raw = serde_json::to_vec_pretty(&idx).unwrap();
    std::fs::write(&idx_path, &raw).unwrap();
    // Reassina índice com a chave da loja (passa no passo 1).
    resign(&idx_path, &store_key);
    // Regen catalog não é necessário p/ install (usa índice).
    let mut core = Core::open_with("outra2", Some(home.path())).await.unwrap();
    add_file_source(&mut core, "local", &v1, &store_pub);
    let e = store::install_package(&mut core, "test/echo", None, None, false)
        .await
        .unwrap_err();
    assert!(e.to_string().contains("publisher"), "erro inesperado: {e}");
    core.shutdown().await;
}

#[tokio::test]
async fn wrong_store_key_is_refused() {
    let work = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let (v1, _store_pub, _k) = make_store(
        work.path(),
        &workspace_root().join("testdata/echo"),
        "0.1.0",
    );
    let (other_pub, _) = store::keygen();
    let mut core = Core::open_with("outra3", Some(home.path())).await.unwrap();
    add_file_source(&mut core, "local", &v1, &other_pub);
    let e = store::install_package(&mut core, "test/echo", None, None, false)
        .await
        .unwrap_err();
    assert!(e.to_string().contains("chave raiz"), "erro inesperado: {e}");
    core.shutdown().await;
}

#[tokio::test]
async fn update_and_rollback() {
    let work = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let echo_src = workspace_root().join("testdata/echo");
    let (v1, store_pub, store_key) = make_store(work.path(), &echo_src, "0.1.0");
    let mut core = Core::open_with("up", Some(home.path())).await.unwrap();
    add_file_source(&mut core, "local", &v1, &store_pub);
    store::install_package(&mut core, "test/echo", None, None, false)
        .await
        .unwrap();
    // Publica 0.2.0 na mesma loja.
    let pkg_src = work.path().join("pkgsrc-0_2_0b");
    copy_dir(&echo_src, &pkg_src);
    let manifest = pkg_src.join("atom.toml");
    let text = std::fs::read_to_string(&manifest)
        .unwrap()
        .replace("version = \"0.1.0\"", "version = \"0.2.0\"");
    std::fs::write(&manifest, text).unwrap();
    let out = work.path().join("out-0_2_0b");
    std::fs::create_dir_all(&out).unwrap();
    // publisher novo (rotação simples: chaves acumulam no índice).
    let keys = work.path().join("keys2");
    std::fs::create_dir_all(&keys).unwrap();
    let (_pp, ps) = store::keygen();
    let pub_key = write_key(&keys, "pub2", &ps);
    let (_art, snip) = store::pack_package(&pkg_src, None, &pub_key, &out).unwrap();
    store::publish_to_store(&v1, &store_key, &[snip], Some(&out), false, false, false).unwrap();
    let done = store::update_packages(&mut core, Some("test/echo"))
        .await
        .unwrap();
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].2, "0.2.0");
    // Rollback volta p/ 0.1.0.
    let prev = store::rollback_lock(&core.paths).unwrap();
    assert!(!prev.is_empty());
    core.refresh().unwrap();
    let lock = pkg::load_lock(&core.paths).unwrap();
    let lp = lock.find("test/echo").unwrap();
    assert_eq!(lp.version, "0.1.0");
    core.shutdown().await;
}

#[tokio::test]
async fn removed_version_is_rejected_as_rollback() {
    let work = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let echo_src = workspace_root().join("testdata/echo");
    let (v1, store_pub, store_key) = make_store(work.path(), &echo_src, "0.1.0");
    // Publica 0.2.0 e instala (visto), depois remove 0.2.0 do índice.
    let pkg_src = work.path().join("pkgsrc-0_2_0c");
    copy_dir(&echo_src, &pkg_src);
    let manifest = pkg_src.join("atom.toml");
    let text = std::fs::read_to_string(&manifest)
        .unwrap()
        .replace("version = \"0.1.0\"", "version = \"0.2.0\"");
    std::fs::write(&manifest, text).unwrap();
    let out = work.path().join("out-0_2_0c");
    std::fs::create_dir_all(&out).unwrap();
    let keys = work.path().join("keys3");
    std::fs::create_dir_all(&keys).unwrap();
    let (_pp, ps) = store::keygen();
    let pub_key = write_key(&keys, "pub3", &ps);
    let (_art, snip) = store::pack_package(&pkg_src, None, &pub_key, &out).unwrap();
    store::publish_to_store(&v1, &store_key, &[snip], Some(&out), false, false, false).unwrap();
    let mut core = Core::open_with("rb", Some(home.path())).await.unwrap();
    add_file_source(&mut core, "local", &v1, &store_pub);
    store::install_package(&mut core, "test/echo", Some("=0.2.0"), None, false)
        .await
        .unwrap();
    // Ataque: índice sem 0.2.0 (não yanked, removida).
    let (idx_path, mut idx) = load_index(&v1, "test/echo");
    idx.versions.retain(|v| v.version != "0.2.0");
    std::fs::write(&idx_path, serde_json::to_vec_pretty(&idx).unwrap()).unwrap();
    resign(&idx_path, &store_key);
    let e = store::install_package(&mut core, "test/echo", Some("=0.2.0"), None, true)
        .await
        .unwrap_err();
    assert!(e.to_string().contains("removeu"), "erro inesperado: {e}");
    core.shutdown().await;
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else if p.extension().and_then(|x| x.to_str()) == Some("gz") {
                out.push(p);
            }
        }
    }
    out
}

fn sha_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn load_index(v1: &Path, name: &str) -> (PathBuf, store::IndexFile) {
    let (publisher, pkg) = name.split_once('/').unwrap();
    let p = v1.join(format!("index/{publisher}/{pkg}.json"));
    let v: store::IndexFile = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
    (p, v)
}

fn resign(idx_path: &Path, store_key: &Path) {
    let raw = std::fs::read(idx_path).unwrap();
    let seed = store::load_seed(store_key).unwrap();
    let sig = store::sign_with_seed(&seed, &raw);
    std::fs::write(format!("{}.sig", idx_path.display()), format!("{sig}\n")).unwrap();
}
