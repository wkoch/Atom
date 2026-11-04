//! `atom/skill-loader`: handler `kind=skill`, provider `ai.context@0`.
//! `kind.load {package, dir, config}` lê `[contributes.skill] file`.
//! `collect` agrega todas as skills como partes `system`.
//! `shortcut: scripts/ com runtime ignorados na v0.4 (só descrição).`

use serde_json::{Value, json};
use std::collections::HashMap;
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
    // pacote -> conteúdo da skill.
    let mut skills: HashMap<String, String> = HashMap::new();
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
        if msg.get("method").and_then(|m| m.as_str()).is_none() {
            continue;
        }
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let id = msg.get("id").cloned();
        let has_id = msg.get("id").is_some();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        match method {
            "atom.initialize" => {
                if let (Some(i), true) = (id, has_id) {
                    ok(
                        &mut out,
                        &i,
                        json!({"protocol": "atom/1",
                        "provides": [{"contract": "ai.context@0", "version": "0.1.0"}],
                        "handles": ["skill"]}),
                    );
                }
            }
            "atom.shutdown" => {
                if let (Some(i), true) = (id, has_id) {
                    ok(&mut out, &i, json!({}));
                }
            }
            "atom.exit" => break,
            "kind.load" => {
                let pkg = params.get("package").cloned().unwrap_or(Value::Null);
                let dir = params.get("dir").and_then(|d| d.as_str()).unwrap_or("");
                let name = pkg
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("?")
                    .to_string();
                let file = pkg
                    .get("contributes")
                    .and_then(|c| c.get("skill"))
                    .and_then(|s| s.get("file"))
                    .and_then(|f| f.as_str())
                    .unwrap_or("SKILL.md");
                if pkg
                    .get("contributes")
                    .and_then(|c| c.get("skill"))
                    .and_then(|s| s.get("scripts"))
                    .is_some()
                {
                    eprintln!("skill {name}: scripts ignorados na v0.4 (só descrição)");
                }
                match std::fs::read_to_string(format!("{dir}/{file}")) {
                    Ok(text) => {
                        skills.insert(name, text);
                        if let (Some(i), true) = (id, has_id) {
                            ok(&mut out, &i, json!({}));
                        }
                    }
                    Err(e) => {
                        if let (Some(i), true) = (id, has_id) {
                            err(&mut out, &i, -32030, &format!("skill: {e}"));
                        }
                    }
                }
            }
            "kind.unload" => {
                let pkg = params.get("package").and_then(|p| p.as_str()).unwrap_or("");
                let name = pkg.split('@').next().unwrap_or(pkg);
                skills.remove(name);
                if let (Some(i), true) = (id, has_id) {
                    ok(&mut out, &i, json!({}));
                }
            }
            "service.invoke" => {
                let m = params.get("method").and_then(|x| x.as_str()).unwrap_or("");
                if m != "collect" {
                    if let (Some(i), true) = (id, has_id) {
                        err(&mut out, &i, -32601, &format!("método {m} desconhecido"));
                    }
                    continue;
                }
                let mut names: Vec<&String> = skills.keys().collect();
                names.sort();
                let system: Vec<Value> = names
                    .iter()
                    .map(|n| {
                        let body = skills.get(*n).cloned().unwrap_or_default();
                        json!({"type": "text", "text": format!("# skill {n}\n{body}")})
                    })
                    .collect();
                if let (Some(i), true) = (id, has_id) {
                    ok(&mut out, &i, json!({"system": system, "messages": []}));
                }
            }
            _ => {
                if let (Some(i), true) = (id, has_id) {
                    err(
                        &mut out,
                        &i,
                        -32601,
                        &format!("método {method} desconhecido"),
                    );
                }
            }
        }
    }
}
