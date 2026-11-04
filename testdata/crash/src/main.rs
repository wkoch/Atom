//! `test/crash`: responde `initialize` e morre 100ms depois.
//! Exercita supervisão com backoff (spec protocol §4.1, v0.6).

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
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let id = msg.get("id").cloned();
        if method == "atom.exit" {
            break;
        }
        if method == "atom.initialized" {
            // Morre logo após o handshake: o supervisor deve reiniciar.
            let handle = std::thread::spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(100));
                std::process::exit(1);
            });
            let _ = handle.join();
            break;
        }
        if let Some(id) = id {
            let resp = match method {
                "atom.initialize" => Some(json!({"protocol": "atom/1",
                    "provides": [{"contract": "test.crash@0", "version": "0.1.0"}]})),
                "atom.shutdown" => Some(json!({})),
                "service.invoke" => Some(json!({"boom": false})),
                _ => None,
            };
            let resp = match resp {
                Some(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
                None => json!({"jsonrpc": "2.0", "id": id,
                    "error": {"code": -32601, "message": "desconhecido"}}),
            };
            writeln!(out, "{}", serde_json::to_string(&resp).unwrap()).unwrap();
            out.flush().unwrap();
        }
    }
}
