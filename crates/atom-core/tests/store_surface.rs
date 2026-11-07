//! Ramos de erro do `store`: req impossível, engine, chaves, fontes,
//! adulteração (artefato/índice), search/info/update/rollback/pack,
//! fetch HTTP (ETag/304/auth/erros), install (refresh/namespace/contratos).

use std::path::{Path, PathBuf};

use atom_core::{bus::Core, profile, store};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
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

fn make_store(work: &Path, version: &str, engine: Option<&str>) -> (PathBuf, String, PathBuf) {
    let (store_pub, store_seed) = store::keygen();
    let (_pub_pub, pub_seed) = store::keygen();
    let keys = work.join("keys");
    std::fs::create_dir_all(&keys).unwrap();
    let store_key = write_key(&keys, "store", &store_seed);
    let pub_key = write_key(&keys, "pub", &pub_seed);
    let pkg_src = work.join(format!("pkgsrc-{}", version.replace('.', "_")));
    copy_dir(&workspace_root().join("testdata/echo"), &pkg_src);
    let manifest = pkg_src.join("atom.toml");
    let mut text = std::fs::read_to_string(&manifest).unwrap();
    text = text.replace("version = \"0.1.0\"", &format!("version = \"{version}\""));
    if let Some(e) = engine {
        text = text.replace("engine = \">=0.1.0\"", &format!("engine = \"{e}\""));
    }
    std::fs::write(&manifest, text).unwrap();
    let out = work.join(format!("out-{}", version.replace('.', "_")));
    std::fs::create_dir_all(&out).unwrap();
    let (_art, snip) = store::pack_package(&pkg_src, None, &pub_key, &out).unwrap();
    let v1 = work.join("v1");
    store::publish_to_store(&v1, &store_key, &[snip], Some(&out), false, false, false).unwrap();
    (v1, store_pub, store_key)
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

/// Servidor HTTP mínimo p/ `fetch`: rotas de config por prefixo.
/// `/ok/` serve config válida com ETag (304 na 2ª chamada);
/// `/auth/` exige bearer; `/badver/`, `/invalid/`, `/err/` falham.
async fn http_store() -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let seen = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
    let seen2 = seen.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else {
                break;
            };
            let seen = seen2.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let Ok(n) = s.read(&mut buf).await else {
                    return;
                };
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                seen.lock().unwrap().push(req.clone());
                let line = req.lines().next().unwrap_or("").to_string();
                let reply: Vec<u8> = if line.contains("GET /ok/") {
                    if req.to_lowercase().contains("if-none-match") {
                        b"HTTP/1.1 304 Not Modified\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
                    } else {
                        let b = serde_json::json!({"store": 1, "name": "t", "index_signature_keys": []}).to_string().into_bytes();
                        [format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: \"v1\"\r\nConnection: close\r\n\r\n", b.len()).into_bytes(), b].concat()
                    }
                } else if line.contains("GET /auth/") {
                    if req.to_lowercase().contains("authorization: bearer s3cr3t") {
                        let b = serde_json::json!({"store": 1, "index_signature_keys": []})
                            .to_string()
                            .into_bytes();
                        [format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", b.len()).into_bytes(), b].concat()
                    } else {
                        b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
                    }
                } else if line.contains("GET /badver/") {
                    let b = serde_json::json!({"store": 2}).to_string().into_bytes();
                    [
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            b.len()
                        )
                        .into_bytes(),
                        b,
                    ]
                    .concat()
                } else if line.contains("GET /invalid/") {
                    let b = b"nao-json".to_vec();
                    [
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            b.len()
                        )
                        .into_bytes(),
                        b,
                    ]
                    .concat()
                } else {
                    b"HTTP/1.1 500 Erro\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
                };
                let _ = s.write_all(&reply).await;
            });
        }
    });
    (base, seen)
}

fn http_source(name: &str, base: &str, route: &str) -> store::Source {
    store::Source {
        name: name.to_string(),
        base: format!("{base}{route}"),
        keys: vec![],
        priority: 0,
        root_sha256: None,
    }
}

