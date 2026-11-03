//! `atom/tools-fs`: provê `ai.tools@0` com `fs_read`, `fs_write`, `fs_list`.
//! Sem estado; operações síncronas no loop principal (rápidas).
//! `shortcut: sem sandbox de paths (caps entram na v0.6); confie no chamador local.`

use serde_json::{Value, json};
use std::io::{BufRead, Write};

fn tool_defs() -> Value {
    json!([
        {"name": "fs_read", "description": "Lê arquivo (texto).",
         "input_schema": {"type": "object", "required": ["path"],
            "properties": {"path": {"type": "string"}, "offset": {"type": "integer"}, "limit": {"type": "integer"}}}},
        {"name": "fs_write", "description": "Escreve arquivo (cria diretórios).",
         "input_schema": {"type": "object", "required": ["path", "content"],
            "properties": {"path": {"type": "string"}, "content": {"type": "string"}}}},
        {"name": "fs_list", "description": "Lista diretório.",
         "input_schema": {"type": "object", "required": ["path"],
            "properties": {"path": {"type": "string"}}}}
    ])
}

fn text_part(s: String) -> Value {
    json!({"type": "text", "text": s})
}

fn execute(name: &str, input: &Value) -> Value {
    let out = match name {
        "fs_read" => (|| {
            let p = input.get("path").and_then(|v| v.as_str()).ok_or("path")?;
            let text = std::fs::read_to_string(p).map_err(|e| e.to_string())?;
            let lines: Vec<&str> = text.lines().collect();
            let off = input.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let lim = input.get("limit").and_then(|v| v.as_u64()).unwrap_or(2000) as usize;
            let end = (off + lim).min(lines.len());
            if off > lines.len() {
                return Err(format!("offset {off} além de {} linhas", lines.len()));
            }
            Ok(lines[off..end].join("\n"))
        })(),
        "fs_write" => (|| {
            let p = input.get("path").and_then(|v| v.as_str()).ok_or("path")?;
            let c = input
                .get("content")
                .and_then(|v| v.as_str())
                .ok_or("content")?;
            let path = std::path::Path::new(p);
            if let Some(parent) = path.parent()
                && !parent.as_os_str().is_empty()
            {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            std::fs::write(path, c).map_err(|e| e.to_string())?;
            Ok(format!("escrito {} ({} bytes)", p, c.len()))
        })(),
        "fs_list" => (|| {
            let p = input.get("path").and_then(|v| v.as_str()).ok_or("path")?;
            let mut names: Vec<String> = vec![];
            for e in std::fs::read_dir(p).map_err(|e| e.to_string())? {
                let e = e.map_err(|e| e.to_string())?;
                let ft = e.file_type().map_err(|e| e.to_string())?;
                names.push(format!(
                    "{}{}",
                    e.file_name().to_string_lossy(),
                    if ft.is_dir() { "/" } else { "" }
                ));
            }
            names.sort();
            Ok(names.join("\n"))
        })(),
        _ => Err(format!("tool desconhecida: {name}")),
    };
    match out {
        Ok(t) => json!({"output": [text_part(t)], "is_error": false}),
        Err(e) => json!({"output": [text_part(e)], "is_error": true}),
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
        let result = match method {
            "atom.initialize" => Some(json!({
                "protocol": "atom/1",
                "provides": [{"contract": "ai.tools@0", "version": "0.1.0"}],
            })),
            "atom.shutdown" => Some(json!({})),
            "service.invoke" => {
                let p = req.get("params").cloned().unwrap_or(Value::Null);
                let svc = p.get("service").and_then(|s| s.as_str()).unwrap_or("");
                let m = p.get("method").and_then(|x| x.as_str()).unwrap_or("");
                let ip = p.get("params").cloned().unwrap_or(Value::Null);
                match (svc, m) {
                    ("ai.tools@0", "list") => Some(tool_defs()),
                    ("ai.tools@0", "execute") => {
                        let n = ip.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let input = ip.get("input").cloned().unwrap_or(Value::Null);
                        Some(execute(n, &input))
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        if id.is_some() {
            let resp = match result {
                Some(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
                None => json!({"jsonrpc": "2.0", "id": id,
                    "error": {"code": -32601, "message": format!("método {method} desconhecido")}}),
            };
            writeln!(stdout, "{}", serde_json::to_string(&resp).unwrap()).unwrap();
            stdout.flush().unwrap();
        }
    }
}
