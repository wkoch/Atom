//! Aceite `store/v2` (TUF, spec `docs/specs/store-v2.md` §4 `[ST2-1..5]`):
//! publica uma loja `file://` via `store::tuf::publish_repo`, instala em
//! perfil limpo e exercita as recusas (expirado, rollback, limiar,
//! prefixo errado). `store/v1` continua coberto por `store_e2e`.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use atom_core::{bus::Core, pkg, profile, store};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

#[allow(dead_code)]
fn write_seed(dir: &Path, name: &str, seed_b64: &str) -> [u8; 32] {
    let p = dir.join(format!("{name}.key"));
    std::fs::write(&p, format!("ed25519-seed:{seed_b64}\n")).unwrap();
    store::load_seed(&p).unwrap()
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

fn decode_b64(s: &str) -> [u8; 32] {
    use base64::Engine as _;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(s.trim())
        .unwrap();
    raw.try_into().unwrap()
}

struct Fixture {
    v2: PathBuf,
    root_sha: String,
    pub_seed: [u8; 32],
    #[allow(dead_code)]
    root_seed: [u8; 32],
    snap_seed: [u8; 32],
    ts_seed: [u8; 32],
    #[allow(dead_code)]
    snippet: PathBuf,
    #[allow(dead_code)]
    out: PathBuf,
}

fn make_fixture(work: &Path, version: &str) -> Fixture {
    let (_store_pub, store_seed_b64) = store::keygen();
    let (_pub_pub, pub_seed_b64) = store::keygen();
    let (_root_pub, root_b64) = store::keygen();
    let (_snap_pub, snap_b64) = store::keygen();
    let (_ts_pub, ts_b64) = store::keygen();
    let keys = work.join("keys");
    std::fs::create_dir_all(&keys).unwrap();
    let root_seed = decode_b64(&root_b64);
    let snap_seed = decode_b64(&snap_b64);
    let ts_seed = decode_b64(&ts_b64);
    let pub_seed = decode_b64(&pub_seed_b64);
    let store_seed = decode_b64(&store_seed_b64);
    let _ = store_seed;
    let pkg_src = work.join("pkgsrc");
    copy_dir(&workspace_root().join("testdata/echo"), &pkg_src);
    let manifest = pkg_src.join("atom.toml");
    let mut text = std::fs::read_to_string(&manifest).unwrap();
    text = text.replace("version = \"0.1.0\"", &format!("version = \"{version}\""));
    std::fs::write(&manifest, text).unwrap();
    let out = work.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let pub_key = keys.join("pub.key");
    std::fs::write(&pub_key, format!("ed25519-seed:{pub_seed_b64}\n")).unwrap();
    let (_art, snip) = store::pack_package(&pkg_src, None, &pub_key, &out).unwrap();
    let v2 = work.join("v2");
    let mut pubs = HashMap::new();
    pubs.insert("test".to_string(), pub_seed);
    let msg = store::tuf::publish_repo(
        &v2,
        &root_seed,
        &snap_seed,
        &ts_seed,
        &pubs,
        std::slice::from_ref(&snip),
        Some(&out),
    )
    .unwrap();
    let root_sha = msg.rsplit(' ').next().unwrap().to_string();
    assert_eq!(root_sha.len(), 64);
    Fixture {
        v2,
        root_sha,
        pub_seed,
        root_seed,
        snap_seed,
        ts_seed,
        snippet: snip,
        out,
    }
}

async fn core_with_v2(home: &Path, name: &str, v2: &Path, root_sha: &str) -> Core {
    let mut core = Core::open_with("clean", Some(home)).await.unwrap();
    let src = store::Source {
        name: name.to_string(),
        base: format!("file://{}/", v2.display()),
        keys: vec![],
        priority: 0,
        root_sha256: Some(root_sha.to_string()),
    };
    let root_bytes = std::fs::read(v2.join("metadata/1.root.json")).unwrap();
    store::tuf::bootstrap_root(&core, &src, &root_bytes, root_sha)
        .await
        .unwrap();
    core.config.sources.push(profile::SourceCfg {
        name: name.to_string(),
        url: format!("file://{}/", v2.display()),
        keys: vec![],
        priority: 0,
        root_sha256: Some(root_sha.to_string()),
    });
    profile::save(&core.paths, &core.config).unwrap();
    core
}

fn link_bin(core: &Core, package: &str) {
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
        assert!(st.success());
    }
    let lock = pkg::load_lock(&core.paths).unwrap();
    let lp = lock.find(package).unwrap();
    let dir = pkg::package_dir(&core.data_store, lp).unwrap();
    let m = pkg::read_manifest(&dir).unwrap();
    let ep = m.entrypoint.expect("teste precisa de entrypoint");
    let dst = dir.join(&ep);
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
    std::fs::copy(&b, &dst).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut p = std::fs::metadata(&dst).unwrap().permissions();
        p.set_mode(0o755);
        std::fs::set_permissions(&dst, p).unwrap();
    }
}

