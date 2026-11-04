//! `test/intercept-fake`: responde `interceptor.invoke` conforme `./action`
//! (lido por mensagem — testes mudam entre fases do mesmo processo):
//! `continue` (padrão), `modify`, `respond`, `reject`, `bad`, `sleep`,
//! `via-proceed` (chama `atom.proceed` com o `$proceed` recebido).

use serde_json::{Value, json};
use std::io::{BufRead, Write};

fn send(out: &mut std::io::Stdout, v: &Value) {
    writeln!(out, "{}", serde_json::to_string(v).unwrap()).unwrap();
    out.flush().unwrap();
}

fn ok(out: &mut std::io::Stdout, id: &Value, r: Value) {
    send(out, &json!({"jsonrpc": "2.0", "id": id, "result": r}));
}

fn main() {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    let mut next: u64 = 0;
    // `atom.proceed` pendente: (rid nosso, id do invoke a responder).
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
        if msg.get("method").and_then(|m| m.as_str()).is_none() {
            // Resposta ao nosso `atom.proceed`: encerra com o resultado do inner.
            if let Some((rid, iid)) = pending.take() {
                let got = msg.get("id").and_then(|i| i.as_str()).unwrap_or("");
                if got != rid {
                    pending = Some((rid, iid));
                    continue;
                }
                let res = msg.get("result").cloned().unwrap_or(Value::Null);
                ok(&mut out, &iid, json!({"action": "respond", "result": res}));
            }
            continue;
        }
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        let has_id = msg.get("id").is_some();
        match method {
            "atom.initialize" => {
                if has_id {
                    ok(&mut out, &id, json!({"protocol": "atom/1"}));
                }
            }
            "atom.shutdown" => {
                if has_id {
                    ok(&mut out, &id, json!({}));
                }
            }
            "atom.exit" => break,
            "interceptor.invoke" => {
                if !has_id {
                    continue;
                }
                // Ação lida de `./action` (cwd = raiz do pacote) a cada
                // mensagem — o teste reescreve entre fases.
                let action =
                    std::fs::read_to_string("./action").unwrap_or_else(|_| "continue".to_string());
                match action.trim() {
                    "via-proceed" => {
                        let params = msg.get("params").cloned().unwrap_or(Value::Null);
                        if let Some(tok) = params.get("$proceed").and_then(|t| t.as_str()) {
                            next += 1;
                            let rid = format!("proceed-{next}");
                            send(
                                &mut out,
                                &json!({"jsonrpc": "2.0", "id": rid, "method": "atom.proceed",
                                    "params": {"token": tok}}),
                            );
                            pending = Some((rid, id));
                            continue;
                        }
                        ok(&mut out, &id, json!({"action": "continue"}));
                    }
                    "modify" => ok(
                        &mut out,
                        &id,
                        json!({"action": "modify", "params": {"x": 99}}),
                    ),
                    "respond" => ok(
                        &mut out,
                        &id,
                        json!({"action": "respond", "result": {"fake": true}}),
                    ),
                    "reject" => ok(
                        &mut out,
                        &id,
                        json!({"action": "reject", "error": {"code": -32020, "message": "bloqueado"}}),
                    ),
                    "bad" => ok(&mut out, &id, json!({"action": "wat"})),
                    "sleep" => {
                        std::thread::sleep(std::time::Duration::from_secs(60));
                        ok(&mut out, &id, json!({"action": "continue"}));
                    }
                    _ => ok(&mut out, &id, json!({"action": "continue"})),
                }
            }
            _ => {}
        }
    }
}
