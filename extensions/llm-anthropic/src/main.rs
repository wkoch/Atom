//! `atom/llm-anthropic`: `ai.llm@0` sobre a API Anthropic (SSE).
//! Mesmo esqueleto do `llm-openai-compat`: thread principal + worker por stream.

use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

struct Cfg {
    base_url: String,
    model: String,
    api_key_from: String,
    max_tokens: u64,
}

impl Cfg {
    fn from(v: &Value) -> Self {
        Self {
            base_url: v
                .get("base_url")
                .and_then(|x| x.as_str())
                .unwrap_or("https://api.anthropic.com")
                .trim_end_matches('/')
                .to_string(),
            model: v
                .get("model")
                .and_then(|x| x.as_str())
                .unwrap_or("claude-sonnet-4-5")
                .to_string(),
            api_key_from: v
                .get("api_key_from")
                .and_then(|x| x.as_str())
                .unwrap_or("anthropic_api_key")
                .to_string(),
            max_tokens: v.get("max_tokens").and_then(|x| x.as_u64()).unwrap_or(4096),
        }
    }
}

struct Bus {
    out: Arc<Mutex<std::io::Stdout>>,
    next: u64,
}

impl Bus {
    fn send(&mut self, method: &str, params: Value, id: Option<Value>) {
        let line = json!({"jsonrpc": "2.0", "method": method, "params": params, "id": id});
        let s = serde_json::to_string(&line).unwrap();
        let mut o = self.out.lock().unwrap();
        writeln!(o, "{s}").unwrap();
        o.flush().unwrap();
    }

    fn rpc(&mut self, stdin: &mut std::io::StdinLock<'_>, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = json!(format!("ext-{}", self.next));
        self.send(method, params, Some(id.clone()));
        loop {
            let mut line = String::new();
            if stdin.read_line(&mut line).unwrap_or(0) == 0 {
                return Value::Null;
            }
            if line.trim().is_empty() {
                continue;
            }
            let v: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if v.get("method").is_some() {
                continue;
            }
            if let Some(rid) = v.get("id")
                && key(rid) == key(&id)
            {
                return v.get("result").cloned().unwrap_or(Value::Null);
            }
        }
    }
}

fn key(id: &Value) -> String {
    match id {
        Value::String(s) => s.clone(),
        o => o.to_string(),
    }
}

fn emit_chunk(out: &Arc<Mutex<std::io::Stdout>>, stream: &str, seq: &mut u64, data: Value) {
    *seq += 1;
    let n = json!({"jsonrpc": "2.0", "method": "atom.stream.chunk",
        "params": {"stream": stream, "seq": seq, "data": data}});
    let mut o = out.lock().unwrap();
    writeln!(o, "{}", serde_json::to_string(&n).unwrap()).unwrap();
    o.flush().unwrap();
}

fn emit_end(out: &Arc<Mutex<std::io::Stdout>>, stream: &str, payload: Value) {
    let mut obj = serde_json::Map::new();
    obj.insert("stream".to_string(), Value::String(stream.to_string()));
    if let Some(e) = payload.get("error") {
        obj.insert("error".to_string(), e.clone());
    } else {
        obj.insert("result".to_string(), payload);
    }
    let n = json!({"jsonrpc": "2.0", "method": "atom.stream.end", "params": obj});
    let mut o = out.lock().unwrap();
    writeln!(o, "{}", serde_json::to_string(&n).unwrap()).unwrap();
    o.flush().unwrap();
}

fn respond(out: &Arc<Mutex<std::io::Stdout>>, id: Value, result: Value) {
    let r = json!({"jsonrpc": "2.0", "id": id, "result": result});
    let mut o = out.lock().unwrap();
    writeln!(o, "{}", serde_json::to_string(&r).unwrap()).unwrap();
    o.flush().unwrap();
}

fn respond_err(out: &Arc<Mutex<std::io::Stdout>>, id: Value, code: i32, msg: String) {
    let r = json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": msg}});
    let mut o = out.lock().unwrap();
    writeln!(o, "{}", serde_json::to_string(&r).unwrap()).unwrap();
    o.flush().unwrap();
}

// --- tradução neutro ↔ Anthropic ---

fn text_of(parts: &[Value]) -> String {
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
}

