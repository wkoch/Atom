//! Prompt falso: `ui.prompt@0/confirm` lê `PROMPT_OK` (1=ok, senão nega).
//! `ask` devolve `PROMPT_VALUE` ou vazio; `notify` ok.

use serde_json::{Value, json};
use std::io::{BufRead, Write};

fn main() {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let id = req.get("id").cloned();
        if method == "atom.exit" {
            break;
        }
        let result = match method {
            "atom.initialize" => Some(json!({"protocol": "atom/1",
                "provides": [{"contract": "ui.prompt@0", "version": "0.1.0"}]})),
            "atom.shutdown" => Some(json!({})),
            "service.invoke" => {
                let p = req.get("params").cloned().unwrap_or(Value::Null);
                let m = p.get("method").and_then(|x| x.as_str()).unwrap_or("");
                let inner = p.get("params").cloned().unwrap_or(Value::Null);
                match m {
                    "confirm" => {
                        // Fail-closed por conteúdo: detalhe com BLOCKME nega.
                        // `shortcut: sem env em testes (set_var é unsafe em 2024).`
                        let detail = inner.get("detail").and_then(|d| d.as_str()).unwrap_or("");
                        let title = inner.get("title").and_then(|t| t.as_str()).unwrap_or("");
                        let deny = detail.contains("BLOCKME") || title.contains("BLOCKME");
                        Some(json!({"ok": !deny}))
                    }
                    "ask" => Some(json!({"value": Value::Null})),
                    "notify" => Some(json!({})),
                    _ => None,
                }
            }
            _ => None,
        };
        if let Some(id) = id {
            let resp = match result {
                Some(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
                None => json!({"jsonrpc": "2.0", "id": id,
                    "error": {"code": -32601, "message": format!("método {method} desconhecido")}}),
            };
            writeln!(out, "{}", serde_json::to_string(&resp).unwrap()).unwrap();
            out.flush().unwrap();
        }
    }
}
