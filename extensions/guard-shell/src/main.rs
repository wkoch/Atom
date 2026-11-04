//! `atom/guard-shell`: interceptor `before ai.tools@0/execute`.
//! Pede confirmação via `ui.prompt@0/confirm` antes de rodar shell.
//! Sem prompt disponível = rejeita (fail-closed).

use serde_json::{Value, json};
use std::io::{BufRead, Write};

fn send(out: &mut std::io::Stdout, v: &Value) {
    writeln!(out, "{}", serde_json::to_string(v).unwrap()).unwrap();
    out.flush().unwrap();
}

fn req(id: &str, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

fn resp_ok(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn resp_err(id: &Value, code: i32, msg: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": msg}})
}

/// Extrai comando shell de `params` de `ai.tools@0/execute`.
fn shell_command(p: &Value) -> Option<String> {
    let name = p.get("name").and_then(|n| n.as_str()).unwrap_or("");
    if !name.contains("shell") && !name.contains("exec") {
        return None;
    }
    let input = p.get("input").unwrap_or(&Value::Null);
    input
        .get("command")
        .and_then(|c| c.as_str())
        .or_else(|| {
            input
                .get("input")
                .and_then(|i| i.get("command"))
                .and_then(|c| c.as_str())
        })
        .map(|s| s.to_string())
}

fn main() {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    let mut next: u64 = 0;
    // Intercept pendente aguardando resposta do prompt: (intercept_id, call_id).
    let mut pending: Option<(Value, String)> = None;
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
            Err(e) => {
                eprintln!("linha inválida: {e}");
                continue;
            }
        };
        // Resposta (sem method): pode ser do `atom.call` ao prompt.
        if msg.get("method").and_then(|m| m.as_str()).is_none() {
            if let Some((iid, cid)) = pending.take() {
                let rid = msg.get("id").and_then(|i| i.as_str()).unwrap_or("");
                if rid != cid {
                    pending = Some((iid, cid));
                    continue;
                }
                let ok = msg
                    .get("result")
                    .and_then(|x| x.get("ok"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let err = msg.get("error").is_some();
                if !err && ok {
                    send(&mut out, &resp_ok(&iid, json!({"action": "continue"})));
                } else {
                    send(
                        &mut out,
                        &resp_ok(
                            &iid,
                            json!({"action": "reject",
                                "error": {"code": -32020, "message": "shell bloqueado pelo guard-shell"}}),
                        ),
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
                    send(
                        &mut out,
                        &resp_ok(
                            &id,
                            json!({"protocol": "atom/1",
                                "intercepts": [{"match": "ai.tools@0/execute", "phase": "before", "priority": 50}]}),
                        ),
                    );
                }
            }
            "atom.shutdown" => {
                if has_id {
                    send(&mut out, &resp_ok(&id, json!({})));
                }
            }
            "atom.exit" => break,
            "interceptor.invoke" => {
                let phase = params.get("phase").and_then(|p| p.as_str()).unwrap_or("");
                let svc = params.get("service").and_then(|s| s.as_str()).unwrap_or("");
                let m = params.get("method").and_then(|x| x.as_str()).unwrap_or("");
                let p = params.get("params").cloned().unwrap_or(Value::Null);
                if phase != "before" || svc != "ai.tools@0" || m != "execute" {
                    if has_id {
                        send(&mut out, &resp_ok(&id, json!({"action": "continue"})));
                    }
                    continue;
                }
                let cmd = match shell_command(&p) {
                    Some(c) => c,
                    None => {
                        if has_id {
                            send(&mut out, &resp_ok(&id, json!({"action": "continue"})));
                        }
                        continue;
                    }
                };
                next += 1;
                let cid = format!("guard-{next}");
                send(
                    &mut out,
                    &req(
                        &cid,
                        "atom.call",
                        json!({"service": "ui.prompt@0", "method": "confirm",
                            "params": {"title": "Executar shell?", "detail": cmd}}),
                    ),
                );
                pending = Some((id, cid));
            }
            _ => {
                if has_id {
                    send(
                        &mut out,
                        &resp_err(&id, -32601, &format!("método {method} desconhecido")),
                    );
                }
            }
        }
    }
}