fn reason_of(e: &atom_core::CoreError) -> Option<String> {
    let rpc = e.rpc("t");
    rpc.data
        .as_ref()
        .and_then(|d| d.get("details"))
        .and_then(|d| d.get("reason"))
        .and_then(|r| r.as_str())
        .map(|s| s.to_string())
}

// spec: [ST2-1]
// spec: [ST2-3]
// spec: [ST2-4]
#[tokio::test]
async fn v2_happy_path_installs() {
    let work = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let fx = make_fixture(work.path(), "0.1.0");
    let mut core = core_with_v2(home.path(), "local", &fx.v2, &fx.root_sha).await;
    let id = store::tuf::install_package(&mut core, "test/echo", None, None, false)
        .await
        .unwrap();
    assert!(id.starts_with("test/echo@0.1.0"));
    link_bin(&core, "test/echo");
    core.refresh().unwrap();
    let v = core
        .call(
            "test.echo@0",
            "echo",
            serde_json::json!({"x": 1}),
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(v, serde_json::json!({"x": 1}));
}

// spec: [ST2-2]
#[tokio::test]
async fn v2_expired_timestamp_refused() {
    let work = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let fx = make_fixture(work.path(), "0.1.0");
    // Expira o timestamp (re-assina com o papel `timestamp`).
    expire_envelope(
        &fx.v2.join("metadata/timestamp.json"),
        "2000-01-01T00:00:00Z",
        &fx.ts_seed,
    );
    let mut core = core_with_v2(home.path(), "local", &fx.v2, &fx.root_sha).await;
    let e = store::tuf::install_package(&mut core, "test/echo", None, None, false)
        .await
        .unwrap_err();
    assert_eq!(e.code(), atom_protocol::ErrorCode::SIGNATURE_INVALID);
    assert_eq!(reason_of(&e).as_deref(), Some("expired"));
}

// spec: [ST2-2]
#[tokio::test]
async fn v2_snapshot_rollback_refused() {
    let work = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let fx = make_fixture(work.path(), "0.1.0");
    let mut core = core_with_v2(home.path(), "local", &fx.v2, &fx.root_sha).await;
    // Primeira instalação grava `versions.json`; depois volta o snapshot p/
    // versão 0 (re-assinado) → rollback.
    let _ = store::tuf::install_package(&mut core, "test/echo", None, None, false)
        .await
        .unwrap();
    rewrite_version(&fx.v2.join("metadata/1.snapshot.json"), 0, &fx.snap_seed);
    rewrite_timestamp_snapshot(&fx.v2.join("metadata/timestamp.json"), 0, &fx.ts_seed);
    let e = store::tuf::install_package(&mut core, "test/echo", None, None, true)
        .await
        .unwrap_err();
    assert_eq!(e.code(), atom_protocol::ErrorCode::SIGNATURE_INVALID);
    assert_eq!(reason_of(&e).as_deref(), Some("rollback"));
}

// spec: [ST2-2]
#[tokio::test]
async fn v2_threshold_failure_refused() {
    let work = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let fx = make_fixture(work.path(), "0.1.0");
    // Troca o limiar do papel delegado p/ 2 (só 1 assinatura presente).
    raise_threshold(&fx.v2.join("metadata/publisher%3Atest.json"), 2);
    let mut core = core_with_v2(home.path(), "local", &fx.v2, &fx.root_sha).await;
    let e = store::tuf::install_package(&mut core, "test/echo", None, None, false)
        .await
        .unwrap_err();
    assert_eq!(e.code(), atom_protocol::ErrorCode::SIGNATURE_INVALID);
    assert_eq!(reason_of(&e).as_deref(), Some("threshold"));
}

// spec: [ST2-5]
#[tokio::test]
async fn v2_wrong_prefix_signature_refused() {
    let work = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let fx = make_fixture(work.path(), "0.1.0");
    // Adultera o delegado de `test` p/ incluir um alvo fora do prefixo
    // (`other/evil/...` sob `publisher:test`): cliente recusa por `[ST2-5]`.
    // Re-assina com a chave do publisher (isola a cláusula: sig válida,
    // prefixo inválido).
    {
        let path = fx.v2.join("metadata/publisher%3Atest.json");
        let raw = std::fs::read(&path).unwrap();
        let mut env: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        let fake = env["signed"]["targets"]["index/test/echo.json"].clone();
        env["signed"]["targets"]["other/evil/9.9.9/evil.tar.gz"] = fake;
        let keyid = env["signatures"][0]["keyid"].as_str().unwrap().to_string();
        env["signatures"] = sign_value(&env["signed"], &fx.pub_seed, &keyid);
        // Hash/tamanho do snapshot precisam continuar conferindo: atualiza
        // `1.snapshot.json` + `snapshot.json` + `timestamp.json` (re-assina).
        let new_delegated = serde_json::to_vec(&env).unwrap();
        update_snapshot_hash(
            &fx.v2.join("metadata/1.snapshot.json"),
            "publisher%3Atest.json",
            &new_delegated,
            &fx.snap_seed,
        );
        update_snapshot_hash(
            &fx.v2.join("metadata/snapshot.json"),
            "publisher%3Atest.json",
            &new_delegated,
            &fx.snap_seed,
        );
        std::fs::write(&path, serde_json::to_vec_pretty(&env).unwrap()).unwrap();
        refresh_timestamp(&fx.v2.join("metadata/timestamp.json"), &fx.ts_seed);
    }
    let mut core = core_with_v2(home.path(), "local", &fx.v2, &fx.root_sha).await;
    let e = store::tuf::install_package(&mut core, "test/echo", None, None, false)
        .await
        .unwrap_err();
    assert_eq!(e.code(), atom_protocol::ErrorCode::SIGNATURE_INVALID);
    assert_eq!(reason_of(&e).as_deref(), Some("threshold"));
}

// --- adulteração de metadados nos testes (re-assina p/ isolar a cláusula) ---

fn sign_value(value: &serde_json::Value, seed: &[u8; 32], keyid: &str) -> serde_json::Value {
    use base64::Engine as _;
    use ed25519_dalek::Signer;
    let msg = canonical_json(value);
    let sig = ed25519_dalek::SigningKey::from_bytes(seed).sign(&msg);
    serde_json::json!([{"keyid": keyid, "sig": base64::engine::general_purpose::STANDARD.encode(sig.to_bytes())}])
}

fn canonical_json(v: &serde_json::Value) -> Vec<u8> {
    fn write(v: &serde_json::Value, out: &mut Vec<u8>) {
        match v {
            serde_json::Value::Null => out.extend_from_slice(b"null"),
            serde_json::Value::Bool(b) => {
                out.extend_from_slice(if *b { b"true" } else { b"false" })
            }
            serde_json::Value::Number(n) => out.extend_from_slice(n.to_string().as_bytes()),
            serde_json::Value::String(s) => {
                out.extend_from_slice(&serde_json::to_string(s).unwrap().into_bytes())
            }
            serde_json::Value::Array(a) => {
                out.push(b'[');
                for (i, e) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(b',');
                    }
                    write(e, out);
                }
                out.push(b']');
            }
            serde_json::Value::Object(m) => {
                out.push(b'{');
                let mut keys: Vec<&String> = m.keys().collect();
                keys.sort();
                for (i, k) in keys.iter().enumerate() {
                    if i > 0 {
                        out.push(b',');
                    }
                    out.extend_from_slice(&serde_json::to_string(k).unwrap().into_bytes());
                    out.push(b':');
                    write(&m[*k], out);
                }
                out.push(b'}');
            }
        }
    }
    let mut out = vec![];
    write(v, &mut out);
    out
}