#[tokio::test]
async fn http_fetch_cache_auth_errors() {
    let home = tempfile::tempdir().unwrap();
    let core = Core::open_with("default", Some(home.path())).await.unwrap();
    let (base, seen) = http_store().await;
    // 200 com ETag grava cache; 2ª chamada com If-None-Match → 304 do cache.
    let src = http_source("h", &base, "ok/");
    let cfg = store::fetch_config(&core, &src).await.unwrap();
    assert_eq!(cfg.store, 1);
    let cfg2 = store::fetch_config(&core, &src).await.unwrap();
    assert_eq!(cfg2.store, 1);
    let log = seen.lock().unwrap().join("\n").to_lowercase();
    assert!(log.contains("if-none-match"), "2ª chamada devia revalidar");
    // Bearer via secret `store:<source>`.
    core.secrets.set("store:priv", "s3cr3t").unwrap();
    let src = http_source("priv", &base, "auth/");
    assert!(store::fetch_config(&core, &src).await.is_ok());
    let src = http_source("anon", &base, "auth/");
    assert!(store::fetch_config(&core, &src).await.is_err());
    // Variantes de erro.
    assert!(
        store::fetch_config(&core, &http_source("v", &base, "badver/"))
            .await
            .is_err()
    );
    assert!(
        store::fetch_config(&core, &http_source("i", &base, "invalid/"))
            .await
            .is_err()
    );
    assert!(
        store::fetch_config(&core, &http_source("e", &base, "err/"))
            .await
            .is_err()
    );
    // Base file:// inexistente.
    let ghost = store::Source {
        name: "g".to_string(),
        base: "file:///nao-existe-xyz/".to_string(),
        keys: vec![],
        priority: 0,
        root_sha256: None,
    };
    assert!(store::fetch_config(&core, &ghost).await.is_err());
}

#[tokio::test]
async fn install_branches() {
    let work = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let (mut core, v1, pubk, store_key) = core_with_store(home.path(), work.path(), None).await;
    // req inválido.
    assert!(
        store::install_package(&mut core, "test/echo", Some("!!!"), None, false)
            .await
            .is_err()
    );
    // source_only inexistente → NoProvider.
    assert!(
        store::install_package(&mut core, "test/echo", None, Some("outra"), false)
            .await
            .is_err()
    );
    // Instala, reinstala (skip pelo lock), refresh força re-resolve.
    store::install_package(&mut core, "test/echo", None, None, false)
        .await
        .unwrap();
    store::install_package(&mut core, "test/echo", None, None, false)
        .await
        .unwrap();
    store::install_package(&mut core, "test/echo", None, None, true)
        .await
        .unwrap();
    // Namespace: publica 0.2.0 e força re-resolve da outra source.
    publish_version(work.path(), "b", "0.2.0", &v1, &store_key);
    add_file_source(&mut core, "clone", &v1, &pubk);
    let e = store::install_package(&mut core, "test/echo", None, Some("clone"), true)
        .await
        .unwrap_err();
    assert!(format!("{e:?}").contains("--source"), "{e:?}");
}

#[tokio::test]
async fn install_contract_requirement() {
    let work = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    // Pacote que exige contrato inexistente.
    let src = work.path().join("needy");
    copy_dir(&workspace_root().join("testdata/echo"), &src);
    let manifest = src.join("atom.toml");
    let mut text = std::fs::read_to_string(&manifest).unwrap();
    text = text.replace("name = \"test/echo\"", "name = \"test/needy\"");
    text.push_str("\n[requires.contracts]\n\"nope@0\" = \"*\"\n");
    std::fs::write(&manifest, text).unwrap();
    let (store_pub, store_seed) = store::keygen();
    let (_pp, pub_seed) = store::keygen();
    let keys = work.path().join("k");
    std::fs::create_dir_all(&keys).unwrap();
    let store_key = write_key(&keys, "store", &store_seed);
    let pub_key = write_key(&keys, "pub", &pub_seed);
    let out = work.path().join("out-needy");
    std::fs::create_dir_all(&out).unwrap();
    let (_art, snip) = store::pack_package(&src, None, &pub_key, &out).unwrap();
    let v1 = work.path().join("v1");
    store::publish_to_store(&v1, &store_key, &[snip], Some(&out), false, false, false).unwrap();
    let mut core = Core::open_with("default", Some(home.path())).await.unwrap();
    add_file_source(&mut core, "local", &v1, &store_pub);
    let e = store::install_package(&mut core, "test/needy", None, None, false)
        .await
        .unwrap_err();
    assert!(format!("{e:?}").contains("contrato"), "{e:?}");
}

async fn core_with_store(
    home: &Path,
    work: &Path,
    engine: Option<&str>,
) -> (Core, PathBuf, String, PathBuf) {
    let (v1, store_pub, store_key) = make_store(work, "0.1.0", engine);
    let mut core = Core::open_with("default", Some(home)).await.unwrap();
    add_file_source(&mut core, "local", &v1, &store_pub);
    (core, v1, store_pub, store_key)
}

