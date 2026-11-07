//! `test/flood`: emite N chunks num stream e termina (soak de backpressure).
//! `service.invoke test.flood@0/run {"chunks": N, "stream": "st_..."}`.

use serde_json::{Value, json};
use std::io::{BufRead, Write};

fn emit(stdout: &mut std::io::Stdout, line: &Value) {
    writeln!(stdout, "{}", serde_json::to_string(line).unwrap()).unwrap();
}

fn respond(stdout: &mut std::io::Stdout, id: Option<Value>, result: Value) {
    if let Some(i) = id {
        emit(
            stdout,
            &json!({"jsonrpc": "2.0", "id": i, "result": result}),
        );
    }
}

fn respond_err(stdout: &mut std::io::Stdout, id: Option<Value>, code: i32, msg: &str) {
    if let Some(i) = id {
        emit(
            stdout,
            &json!({"jsonrpc": "2.0", "id": i,
                "error": {"code": code, "message": msg}}),
        );
    }
}

fn main() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
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
            Err(e) => {
                eprintln!("linha inválida: {e}");
                continue;
            }
        };
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let id = req.get("id").cloned();
        if method == "atom.exit" {
            break;
        }
        match method {
            "atom.initialize" => respond(
                &mut stdout,
                id,
                json!({"protocol": "atom/1",
                    "provides": [{"contract": "test.flood@0", "version": "0.1.0"}]}),
            ),
            "atom.shutdown" => respond(&mut stdout, id, json!({})),
            "service.invoke" => {
                let p = req.get("params").cloned().unwrap_or(Value::Null);
                let ip = p.get("params").cloned().unwrap_or(Value::Null);
                let n = ip
                    .get("chunks")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0)
                    .min(2_000_000);
                let stream = p
                    .get("stream")
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string());
                if let Some(s) = stream {
                    for i in 0..n {
                        emit(
                            &mut stdout,
                            &json!({"jsonrpc": "2.0",
                                "method": "atom.stream.chunk",
                                "params": {"stream": s, "seq": i, "data": {"i": i}}}),
                        );
                    }
                    emit(
                        &mut stdout,
                        &json!({"jsonrpc": "2.0",
                            "method": "atom.stream.end",
                            "params": {"stream": s, "result": {"chunks": n}}}),
                    );
                }
                respond(&mut stdout, id, json!({"chunks": n}));
            }
            _ => respond_err(&mut stdout, id, -32601, "método desconhecido"),
        }
        stdout.flush().unwrap();
    }
}