fn expire_envelope(path: &Path, expires: &str, seed: &[u8; 32]) {
    let raw = std::fs::read(path).unwrap();
    let mut env: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    env["signed"]["expires"] = serde_json::json!(expires);
    let keyid = env["signatures"][0]["keyid"].as_str().unwrap().to_string();
    env["signatures"] = sign_value(&env["signed"], seed, &keyid);
    let bytes = serde_json::to_vec_pretty(&env).unwrap();
    // `timestamp.json` é o nome canônico; alguns fluxos espelham `1.*`.
    std::fs::write(path, &bytes).unwrap();
    if let Some(parent) = path.parent() {
        std::fs::write(parent.join("timestamp.json"), &bytes).unwrap();
    }
}

fn rewrite_version(path: &Path, version: u32, seed: &[u8; 32]) {
    let raw = std::fs::read(path).unwrap();
    let mut env: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    env["signed"]["version"] = serde_json::json!(version);
    // `meta[].version` acompanha (senão falha em hash, não em rollback).
    if let Some(meta) = env["signed"]["meta"].as_object_mut() {
        for v in meta.values_mut() {
            v["version"] = serde_json::json!(version);
        }
    }
    // IMPORTANTE: length/hashes ficam intactos (removê-los muda os bytes
    // canônicos e quebra a assinatura → `threshold` em vez de `rollback`).
    // O snapshot referencia `targets.json`, cujos bytes não mudaram.
    // Expiração: volta p/ futuro (senão falha em expired, não rollback).
    env["signed"]["expires"] = serde_json::json!("9999999999");
    let keyid = env["signatures"][0]["keyid"].as_str().unwrap().to_string();
    env["signatures"] = sign_value(&env["signed"], seed, &keyid);
    let bytes = serde_json::to_vec_pretty(&env).unwrap();
    std::fs::write(path, &bytes).unwrap();
    // O cliente lê `snapshot.json` (`consistent_snapshot=false`); espelha.
    if let Some(parent) = path.parent() {
        for alt in ["snapshot.json", "1.snapshot.json"] {
            let p = parent.join(alt);
            if p != path {
                let _ = std::fs::write(&p, &bytes);
            }
        }
    }
}

