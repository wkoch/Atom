//! `test/caller`: provider `test.caller@0` que repassa chamadas ao bus.
//! `call {service, method, params}` → `atom.call` → devolve resultado/erro.
//! `ask {capabilities}` → `atom.grants.request` → `{status}`.
//! Usado p/ testar enforcement (`-32003`) com e sem grant (v0.6).
//! Loop único com pendência (sem stdin aninhado, que trava).

use serde_json::{Value, json};
use std::io::{BufRead, Write};

fn send(out: &mut std::io::Stdout, v: &Value) {
    writeln!(out, "{}", serde_json::to_string(v).unwrap()).unwrap();
    out.flush().unwrap();
}

fn ok(out: &mut std::io::Stdout, id: &Value, r: Value) {
    send(out, &json!({"jsonrpc": "2.0", "id": id, "result": r}));
}

fn err(out: &mut std::io::Stdout, id: &Value, code: i32, m: &str) {
    send(
        out,
        &json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": m}}),
    );
}

fn main() {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    let mut next: u64 = 0;
    // Resposta aguardada do bus: (rid, invoke_id a responder).
    let mut pending: Option<(String, Value)> = None;
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
        // Resposta do bus a um `atom.call`/`atom.grants.request` nosso.
        if msg.get("method").and_then(|m| m.as_str()).is_none() {
            if let Some((rid, iid)) = pending.take() {
                let got = msg.get("id").and_then(|i| i.as_str()).unwrap_or("");
                if got != rid {
                    pending = Some((rid, iid));
                    continue;
                }
                if let Some(e) = msg.get("error") {
                    let code = e.get("code").and_then(|c| c.as_i64()).unwrap_or(-32603) as i32;
                    let m = e.get("message").and_then(|x| x.as_str()).unwrap_or("erro");
                    err(&mut out, &iid, code, m);
                } else {
                    ok(
                        &mut out,
                        &iid,
                        msg.get("result").cloned().unwrap_or(Value::Null),
                    );
                }
            }
            continue;
        }
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        let has_id = msg.get("id").is_some();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        match method {
            "atom.initialize" => {
                if has_id {
                    ok(
                        &mut out,
                        &id,
                        json!({"protocol": "atom/1",
                        "provides": [{"contract": "test.caller@0", "version": "0.1.0"}]}),
                    );
                }
            }
            "atom.shutdown" => {
                if has_id {
                    ok(&mut out, &id, json!({}));
                }
            }
            "atom.exit" => break,
            "service.invoke" => {
                let m = params.get("method").and_then(|x| x.as_str()).unwrap_or("");
                let p = params.get("params").cloned().unwrap_or(Value::Null);
                if !has_id {
                    continue;
                }
                match m {
                    "call" => {
                        let svc = p.get("service").and_then(|s| s.as_str()).unwrap_or("");
                        let meth = p.get("method").and_then(|x| x.as_str()).unwrap_or("");
                        next += 1;
                        let rid = format!("caller-{next}");
                        send(
                            &mut out,
                            &json!({"jsonrpc": "2.0", "id": rid, "method": "atom.call",
                                "params": {"service": svc, "method": meth,
                                    "params": p.get("params").cloned().unwrap_or(Value::Null)}}),
                        );
                        pending = Some((rid, id));
                    }
                    "ask" => {
                        let caps = p.get("capabilities").cloned().unwrap_or(json!([]));
                        next += 1;
                        let rid = format!("caller-{next}");
                        send(
                            &mut out,
                            &json!({"jsonrpc": "2.0", "id": rid, "method": "atom.grants.request",
                                "params": {"capabilities": caps}}),
                        );
                        pending = Some((rid, id));
                    }
                    _ => err(&mut out, &id, -32601, &format!("método {m} desconhecido")),
                }
            }
            _ => {
                if has_id {
                    err(
                        &mut out,
                        &id,
                        -32601,
                        &format!("método {method} desconhecido"),
                    );
                }
            }
        }
    }
}
