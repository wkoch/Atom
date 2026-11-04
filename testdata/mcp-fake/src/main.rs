//! MCP falso: `tools/list` → `[{hello}]`; `tools/call hello {name}` → saudação.
//! stdio NDJSON por padrão; `--http PORT` serve JSON-RPC via HTTP (std, sem deps).

use serde_json::{Value, json};
use std::io::{BufRead, Write};

fn handle(method: &str, params: &Value) -> Option<Value> {
    match method {
        "initialize" => Some(
            json!({"protocolVersion": "2024-11-05", "capabilities": {}, "serverInfo": {"name": "mcp-fake", "version": "0.1.0"}}),
        ),
        "tools/list" => Some(
            json!({"tools": [{"name": "hello", "description": "Saúda", "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}}}}]}),
        ),
        "tools/call" => {
            let tool = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
            if tool != "hello" {
                return None;
            }
            let who = params
                .get("arguments")
                .and_then(|a| a.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("mundo");
            Some(json!({"content": [{"type": "text", "text": format!("olá, {who}!")}]}))
        }
        _ => None,
    }
}

fn serve_stdio() {
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
        if msg.get("method").and_then(|m| m.as_str()) == Some("notifications/initialized") {
            continue;
        }
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        let resp = match handle(method, &params) {
            Some(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
            None => {
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "método desconhecido"}})
            }
        };
        writeln!(out, "{}", serde_json::to_string(&resp).unwrap()).unwrap();
        out.flush().unwrap();
    }
}

fn serve_http(port: u16) {
    let listener = std::net::TcpListener::bind(("127.0.0.1", port)).expect("bind http fake");
    for stream in listener.incoming().flatten() {
        let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
        let mut content_len = 0usize;
        let mut line = String::new();
        // Cabeçalhos.
        loop {
            line.clear();
            if reader.read_line(&mut line).is_err() {
                break;
            }
            let t = line.trim();
            if t.is_empty() {
                break;
            }
            if let Some(n) = t
                .strip_prefix("Content-Length:")
                .or_else(|| t.strip_prefix("content-length:"))
            {
                content_len = n.trim().parse().unwrap_or(0);
            }
        }
        let mut body = vec![0u8; content_len];
        use std::io::Read;
        if content_len > 0 && reader.read_exact(&mut body).is_err() {
            continue;
        }
        let msg: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        let method = msg
            .get("method")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        let resp = match handle(&method, &params) {
            Some(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
            None => {
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "método desconhecido"}})
            }
        };
        let bytes = serde_json::to_vec(&resp).unwrap();
        let mut stream = reader.into_inner();
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            bytes.len()
        );
        let _ = stream.write_all(&bytes);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.windows(2).any(|w| w[0] == "--http") {
        let port: u16 = args
            .windows(2)
            .find(|w| w[0] == "--http")
            .and_then(|w| w[1].parse().ok())
            .unwrap_or(8377);
        serve_http(port);
    } else {
        serve_stdio();
    }
}