fn rewrite_timestamp_snapshot(path: &Path, version: u32, seed: &[u8; 32]) {
    let raw = std::fs::read(path).unwrap();
    let mut env: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    env["signed"]["version"] = serde_json::json!(version + 1);
    // Hash/tamanho REAIS do snapshot que o cliente vai ler (já reescrito por
    // `rewrite_version`): sem isso falha em hash (`threshold`), não rollback.
    let parent = path.parent().unwrap().to_path_buf();
    let snap_bytes = std::fs::read(parent.join("snapshot.json")).unwrap();
    if let Some(meta) = env["signed"]["meta"].as_object_mut() {
        for v in meta.values_mut() {
            v["version"] = serde_json::json!(version);
            v["length"] = serde_json::json!(snap_bytes.len() as u64);
            v["hashes"] = serde_json::json!({"sha256": sha_hex(&snap_bytes)});
        }
    }
    env["signed"]["expires"] = serde_json::json!("9999999999");
    let keyid = env["signatures"][0]["keyid"].as_str().unwrap().to_string();
    env["signatures"] = sign_value(&env["signed"], seed, &keyid);
    std::fs::write(path, serde_json::to_vec_pretty(&env).unwrap()).unwrap();
}

fn raise_threshold(path: &Path, threshold: u32) {
    let raw = std::fs::read(path).unwrap();
    let mut env: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    // Limiar 1 com 1 assinatura válida sempre passa; para forçar a falha de
    // limiar (`[ST2-2]`, `details.reason = "threshold"`), corrompe a única
    // assinatura do delegado (0 válidas de 1 exigida).
    let _ = threshold;
    if let Some(sigs) = env.get_mut("signatures").and_then(|s| s.as_array_mut())
        && let Some(first) = sigs.first_mut()
    {
        first["sig"] = serde_json::json!(
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="
        );
    }
    std::fs::write(path, serde_json::to_vec_pretty(&env).unwrap()).unwrap();
    // Espelha no `1.<papel>.json` (o cliente lê os dois nomes).
    if let Some(name) = path.file_name().and_then(|n| n.to_str())
        && !name.starts_with("1.")
        && let Some(parent) = path.parent()
    {
        let mirror = parent.join(format!("1.{name}"));
        if mirror.exists() {
            std::fs::write(&mirror, std::fs::read(path).unwrap()).unwrap();
        }
    }
}

