//! `atom/tools-shell`: provê `ai.tools@0` com `shell_exec`.
//! Bloqueante com timeout via thread + `wait_timeout` manual (poll).
//! `shortcut: sem sandbox (caps na v0.6); shell real do usuário.`

use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn tool_defs() -> Value {
    json!([
        {"name": "shell_exec", "description": "Executa comando via sh -c.",
         "input_schema": {"type": "object", "required": ["command"],
            "properties": {"command": {"type": "string"}, "workdir": {"type": "string"},
                           "timeout_ms": {"type": "integer", "minimum": 100}}}}
    ])
}

fn execute(input: &Value) -> Value {
    let cmd = match input.get("command").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => {
            return json!({"output": [{"type": "text", "text": "command ausente"}], "is_error": true});
        }
    };
    let timeout = Duration::from_millis(
        input
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(30_000)
            .clamp(100, 300_000),
    );
    let mut c = Command::new("sh");
    c.arg("-c")
        .arg(cmd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(w) = input.get("workdir").and_then(|v| v.as_str()) {
        c.current_dir(w);
    }
    let mut child = match c.spawn() {
        Ok(c) => c,
        Err(e) => {
            return json!({"output": [{"type": "text", "text": format!("spawn: {e}")}], "is_error": true});
        }
    };
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => {
                return json!({"output": [{"type": "text", "text": format!("wait: {e}")}], "is_error": true});
            }
        }
    };
    let mut out = child
        .wait_with_output()
        .map(|o| {
            let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
            let err = String::from_utf8_lossy(&o.stderr);
            if !err.is_empty() {
                s.push_str("\n[stderr]\n");
                s.push_str(&err);
            }
            s
        })
        .unwrap_or_default();
    let (code, timed_out) = match status {
        Some(s) => (s.code().unwrap_or(-1), false),
        None => {
            out.push_str("\n[timeout]");
            (-1, true)
        }
    };
    let is_error = timed_out || code != 0;
    json!({"output": [{"type": "text", "text": out}], "is_error": is_error, "exit_code": code})
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
                        let input = ip.get("input").cloned().unwrap_or(Value::Null);
                        Some(execute(&input))
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
