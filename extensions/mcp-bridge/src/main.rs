//! `atom/mcp-bridge`: handler `kind=mcp`, provider `ai.tools@0`.
//! Cada pacote `kind=mcp` declara `[contributes.mcp]` com
//! `transport = "stdio"|"http"`, `command`/`args` ou `url`.
//! `list` agrega tools com prefixo `{server}__`; `execute` roteia.
//! Conexões são por chamada (spawn/POST + initialize), sem estado.
//! `shortcut: stdio NDJSON e HTTP JSON-RPC simples; sem SSE/sessão/resume.`

use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

#[derive(Debug, Clone)]
struct ServerCfg {
    transport: String,
    command: Option<String>,
    args: Vec<String>,
    url: Option<String>,
}

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

fn short_name(pkg: &str) -> String {
    pkg.split('/').next_back().unwrap_or(pkg).replace('-', "_")
}

fn tool_full(pkg: &str, tool: &str) -> String {
    format!("{}__{tool}", short_name(pkg))
}

fn mcp_stdio(cfg: &ServerCfg, dir: &str, method: &str, params: Value) -> Result<Value, String> {
    let cmd = cfg.command.clone().ok_or("mcp stdio sem command")?;
    let prog = if std::path::Path::new(&cmd).is_absolute() {
        cmd
    } else {
        format!("{dir}/{cmd}")
    };
    let mut child = Command::new(&prog)
        .args(&cfg.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn {prog}: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("sem stdin")?;
    let stdout = child.stdout.take().ok_or("sem stdout")?;
    let mut reader = BufReader::new(stdout);
    let mut write_line = |v: &Value| -> Result<(), String> {
        writeln!(stdin, "{}", serde_json::to_string(v).unwrap()).map_err(|e| e.to_string())?;
        stdin.flush().map_err(|e| e.to_string())
    };
    write_line(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2024-11-05", "capabilities": {},
            "clientInfo": {"name": "atom-mcp-bridge", "version": "0.4.0"}}}))?;
    write_line(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))?;
    write_line(&json!({"jsonrpc": "2.0", "id": 2, "method": method, "params": params}))?;
    // Respostas: id 1 (initialize) e id 2 (chamada real).
    let mut result: Option<Value> = None;
    let mut line = String::new();
    for _ in 0..50 {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                if line.trim().is_empty() {
                    continue;
                }
                let v: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if v.get("id") == Some(&json!(2)) {
                    if let Some(e) = v.get("error") {
                        let _ = child.kill();
                        return Err(format!("mcp erro: {e}"));
                    }
                    result = v.get("result").cloned();
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let _ = child.kill();
    result.ok_or_else(|| "mcp sem resposta".to_string())
}

fn mcp_http(url: &str, method: &str, params: Value) -> Result<Value, String> {
    let client = reqwest::blocking::Client::new();
    for (id, m, p) in [
        (
            1,
            "initialize",
            json!({"protocolVersion": "2024-11-05", "capabilities": {},
            "clientInfo": {"name": "atom-mcp-bridge", "version": "0.4.0"}}),
        ),
        (2, method, params),
    ] {
        let body = json!({"jsonrpc": "2.0", "id": id, "method": m, "params": p});
        let resp: Value = client
            .post(url)
            .json(&body)
            .send()
            .map_err(|e| format!("http {url}: {e}"))?
            .json()
            .map_err(|e| format!("http json: {e}"))?;
        if id == 2 {
            if let Some(e) = resp.get("error") {
                return Err(format!("mcp erro: {e}"));
            }
            return resp
                .get("result")
                .cloned()
                .ok_or_else(|| "mcp sem result".to_string());
        }
    }
    Err("inacessível".to_string())
}

fn server_call(cfg: &ServerCfg, dir: &str, method: &str, params: Value) -> Result<Value, String> {
    if cfg.transport == "http" {
        let url = cfg.url.clone().ok_or("mcp http sem url")?;
        mcp_http(&url, method, params)
    } else {
        mcp_stdio(cfg, dir, method, params)
    }
}

fn main() {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    // pacote -> (cfg, dir).
    let mut servers: HashMap<String, (ServerCfg, String)> = HashMap::new();
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
                        "provides": [{"contract": "ai.tools@0", "version": "0.1.0"}],
                        "handles": ["mcp"]}),
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
                let dir = params
                    .get("dir")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string();
                let name = pkg
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("?")
                    .to_string();
                let mc = pkg
                    .get("contributes")
                    .and_then(|c| c.get("mcp"))
                    .cloned()
                    .unwrap_or(Value::Null);
                let cfg = ServerCfg {
                    transport: mc
                        .get("transport")
                        .and_then(|t| t.as_str())
                        .unwrap_or("stdio")
                        .to_string(),
                    command: mc
                        .get("command")
                        .and_then(|c| c.as_str())
                        .map(|s| s.to_string()),
                    args: mc
                        .get("args")
                        .and_then(|a| a.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default(),
                    url: mc
                        .get("url")
                        .and_then(|u| u.as_str())
                        .map(|s| s.to_string()),
                };
                servers.insert(name, (cfg, dir));
                if let (Some(i), true) = (id, has_id) {
                    ok(&mut out, &i, json!({}));
                }
            }
            "kind.unload" => {
                let pkg = params.get("package").and_then(|p| p.as_str()).unwrap_or("");
                let name = pkg.split('@').next().unwrap_or(pkg);
                servers.remove(name);
                if let (Some(i), true) = (id, has_id) {
                    ok(&mut out, &i, json!({}));
                }
            }
            "service.invoke" => {
                let m = params.get("method").and_then(|x| x.as_str()).unwrap_or("");
                let p = params.get("params").cloned().unwrap_or(Value::Null);
                match m {
                    "list" => {
                        let mut tools: Vec<Value> = vec![];
                        let mut names: Vec<String> = servers.keys().cloned().collect();
                        names.sort();
                        for srv in names {
                            let (cfg, dir) = servers.get(&srv).cloned().unwrap();
                            match server_call(&cfg, &dir, "tools/list", json!({})) {
                                Ok(r) => {
                                    let arr = r
                                        .get("tools")
                                        .and_then(|t| t.as_array())
                                        .cloned()
                                        .unwrap_or_default();
                                    for t in arr {
                                        let tn =
                                            t.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                                        tools.push(json!({
                                            "name": tool_full(&srv, tn),
                                            "description": t.get("description").cloned().unwrap_or(json!("")),
                                            "input_schema": t.get("inputSchema").cloned().unwrap_or(json!({"type": "object"})),
                                        }));
                                    }
                                }
                                Err(e) => eprintln!("mcp {srv} list: {e}"),
                            }
                        }
                        if let (Some(i), true) = (id, has_id) {
                            ok(&mut out, &i, Value::Array(tools));
                        }
                    }
                    "execute" => {
                        let name = p.get("name").and_then(|n| n.as_str()).unwrap_or("");
                        let input = p.get("input").cloned().unwrap_or(Value::Null);
                        // Roteia pelo prefixo `{server}__`; sem prefixo, primeiro dono.
                        let mut target: Option<(String, String, ServerCfg, String)> = None;
                        for (srv, (cfg, dir)) in &servers {
                            let prefix = format!("{}__", short_name(srv));
                            if let Some(rest) = name.strip_prefix(&prefix) {
                                target =
                                    Some((srv.clone(), rest.to_string(), cfg.clone(), dir.clone()));
                                break;
                            }
                        }
                        if target.is_none() {
                            let mut names: Vec<String> = servers.keys().cloned().collect();
                            names.sort();
                            if let Some(srv) = names.first() {
                                let (cfg, dir) = servers.get(srv).cloned().unwrap();
                                target = Some((srv.clone(), name.to_string(), cfg, dir));
                            }
                        }
                        let (_, tool, cfg, dir) = match target {
                            Some(t) => t,
                            None => {
                                if let (Some(i), true) = (id, has_id) {
                                    err(&mut out, &i, -32011, "sem servidor mcp");
                                }
                                continue;
                            }
                        };
                        match server_call(
                            &cfg,
                            &dir,
                            "tools/call",
                            json!({"name": tool, "arguments": input}),
                        ) {
                            Ok(r) => {
                                let content = r
                                    .get("content")
                                    .and_then(|c| c.as_array())
                                    .cloned()
                                    .unwrap_or_default();
                                let is_error =
                                    r.get("isError").and_then(|v| v.as_bool()).unwrap_or(false);
                                let output: Vec<Value> = content.iter().map(|c| {
                                    if c.get("type").and_then(|t| t.as_str()) == Some("text") {
                                        json!({"type": "text", "text": c.get("text").cloned().unwrap_or(json!(""))})
                                    } else {
                                        json!({"type": "text", "text": serde_json::to_string(c).unwrap_or_default()})
                                    }
                                }).collect();
                                if let (Some(i), true) = (id, has_id) {
                                    ok(
                                        &mut out,
                                        &i,
                                        json!({"output": output, "is_error": is_error}),
                                    );
                                }
                            }
                            Err(e) => {
                                if let (Some(i), true) = (id, has_id) {
                                    ok(
                                        &mut out,
                                        &i,
                                        json!({"output": [{"type": "text", "text": format!("mcp: {e}")}], "is_error": true}),
                                    );
                                }
                            }
                        }
                    }
                    _ => {
                        if let (Some(i), true) = (id, has_id) {
                            err(&mut out, &i, -32601, &format!("método {m} desconhecido"));
                        }
                    }
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