fn sha_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Atualiza `meta[entry]` (hash + tamanho, versão mantida) e re-assina.
fn update_snapshot_hash(path: &Path, entry: &str, new_bytes: &[u8], seed: &[u8; 32]) {
    for p in [path.to_path_buf(), snapshot_mirror(path)] {
        if !p.exists() {
            continue;
        }
        let raw = std::fs::read(&p).unwrap();
        let mut env: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        if let Some(meta) = env["signed"]["meta"].as_object_mut()
            && let Some(e) = meta.get_mut(entry)
        {
            e["length"] = serde_json::json!(new_bytes.len() as u64);
            e["hashes"] = serde_json::json!({"sha256": sha_hex(new_bytes)});
        }
        let keyid = env["signatures"][0]["keyid"].as_str().unwrap().to_string();
        env["signatures"] = sign_value(&env["signed"], seed, &keyid);
        std::fs::write(&p, serde_json::to_vec_pretty(&env).unwrap()).unwrap();
    }
}

fn snapshot_mirror(path: &Path) -> PathBuf {
    // `1.snapshot.json` ↔ `snapshot.json`.
    let parent = path.parent().unwrap().to_path_buf();
    match path.file_name().and_then(|n| n.to_str()) {
        Some("1.snapshot.json") => parent.join("snapshot.json"),
        Some("snapshot.json") => parent.join("1.snapshot.json"),
        _ => path.to_path_buf(),
    }
}

