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
