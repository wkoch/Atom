//! `test/fake-llm`: `ai.llm@0` determinístico para testes da v0.2.
//! - última mensagem `WRITE <path> :: <conteúdo>` → tool_call `fs_write`.
//! - conversa já com tool_result → texto `done`.
//! - senão → `echo: <último texto>`.

use serde_json::{Value, json};
use std::io::{BufRead, Write};

fn last_user_text(messages: &[Value]) -> String {
    messages
        .iter()
        .rev()
        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
        .map(|parts| {
            parts
                .iter()
                .filter_map(|p| {
                    if p.get("type").and_then(|t| t.as_str()) == Some("text") {
                        p.get("text").and_then(|t| t.as_str())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

fn has_tool_result(messages: &[Value]) -> bool {
    messages.iter().any(|m| {
        m.get("content")
            .and_then(|c| c.as_array())
            .map(|parts| {
                parts
                    .iter()
                    .any(|p| p.get("type").and_then(|t| t.as_str()) == Some("tool_result"))
            })
            .unwrap_or(false)
    })
}

fn decide(messages: &[Value]) -> Value {
    // tool_result na conversa tem prioridade: tarefa cumprida.
    if has_tool_result(messages) {
        return json!({
            "message": {"role": "assistant", "content": [{"type": "text", "text": "done"}]},
            "stop_reason": "stop",
        });
    }
    let mut last = last_user_text(messages);
    // `SLOW <ms> :: resto` (testes de cancelamento): dorme antes de responder.
    if let Some(rest) = last.strip_prefix("SLOW ")
        && let Some((ms, rest)) = rest.split_once(" :: ")
        && let Ok(ms) = ms.trim().parse::<u64>()
    {
        std::thread::sleep(std::time::Duration::from_millis(ms.min(30_000)));
        last = rest.to_string();
    }
    if let Some(rest) = last.strip_prefix("WRITE ")
        && let Some((path, content)) = rest.split_once(" :: ")
    {
        return json!({
            "message": {"role": "assistant", "content": [
                {"type": "tool_call", "tool_call": {
                    "id": "tc-1", "name": "fs_write",
                    "input": {"path": path.trim(), "content": content}}}
            ]},
            "stop_reason": "tool_call",
        });
    }
    json!({
        "message": {"role": "assistant", "content": [{"type": "text", "text": format!("echo: {last}")}]},
        "stop_reason": "stop",
    })
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
        let respond = |stdout: &mut std::io::Stdout, id: Option<Value>, result: Value| {
            if let Some(i) = id {
                writeln!(
                    stdout,
                    "{}",
                    serde_json::to_string(&json!({"jsonrpc": "2.0", "id": i, "result": result}))
                        .unwrap()
                )
                .unwrap();
                stdout.flush().unwrap();
            }
        };
        let respond_err =
            |stdout: &mut std::io::Stdout, id: Option<Value>, code: i32, msg: String| {
                if let Some(i) = id {
                    writeln!(
                    stdout,
                    "{}",
                    serde_json::to_string(
                        &json!({"jsonrpc": "2.0", "id": i, "error": {"code": code, "message": msg}})
                    )
                    .unwrap()
                )
                    .unwrap();
                    stdout.flush().unwrap();
                }
            };
        match method {
            "atom.initialize" => respond(
                &mut stdout,
                id.clone(),
                json!({"protocol": "atom/1",
                "provides": [{"contract": "ai.llm@0", "version": "0.1.0"}]}),
            ),
            "atom.shutdown" => respond(&mut stdout, id.clone(), json!({})),
            "service.invoke" => {
                let p = req.get("params").cloned().unwrap_or(Value::Null);
                let svc = p.get("service").and_then(|s| s.as_str()).unwrap_or("");
                let m = p.get("method").and_then(|x| x.as_str()).unwrap_or("");
                let ip = p.get("params").cloned().unwrap_or(Value::Null);
                let stream = p
                    .get("stream")
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string());
                match (svc, m) {
                    ("ai.llm@0", "models") => {
                        respond(&mut stdout, id.clone(), json!([{"id": "fake-llm"}]))
                    }
                    ("ai.llm@0", "complete") => {
                        let messages: Vec<Value> = ip
                            .get("messages")
                            .and_then(|x| x.as_array())
                            .cloned()
                            .unwrap_or_default();
                        let result = decide(&messages);
                        if let Some(s) = stream {
                            let text = result["message"]["content"]
                                .as_array()
                                .and_then(|a| a.iter().find(|x| x["type"] == "text"))
                                .and_then(|x| x.get("text"))
                                .cloned()
                                .unwrap_or(Value::Null);
                            let chunk = json!({"jsonrpc": "2.0", "method": "atom.stream.chunk",
                                "params": {"stream": s, "seq": 1, "data": {"delta_text": text.as_str().unwrap_or("")}}});
                            writeln!(stdout, "{}", serde_json::to_string(&chunk).unwrap()).unwrap();
                            let end = json!({"jsonrpc": "2.0", "method": "atom.stream.end",
                                "params": {"stream": s, "result": result}});
                            writeln!(stdout, "{}", serde_json::to_string(&end).unwrap()).unwrap();
                            stdout.flush().unwrap();
                        }
                        respond(&mut stdout, id.clone(), result);
                    }
                    _ => respond_err(
                        &mut stdout,
                        id.clone(),
                        -32601,
                        format!("método {svc}/{m} desconhecido"),
                    ),
                }
            }
            _ => {
                if id.is_some() {
                    respond_err(
                        &mut stdout,
                        id,
                        -32601,
                        format!("método {method} desconhecido"),
                    );
                }
            }
        }
    }
}
