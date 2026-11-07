//! `test/evil`: extensão adversária (conformance.md §6).
//!
//! Cada modo tenta um ataque e o teste de integração correspondente exige a
//! falha especificada. O modo é escolhido por `argv[1]`:
//! - `late-reply <ms>`: dorme antes de responder `service.invoke` (para
//!   testar descarte de resposta tardia pós-cancelamento, `[P-CANCEL-3]`).
//! - `flood`: cospe chunks sem crédito (para `[P-LIM-2]` / `-32050`).
//! - `plain`: eco honesto (controle: responde e encerra normal).

use serde_json::{Value, json};
use std::io::{BufRead, Write};

fn send(out: &mut std::io::Stdout, v: &Value) {
    writeln!(out, "{}", serde_json::to_string(v).unwrap()).unwrap();
    out.flush().unwrap();
}

fn main() {
    let mode = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "plain".to_string());
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
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        let has_id = msg.get("id").is_some();
        match method {
            "atom.initialize" => send(
                &mut out,
                &json!({"jsonrpc": "2.0", "id": id, "result": {
                    "protocol": "atom/1",
                    "provides": [{"contract": "test.evil@0", "version": "0.1.0"}],
                    "handles": [], "intercepts": [], "subscribes": [],
                }}),
            ),
            "service.invoke" => {
                if mode.starts_with("late-reply") {
                    let ms: u64 = mode
                        .split('=')
                        .nth(1)
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(4000);
                    std::thread::sleep(std::time::Duration::from_millis(ms));
                }
                if has_id {
                    send(
                        &mut out,
                        &json!({"jsonrpc": "2.0", "id": id, "result": {"evil": true}}),
                    );
                }
            }
            "atom.stream.chunk" if mode == "flood" => {
                // Flood: ignora crédito e cospe 10k chunks de uma vez.
                if let Some(stream) = msg
                    .get("params")
                    .and_then(|p| p.get("stream"))
                    .and_then(|s| s.as_str())
                {
                    for seq in 0..10_000u64 {
                        send(
                            &mut out,
                            &json!({"jsonrpc": "2.0", "method": "atom.stream.chunk",
                                "params": {"stream": stream, "seq": seq, "data": {"n": seq}}}),
                        );
                    }
                }
            }
            "atom.shutdown" if has_id => {
                send(&mut out, &json!({"jsonrpc": "2.0", "id": id, "result": {}}));
            }
            "atom.exit" if !has_id => break,
            _ => {
                if has_id {
                    send(
                        &mut out,
                        &json!({"jsonrpc": "2.0", "id": id,
                            "error": {"code": -32601, "message": "desconhecido"}}),
                    );
                }
            }
        }
    }
}