/// Publica mais uma versão de test/echo na MESMA loja (reusa store key).
fn publish_version(work: &Path, tag: &str, version: &str, v1: &Path, store_key: &Path) {
    let (_pp, pub_seed) = store::keygen();
    let keys = work.join(format!("keys-{tag}"));
    std::fs::create_dir_all(&keys).unwrap();
    let pub_key = write_key(&keys, "pub", &pub_seed);
    let pkg_src = work.join(format!("pkgsrc-{tag}"));
    copy_dir(&workspace_root().join("testdata/echo"), &pkg_src);
    let manifest = pkg_src.join("atom.toml");
    let text = std::fs::read_to_string(&manifest)
        .unwrap()
        .replace("version = \"0.1.0\"", &format!("version = \"{version}\""));
    std::fs::write(&manifest, text).unwrap();
    let out = work.join(format!("out-{tag}"));
    std::fs::create_dir_all(&out).unwrap();
    let (_art, snip) = store::pack_package(&pkg_src, None, &pub_key, &out).unwrap();
    store::publish_to_store(v1, store_key, &[snip], Some(&out), false, false, false).unwrap();
}

fn is_no_provider(e: &atom_core::error::CoreError) -> bool {
    matches!(e, atom_core::error::CoreError::NoProvider(_))
}

#[tokio::test]
async fn resolve_req_impossible() {
    let work = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let (mut core, _v1, _pub, _sk) = core_with_store(home.path(), work.path(), None).await;
    let e = store::install_package(&mut core, "test/echo", Some(">=99.0.0"), None, false)
        .await
        .unwrap_err();
    assert!(is_no_provider(&e), "{e:?}");
}

#[tokio::test]
async fn engine_mismatch_refuses() {
    let work = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let (mut core, _v1, _pub, _sk) =
        core_with_store(home.path(), work.path(), Some(">=99.0.0")).await;
    let e = store::install_package(&mut core, "test/echo", None, None, false)
        .await
        .unwrap_err();
    assert!(is_no_provider(&e), "{e:?}");
}

#[tokio::test]
async fn wrong_store_key_rejects_index() {
    let work = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let (v1, _real_pub, _k) = make_store(work.path(), "0.1.0", None);
    let (other_pub, _s) = store::keygen();
    let mut core = Core::open_with("default", Some(home.path())).await.unwrap();
    add_file_source(&mut core, "local", &v1, &other_pub);
    let e = store::install_package(&mut core, "test/echo", None, None, false)
        .await
        .unwrap_err();
    // Chave errada: a verificação de assinatura barra (empacotada em NoProvider).
    assert!(format!("{e:?}").contains("assinatura"), "{e:?}");
}

#[tokio::test]
async fn missing_source_dir_errors() {
    let home = tempfile::tempdir().unwrap();
    let mut core = Core::open_with("default", Some(home.path())).await.unwrap();
    let (fake_pub, _s) = store::keygen();
    let ghost = home.path().join("ghost-store");
    add_file_source(&mut core, "ghost", &ghost, &fake_pub);
    assert!(
        store::install_package(&mut core, "test/echo", None, None, false)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn tampered_artifact_refused() {
    let work = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let (mut core, v1, _pub, _sk) = core_with_store(home.path(), work.path(), None).await;
    // Vira 1 byte do .tar.gz publicado.
    let mut art = None;
    let mut stack = vec![v1.join("artifacts")];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|x| x.to_str()) == Some("gz") {
                art = Some(p);
            }
        }
    }
    let art = art.expect("artefato publicado");
    let mut bytes = std::fs::read(&art).unwrap();
    let i = bytes.len() / 2;
    bytes[i] ^= 0xff;
    std::fs::write(&art, &bytes).unwrap();
    assert!(
        store::install_package(&mut core, "test/echo", None, None, false)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn tampered_index_and_check_store() {
    let work = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let (mut core, v1, _pub, _sk) = core_with_store(home.path(), work.path(), None).await;
    assert!(store::check_store(&v1).is_ok());
    // Acha o índice e adultera.
    let mut idx = None;
    let mut stack = vec![v1.join("index")];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|x| x.to_str()) == Some("json") {
                idx = Some(p);
            }
        }
    }
    let idx = idx.expect("índice publicado");
    let mut bytes = std::fs::read(&idx).unwrap();
    bytes[10] ^= 0xff;
    std::fs::write(&idx, &bytes).unwrap();
    assert!(store::check_store(&v1).is_err());
    assert!(
        store::install_package(&mut core, "test/echo", None, None, false)
            .await
            .is_err()
    );
    // Dir vazio não é loja.
    assert!(store::check_store(work.path()).is_err());
}