/// Retorna (system, messages) — system vai no campo próprio da API.
fn to_anthropic(messages: &[Value]) -> (String, Vec<Value>) {
    let mut system = vec![];
    let mut out = vec![];
    for m in messages {
        let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("user");
        let parts = m
            .get("content")
            .and_then(|c| c.as_array())
            .cloned()
            .unwrap_or_default();
        if role == "system" {
            system.push(text_of(&parts));
            continue;
        }
        if role == "tool" {
            for p in &parts {
                if p.get("type").and_then(|t| t.as_str()) != Some("tool_result") {
                    continue;
                }
                let t = &p["tool_result"];
                out.push(json!({"role": "user", "content": [{
                    "type": "tool_result",
                    "tool_use_id": t.get("call_id"),
                    "content": match t.get("content").cloned().unwrap_or(Value::Null) {
                        Value::String(s) => Value::String(s),
                        v => Value::String(serde_json::to_string(&v).unwrap_or_default()),
                    },
                    "is_error": t.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false),
                }]}));
            }
            continue;
        }
        if role == "assistant" {
            let mut content = vec![];
            let t = text_of(&parts);
            if !t.is_empty() {
                content.push(json!({"type": "text", "text": t}));
            }
            for p in &parts {
                if p.get("type").and_then(|x| x.as_str()) != Some("tool_call") {
                    continue;
                }
                let t = &p["tool_call"];
                content.push(json!({"type": "tool_use",
                    "id": t.get("id"), "name": t.get("name"), "input": t.get("input").cloned().unwrap_or(json!({}))}));
            }
            out.push(json!({"role": "assistant", "content": content}));
            continue;
        }
        out.push(json!({"role": "user", "content": text_of(&parts)}));
    }
    (system.join("\n"), out)
}

fn to_anthropic_tools(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            json!({"name": t.get("name"),
                "description": t.get("description").and_then(|d| d.as_str()).unwrap_or(""),
                "input_schema": t.get("input_schema").cloned().unwrap_or(json!({"type": "object"}))})
        })
        .collect()
}

fn api_key(bus: &mut Bus, stdin: &mut std::io::StdinLock<'_>, cfg: &Cfg) -> Option<String> {
    match bus.rpc(stdin, "atom.secrets.get", json!({"name": cfg.api_key_from})) {
        Value::String(s) if !s.is_empty() => Some(s),
        _ => None,
    }
}

fn body_for(cfg: &Cfg, model: &str, p: &Value, stream: bool) -> Value {
    let messages: Vec<Value> = p
        .get("messages")
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default();
    let (system, msgs) = to_anthropic(&messages);
    let mut body =
        json!({"model": model, "max_tokens": cfg.max_tokens, "messages": msgs, "stream": stream});
    if !system.is_empty() {
        body["system"] = Value::String(system);
    }
    if let Some(t) = p.get("tools").and_then(|t| t.as_array())
        && !t.is_empty()
    {
        body["tools"] = Value::Array(to_anthropic_tools(t));
    }
    body
}

fn send_req(cfg: &Cfg, key: &str, body: &Value) -> Result<reqwest::blocking::Response, String> {
    reqwest::blocking::Client::new()
        .post(format!("{}/v1/messages", cfg.base_url))
        .header("x-api-key", key)
        .header("anthropic-version", "2023-06-01")
        .json(body)
        .send()
        .map_err(|e| format!("http: {e}"))
}

fn do_complete(cfg: &Cfg, key: &str, p: &Value) -> Result<Value, String> {
    let model = p
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or(&cfg.model)
        .to_string();
    let resp = send_req(cfg, key, &body_for(cfg, &model, p, false))?;
    let v: Value = resp.json().map_err(|e| format!("json: {e}"))?;
    if v.get("type").and_then(|t| t.as_str()) == Some("error") {
        return Err(v["error"]
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("erro da API")
            .to_string());
    }
    let mut parts = vec![];
    let mut stop = "stop".to_string();
    for b in v
        .get("content")
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default()
    {
        match b.get("type").and_then(|t| t.as_str()) {
            Some("text") => {
                if let Some(t) = b.get("text").and_then(|x| x.as_str()) {
                    parts.push(json!({"type": "text", "text": t}));
                }
            }
            Some("tool_use") => {
                stop = "tool_call".to_string();
                parts.push(json!({"type": "tool_call", "tool_call": {
                    "id": b.get("id"), "name": b.get("name"),
                    "input": b.get("input").cloned().unwrap_or(json!({}))}}));
            }
            _ => {}
        }
    }
    Ok(json!({
        "message": {"role": "assistant", "content": parts},
        "usage": {"input_tokens": v["usage"].get("input_tokens"), "output_tokens": v["usage"].get("output_tokens")},
        "stop_reason": stop,
    }))
}

