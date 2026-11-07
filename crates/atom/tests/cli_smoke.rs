//! Fumaça do binário `atom`: comandos offline via `--home` temporário.
//! Sem dependência nova (std::process + CARGO_BIN_EXE_atom).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn atom() -> Command {
    Command::new(env!("CARGO_BIN_EXE_atom"))
}

fn run(home: &Path, args: &[&str]) -> Output {
    atom()
        .current_dir(workspace_root())
        .arg("--home")
        .arg(home)
        .args(args)
        .output()
        .expect("spawn atom")
}

fn run_stdin(home: &Path, args: &[&str], input: &str) -> Output {
    let mut child = atom()
        .current_dir(workspace_root())
        .arg("--home")
        .arg(home)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn atom");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().expect("wait atom")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

fn echo_bin() -> PathBuf {
    let b = workspace_root().join("target/debug/echo");
    if !b.exists() {
        let st = Command::new("cargo")
            .args(["build", "-p", "test-echo"])
            .status()
            .expect("cargo build");
        assert!(st.success(), "cargo build -p test-echo falhou");
    }
    b
}

/// Liga o binário compilado ao entrypoint da store (o que o CLI faz).
fn link_entrypoint(home: &Path, package: &str) {
    let store = home.join("data/store");
    let mut found = None;
    let mut stack = vec![store];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().and_then(|n| n.to_str()) == Some("atom.toml") {
                found = Some(p.parent().unwrap().to_path_buf());
            }
        }
    }
    let dir = found.expect("pacote instalado");
    let _ = package;
    let dst = dir.join("bin/echo");
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
    std::fs::copy(echo_bin(), &dst).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut p = std::fs::metadata(&dst).unwrap().permissions();
        p.set_mode(0o755);
        std::fs::set_permissions(&dst, p).unwrap();
    }
}

#[test]
fn version_json() {
    let home = tempfile::tempdir().unwrap();
    let o = run(home.path(), &["version"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).unwrap();
    assert!(v.get("core").is_some() && v.get("protocol").is_some());
}

#[test]
fn profile_lifecycle() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    assert!(run(h, &["profile", "create", "dev"]).status.success());
    let o = run(h, &["profile", "list"]);
    assert!(o.status.success() && stdout(&o).contains("dev"));
    assert!(run(h, &["profile", "use", "dev"]).status.success());
    assert!(run(h, &["profile", "reload"]).status.success());
    let out = h.join("dev.json");
    assert!(
        run(h, &["profile", "export", "dev", out.to_str().unwrap()])
            .status
            .success()
    );
    assert!(
        run(
            h,
            &["profile", "import", out.to_str().unwrap(), "--name", "dev2"]
        )
        .status
        .success()
    );
    assert!(run(h, &["profile", "delete", "dev2"]).status.success());
    // Apagar o perfil em uso (default) falha.
    assert!(!run(h, &["profile", "delete", "default"]).status.success());
}

#[test]
fn ext_config_grants_secret_source_status() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let src = workspace_root().join("testdata/echo");
    assert!(
        run(h, &["ext", "install", "--dev", src.to_str().unwrap()])
            .status
            .success()
    );
    let o = run(h, &["ext", "list"]);
    assert!(o.status.success() && stdout(&o).contains("test/echo"));
    assert!(
        run(h, &["config", "set", "test/echo", "k", "{\"v\":1}"])
            .status
            .success()
    );
    let o = run(h, &["config", "get", "test/echo", "k"]);
    assert!(o.status.success() && stdout(&o).contains("\"v\""));
    assert!(
        run(
            h,
            &["grants", "approve", "test/echo", "service:test.echo@0"]
        )
        .status
        .success()
    );
    let o = run(h, &["grants", "list"]);
    assert!(stdout(&o).contains("test/echo"));
    assert!(
        run(h, &["grants", "deny", "test/echo", "service:test.echo@0"])
            .status
            .success()
    );
    assert!(run(h, &["grants", "pending"]).status.success());
    assert!(
        run_stdin(h, &["secret", "set", "tok"], "s3cr3t")
            .status
            .success()
    );
    let o = run(h, &["secret", "list"]);
    assert!(o.status.success() && stdout(&o).contains("tok"));
    assert!(run(h, &["secret", "delete", "tok"]).status.success());
    assert!(
        run(
            h,
            &[
                "source",
                "add",
                "local",
                "file:///tmp/nada/",
                "--keys",
                "ed25519:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
            ]
        )
        .status
        .success()
    );
    let o = run(h, &["source", "list"]);
    assert!(stdout(&o).contains("local"));
    assert!(run(h, &["source", "remove", "local"]).status.success());
    // Status sem daemon: lê do disco.
    let o = run(h, &["status"]);
    assert!(o.status.success() && stdout(&o).contains("test/echo"));
    // Bind/unbind.
    assert!(
        run(h, &["bind", "test.echo@0", "test/echo"])
            .status
            .success()
    );
    assert!(run(h, &["unbind", "test.echo@0"]).status.success());
    // Erros definidos: remover o que não existe, config de pacote ausente.
    assert!(!run(h, &["ext", "remove", "nada/aqui"]).status.success());
}