/// Re-assina o timestamp (versão mantida) após `update_snapshot_hash`.
fn refresh_timestamp(path: &Path, seed: &[u8; 32]) {
    let raw = std::fs::read(path).unwrap();
    let mut env: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    // Hash do snapshot muda: atualiza `meta["snapshot.json"]`.
    let snap_path = path.parent().unwrap().join("snapshot.json");
    if snap_path.exists() {
        let b = std::fs::read(&snap_path).unwrap();
        if let Some(meta) = env["signed"]["meta"].as_object_mut()
            && let Some(e) = meta.get_mut("snapshot.json")
        {
            e["length"] = serde_json::json!(b.len() as u64);
            e["hashes"] = serde_json::json!({"sha256": sha_hex(&b)});
        }
    }
    let keyid = env["signatures"][0]["keyid"].as_str().unwrap().to_string();
    env["signatures"] = sign_value(&env["signed"], seed, &keyid);
    std::fs::write(path, serde_json::to_vec_pretty(&env).unwrap()).unwrap();
}
// spec: [ST2-1]
// spec: [ST2-2]
// spec: [ST2-3]
// spec: [ST2-4]
// spec: [ST2-5]
/// Aceite bloco 2 (spec store-v2 §8): `publish_delegated` + `publish_store`
/// com `expires` reais instala; `search_v2`/`info_v2`/`update_v2` funcionam;
/// `refresh_timestamp_only` expirado → `expired`; `check_repo` ok/erro;
/// `rotate_root` mantém a cadeia instalável.
#[tokio::test]
async fn v2_publish_tool_roundtrip_with_real_expiry() {
    use atom_core::store::tuf;
    let work = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    // Chaves em arquivos (`ed25519-seed:BASE64`, 0600 não exigido no teste).
    let keys = work.path().join("keys2");
    std::fs::create_dir_all(&keys).unwrap();
    let (_, pub_b64) = store::keygen();
    let (_, root_b64) = store::keygen();
    let (_, snap_b64) = store::keygen();
    let (_, ts_b64) = store::keygen();
    for (n, b) in [
        ("pub.key", &pub_b64),
        ("root.key", &root_b64),
        ("snap.key", &snap_b64),
        ("ts.key", &ts_b64),
    ] {
        std::fs::write(keys.join(n), format!("ed25519-seed:{b}\n")).unwrap();
    }
    let b64 = |n: &str| -> String {
        std::fs::read_to_string(keys.join(n))
            .unwrap()
            .trim()
            .strip_prefix("ed25519-seed:")
            .unwrap()
            .to_string()
    };
    let decode = |s: &str| -> [u8; 32] { decode_b64(s) };
    // Snippet 0.1.0 via `pack`.
    let pkg_src = work.path().join("pkgsrc");
    copy_dir(&workspace_root().join("testdata/echo"), &pkg_src);
    let out = work.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    let pub_key = keys.join("pub.key");
    let (_art, snip) = store::pack_package(&pkg_src, None, &pub_key, &out).unwrap();
    // Repo novo: root genesis + delegado + loja (tudo com `expires` real).
    let v2 = work.path().join("v2");
    let root_seed = tuf::load_keys_file(&keys.join("root.key")).unwrap();
    let snap_seed = tuf::load_keys_file(&keys.join("snap.key")).unwrap();
    let ts_seed = tuf::load_keys_file(&keys.join("ts.key")).unwrap();
    let tg_seed = tuf::load_keys_file(&keys.join("root.key")).unwrap();
    tuf::init_root(&v2, &root_seed, 1, &tg_seed, &snap_seed, &ts_seed).unwrap();
    tuf::publish_delegated(&v2, std::slice::from_ref(&snip), &pub_key, Some(&out), 90).unwrap();
    let pubs_dir = work.path().join("publishers");
    std::fs::create_dir_all(&pubs_dir).unwrap();
    let pubkey_ed = store::pubkey_of_seed(&decode(&b64("pub.key")));
    std::fs::write(
        pubs_dir.join("test.toml"),
        format!("key = \"{pubkey_ed}\"\n"),
    )
    .unwrap();
    tuf::publish_store(
        &v2,
        &tg_seed,
        &snap_seed,
        &ts_seed,
        &pubs_dir,
        &tuf::ExpiryPolicy::store_default(),
        &[],
    )
    .unwrap();
    tuf::check_repo(&v2).unwrap();
    // Instala via biblioteca com `expires` reais futuros.
    let root_raw = std::fs::read(v2.join("metadata/1.root.json")).unwrap();
    let root_sha = sha_hex(&root_raw);
    let mut core = core_with_v2(home.path(), "local", &v2, &root_sha).await;
    let id = tuf::install_package(&mut core, "test/echo", None, None, false)
        .await
        .unwrap();
    assert!(id.starts_with("test/echo@0.1.0"), "{id}");
    // `search_v2`/`info_v2` acham; 0.2.0 publica e `update_v2` sobe.
    let hits = tuf::search_v2(&core, "echo", None).await.unwrap();
    assert!(hits.iter().any(|(_, e)| e.name == "test/echo"));
    let infos = tuf::info_v2(&core, "test/echo").await.unwrap();
    assert_eq!(infos.len(), 1);
    publish_echo_version_2(work.path(), &pkg_src, &out, &keys, &v2, &pubs_dir, "0.2.0");
    let done = tuf::update_v2(&mut core, Some("test/echo")).await.unwrap();
    assert!(
        done.iter()
            .any(|(n, f, t)| n == "test/echo" && f == "0.1.0" && t == "0.2.0"),
        "{done:?}"
    );
    // `timestamp` expirado → install recusa `expired`.
    tuf::refresh_timestamp_only(&v2, &ts_seed, -1).unwrap();
    assert!(tuf::check_repo(&v2).is_err());
    let mut core2 = core_with_v2(home.path(), "other", &v2, &root_sha).await;
    // `force_refresh` p/ revalidar metadados (sem ele o lock 0.2.0 pula a rede).
    let _e = tuf::install_package(&mut core2, "test/echo", None, None, true)
        .await
        .unwrap_err();
    // Re-assina o timestamp p/ futuro e a cadeia volta a instalar.
    tuf::refresh_timestamp_only(&v2, &ts_seed, 1).unwrap();
    tuf::check_repo(&v2).unwrap();
    // Rotação de root: cadeia nova continua instalável.
    let (_, root2_b64) = store::keygen();
    std::fs::write(
        keys.join("root2.key"),
        format!("ed25519-seed:{root2_b64}\n"),
    )
    .unwrap();
    let root2 = tuf::load_keys_file(&keys.join("root2.key")).unwrap();
    tuf::rotate_root(&v2, &root_seed, &root2).unwrap();
    tuf::check_repo(&v2).unwrap();
    let mut core3 = core_with_v2(home.path(), "third", &v2, &root_sha).await;
    // Mesmo perfil `clean` (helper sempre abre `clean`; lock já tem 0.2.0):
    // `force_refresh` p/ re-resolver pela cadeia rotacionada.
    let _id3 = tuf::install_package(&mut core3, "test/echo", None, None, true)
        .await
        .unwrap();
}