fn stream_worker(
    cfg: Cfg,
    key: String,
    p: Value,
    stream: String,
    out: Arc<Mutex<std::io::Stdout>>,
    cancel: Arc<AtomicBool>,
    invoke_id: Value,
) {
    let mut seq = 0u64;
    let fail = |msg: String| {
        emit_end(
            &out,
            &stream,
            json!({"error": {"code": -32603, "message": msg}}),
        );
        respond_err(&out, invoke_id.clone(), -32603, msg);
    };
    let model = p
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or(&cfg.model)
        .to_string();
    let mut resp = match send_req(&cfg, &key, &body_for(&cfg, &model, &p, true)) {
        Ok(r) => r,
        Err(e) => {
            fail(e);
            return;
        }
    };
    if !resp.status().is_success() {
        let st = resp.status();
        let t = resp.text().unwrap_or_default();
        fail(format!("http {st}: {t}"));
        return;
    }
    let mut buf = String::new();
    let mut ev_type = String::new();
    let mut text_acc = String::new();
    // tool_use em curso: (id, name, args acumulados), por índice de bloco.
    let mut calls: HashMap<u64, (String, String, String)> = HashMap::new();
    let mut order: Vec<u64> = vec![];
    let mut usage = json!({});
    let mut stop = "stop".to_string();
    let mut errored = false;
    let mut tmp = [0u8; 8192];
    loop {
        if cancel.load(Ordering::Relaxed) {
            emit_end(
                &out,
                &stream,
                json!({"error": {"code": -32800, "message": "cancelado"}}),
            );
            respond_err(&out, invoke_id.clone(), -32800, "cancelado".to_string());
            return;
        }
        let n = match std::io::Read::read(&mut resp, &mut tmp) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                emit_end(
                    &out,
                    &stream,
                    json!({"error": {"code": -32603, "message": e.to_string()}}),
                );
                respond_err(&out, invoke_id.clone(), -32603, e.to_string());
                errored = true;
                break;
            }
        };
        buf.push_str(&String::from_utf8_lossy(&tmp[..n]));
        while let Some(pos) = buf.find('\n') {
            let line: String = buf.drain(..=pos).collect();
            let line = line.trim();
            if let Some(t) = line.strip_prefix("event:") {
                ev_type = t.trim().to_string();
                continue;
            }
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let ev: Value = match serde_json::from_str(data.trim()) {
                Ok(v) => v,
                Err(_) => continue,
            };
            match ev_type.as_str() {
                "content_block_delta" => {
                    let d = &ev["delta"];
                    match d.get("type").and_then(|t| t.as_str()) {
                        Some("text_delta") => {
                            if let Some(t) = d.get("text").and_then(|v| v.as_str()) {
                                text_acc.push_str(t);
                                emit_chunk(&out, &stream, &mut seq, json!({"delta_text": t}));
                            }
                        }
                        Some("input_json_delta") => {
                            let idx = ev.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
                            let frag = d.get("partial_json").and_then(|v| v.as_str()).unwrap_or("");
                            if !frag.is_empty() {
                                let e = calls.entry(idx).or_insert_with(|| {
                                    (String::new(), String::new(), String::new())
                                });
                                e.2.push_str(frag);
                                if !order.contains(&idx) {
                                    order.push(idx);
                                }
                                emit_chunk(
                                    &out,
                                    &stream,
                                    &mut seq,
                                    json!({"delta_tool_call": {"id": e.0, "name": e.1, "input_delta": frag}}),
                                );
                            }
                        }
                        _ => {}
                    }
                }
                "content_block_start" => {
                    let b = &ev["content_block"];
                    if b.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                        let idx = ev.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
                        calls.insert(
                            idx,
                            (
                                b.get("id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                                b.get("name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                                String::new(),
                            ),
                        );
                    }
                }
                "message_delta" => {
                    if let Some(s) = ev["delta"].get("stop_reason").and_then(|v| v.as_str()) {
                        stop = if s == "tool_use" {
                            "tool_call".to_string()
                        } else {
                            s.to_string()
                        };
                    }
                    if ev.get("usage").is_some() {
                        usage = ev["usage"].clone();
                    }
                }
                "message_stop" => {}
                _ => {}
            }
        }
    }
    if errored {
        return;
    }
    let mut parts = vec![];
    if !text_acc.is_empty() {
        parts.push(json!({"type": "text", "text": text_acc}));
    }
    for idx in &order {
        if let Some((cid, name, args)) = calls.get(idx) {
            let input: Value = serde_json::from_str(args).unwrap_or(Value::Null);
            parts.push(json!({"type": "tool_call", "tool_call": {"id": cid, "name": name, "input": input}}));
        }
    }
    if !order.is_empty() {
        stop = "tool_call".to_string();
    }
    let result = json!({"message": {"role": "assistant", "content": parts}, "usage": usage, "stop_reason": stop});
    emit_end(&out, &stream, result.clone());
    respond(&out, invoke_id, result);
}