#[test]
fn call_echo_direct() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let src = workspace_root().join("testdata/echo");
    assert!(
        run(h, &["ext", "install", "--dev", src.to_str().unwrap()])
            .status
            .success()
    );
    link_entrypoint(h, "test/echo");
    let o = run(h, &["call", "test.echo@0", "echo", "{\"x\":1}"]);
    assert!(
        o.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(stdout(&o).contains("\"x\":1") || stdout(&o).contains("{\"x\":1}"));
    // Params inválidos → erro.
    assert!(
        !run(h, &["call", "test.echo@0", "echo", "{invalido"])
            .status
            .success()
    );
    // Sem provider → erro.
    assert!(!run(h, &["call", "nada@0", "m", "{}"]).status.success());
    // Stdin como params.
    let o = run_stdin(h, &["call", "test.echo@0", "echo", "-"], "{\"y\":2}");
    assert!(o.status.success());
    assert!(stdout(&o).contains("\"y\":2"));
}

#[test]
fn keygen_pack_publish_search_info() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let key = h.join("pub.key");
    let o = run(h, &["ext", "keygen", "--out", key.to_str().unwrap()]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out_dir = h.join("out");
    let src = workspace_root().join("testdata/echo");
    let o = run(
        h,
        &[
            "ext",
            "pack",
            src.to_str().unwrap(),
            "--key",
            key.to_str().unwrap(),
            "--out-dir",
            out_dir.to_str().unwrap(),
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let snips: Vec<PathBuf> = std::fs::read_dir(&out_dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
        .collect();
    assert!(!snips.is_empty());
    let store_key = h.join("store.key");
    assert!(
        run(h, &["ext", "keygen", "--out", store_key.to_str().unwrap()])
            .status
            .success()
    );
    let v1 = h.join("v1");
    let mut full: Vec<&str> = vec!["ext", "publish"];
    for s in &snips {
        full.push(s.to_str().unwrap());
    }
    full.extend([
        "--store-dir",
        v1.to_str().unwrap(),
        "--store-key",
        store_key.to_str().unwrap(),
    ]);
    let o = atom()
        .current_dir(workspace_root())
        .arg("--home")
        .arg(h)
        .args(&full)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    // Source + search + info via CLI.
    let _ = stdout(&o);
    // Extrai a pubkey da loja do catalog publicado p/ fixar confiança.
    let cfg_raw = std::fs::read(v1.join("config.json")).unwrap_or_default();
    let cfg: serde_json::Value = serde_json::from_slice(&cfg_raw).unwrap();
    let keys = cfg.get("index_signature_keys").cloned().unwrap_or_default();
    let key_str = keys
        .as_array()
        .and_then(|a| a.first())
        .and_then(|k| k.as_str())
        .unwrap_or("")
        .to_string();
    assert!(!key_str.is_empty());
    assert!(
        run(
            h,
            &[
                "source",
                "add",
                "local",
                &format!("file://{}/", v1.display()),
                "--keys",
                &key_str
            ]
        )
        .status
        .success()
    );
    let o = run(h, &["ext", "search", "echo"]);
    assert!(
        o.status.success() && stdout(&o).contains("test/echo"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let o = run(h, &["ext", "info", "test/echo"]);
    assert!(o.status.success());
}

#[cfg(unix)]
#[test]
fn serve_status_stop_cycle() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let src = workspace_root().join("testdata/echo");
    assert!(
        run(h, &["ext", "install", "--dev", src.to_str().unwrap()])
            .status
            .success()
    );
    link_entrypoint(h, "test/echo");
    let mut child = atom()
        .current_dir(workspace_root())
        .arg("--home")
        .arg(h)
        .args(["serve"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn serve");
    let sock = h.join("runtime/atom-default.sock");
    let mut up = false;
    for _ in 0..200 {
        if sock.exists() {
            // Sonda até responder.
            if run(h, &["status"]).status.success() {
                up = true;
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(up, "daemon não subiu");
    // Status via daemon.
    let o = run(h, &["status"]);
    assert!(stdout(&o).contains("daemon") && stdout(&o).contains("true"));
    // Call via daemon.
    let o = run(h, &["call", "test.echo@0", "echo", "{\"x\":3}"]);
    assert!(o.status.success() && stdout(&o).contains("\"x\":3"));
    // Stop derruba.
    let o = run(h, &["stop"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let st = child.wait().expect("wait serve");
    assert!(st.success());
}

#[test]
fn store_gc_dry_run() {
    let home = tempfile::tempdir().unwrap();
    let o = run(home.path(), &["store", "gc", "--dry-run"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}

/// `[C-AMB-2]`: `grants reload` aceita o `grants.toml` externo.
// spec: [C-AMB-2]
#[test]
fn grants_reload_adopts_external() {
    use std::io::Write as _;
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let src = workspace_root().join("testdata/echo");
    assert!(
        run(h, &["ext", "install", "--dev", src.to_str().unwrap()])
            .status
            .success()
    );
    let grants = h.join("config/profiles/default/grants.toml");
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&grants)
        .unwrap();
    writeln!(
        f,
        "[\"test/echo\"]\ngranted = [\"service:test.echo@0\"]\ndenied = []\n"
    )
    .unwrap();
    drop(f);
    let o = run(h, &["grants", "reload"]);
    assert!(
        o.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    let o = run(h, &["grants", "list"]);
    assert!(stdout(&o).contains("service:test.echo@0"));
}

/// `[C-AMB-3]`: `profile reload` recarrega campos de segurança do disco.
// spec: [C-AMB-3]
#[test]
fn profile_reload_adopts_security_fields() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    assert!(run(h, &["profile", "create", "rel"]).status.success());
    let o = run(h, &["--profile", "rel", "status"]);
    assert!(o.status.success());
    let o = run(h, &["--profile", "rel", "profile", "reload"]);
    assert!(
        o.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(stdout(&o).contains("recarregado"));
}

/// `[P-EV-3]`: `events tail --from` inválido falha com mensagem PT.
// spec: [P-EV-3]
#[test]
fn events_tail_bad_cursor_fails() {
    let home = tempfile::tempdir().unwrap();
    let o = run(home.path(), &["events", "tail", "--from", "sem-separador"]);
    assert!(!o.status.success());
    let o = run(home.path(), &["events", "tail"]);
    assert!(
        o.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

/// `[C-UI-1]`: instalação sem TTY imprime o aviso advisory de não-enforcement.
// spec: [C-UI-1]
#[test]
fn install_prints_advisory_warning() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let work = tempfile::tempdir().unwrap();
    let src = workspace_root().join("testdata/echo");
    let dst = work.path().join("echo-adv");
    copy_dir(&src, &dst);
    let manifest = std::fs::read_to_string(dst.join("atom.toml")).unwrap();
    // `capabilities` é campo de topo do manifesto: insere antes da primeira
    // tabela (anexar no fim cairia dentro de `[[provides]]`).
    let patched = manifest.replacen(
        "protocol = \"atom/1\"",
        "protocol = \"atom/1\"\ncapabilities = [\"service:test.echo@0\", \"fs.read:pkg\"]",
        1,
    );
    assert!(patched != manifest, "gancho de capabilities não aplicado");
    std::fs::write(dst.join("atom.toml"), patched).unwrap();
    let o = run(h, &["ext", "install", "--dev", dst.to_str().unwrap()]);
    assert!(
        o.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    let out = stdout(&o);
    let err = String::from_utf8_lossy(&o.stderr).to_string();
    assert!(
        out.contains("advisory") || err.contains("advisory"),
        "out: {out} err: {err}"
    );
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap().flatten() {
        let p = e.path();
        let d = dst.join(e.file_name());
        if p.is_dir() {
            copy_dir(&p, &d);
        } else {
            std::fs::copy(&p, &d).unwrap();
        }
    }
}

/// `[ST2-1]` (CLI): `source add --root` com hash divergente recusa antes de persistir.
// spec: [ST2-1]
#[test]
fn source_add_root_mismatch_refused() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let store = h.join("loja/v2");
    std::fs::create_dir_all(store.join("metadata")).unwrap();
    std::fs::write(store.join("metadata/1.root.json"), b"{}").unwrap();
    let url = format!("file://{}/", store.display());
    let o = run(
        h,
        &[
            "source",
            "add",
            "tuf",
            &url,
            "--root",
            "0000000000000000000000000000000000000000000000000000000000000000",
        ],
    );
    assert!(!o.status.success());
    let o = run(h, &["source", "list"]);
    assert!(!stdout(&o).contains("tuf"));
}

/// `source list` avisa source v1 (migração pendente).
#[test]
fn source_list_warns_v1() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    assert!(
        run(
            h,
            &[
                "source",
                "add",
                "local",
                "file:///tmp/nada/",
                "--keys",
                "ed25519:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
            ]
        )
        .status
        .success()
    );
    let o = run(h, &["source", "list"]);
    assert!(o.status.success() && stdout(&o).contains("aviso"));
}

/// `[ST2-1..4]` (CLI→v2): `keygen→pack→publish --v2→source add --root→search→info→install`
/// contra source `file://` com `expires` reais futuros; `timestamp` expirado → `tuf expired`.
#[test]
fn cli_v2_publish_search_info_install_with_real_expiry() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let mkkey = |n: &str| {
        let p = h.join(n);
        assert!(
            run(h, &["ext", "keygen", "--out", p.to_str().unwrap()])
                .status
                .success()
        );
        p
    };
    let pub_key = mkkey("pub.key");
    let root_key = mkkey("root.key");
    let snap_key = mkkey("snap.key");
    let ts_key = mkkey("ts.key");
    let out_dir = h.join("out");
    let src = workspace_root().join("testdata/echo");
    let o = run(
        h,
        &[
            "ext",
            "pack",
            src.to_str().unwrap(),
            "--key",
            pub_key.to_str().unwrap(),
            "--out-dir",
            out_dir.to_str().unwrap(),
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let snips: Vec<PathBuf> = std::fs::read_dir(&out_dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
        .collect();
    assert!(!snips.is_empty());
    let v2 = h.join("v2");
    let pubs_dir = h.join("publishers");
    // `publish_store` verifica o delegado contra `publishers/test.toml`,
    // então pré-registramos a pubkey do snippet (mesmo formato v1).
    std::fs::create_dir_all(&pubs_dir).unwrap();
    let snip_raw = std::fs::read(&snips[0]).unwrap();
    let snip_json: serde_json::Value = serde_json::from_slice(&snip_raw).unwrap();
    let pubkey_ed = snip_json
        .get("publisher_key")
        .and_then(|k| k.as_str())
        .unwrap_or("")
        .to_string();
    assert!(!pubkey_ed.is_empty(), "snippet sem publisher_key");
    std::fs::write(
        pubs_dir.join("test.toml"),
        format!("key = \"{pubkey_ed}\"\n"),
    )
    .unwrap();
    let o = run(
        h,
        &[
            "ext",
            "publish",
            "--v2",
            "--v2-dir",
            v2.to_str().unwrap(),
            "--publisher-key",
            pub_key.to_str().unwrap(),
            "--targets-key",
            root_key.to_str().unwrap(),
            "--snapshot-key",
            snap_key.to_str().unwrap(),
            "--timestamp-key",
            ts_key.to_str().unwrap(),
            "--init-root",
            "--root-key",
            root_key.to_str().unwrap(),
            "--publishers-dir",
            pubs_dir.to_str().unwrap(),
            snips[0].to_str().unwrap(),
        ],
    );
    assert!(
        o.status.success(),
        "publish --v2: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    let root_sha = {
        let raw = std::fs::read(v2.join("metadata/1.root.json")).expect("genesis via publish --v2");
        let mut hh = sha2::Sha256::new();
        use sha2::Digest as _;
        hh.update(&raw);
        format!("{:x}", hh.finalize())
    };
    assert!(
        run(
            h,
            &[
                "source",
                "add",
                "local2",
                &format!("file://{}/", v2.display()),
                "--root",
                &root_sha
            ]
        )
        .status
        .success()
    );
    let o = run(h, &["ext", "search", "echo"]);
    assert!(
        o.status.success() && stdout(&o).contains("test/echo"),
        "search: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    let o = run(h, &["ext", "info", "test/echo"]);
    assert!(
        o.status.success(),
        "info: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    let o = run(h, &["ext", "install", "test/echo"]);
    assert!(
        o.status.success(),
        "install: {} {}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    link_entrypoint(h, "test/echo");
    let o = run(h, &["call", "test.echo@0", "echo", "{\"x\": 1}"]);
    assert!(
        o.status.success() && stdout(&o).contains("\"x\":1"),
        "call: {} {}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    // `timestamp` expirado → `install --refresh` recusa com `tuf expired`.
    let o = run(
        h,
        &[
            "ext",
            "publish",
            "--v2",
            "--v2-dir",
            v2.to_str().unwrap(),
            "--timestamp-key",
            ts_key.to_str().unwrap(),
            "--timestamp-only",
        ],
    );
    assert!(o.status.success());
    // Rebaixa o timestamp p/ passado via re-assinatura local (mesma chave do teste).
    expire_ts_local(&v2, &ts_key);
    let o = run(h, &["ext", "install", "test/echo", "--refresh"]);
    assert!(
        !o.status.success() && String::from_utf8_lossy(&o.stderr).contains("tuf expired"),
        "expirado devia recusar: {} {}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
}

/// Re-assina `metadata/timestamp.json` com `expires` passado (mesma chave do teste).
fn expire_ts_local(v2: &Path, ts_key: &Path) {
    let raw = std::fs::read(v2.join("metadata/timestamp.json")).unwrap();
    let mut env: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    env["signed"]["expires"] = serde_json::json!("2000-01-01T00:00:00Z");
    // Assinatura canônica TUF (chaves ordenadas, sem espaços) — espelha `store_tuf::canonical`.
    let signed = env["signed"].clone();
    let msg = tuf_canonical(&signed);
    let seed = load_seed_local(ts_key);
    let sig = ed_sign(&seed, &msg);
    env["signatures"] = serde_json::json!([{"keyid": tuf_keyid_local(&seed), "sig": sig}]);
    std::fs::write(
        v2.join("metadata/timestamp.json"),
        serde_json::to_vec_pretty(&env).unwrap(),
    )
    .unwrap();
}

fn load_seed_local(p: &Path) -> [u8; 32] {
    let t = std::fs::read_to_string(p).unwrap();
    let part = t
        .trim()
        .strip_prefix("ed25519-seed:")
        .unwrap_or(t.trim())
        .trim();
    use base64::Engine as _;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(part)
        .unwrap();
    raw.try_into().unwrap()
}

fn ed_sign(seed: &[u8; 32], msg: &[u8]) -> String {
    use base64::Engine as _;
    use ed25519_dalek::Signer as _;
    let sig = ed25519_dalek::SigningKey::from_bytes(seed).sign(msg);
    base64::engine::general_purpose::STANDARD.encode(sig.to_bytes())
}

fn tuf_keyid_local(seed: &[u8; 32]) -> String {
    use sha2::Digest as _;
    let pk = ed25519_dalek::SigningKey::from_bytes(seed)
        .verifying_key()
        .to_bytes();
    let mut h = sha2::Sha256::new();
    h.update(pk);
    let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    hex[..8].to_string()
}

fn tuf_canonical(v: &serde_json::Value) -> Vec<u8> {
    fn w(v: &serde_json::Value, out: &mut Vec<u8>) {
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
                    w(e, out);
                }
                out.push(b']');
            }
            serde_json::Value::Object(m) => {
                out.push(b'{');
                let mut ks: Vec<&String> = m.keys().collect();
                ks.sort();
                for (i, k) in ks.iter().enumerate() {
                    if i > 0 {
                        out.push(b',');
                    }
                    out.extend_from_slice(&serde_json::to_string(k).unwrap().into_bytes());
                    out.push(b':');
                    w(&m[*k], out);
                }
                out.push(b'}');
            }
        }
    }
    let mut out = vec![];
    w(v, &mut out);
    out
}