#[tokio::test]
async fn search_info_update_rollback_paths() {
    let work = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    // Sem sources: search/info falham de forma definida.
    let mut core = Core::open_with("default", Some(home.path())).await.unwrap();
    assert!(store::search(&core, "echo", None).await.is_err());
    assert!(store::info(&core, "test/echo").await.is_err());
    assert!(store::update_packages(&mut core, None).await.is_err());
    // Rollback sem .prev falha.
    assert!(store::rollback_lock(&core.paths).is_err());
    drop(core);
    let (mut core, _v1, _pub, _sk) = core_with_store(home.path(), work.path(), None).await;
    let hits = store::search(&core, "echo", None).await.unwrap();
    assert!(!hits.is_empty());
    assert!(
        store::search(&core, "echo", Some("kind-inexistente"))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store::search(&core, "zzz-nada", None)
            .await
            .unwrap()
            .is_empty()
    );
    let infos = store::info(&core, "test/echo").await.unwrap();
    assert_eq!(infos.len(), 1);
    assert!(store::info(&core, "nada/aqui").await.is_err());
    // Nada instalado da loja → update recusa.
    assert!(store::update_packages(&mut core, None).await.is_err());
}

#[tokio::test]
async fn publish_regen_update_ok() {
    let work = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let (mut core, v1, _pub, store_key) = core_with_store(home.path(), work.path(), None).await;
    // check_only valida sem escrever.
    let msg = store::publish_to_store(&v1, &store_key, &[], None, false, true, false).unwrap();
    assert!(msg.contains("store ok"));
    // reindex reassina tudo.
    store::publish_to_store(&v1, &store_key, &[], None, true, false, false).unwrap();
    // Chave estranha na loja existente → rotação não autorizada.
    let keys = work.path().join("kk");
    std::fs::create_dir_all(&keys).unwrap();
    let (_fp, fs) = store::keygen();
    let foreign = write_key(&keys, "f", &fs);
    assert!(store::publish_to_store(&v1, &foreign, &[], None, false, false, false).is_err());
    // regen direto conta índices.
    let seed = store::load_seed(&store_key).unwrap();
    assert!(store::regen_catalog(&v1, &seed).unwrap() >= 1);
    // Artefato http:// não é verificado localmente → erro definido.
    let infos = store::info(&core, "test/echo").await.unwrap();
    let v = &infos[0].1.versions[0];
    let mut a = v.artifacts[0].clone();
    a.url = "http://exemplo/x.tar.gz".to_string();
    assert!(store::verify_store_artifact(&v1, "test/echo", v, &a, &[]).is_err());
    // Update feliz 0.1.0 → 0.2.0 via update_packages.
    store::install_package(&mut core, "test/echo", None, None, false)
        .await
        .unwrap();
    publish_version(work.path(), "u", "0.2.0", &v1, &store_key);
    let done = store::update_packages(&mut core, None).await.unwrap();
    assert!(
        done.iter()
            .any(|(n, f, t)| n == "test/echo" && f == "0.1.0" && t == "0.2.0"),
        "{done:?}"
    );
    // Rollback com .prev agora funciona.
    assert!(store::rollback_lock(&core.paths).is_ok());
}

#[tokio::test]
async fn sources_order_and_pack_errors() {
    let home = tempfile::tempdir().unwrap();
    let mut core = Core::open_with("default", Some(home.path())).await.unwrap();
    let (k1, _s1) = store::keygen();
    core.config.sources.push(profile::SourceCfg {
        name: "b".to_string(),
        url: "file:///b/".to_string(),
        keys: vec![k1],
        priority: 5,
        root_sha256: None,
    });
    let (k2, _s2) = store::keygen();
    core.config.sources.push(profile::SourceCfg {
        name: "a".to_string(),
        url: "file:///a".to_string(),
        keys: vec![k2],
        priority: 0,
        root_sha256: None,
    });
    let names: Vec<String> = store::profile_sources(&core)
        .iter()
        .map(|s| s.name.clone())
        .collect();
    assert_eq!(names, vec!["a".to_string(), "b".to_string()]);
    // pack sem manifesto falha.
    assert!(store::pack_package(home.path(), None, home.path(), home.path()).is_err());
}