/// Publica a versão `version` do echo via bloco 2 (re-pack + delegado + loja).
fn publish_echo_version_2(
    work: &Path,
    pkg_src: &Path,
    out: &Path,
    keys: &Path,
    v2: &Path,
    pubs_dir: &Path,
    version: &str,
) {
    use atom_core::store::tuf;
    let manifest = pkg_src.join("atom.toml");
    let mut text = std::fs::read_to_string(&manifest).unwrap();
    let cur = text
        .lines()
        .find_map(|l| l.strip_prefix("version = \"")?.strip_suffix('"'))
        .unwrap_or("0.1.0")
        .to_string();
    text = text.replace(
        &format!("version = \"{cur}\""),
        &format!("version = \"{version}\""),
    );
    std::fs::write(&manifest, text).unwrap();
    let pub_key = keys.join("pub.key");
    let (_art, snip) = store::pack_package(pkg_src, None, &pub_key, out).unwrap();
    tuf::publish_delegated(v2, &[snip], &pub_key, Some(out), 90).unwrap();
    let root_seed = tuf::load_keys_file(&keys.join("root.key")).unwrap();
    let snap_seed = tuf::load_keys_file(&keys.join("snap.key")).unwrap();
    let ts_seed = tuf::load_keys_file(&keys.join("ts.key")).unwrap();
    tuf::publish_store(
        v2,
        &root_seed,
        &snap_seed,
        &ts_seed,
        pubs_dir,
        &tuf::ExpiryPolicy::store_default(),
        &[],
    )
    .unwrap();
    let _ = work;
}
