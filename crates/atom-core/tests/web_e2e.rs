//! Aceite v0.8: WS com token (`serve --ws`) + ui-mods que adicionam views
//! sem tocar no shell; troca de renderer preserva itens/seções.

#![forbid(unsafe_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use atom_core::{bus::Core, pkg};
use serde_json::{Value, json};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn bin(name: &str, pkg: &str) -> PathBuf {
    // Sempre rebuilda (incremental é barato e evita binário obsoleto).
    let st = std::process::Command::new("cargo")
        .args(["build", "-p", pkg])
        .status()
        .expect("cargo build");
    assert!(st.success(), "cargo build -p {pkg} falhou");
    let b = workspace_root().join(format!("target/debug/{name}"));
    assert!(b.exists(), "binário {name} ausente após build");
    b
}

fn echo_bin() -> PathBuf {
    bin("echo", "test-echo")
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

fn install_data(core: &mut Core, src: &str, pkg_name: &str) {
    let full = workspace_root().join(src);
    let id = pkg::install_dev(&core.paths, &core.data_store, &full).unwrap();
    assert!(id.starts_with(pkg_name), "id {id} != {pkg_name}@");
    core.refresh().unwrap();
}

// ---------------------------------------------------------------------------
// WS via daemon real (`atom serve --ws`).

struct Daemon {
    child: Child,
    url: String,
    token: String,
}

fn spawn_daemon(home: &Path) -> Daemon {
    let atom_bin = bin("atom", "atom");
    let mut child = Command::new(atom_bin)
        .args([
            "--home",
            &home.to_string_lossy(),
            "-p",
            "default",
            "serve",
            "--ws",
            "127.0.0.1:0",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn atom serve");
    let stderr = child.stderr.take().expect("stderr");
    let mut reader = BufReader::new(stderr);
    let mut ws_url = String::new();
    let start = std::time::Instant::now();
    while start.elapsed().as_secs() < 30 {
        let mut line = String::new();
        reader.read_line(&mut line).expect("ler stdout do daemon");
        if let Some(rest) = line.trim().strip_prefix("ws ") {
            ws_url = rest.split_whitespace().next().unwrap_or("").to_string();
            if !ws_url.is_empty() {
                break;
            }
        }
    }
    assert!(!ws_url.is_empty(), "daemon não anunciou ws");
    // Token gravado em <home>/data/profiles/default/serve.token.
    let token_path = home.join("data/profiles/default/serve.token");
    let mut token = String::new();
    for _ in 0..100 {
        if let Ok(t) = std::fs::read_to_string(&token_path) {
            token = t.trim().to_string();
            if !token.is_empty() {
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(!token.is_empty(), "sem token do WS");
    // Devolve o stdout p/ não SIGPIPEar o filho ao fechar o reader.
    Daemon {
        child,
        url: ws_url,
        token,
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

async fn ws_call(url: &str, token: &str, method: &str, params: Value, id: &str) -> Value {
    use tokio_tungstenite::tungstenite::Message;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("{url}/?token={token}"))
        .await
        .expect("conectar WS");
    let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    futures_util::SinkExt::send(
        &mut ws,
        Message::Text(serde_json::to_string(&req).unwrap().into()),
    )
    .await
    .unwrap();
    while let Some(msg) = futures_util::StreamExt::next(&mut ws).await {
        let msg = msg.expect("msg WS");
        if let Message::Text(t) = msg {
            let v: Value = serde_json::from_str(&t).unwrap();
            if v.get("id").and_then(|i| i.as_str()) == Some(id) {
                let _ = ws.close(None).await;
                return v;
            }
        }
    }
    panic!("WS fechou sem resposta");
}

#[tokio::test]
async fn ws_hello_call_and_auth() {
    let home = tempfile::tempdir().unwrap();
    // Perfil com echo instalável; daemon serve o mesmo ATOM_HOME.
    {
        let mut core = Core::open_with("default", Some(home.path())).await.unwrap();
        install_data(&mut core, "testdata/echo", "test/echo");
        link_bin(&core, "test/echo", &echo_bin());
        install_data(&mut core, "testdata/fake-llm", "test/fake-llm");
        link_bin(&core, "test/fake-llm", &bin("fake-llm", "test-fake-llm"));
        core.shutdown().await;
    }
    let _daemon = spawn_daemon(home.path());

    // hello ok.
    let r = ws_call(
        &_daemon.url,
        &_daemon.token,
        "atom.hello",
        json!({"protocol": "atom/1", "client": {"name": "t", "version": "0"}}),
        "h1",
    )
    .await;
    assert!(r.get("result").is_some(), "hello falhou: {r}");

    // Token errado: handshake recusado.
    let bad = tokio_tungstenite::connect_async(format!("{}/?token=x", _daemon.url)).await;
    assert!(bad.is_err(), "WS aceitou token inválido");

    // call direto.
    let r = ws_call(
        &_daemon.url,
        &_daemon.token,
        "atom.call",
        json!({"service": "test.echo@0", "method": "echo", "params": {"x": 7}}),
        "c1",
    )
    .await;
    assert_eq!(r["result"], json!({"x": 7}), "call falhou: {r}");

    // Stream fim a fim sobre WS.
    use tokio_tungstenite::tungstenite::Message;
    let (mut ws, _) =
        tokio_tungstenite::connect_async(format!("{}/?token={}", _daemon.url, _daemon.token))
            .await
            .unwrap();
    futures_util::SinkExt::send(
        &mut ws,
        Message::Text(
            json!({"jsonrpc": "2.0", "id": "s1", "method": "atom.call",
        "params": {"service": "ai.llm@0", "method": "complete",
            "params": {"messages": []}, "stream": true}})
            .to_string()
            .into(),
        ),
    )
    .await
    .unwrap();
    let mut saw_stream = false;
    let mut saw_end = false;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while std::time::Instant::now() < deadline && !(saw_stream && saw_end) {
        let msg = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            futures_util::StreamExt::next(&mut ws),
        )
        .await
        .expect("timeout WS")
        .expect("fim WS")
        .expect("erro WS");
        if let Message::Text(t) = msg {
            let v: Value = serde_json::from_str(&t).unwrap();
            if v.get("id").and_then(|i| i.as_str()) == Some("s1")
                && v.get("result").and_then(|r| r.get("stream")).is_some()
            {
                saw_stream = true;
                continue;
            }
            if v.get("method").and_then(|m| m.as_str()) == Some("atom.stream.end") {
                saw_end = true;
            }
        }
    }
    assert!(
        saw_stream && saw_end,
        "stream incompleto: stream={saw_stream} end={saw_end}"
    );
    let _ = ws.close(None).await;
}

// ---------------------------------------------------------------------------
// ui-mods: view nova sem tocar no shell; troca de renderer preserva conteúdo.

/// GET HTTP cru (sem deps extras). Retorna (status, corpo).
fn http_get(url: &str) -> (String, Vec<u8>) {
    let url = url.strip_prefix("http://").unwrap_or(url);
    let (host_port, path) = url.split_once('/').unwrap_or((url, ""));
    let path = format!("/{path}");
    let mut s = std::net::TcpStream::connect(host_port).expect("conectar ui-web");
    s.set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes()).unwrap();
    let mut buf = vec![];
    s.read_to_end(&mut buf).unwrap();
    let head_end = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4)
        .unwrap_or(0);
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let status = head.lines().next().unwrap_or("").to_string();
    (status, buf[head_end..].to_vec())
}

/// Bomba o bus até `cond` ou timeout (ui-web fala via bus sem drive ativo).
async fn pump_until(core: &mut Core, cond: impl Fn(&Core) -> bool) {
    let start = std::time::Instant::now();
    while !cond(core) {
        while let Some(i) = core.host.try_incoming() {
            core.pump_one(i).await;
        }
        if start.elapsed() > std::time::Duration::from_secs(15) {
            panic!("timeout esperando condição do bus");
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

fn install_ui_stack(core: &mut Core) {
    // ui-web é executável: precisa do binário linkado (como nos outros e2e).
    install_data(core, "extensions/ui-web", "atom/ui-web");
    link_bin(core, "atom/ui-web", &bin("ui-web", "atom-ui-web"));
    core.refresh().unwrap();
    for (src, name) in [
        ("extensions/ui-layout-basic", "atom/layout-basic"),
        ("extensions/ui-menu-top", "atom/menu-top"),
        ("extensions/ui-menu-side", "atom/menu-side"),
        ("extensions/ui-settings-popup", "atom/settings-popup"),
        ("extensions/ui-settings-panel", "atom/settings-panel"),
        ("extensions/ui-chat", "atom/chat-ui"),
        ("testdata/ui-hello-view", "test/hello-view"),
    ] {
        install_data(core, src, name);
    }
    // Bindings iniciais: layout + menu superior + settings popup.
    core.config
        .bindings
        .insert("ui.root".to_string(), "atom/layout-basic".to_string());
    core.config
        .bindings
        .insert("ui.views".to_string(), "atom/layout-basic".to_string());
    core.config
        .bindings
        .insert("ui.menu.items".to_string(), "atom/menu-top".to_string());
    core.config.bindings.insert(
        "ui.settings.sections".to_string(),
        "atom/settings-popup".to_string(),
    );
    core.config.bindings.insert(
        "ui.status.items".to_string(),
        "atom/layout-basic".to_string(),
    );
    atom_core::profile::save(&core.paths, &core.config).unwrap();
    core.refresh().unwrap();
}

/// GET HTTP com pump concorrente do bus (ui-web responde via bus).
/// Sem isso o teste trava: ninguém bombearia o `atom.call` do shell.
async fn http_get_pumped(core: &mut Core, url: &str) -> (String, Vec<u8>) {
    let u = url.to_string();
    let h = std::thread::spawn(move || http_get(&u));
    let start = std::time::Instant::now();
    loop {
        while let Some(i) = core.host.try_incoming() {
            core.pump_one(i).await;
        }
        if h.is_finished() {
            return h.join().unwrap();
        }
        if start.elapsed() > std::time::Duration::from_secs(15) {
            panic!("timeout no GET");
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

fn mods_of(body: &[u8]) -> Value {
    serde_json::from_slice(body).expect("api/mods JSON")
}

fn point_items(doc: &Value, point: &str) -> Vec<Value> {
    let mut out = vec![];
    let mods = match doc.get("mods").and_then(|m| m.as_array()) {
        Some(m) => m,
        None => return out,
    };
    for m in mods {
        if let Some(arr) = m
            .get("contributes")
            .and_then(|c| c.get(point))
            .and_then(|a| a.as_array())
        {
            for it in arr {
                out.push(it.clone());
            }
        }
    }
    out
}

#[tokio::test]
async fn ui_mod_adds_view_and_renderer_swap_keeps_content() {
    let _ = tracing_subscriber::fmt::try_init();
    let home = tempfile::tempdir().unwrap();
    let mut core = Core::open_with("web", Some(home.path())).await.unwrap();
    install_ui_stack(&mut core);
    // ui-web sobe no boot (onStartup) e anuncia a base.
    core.boot().await.unwrap();
    pump_until(&mut core, |c| c.ui_url().is_some()).await;
    let base = core.ui_url().unwrap();

    // Shell serve raiz + assets.
    let (st, body) = http_get_pumped(&mut core, &format!("{base}/")).await;
    assert!(st.contains("200"), "GET /: {st}");
    assert!(String::from_utf8_lossy(&body).contains("ui-root"));

    // (1) um ui-mod adiciona views/menus/settings sem tocar no shell.
    let (_st, body) = http_get_pumped(&mut core, &format!("{base}/api/mods")).await;
    let doc = mods_of(&body);
    let views = point_items(&doc, "ui.views");
    assert!(
        views.iter().any(|v| v["id"] == "hello"),
        "sem view hello: {views:?}"
    );
    assert!(
        views.iter().any(|v| v["id"] == "chat"),
        "sem view chat: {views:?}"
    );
    let menus = point_items(&doc, "ui.menu.items");
    assert!(menus.iter().any(|m| m["id"] == "menu-hello"));
    assert!(menus.iter().any(|m| m["id"] == "menu-chat"));
    let sections = point_items(&doc, "ui.settings.sections");
    assert!(sections.iter().any(|s| s["id"] == "hello-settings"));
    assert!(sections.iter().any(|s| s["id"] == "chat-settings"));
    assert_eq!(doc["bindings"]["ui.menu.items"], json!("atom/menu-top"));

    // Asset de mod via shell.
    let (st, body) = http_get_pumped(
        &mut core,
        &format!("{base}/mod/atom/layout-basic/assets/layout.js"),
    )
    .await;
    assert!(st.contains("200"), "asset layout: {st}");
    assert!(String::from_utf8_lossy(&body).contains("renderRoot"));

    // (2) Troca renderer menu + settings: conteúdo idêntico, renderer novo.
    core.config
        .bindings
        .insert("ui.menu.items".to_string(), "atom/menu-side".to_string());
    core.config.bindings.insert(
        "ui.settings.sections".to_string(),
        "atom/settings-panel".to_string(),
    );
    atom_core::profile::save(&core.paths, &core.config).unwrap();
    core.refresh().unwrap();
    let (st, body) = http_get_pumped(&mut core, &format!("{base}/api/mods")).await;
    assert!(st.contains("200"), "api/mods após troca: {st}");
    let doc2 = mods_of(&body);
    assert_eq!(doc2["bindings"]["ui.menu.items"], json!("atom/menu-side"));
    assert_eq!(
        doc2["bindings"]["ui.settings.sections"],
        json!("atom/settings-panel")
    );
    for point in ["ui.views", "ui.menu.items", "ui.settings.sections"] {
        let before: Vec<String> = point_items(&doc, point)
            .iter()
            .map(|v| v["id"].as_str().unwrap_or("").to_string())
            .collect();
        let after: Vec<String> = point_items(&doc2, point)
            .iter()
            .map(|v| v["id"].as_str().unwrap_or("").to_string())
            .collect();
        assert_eq!(
            before, after,
            "conteúdo de {point} mudou na troca de renderer"
        );
    }
    core.shutdown().await;
}
