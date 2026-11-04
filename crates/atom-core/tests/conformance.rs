//! Suite de conformidade do protocolo `atom/1` (v1.0).
//!
//! Roda os mesmos invariantes universais contra qualquer extensão:
//! handshake, envelope de erro, linhas inválidas ignoradas, notificações sem
//! resposta, `service.invoke` e encerramento. Para usar com outra extensão,
//! adicione um `Subject` em `subjects()` com seus casos de invoke.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn ensure_bin(pkg: &str, bin: &str) -> PathBuf {
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
struct InvokeCase {
    service: &'static str,
    method: &'static str,
    params: Value,
    expected: Value,
}

struct Subject {
    /// Nome p/ mensagens de falha.
    name: &'static str,
    /// Pacote cargo (p/ compilar) + binário.
    pkg: &'static str,
    bin: &'static str,
    /// Pasta com `atom.toml` (checagem de subconjunto do handshake).
    manifest_dir: &'static str,
    cases: Vec<InvokeCase>,
}

fn subjects() -> Vec<Subject> {
    vec![
        Subject {
            name: "echo (atom-sdk)",
            pkg: "test-echo",
            bin: "echo",
            manifest_dir: "testdata/echo",
            cases: vec![InvokeCase {
                service: "test.echo@0",
                method: "echo",
                params: json!({"x": 1}),
                expected: json!({"x": 1}),
            }],
        },
        Subject {
            name: "prompt-fake",
            pkg: "test-prompt-fake",
            bin: "prompt-fake",
            manifest_dir: "testdata/prompt-fake",
            cases: vec![
                InvokeCase {
                    service: "ui.prompt@0",
                    method: "ask",
                    params: json!({}),
                    expected: json!({"value": Value::Null}),
                },
                InvokeCase {
                    service: "ui.prompt@0",
                    method: "confirm",
                    params: json!({"title": "BLOCKME"}),
                    expected: json!({"ok": false}),
                },
            ],
        },
    ]
}

struct Harness {
    child: tokio::process::Child,
    lines: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    stdin: tokio::process::ChildStdin,
    next_id: u64,
}

impl Harness {
    async fn spawn(bin: &Path) -> Self {
        let mut child = tokio::process::Command::new(bin)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn extensão");
        let stdout = child.stdout.take().expect("stdout");
        let stdin = child.stdin.take().expect("stdin");
        Self {
            child,
            lines: BufReader::new(stdout).lines(),
            stdin,
            next_id: 1,
        }
    }

    async fn send(&mut self, v: &Value) {
        let mut s = serde_json::to_string(v).unwrap();
        s.push('\n');
        self.stdin.write_all(s.as_bytes()).await.unwrap();
        self.stdin.flush().await.unwrap();
    }

    async fn req(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        self.read().await
    }

    async fn read(&mut self) -> Value {
        let line = tokio::time::timeout(std::time::Duration::from_secs(10), self.lines.next_line())
            .await
            .expect("timeout lendo extensão")
            .expect("io")
            .expect("extensão fechou stdout");
        serde_json::from_str(&line).expect("resposta não é JSON")
    }

    async fn shutdown(&mut self) {
        let r = self.req("atom.shutdown", json!({})).await;
        assert_eq!(r["result"], json!({}), "shutdown deve responder {{}}");
        self.send(&json!({"jsonrpc": "2.0", "method": "atom.exit", "params": {}}))
            .await;
        tokio::time::timeout(std::time::Duration::from_secs(5), self.child.wait())
            .await
            .expect("extensão não encerrou após atom.exit")
            .expect("wait");
    }
}

fn ctx() -> Value {
    json!({"tenant": "local", "user": "local", "profile": "conf", "session": "s_1", "trace": "t_1"})
}

fn manifest_list(m: &toml::Value, key: &str) -> Vec<Value> {
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

#[tokio::test]
async fn conformance() {
    for s in subjects() {
        let bin = ensure_bin(s.pkg, s.bin);
        let mut h = Harness::spawn(&bin).await;
        let tag = s.name;

        // 1. Handshake: protocolo + subconjunto do manifesto.
        let r = h
            .req(
                "atom.initialize",
                json!({"protocol": "atom/1",
                    "core": {"version": "1.0.0"},
                    "extension": {"name": "conf/subject", "version": "0.0.0"},
                    "config": {}, "grants": [], "$ctx": ctx()}),
            )
            .await;
        assert_eq!(r["result"]["protocol"], json!("atom/1"), "{tag}: protocolo");
        let mtext =
            std::fs::read_to_string(workspace_root().join(s.manifest_dir).join("atom.toml"))
                .unwrap();
        let m: toml::Value = toml::from_str(&mtext).unwrap();
        for key in ["provides", "handles", "intercepts", "subscribes"] {
            let declared = manifest_list(&m, key);
            let returned = r["result"][key].as_array().cloned().unwrap_or_default();
            for item in &returned {
                assert!(
                    declared.contains(item),
                    "{tag}: handshake devolve {key}={item} fora do manifesto"
                );
            }
        }

        // 2. Linha inválida é ignorada (próxima resposta é do próximo request).
        h.send(&json!(
            "isto não é NDJSON válido... espere, é JSON: mas sem rpc"
        ))
        .await;
        let r = h.req("metodo.que.nao.existe", json!({})).await;
        assert_eq!(
            r["error"]["code"],
            json!(-32601),
            "{tag}: método desconhecido → -32601"
        );

        // 3. Notificação não gera resposta (só a resposta do req seguinte chega).
        h.send(&json!({"jsonrpc": "2.0", "method": "metodo.que.nao.existe", "params": {}}))
            .await;
        let r = h.req("outro.que.nao.existe", json!({})).await;
        assert_eq!(
            r["error"]["code"],
            json!(-32601),
            "{tag}: notificação sem resposta"
        );

        // 4. Casos de service.invoke (com $ctx do núcleo).
        for c in &s.cases {
            let r = h
                .req(
                    "service.invoke",
                    json!({"service": c.service, "method": c.method,
                        "params": c.params, "$ctx": ctx()}),
                )
                .await;
            assert!(
                r.get("error").is_none(),
                "{tag}: invoke errou: {}",
                r["error"]
            );
            assert_eq!(r["result"], c.expected, "{tag}: {}#{}", c.service, c.method);
        }

        // 5. Encerramento gracioso.
        h.shutdown().await;
    }
}