fn main() {
    let stdin = std::io::stdin();
    let out = Arc::new(Mutex::new(std::io::stdout()));
    let mut bus = Bus {
        out: out.clone(),
        next: 0,
    };
    let mut cfg = Cfg::from(&Value::Null);
    let cancels: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let mut stdin_lock = stdin.lock();
    loop {
        let mut line = String::new();
        if stdin_lock.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
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
        let method = req
            .get("method")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        let id = req.get("id").cloned();
        let params = req.get("params").cloned().unwrap_or(Value::Null);
        match method.as_str() {
            "atom.exit" => break,
            "atom.initialize" => {
                let c = params.get("config").cloned().unwrap_or(Value::Null);
                cfg = Cfg::from(&c);
                if let Some(i) = id {
                    respond(
                        &out,
                        i,
                        json!({"protocol": "atom/1",
                        "provides": [{"contract": "ai.llm@0", "version": "0.1.0"}]}),
                    );
                }
            }
            "atom.shutdown" => {
                if let Some(i) = id {
                    respond(&out, i, json!({}));
                }
            }
            "atom.stream.cancel" => {
                if let Some(s) = params.get("stream").and_then(|v| v.as_str())
                    && let Some(f) = cancels.lock().unwrap().get(s)
                {
                    f.store(true, Ordering::Relaxed);
                }
            }
            "service.invoke" => {
                let svc = params.get("service").and_then(|s| s.as_str()).unwrap_or("");
                let m = params.get("method").and_then(|x| x.as_str()).unwrap_or("");
                let ip = params.get("params").cloned().unwrap_or(Value::Null);
                let stream = params
                    .get("stream")
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string());
                match (svc, m) {
                    ("ai.llm@0", "models") => {
                        if let Some(i) = id {
                            respond(&out, i, json!([{"id": cfg.model}]));
                        }
                    }
                    ("ai.llm@0", "complete") => {
                        let key = api_key(&mut bus, &mut stdin_lock, &cfg);
                        let Some(key) = key else {
                            let msg = format!(
                                "secret {} ausente (`atom secret set {}`)",
                                cfg.api_key_from, cfg.api_key_from
                            );
                            if let Some(i) = id.clone() {
                                if let Some(s) = stream.clone() {
                                    emit_end(
                                        &out,
                                        &s,
                                        json!({"error": {"code": -32003, "message": msg}}),
                                    );
                                }
                                respond_err(&out, i, -32003, msg);
                            }
                            continue;
                        };
                        match (stream, id) {
                            (Some(s), Some(i)) => {
                                let flag = Arc::new(AtomicBool::new(false));
                                cancels.lock().unwrap().insert(s.clone(), flag.clone());
                                let worker_cfg = Cfg {
                                    base_url: cfg.base_url.clone(),
                                    model: cfg.model.clone(),
                                    api_key_from: cfg.api_key_from.clone(),
                                    max_tokens: cfg.max_tokens,
                                };
                                let out2 = out.clone();
                                let cancels2 = cancels.clone();
                                let s2 = s.clone();
                                std::thread::spawn(move || {
                                    stream_worker(worker_cfg, key, ip, s2.clone(), out2, flag, i);
                                    cancels2.lock().unwrap().remove(&s2);
                                });
                            }
                            (None, Some(i)) => match do_complete(&cfg, &key, &ip) {
                                Ok(v) => respond(&out, i, v),
                                Err(e) => respond_err(&out, i, -32603, e),
                            },
                            _ => {}
                        }
                    }
                    _ => {
                        if let Some(i) = id {
                            respond_err(&out, i, -32601, format!("método {svc}/{m} desconhecido"));
                        }
                    }
                }
            }
            _ => {
                if let Some(i) = id {
                    respond_err(&out, i, -32601, format!("método {method} desconhecido"));
                }
            }
        }
    }
}
