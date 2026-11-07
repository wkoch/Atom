//! Suite de conformidade executável do protocolo `atom/1` ([CF-1]).
//!
//! Roda os mesmos invariantes universais contra qualquer extensão
//! (modo `ext -- <comando>`, o harness faz papel de core) ou qualquer core
//! (modo `core --socket <caminho>`, o harness faz papel de extensão):
//! handshake, envelope de erro, linhas inválidas ignoradas, notificações sem
//! resposta, `service.invoke` e encerramento. Saída: JSON `{id, pass}`
//! por cláusula `P-*` (conformance.md §3).
//!
//! Como teste: `cargo test -p atom-core --test conformance` roda os
//! `subjects()` embutidos. Como binário dev-only (example):
//! `cargo run -p atom-core --example atom-conformance -- ext -- ./minha-ext`
//! ou `... core --socket /tmp/atom.sock`.
//! Para usar com outra extensão, adicione um `Subject` em `subjects()`
//! com seus casos de invoke.

#[path = "conformance_common.rs"]
mod common;
use common::{Harness, InvokeCase, ensure_bin, run_ext_suite, run_invoke_cases};
use serde_json::{Value, json};

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

// spec: [CF-1]
#[tokio::test]
async fn conformance() {
    for s in subjects() {
        let bin = ensure_bin(s.pkg, s.bin);
        let mut h = Harness::spawn(&bin).await;
        let tag = s.name;
        let mut checks = run_ext_suite(&mut h, tag, Some(s.manifest_dir)).await;
        checks.extend(run_invoke_cases(&mut h, tag, &s.cases).await);
        // 5. Encerramento gracioso.
        h.shutdown().await;
        for c in &checks {
            assert!(c.pass, "{tag}: cláusula {} falhou: {}", c.id, c.detail);
        }
    }
}
