use std::io::{BufRead, Write};

// Extensão mínima de teste: NDJSON em stdin/stdout, logs em stderr.
// Responde `atom.initialize` e devolve `params.params` em `service.invoke`.
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
        let req: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("linha inválida: {e}");
                continue;
            }
        };
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let id = req.get("id").cloned().unwrap_or(serde_json::Value::Null);
        let has_id = req.get("id").is_some();
        let result = match method {
            "atom.initialize" => Some(serde_json::json!({
                "protocol": "atom/1",
                "provides": [{"contract": "test.echo@0", "version": "0.1.0"}],
            })),
            "service.invoke" => {
                let params = req
                    .get("params")
                    .and_then(|p| p.get("params"))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                Some(params)
            }
            "atom.shutdown" => Some(serde_json::json!({})),
            _ => None,
        };
        if method == "atom.exit" {
            break;
        }
        if has_id {
            let resp = match result {
                Some(r) => serde_json::json!({"jsonrpc": "2.0", "id": id, "result": r}),
                None => serde_json::json!({"jsonrpc": "2.0", "id": id,
                    "error": {"code": -32601, "message": format!("método {method} desconhecido")}}),
            };
            writeln!(out, "{}", serde_json::to_string(&resp).unwrap()).unwrap();
            out.flush().unwrap();
        }
    }
}
