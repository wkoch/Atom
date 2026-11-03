//! `atom/llm-openai-compat`: `ai.llm@0` sobre API OpenAI-compatível.
//! Thread principal = loop stdin; cada `complete` com stream roda numa worker
//! (HTTP bloqueante) que verifica `atom.stream.cancel` entre chunks.

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
}

impl Cfg {
    fn from(v: &Value) -> Self {
        Self {
            base_url: v
                .get("base_url")
                .and_then(|x| x.as_str())
                .unwrap_or("https://api.openai.com/v1")
                .trim_end_matches('/')
                .to_string(),
            model: v
                .get("model")
                .and_then(|x| x.as_str())
                .unwrap_or("gpt-4o-mini")
                .to_string(),
            api_key_from: v
                .get("api_key_from")
                .and_then(|x| x.as_str())
                .unwrap_or("openai_api_key")
                .to_string(),
        }
    }

    fn local(&self) -> bool {
        self.base_url.contains("localhost") || self.base_url.contains("127.0.0.1")
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

    /// Request com resposta (usado no thread principal, antes de spawnar worker).
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
                // Notificação no meio do round-trip: só cancel importa aqui.
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

// --- tradução neutro ↔ OpenAI ---

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

fn to_openai_msg(m: &Value) -> Value {
    let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("user");
    let parts = m
        .get("content")
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default();
    match role {
        "tool" => {
            let tr = parts
                .iter()
                .find(|p| p.get("type").and_then(|t| t.as_str()) == Some("tool_result"));
            let (call_id, content) = tr
                .map(|p| {
                    let t = &p["tool_result"];
                    (
                        t.get("call_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        t.get("content").cloned().unwrap_or(Value::Null),
                    )
                })
                .unwrap_or_default();
            let content = match content {
                Value::String(s) => s,
                v => serde_json::to_string(&v).unwrap_or_default(),
            };
            json!({"role": "tool", "tool_call_id": call_id, "content": content})
        }
        "assistant" => {
            let calls: Vec<Value> = parts
                .iter()
                .filter(|p| p.get("type").and_then(|t| t.as_str()) == Some("tool_call"))
                .map(|p| {
                    let t = &p["tool_call"];
                    json!({"id": t.get("id"), "type": "function",
                        "function": {"name": t.get("name"), "arguments": serde_json::to_string(&t.get("input").cloned().unwrap_or(Value::Null)).unwrap_or_default()}})
                })
                .collect();
            let mut o = json!({"role": "assistant", "content": text_of(&parts)});
            if !calls.is_empty() {
                o["tool_calls"] = Value::Array(calls);
            }
            o
        }
        _ => json!({"role": role, "content": text_of(&parts)}),
    }
}

fn to_openai_tools(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            json!({"type": "function", "function": {
                "name": t.get("name"),
                "description": t.get("description").and_then(|d| d.as_str()).unwrap_or(""),
                "parameters": t.get("input_schema").cloned().unwrap_or(json!({"type": "object"})),
            }})
        })
        .collect()
}

fn from_openai_msg(m: &Value) -> (Value, String) {
    let mut parts = vec![];
    if let Some(t) = m.get("content").and_then(|c| c.as_str())
        && !t.is_empty()
    {
        parts.push(json!({"type": "text", "text": t}));
    }
    for tc in m
        .get("tool_calls")
        .and_then(|t| t.as_array())
        .cloned()
        .unwrap_or_default()
    {
        let f = &tc["function"];
        let input: Value = f
            .get("arguments")
            .and_then(|a| a.as_str())
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(Value::Null);
        parts.push(json!({"type": "tool_call", "tool_call": {
            "id": tc.get("id").and_then(|v| v.as_str()).unwrap_or(""),
            "name": f.get("name").and_then(|v| v.as_str()).unwrap_or(""),
            "input": input}}));
    }
    let stop = "stop".to_string();
    (json!({"role": "assistant", "content": parts}), stop)
}

fn map_stop(finish: &str) -> &str {
    match finish {
        "tool_calls" => "tool_call",
        "length" => "length",
        "content_filter" | "error" => "error",
        _ => "stop",
    }
}

fn api_key(bus: &mut Bus, stdin: &mut std::io::StdinLock<'_>, cfg: &Cfg) -> Option<String> {
    let v = bus.rpc(stdin, "atom.secrets.get", json!({"name": cfg.api_key_from}));
    match v {
        Value::String(s) if !s.is_empty() => Some(s),
        _ if cfg.local() => None,
        _ => None,
    }
}

fn do_complete(cfg: &Cfg, key: Option<&str>, p: &Value, stream: bool) -> Result<Value, String> {
    let model = p
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or(&cfg.model)
        .to_string();
    let messages: Vec<Value> = p
        .get("messages")
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(to_openai_msg)
        .collect();
    let mut body = json!({"model": model, "messages": messages, "stream": stream});
    if let Some(t) = p.get("tools").and_then(|t| t.as_array())
        && !t.is_empty()
    {
        body["tools"] = Value::Array(to_openai_tools(t));
    }
    if let Some(extra) = p.get("params")
        && let (Some(b), Some(e)) = (body.as_object_mut(), extra.as_object())
    {
        for (k, v) in e {
            b.insert(k.clone(), v.clone());
        }
    }
    let client = reqwest::blocking::Client::new();
    let mut req = client
        .post(format!("{}/chat/completions", cfg.base_url))
        .json(&body);
    if let Some(k) = key {
        req = req.bearer_auth(k);
    }
    let resp = req.send().map_err(|e| format!("http: {e}"))?;
    if !stream {
        let v: Value = resp.json().map_err(|e| format!("json: {e}"))?;
        if let Some(e) = v.get("error") {
            return Err(e
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("erro da API")
                .to_string());
        }
        let choice = &v["choices"][0];
        let (message, _) = from_openai_msg(&choice["message"]);
        let finish = choice
            .get("finish_reason")
            .and_then(|f| f.as_str())
            .unwrap_or("stop");
        return Ok(json!({
            "message": message,
            "usage": v.get("usage"),
            "stop_reason": map_stop(finish),
        }));
    }
    Err("streaming usa worker".to_string())
}

fn stream_worker(
    cfg: Cfg,
    key: Option<String>,
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
    let messages: Vec<Value> = p
        .get("messages")
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(to_openai_msg)
        .collect();
    let mut body = json!({"model": model, "messages": messages, "stream": true, "stream_options": {"include_usage": true}});
    if let Some(t) = p.get("tools").and_then(|t| t.as_array())
        && !t.is_empty()
    {
        body["tools"] = Value::Array(to_openai_tools(t));
    }
    let client = reqwest::blocking::Client::new();
    let mut req = client
        .post(format!("{}/chat/completions", cfg.base_url))
        .json(&body);
    if let Some(k) = &key {
        req = req.bearer_auth(k);
    }
    let mut resp = match req.send() {
        Ok(r) => r,
        Err(e) => {
            fail(format!("http: {e}"));
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
    let mut text_acc = String::new();
    // (id, name, args acumulados) por índice de tool_call.
    let mut calls: HashMap<String, (String, String, String)> = HashMap::new();
    let mut order: Vec<String> = vec![];
    let mut usage = Value::Null;
    let mut finish = "stop".to_string();
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
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data == "[DONE]" {
                continue;
            }
            let ev: Value = match serde_json::from_str(data) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if let Some(u) = ev.get("usage") {
                usage = u.clone();
            }
            let choice = &ev["choices"][0];
            if let Some(f) = choice.get("finish_reason").and_then(|v| v.as_str()) {
                finish = f.to_string();
            }
            let delta = &choice["delta"];
            if let Some(t) = delta.get("content").and_then(|v| v.as_str())
                && !t.is_empty()
            {
                text_acc.push_str(t);
                emit_chunk(&out, &stream, &mut seq, json!({"delta_text": t}));
            }
            for tc in delta
                .get("tool_calls")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default()
            {
                let idx = tc
                    .get("index")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0)
                    .to_string();
                let entry = calls
                    .entry(idx.clone())
                    .or_insert_with(|| (String::new(), String::new(), String::new()));
                if let Some(id) = tc.get("id").and_then(|v| v.as_str())
                    && !id.is_empty()
                    && entry.0.is_empty()
                {
                    entry.0 = id.to_string();
                }
                if let Some(n) = tc
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|v| v.as_str())
                    && !n.is_empty()
                {
                    entry.1 = n.to_string();
                }
                let frag = tc
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if !frag.is_empty() {
                    if entry.0.is_empty() {
                        entry.0 = format!("call_{idx}");
                    }
                    entry.2.push_str(frag);
                    if !order.contains(&idx) {
                        order.push(idx.clone());
                    }
                    emit_chunk(
                        &out,
                        &stream,
                        &mut seq,
                        json!({"delta_tool_call": {"id": entry.0, "name": entry.1, "input_delta": frag}}),
                    );
                }
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
            parts.push(json!({"type": "tool_call", "tool_call": {
                "id": cid, "name": name, "input": input}}));
        }
    }
    let stop_reason = if order.is_empty() {
        map_stop(&finish).to_string()
    } else {
        "tool_call".to_string()
    };
    let result = json!({
        "message": {"role": "assistant", "content": parts},
        "usage": usage,
        "stop_reason": stop_reason,
    });
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
                            let key = api_key(&mut bus, &mut stdin_lock, &cfg);
                            let mut req = reqwest::blocking::Client::new()
                                .get(format!("{}/models", cfg.base_url));
                            if let Some(k) = key {
                                req = req.bearer_auth(k);
                            }
                            let list: Vec<Value> = req
                                .send()
                                .ok()
                                .and_then(|r| r.json::<Value>().ok())
                                .and_then(|v| v.get("data").cloned())
                                .and_then(|d| d.as_array().cloned())
                                .unwrap_or_default()
                                .iter()
                                .map(|x| json!({"id": x.get("id")}))
                                .collect();
                            respond(&out, i, Value::Array(list));
                        }
                    }
                    ("ai.llm@0", "complete") => {
                        let key = api_key(&mut bus, &mut stdin_lock, &cfg);
                        if key.is_none() && !cfg.local() {
                            let msg = format!(
                                "secret {} ausente (`atom secret set {}`)",
                                cfg.api_key_from, cfg.api_key_from
                            );
                            if let Some(i) = id.clone() {
                                if stream.is_some() {
                                    let s = stream.clone().unwrap();
                                    emit_end(
                                        &out,
                                        &s,
                                        json!({"error": {"code": -32003, "message": msg}}),
                                    );
                                }
                                respond_err(&out, i, -32003, msg);
                            }
                            continue;
                        }
                        match (stream, id) {
                            (Some(s), Some(i)) => {
                                let flag = Arc::new(AtomicBool::new(false));
                                cancels.lock().unwrap().insert(s.clone(), flag.clone());
                                let worker_cfg = Cfg {
                                    base_url: cfg.base_url.clone(),
                                    model: cfg.model.clone(),
                                    api_key_from: cfg.api_key_from.clone(),
                                };
                                let out2 = out.clone();
                                let cancels2 = cancels.clone();
                                let s2 = s.clone();
                                std::thread::spawn(move || {
                                    stream_worker(worker_cfg, key, ip, s2.clone(), out2, flag, i);
                                    cancels2.lock().unwrap().remove(&s2);
                                });
                            }
                            (None, Some(i)) => {
                                match do_complete(&cfg, key.as_deref(), &ip, false) {
                                    Ok(v) => respond(&out, i, v),
                                    Err(e) => respond_err(&out, i, -32603, e),
                                }
                            }
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
                // Resposta a request próprio (secrets) é consumida em `rpc`; aqui ignora.
                if let Some(i) = id {
                    respond_err(&out, i, -32601, format!("método {method} desconhecido"));
                }
            }
        }
    }
}
