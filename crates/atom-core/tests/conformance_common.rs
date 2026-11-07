//! Harness compartilhado da suíte de conformidade (`conformance.md` §3).
//! Incluído por `tests/conformance.rs` e por `examples/atom-conformance.rs`.
// spec: [CF-1]

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Raiz do workspace (`CARGO_MANIFEST_DIR = crates/atom-core` em testes e examples).
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// Um resultado de cláusula: `{id, pass}` (+ `detail` em falha).
#[derive(Debug, Clone)]
pub struct Check {
    /// Id da cláusula (`P-*`).
    pub id: &'static str,
    /// `true` = passou.
    pub pass: bool,
    /// Detalhe da falha (vazio se passou).
    pub detail: String,
}

impl Check {
    pub fn ok(id: &'static str) -> Self {
        Self {
            id,
            pass: true,
            detail: String::new(),
        }
    }
    pub fn fail(id: &'static str, detail: String) -> Self {
        Self {
            id,
            pass: false,
            detail,
        }
    }
    /// JSON `{id, pass, detail}` (usado pelo binário `atom-conformance`).
    #[allow(dead_code)]
    pub fn json(&self) -> Value {
        json!({"id": self.id, "pass": self.pass, "detail": self.detail})
    }
}

pub struct Harness {
    child: tokio::process::Child,
    lines: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    stdin: tokio::process::ChildStdin,
    next_id: u64,
}

impl Harness {
    pub async fn spawn_cmd(cmd: &[String]) -> Result<Self, String> {
        let mut c = tokio::process::Command::new(&cmd[0]);
        c.args(&cmd[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = c.spawn().map_err(|e| format!("spawn falhou: {e}"))?;
        let stdout = child.stdout.take().ok_or("sem stdout")?;
        let stdin = child.stdin.take().ok_or("sem stdin")?;
        Ok(Self {
            child,
            lines: BufReader::new(stdout).lines(),
            stdin,
            next_id: 1,
        })
    }

    /// Spawna um binário com stdio em pipe (usado pelo teste; o binário usa `spawn_cmd`).
    #[allow(dead_code)]
    pub async fn spawn(bin: &Path) -> Self {
        Self::spawn_cmd(&[bin.display().to_string()])
            .await
            .expect("spawn extensão")
    }

    pub async fn send(&mut self, v: &Value) {
        let mut s = serde_json::to_string(v).unwrap();
        s.push('\n');
        self.stdin.write_all(s.as_bytes()).await.unwrap();
        self.stdin.flush().await.unwrap();
    }

    #[allow(dead_code)]
    pub async fn req(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        self.read().await
    }

    pub async fn read(&mut self) -> Value {
        let line = tokio::time::timeout(std::time::Duration::from_secs(10), self.lines.next_line())
            .await
            .expect("timeout lendo extensão")
            .expect("io")
            .expect("extensão fechou stdout");
        serde_json::from_str(&line).expect("resposta não é JSON")
    }

    /// Checa um request com timeout curto, retornando `Check` em vez de panic
    /// (modo binário precisa de JSON `{id, pass}`, nunca de abort).
    pub async fn try_req(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let mut s = serde_json::to_string(
            &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
        )
        .unwrap();
        s.push('\n');
        self.stdin
            .write_all(s.as_bytes())
            .await
            .map_err(|e| format!("write: {e}"))?;
        self.stdin
            .flush()
            .await
            .map_err(|e| format!("flush: {e}"))?;
        let line = tokio::time::timeout(std::time::Duration::from_secs(10), self.lines.next_line())
            .await
            .map_err(|_| "timeout lendo extensão".to_string())?
            .map_err(|e| format!("io: {e}"))?
            .ok_or_else(|| "extensão fechou stdout".to_string())?;
        serde_json::from_str(&line).map_err(|e| format!("resposta não é JSON: {e}: {line}"))
    }

    /// Encerramento com assert (usado pelo teste; o binário usa `shutdown_best_effort`).
    #[allow(dead_code)]
    pub async fn shutdown(&mut self) {
        let r = self.req("atom.shutdown", json!({})).await;
        assert_eq!(r["result"], json!({}), "shutdown deve responder {{}}");
        self.send(&json!({"jsonrpc": "2.0", "method": "atom.exit", "params": {}}))
            .await;
        tokio::time::timeout(std::time::Duration::from_secs(5), self.child.wait())
            .await
            .expect("extensão não encerrou após atom.exit")
            .expect("wait");
    }

    /// Encerramento sem panic (usado pelo binário `atom-conformance`).
    #[allow(dead_code)]
    pub async fn shutdown_best_effort(&mut self) {
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.req("atom.shutdown", json!({})),
        )
        .await;
        self.send(&json!({"jsonrpc": "2.0", "method": "atom.exit", "params": {}}))
            .await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), self.child.wait()).await;
    }
}

/// Garante o binário da extensão compilado (`target/debug/<bin>`).
/// (Usado pelo teste; o binário recebe o comando pronto.)
#[allow(dead_code)]
pub fn ensure_bin(pkg: &str, bin: &str) -> PathBuf {
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

/// Caso de `service.invoke` com resposta esperada exata.
pub struct InvokeCase {
    /// Contrato (`service`) invocado.
    pub service: &'static str,
    /// Método do contrato.
    pub method: &'static str,
    /// Parâmetros enviados.
    pub params: Value,
    /// Resposta exata esperada.
    pub expected: Value,
}

pub fn ctx() -> Value {
    json!({"tenant": "local", "user": "local", "profile": "conf", "session": "s_1", "trace": "t_1"})
}

pub fn manifest_list(m: &toml::Value, key: &str) -> Vec<Value> {
    match key {
        "provides" => m
            .get("provides")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|p| json!({"contract": p.get("contract"), "version": p.get("version")}))
            .collect(),
        "intercepts" => m
            .get("intercepts")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|p| {
                json!({"match": p.get("match"), "phase": p.get("phase"),
                    "priority": p.get("priority").and_then(|x| x.as_integer()).unwrap_or(100)})
            })
            .collect(),
        _ => m
            .get(key)
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|v| json!(v.as_str().unwrap_or("")))
            .collect(),
    }
}

/// Roda a suíte completa contra um harness já com spawn; devolve os checks.
/// `manifest_dir`: `Some` = checa subconjunto do manifesto (subjects locais).
pub async fn run_ext_suite(h: &mut Harness, tag: &str, manifest_dir: Option<&str>) -> Vec<Check> {
    let mut out = Vec::new();
    // 1. Handshake: protocolo + subconjunto do manifesto. [P-CONN-1]
    match h
        .try_req(
            "atom.initialize",
            json!({"protocol": "atom/1",
                "core": {"version": "1.0.0"},
                "extension": {"name": "conf/subject", "version": "0.0.0"},
                "config": {}, "grants": [], "$ctx": ctx()}),
        )
        .await
    {
        Ok(r) if r["result"]["protocol"] == json!("atom/1") => {
            out.push(Check::ok("P-CONN-1"));
        }
        Ok(r) => out.push(Check::fail(
            "P-CONN-1",
            format!("{tag}: protocolo inesperado: {r}"),
        )),
        Err(e) => out.push(Check::fail("P-CONN-1", format!("{tag}: handshake: {e}"))),
    }
    if let Some(dir) = manifest_dir
        && let (Ok(r), Ok(mtext)) = (
            h.try_req(
                "atom.initialize",
                json!({"protocol": "atom/1",
                "core": {"version": "1.0.0"},
                "extension": {"name": "conf/subject2", "version": "0.0.0"},
                "config": {}, "grants": [], "$ctx": ctx()}),
            )
            .await,
            std::fs::read_to_string(workspace_root().join(dir).join("atom.toml")),
        )
    {
        let m: toml::Value =
            toml::from_str(&mtext).unwrap_or(toml::Value::Table(Default::default()));
        let mut bad: Vec<String> = vec![];
        for key in ["provides", "handles", "intercepts", "subscribes"] {
            let declared = manifest_list(&m, key);
            let returned = r["result"][key].as_array().cloned().unwrap_or_default();
            for item in &returned {
                if !declared.contains(item) {
                    bad.push(format!("{key}={item}"));
                }
            }
        }
        if bad.is_empty() {
            out.push(Check::ok("P-SEC-1"));
        } else {
            out.push(Check::fail(
                "P-SEC-1",
                format!("{tag}: handshake fora do manifesto: {}", bad.join(", ")),
            ));
        }
    }
    // 2. Linha inválida é ignorada (próxima resposta é do próximo request).
    h.send(&json!(
        "isto não é NDJSON válido... espere, é JSON: mas sem rpc"
    ))
    .await;
    match h.try_req("metodo.que.nao.existe", json!({})).await {
        Ok(r) if r["error"]["code"] == json!(-32601) => out.push(Check::ok("P-CANCEL-4")),
        Ok(r) => out.push(Check::fail(
            "P-CANCEL-4",
            format!("{tag}: método desconhecido sem -32601: {r}"),
        )),
        Err(e) => out.push(Check::fail("P-CANCEL-4", format!("{tag}: {e}"))),
    }
    // 3. Notificação não gera resposta (só a resposta do req seguinte chega).
    h.send(&json!({"jsonrpc": "2.0", "method": "metodo.que.nao.existe", "params": {}}))
        .await;
    match h.try_req("outro.que.nao.existe", json!({})).await {
        Ok(r) if r["error"]["code"] == json!(-32601) => out.push(Check::ok("P-STREAM-4")),
        Ok(r) => out.push(Check::fail(
            "P-STREAM-4",
            format!("{tag}: notificação gerou resposta inesperada: {r}"),
        )),
        Err(e) => out.push(Check::fail("P-STREAM-4", format!("{tag}: {e}"))),
    }
    out
}

/// Casos de `service.invoke` contra o harness (com `$ctx` do núcleo).
pub async fn run_invoke_cases(h: &mut Harness, tag: &str, cases: &[InvokeCase]) -> Vec<Check> {
    let mut out = Vec::new();
    for c in cases {
        match h
            .try_req(
                "service.invoke",
                json!({"service": c.service, "method": c.method,
                    "params": c.params, "$ctx": ctx()}),
            )
            .await
        {
            Ok(r) if r.get("error").is_none() && r["result"] == c.expected => {
                out.push(Check::ok("P-DL-3"));
            }
            Ok(r) => out.push(Check::fail(
                "P-DL-3",
                format!("{}#{} em {tag}: {r}", c.service, c.method),
            )),
            Err(e) => out.push(Check::fail(
                "P-DL-3",
                format!("{}#{} em {tag}: {e}", c.service, c.method),
            )),
        }
    }
    out
}

/// Modo core: o harness faz papel de cliente contra um socket real
/// (protocolo do socket: `atom.status`/`atom.call`; `atom.hello` é só WS).
/// Checa conexão aceita + envelope ([P-CONN-2]) e erro -32601 ([P-CANCEL-4]).
/// (Só o binário `atom-conformance` usa; o teste cobre o modo ext.)
#[allow(dead_code)]
#[cfg(unix)]
pub async fn run_core_suite(sock: &Path, _tag: &str) -> Vec<Check> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let mut out = Vec::new();
    let stream = match tokio::net::UnixStream::connect(sock).await {
        Ok(s) => s,
        Err(e) => {
            return vec![Check::fail(
                "P-CONN-2",
                format!("conectar em {}: {e}", sock.display()),
            )];
        }
    };
    let (r, mut w) = stream.into_split();
    let mut lines = BufReader::new(r).lines();
    let mut next_id = 1u64;
    let mut req = async |method: &str, params: Value| -> Result<Value, String> {
        let id = next_id;
        next_id += 1;
        let mut s = serde_json::to_string(
            &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
        )
        .unwrap();
        s.push('\n');
        w.write_all(s.as_bytes())
            .await
            .map_err(|e| format!("write: {e}"))?;
        w.flush().await.map_err(|e| format!("flush: {e}"))?;
        let line = tokio::time::timeout(std::time::Duration::from_secs(10), lines.next_line())
            .await
            .map_err(|_| "timeout lendo core".to_string())?
            .map_err(|e| format!("io: {e}"))?
            .ok_or_else(|| "core fechou conexão".to_string())?;
        serde_json::from_str(&line).map_err(|e| format!("resposta não é JSON: {e}"))
    };
    // `atom.status`: conexão aceita + envelope válido ([P-CONN-2]).
    match req("atom.status", json!({})).await {
        Ok(r) if r.get("error").is_none() => out.push(Check::ok("P-CONN-2")),
        Ok(r) => out.push(Check::fail("P-CONN-2", format!("atom.status: {r}"))),
        Err(e) => out.push(Check::fail("P-CONN-2", format!("atom.status: {e}"))),
    }
    match req("metodo.que.nao.existe", json!({})).await {
        Ok(r) if r["error"]["code"] == json!(-32601) => out.push(Check::ok("P-CANCEL-4")),
        Ok(r) => out.push(Check::fail("P-CANCEL-4", format!("sem -32601: {r}"))),
        Err(e) => out.push(Check::fail("P-CANCEL-4", e.to_string())),
    }
    out
}
