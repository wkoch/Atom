//! `atom/agent-loop-basic`: `ai.agent@0` em loop simples (llm ↔ tools).
//! Single-thread, um `run` ativo por vez; resto é máquina de estados no stdin.
//! Descobre tools via `atom.registry.list` + união dos `list` de cada provider.

use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, Write};

#[derive(Default)]
struct Cfg {
    model: Option<String>,
    llm_target: Option<String>,
    max_turns: u64,
}

impl Cfg {
    fn from(v: &Value) -> Self {
        Self {
            model: v
                .get("model")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string()),
            llm_target: v
                .get("llm_target")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string()),
            max_turns: v
                .get("max_turns")
                .and_then(|x| x.as_u64())
                .unwrap_or(10)
                .clamp(1, 50),
        }
    }
}

struct Run {
    session: String,
    stream: Option<String>,
    invoke_id: Value,
    history: Vec<Value>,
    /// tool → (provedor, definição completa do `list`).
    tools: HashMap<String, (String, Value)>,
    llm_stream: Option<String>,
    pending_tool_calls: Vec<(String, String, Value)>,
    /// tool_call em voo (call_id, nome) — resposta casa aqui.
    inflight: Option<(String, String)>,
    tool_acc: HashMap<String, (String, String)>,
    turns: u64,
    max_turns: u64,
    model: Option<String>,
    workdir: Option<String>,
    cancelled: bool,
    llm_target: Option<String>,
    /// true até a união de tools terminar (sem turnos antes disso).
    gathering: bool,
    pending_lists: usize,
    /// texto já repassado ao vivo neste turno (evita ecoar no fim).
    streamed_text: bool,
    /// mensagem final (a que vai no `done`).
    final_message: Option<String>,
}

fn send(out: &mut std::io::Stdout, v: &Value) {
    writeln!(out, "{}", serde_json::to_string(v).unwrap()).unwrap();
    out.flush().unwrap();
}

fn notif(method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "method": method, "params": params})
}

/// Emite evento do agente no próprio stream (só se o chamador pediu stream).
fn emit(stdout: &mut std::io::Stdout, r: &Run, data: Value, seq: &mut u64) {
    if let Some(s) = &r.stream {
        *seq += 1;
        send(
            stdout,
            &notif(
                "atom.stream.chunk",
                json!({"stream": s, "seq": seq, "data": data}),
            ),
        );
    }
}

fn req(id: String, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

fn bus_call(
    stdout: &mut std::io::Stdout,
    next: &mut u64,
    svc: &str,
    m: &str,
    p: Value,
    target: Option<&str>,
    stream: bool,
) -> String {
    *next += 1;
    let id = format!("agent-{next}");
    let mut cp = json!({"service": svc, "method": m, "params": p, "stream": stream});
    if let Some(t) = target {
        cp["target"] = json!(t);
    }
    send(stdout, &req(id.clone(), "atom.call", cp));
    id
}

fn system_text(workdir: &Option<String>) -> String {
    let mut s = String::from(
        "You are an agent operating on the user's machine. Use tools to act; \
         answer concisely with what was done. Always use absolute paths.",
    );
    if let Some(w) = workdir {
        s.push_str(&format!(" Working directory: {w}."));
    }
    s
}

fn main() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut stdin_lock = stdin.lock();
    let mut cfg = Cfg::default();
    let mut sessions: HashMap<String, Vec<Value>> = HashMap::new();
    let mut next: u64 = 0;
    let mut run: Option<Run> = None;
    // pendentes de round-trip: id → ação.
    let mut pending: HashMap<String, Value> = HashMap::new();

    let mut seq = 0u64;
    loop {
        let mut line = String::new();
        if stdin_lock.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("linha inválida: {e}");
                continue;
            }
        };
        let method = v
            .get("method")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        let id = v.get("id").cloned();
        let params = v.get("params").cloned().unwrap_or(Value::Null);

        // Resposta a `atom.call` próprio?
        if method.is_empty() && id.is_some() {
            let rid = id.clone().unwrap().as_str().unwrap_or("").to_string();
            if let Some(tag) = pending.remove(&rid) {
                let result = v.get("result").cloned().unwrap_or(Value::Null);
                if tag == "llm-immediate" {
                    if let Some(r) = run.as_mut() {
                        r.llm_stream = result
                            .get("stream")
                            .and_then(|s| s.as_str())
                            .map(|s| s.to_string());
                    }
                    continue;
                }
                if tag == "registry" {
                    // União de tools, dirigida a eventos (nunca bloqueia).
                    let providers: Vec<String> = result
                        .get("providers")
                        .and_then(|x| x.get("ai.tools@0"))
                        .and_then(|x| x.as_array())
                        .cloned()
                        .unwrap_or_default()
                        .iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect();
                    if let Some(r) = run.as_mut() {
                        r.pending_lists = providers.len();
                        for pn in providers {
                            next += 1;
                            let lid = format!("agent-{next}");
                            pending.insert(lid.clone(), json!(format!("toolslist:{pn}")));
                            let cp = json!({"service": "ai.tools@0", "method": "list", "params": {}, "stream": false, "target": pn});
                            send(&mut stdout, &req(lid, "atom.call", cp));
                        }
                        if r.pending_lists == 0 {
                            r.gathering = false;
                            emit(
                                &mut stdout,
                                r,
                                json!({"type": "status", "text": "pensando..."}),
                                &mut seq,
                            );
                            start_turn(&mut stdout, r, &cfg, &mut next, &mut pending);
                        }
                    }
                    continue;
                }
                if let Some(prov) = tag.as_str().and_then(|t| t.strip_prefix("toolslist:")) {
                    let prov = prov.to_string();
                    if let Some(r) = run.as_mut() {
                        for d in result.as_array().cloned().unwrap_or_default() {
                            if let Some(n) = d.get("name").and_then(|x| x.as_str()) {
                                r.tools.entry(n.to_string()).or_insert((prov.clone(), d));
                            }
                        }
                        if r.pending_lists > 0 {
                            r.pending_lists -= 1;
                        }
                        if r.pending_lists == 0 && r.gathering {
                            r.gathering = false;
                            emit(
                                &mut stdout,
                                r,
                                json!({"type": "status", "text": "pensando..."}),
                                &mut seq,
                            );
                            start_turn(&mut stdout, r, &cfg, &mut next, &mut pending);
                        }
                    }
                    continue;
                }
                if tag.as_str().map(|t| t.starts_with("tool:")) == Some(true) {
                    if let Some(r) = run.as_mut() {
                        let (call_id, name) = r.inflight.clone().unwrap_or_default();
                        let text = text_of_result(&result);
                        let is_error = result
                            .get("is_error")
                            .and_then(|x| x.as_bool())
                            .unwrap_or(false);
                        r.inflight = None;
                        r.history.push(json!({"role": "tool", "content": [
                            {"type": "tool_result", "tool_result": {"call_id": call_id, "content": text, "is_error": is_error}}
                        ]}));
                        emit(
                            &mut stdout,
                            r,
                            json!({"type": "tool_result", "name": name, "output": text_of_result(&result), "is_error": is_error}),
                            &mut seq,
                        );
                        // próxima tool da fila ou próximo turno
                        if let Some((cid, name, input)) = r.pending_tool_calls.first().cloned() {
                            r.pending_tool_calls.remove(0);
                            fire_tool(
                                &mut stdout,
                                r,
                                &mut next,
                                &mut pending,
                                &mut seq,
                                (cid, name, input),
                            );
                        } else {
                            start_turn(&mut stdout, r, &cfg, &mut next, &mut pending);
                        }
                    }
                    continue;
                }
            }
            continue;
        }

        match method.as_str() {
            "atom.exit" => break,
            "atom.initialize" => {
                let c = params.get("config").cloned().unwrap_or(Value::Null);
                cfg = Cfg::from(&c);
                if let Some(i) = id {
                    send(
                        &mut stdout,
                        &json!({"jsonrpc": "2.0", "id": i, "result":
                        {"protocol": "atom/1", "provides": [{"contract": "ai.agent@0", "version": "0.1.0"}]}}),
                    );
                }
            }
            "atom.shutdown" => {
                if let Some(i) = id {
                    send(
                        &mut stdout,
                        &json!({"jsonrpc": "2.0", "id": i, "result": {}}),
                    );
                }
            }
            "atom.stream.cancel" => {
                if let (Some(r), Some(s)) =
                    (run.as_mut(), params.get("stream").and_then(|x| x.as_str()))
                    && r.stream.as_deref() == Some(s)
                {
                    r.cancelled = true;
                    if let Some(ls) = r.llm_stream.clone() {
                        send(
                            &mut stdout,
                            &notif("atom.stream.cancel", json!({"stream": ls})),
                        );
                    } else {
                        finish_cancelled(&mut stdout, &mut sessions, r, &mut seq);
                        run = None;
                    }
                }
            }
            "atom.stream.chunk" => {
                let s = params.get("stream").and_then(|x| x.as_str()).unwrap_or("");
                let take = run.as_ref().and_then(|r| r.llm_stream.clone()).as_deref() == Some(s);
                if take {
                    let data = params.get("data").cloned().unwrap_or(Value::Null);
                    if let Some(r) = run.as_mut() {
                        if let Some(t) = data.get("delta_text").and_then(|x| x.as_str()) {
                            r.streamed_text = true;
                            emit(&mut stdout, r, json!({"type": "text", "text": t}), &mut seq);
                        }
                        if let Some(dt) = data.get("delta_tool_call") {
                            let cid = dt
                                .get("id")
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string();
                            if !cid.is_empty() {
                                let e = r
                                    .tool_acc
                                    .entry(cid.clone())
                                    .or_insert((String::new(), String::new()));
                                if let Some(n) = dt.get("name").and_then(|x| x.as_str())
                                    && !n.is_empty()
                                {
                                    e.0 = n.to_string();
                                }
                                if let Some(f) = dt.get("input_delta").and_then(|x| x.as_str()) {
                                    e.1.push_str(f);
                                }
                            }
                        }
                    }
                }
            }
            "atom.stream.end" => {
                let s = params.get("stream").and_then(|x| x.as_str()).unwrap_or("");
                let take = run.as_ref().and_then(|r| r.llm_stream.clone()).as_deref() == Some(s);
                if !take {
                    continue;
                }
                if let Some(err) = params.get("error") {
                    if let Some(r) = run.as_mut() {
                        emit(
                            &mut stdout,
                            r,
                            json!({"type": "done", "message": format!("llm: {}", err.get("message").and_then(|x| x.as_str()).unwrap_or("erro"))}),
                            &mut seq,
                        );
                        r.final_message = Some(format!(
                            "llm: {}",
                            err.get("message")
                                .and_then(|x| x.as_str())
                                .unwrap_or("erro")
                        ));
                        end_run(&mut stdout, &mut sessions, r, &mut seq, true);
                    }
                    run = None;
                    continue;
                }
                let result = params.get("result").cloned().unwrap_or(Value::Null);
                let msg = result.get("message").cloned().unwrap_or(Value::Null);
                let parts = msg
                    .get("content")
                    .and_then(|c| c.as_array())
                    .cloned()
                    .unwrap_or_default();
                let calls: Vec<(String, String, Value)> = parts
                    .iter()
                    .filter(|p| p.get("type").and_then(|t| t.as_str()) == Some("tool_call"))
                    .map(|p| {
                        let t = &p["tool_call"];
                        (
                            t.get("id")
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string(),
                            t.get("name")
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string(),
                            t.get("input").cloned().unwrap_or(Value::Null),
                        )
                    })
                    .collect();
                if let Some(r) = run.as_mut() {
                    // texto final do turno (só se não foi repassado ao vivo)
                    if !r.streamed_text {
                        for p in &parts {
                            if p.get("type").and_then(|t| t.as_str()) == Some("text")
                                && let Some(t) = p.get("text").and_then(|x| x.as_str())
                            {
                                emit(&mut stdout, r, json!({"type": "text", "text": t}), &mut seq);
                            }
                        }
                    }
                    r.history.push(msg);
                    r.llm_stream = None;
                    r.tool_acc.clear();
                    if r.cancelled {
                        finish_cancelled(&mut stdout, &mut sessions, r, &mut seq);
                        run = None;
                        continue;
                    }
                    if calls.is_empty() {
                        let text = text_of_parts(&parts);
                        emit(
                            &mut stdout,
                            r,
                            json!({"type": "done", "message": text.clone()}),
                            &mut seq,
                        );
                        r.final_message = Some(text);
                        end_run(&mut stdout, &mut sessions, r, &mut seq, false);
                        run = None;
                        continue;
                    }
                    if r.turns >= r.max_turns {
                        emit(
                            &mut stdout,
                            r,
                            json!({"type": "done", "message": "limite de turnos"}),
                            &mut seq,
                        );
                        r.final_message = Some("limite de turnos".to_string());
                        end_run(&mut stdout, &mut sessions, r, &mut seq, false);
                        run = None;
                        continue;
                    }
                    r.pending_tool_calls = calls;
                    // dispara a primeira tool; as demais encadeiam nas respostas
                    if let Some((cid, name, input)) = r.pending_tool_calls.first().cloned() {
                        r.pending_tool_calls.remove(0);
                        fire_tool(
                            &mut stdout,
                            r,
                            &mut next,
                            &mut pending,
                            &mut seq,
                            (cid, name, input),
                        );
                    }
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
                    ("ai.agent@0", "sessions.list") => {
                        if let Some(i) = id {
                            let names: Vec<String> = sessions.keys().cloned().collect();
                            send(
                                &mut stdout,
                                &json!({"jsonrpc": "2.0", "id": i, "result": names}),
                            );
                        }
                    }
                    ("ai.agent@0", "sessions.get") => {
                        if let Some(i) = id {
                            let s = ip.get("session").and_then(|x| x.as_str()).unwrap_or("");
                            let turns = sessions.get(s).cloned().unwrap_or_default();
                            send(
                                &mut stdout,
                                &json!({"jsonrpc": "2.0", "id": i,
                                "result": {"session": s, "turns": turns}}),
                            );
                        }
                    }
                    ("ai.agent@0", "sessions.delete") => {
                        if let Some(i) = id {
                            let s = ip.get("session").and_then(|x| x.as_str()).unwrap_or("");
                            let had = sessions.remove(s).is_some();
                            send(
                                &mut stdout,
                                &json!({"jsonrpc": "2.0", "id": i, "result": {"deleted": had}}),
                            );
                        }
                    }
                    ("ai.agent@0", "run") => {
                        if run.is_some() {
                            if let Some(i) = id {
                                send(
                                    &mut stdout,
                                    &json!({"jsonrpc": "2.0", "id": i,
                                    "error": {"code": -32603, "message": "run já em andamento"}}),
                                );
                            }
                            continue;
                        }
                        let session = ip
                            .get("session")
                            .and_then(|x| x.as_str())
                            .unwrap_or("chat")
                            .to_string();
                        let input = ip
                            .get("input")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();
                        let opts = ip.get("options").cloned().unwrap_or(Value::Null);
                        let mut history = sessions.get(&session).cloned().unwrap_or_default();
                        history.push(
                            json!({"role": "user", "content": [{"type": "text", "text": input}]}),
                        );
                        let workdir = opts
                            .get("workdir")
                            .and_then(|x| x.as_str())
                            .map(|s| s.to_string());
                        run = Some(Run {
                            session,
                            stream,
                            invoke_id: id.clone().unwrap_or(Value::Null),
                            history,
                            tools: HashMap::new(),
                            llm_stream: None,
                            pending_tool_calls: vec![],
                            inflight: None,
                            tool_acc: HashMap::new(),
                            turns: 0,
                            max_turns: opts
                                .get("max_turns")
                                .and_then(|x| x.as_u64())
                                .unwrap_or(cfg.max_turns),
                            model: opts
                                .get("model")
                                .and_then(|x| x.as_str())
                                .map(|s| s.to_string())
                                .or_else(|| cfg.model.clone()),
                            workdir,
                            cancelled: false,
                            llm_target: cfg.llm_target.clone(),
                            gathering: true,
                            pending_lists: 0,
                            streamed_text: false,
                            final_message: None,
                        });
                        // União de tools dirigida a eventos (nunca bloqueia o stdin).
                        next += 1;
                        let rid = format!("agent-{next}");
                        pending.insert(rid.clone(), json!("registry"));
                        send(&mut stdout, &req(rid, "atom.registry.list", json!({})));
                    }
                    _ => {
                        if let Some(i) = id {
                            send(
                                &mut stdout,
                                &json!({"jsonrpc": "2.0", "id": i,
                                "error": {"code": -32601, "message": format!("método {svc}/{m} desconhecido")}}),
                            );
                        }
                    }
                }
            }
            _ => {
                if let Some(i) = id {
                    send(
                        &mut stdout,
                        &json!({"jsonrpc": "2.0", "id": i,
                        "error": {"code": -32601, "message": format!("método {method} desconhecido")}}),
                    );
                }
            }
        }
    }
}

fn text_of_parts(parts: &[Value]) -> String {
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

fn text_of_result(v: &Value) -> String {
    v.get("output")
        .and_then(|o| o.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_else(|| serde_json::to_string(v).unwrap_or_default())
}

/// Prefixa `path` relativo com workdir (tools de arquivo).
fn absolutize(input: &mut Value, workdir: &Option<String>) {
    if let (Some(w), Some(obj)) = (workdir, input.as_object_mut())
        && let Some(p) = obj.get("path").and_then(|x| x.as_str())
        && !p.starts_with('/')
        && !p.is_empty()
    {
        obj.insert("path".to_string(), Value::String(format!("{w}/{p}")));
    }
}

/// Dispara um turno no LLM (stream sempre; quem não pediu stream ignora chunks).
/// Dispara uma tool_call (emite evento, absolutiza path, registra inflight).
fn fire_tool(
    stdout: &mut std::io::Stdout,
    r: &mut Run,
    next: &mut u64,
    pending: &mut HashMap<String, Value>,
    seq: &mut u64,
    call: (String, String, Value),
) {
    let (cid, name, input) = call;
    emit(
        stdout,
        r,
        json!({"type": "tool_call", "name": name, "input": input}),
        seq,
    );
    let prov = r.tools.get(&name).map(|(p, _)| p.clone());
    let mut call_input = input;
    absolutize(&mut call_input, &r.workdir);
    let cp = json!({"name": name, "input": call_input});
    let tid = bus_call(
        stdout,
        next,
        "ai.tools@0",
        "execute",
        cp,
        prov.as_deref(),
        false,
    );
    pending.insert(tid, json!("tool:exec"));
    r.inflight = Some((cid, name));
}

fn start_turn(
    stdout: &mut std::io::Stdout,
    r: &mut Run,
    _cfg: &Cfg,
    next: &mut u64,
    pending: &mut HashMap<String, Value>,
) {
    r.turns += 1;
    r.llm_stream = None;
    r.tool_acc.clear();
    r.streamed_text = false;
    let mut messages = vec![
        json!({"role": "system", "content": [{"type": "text", "text": system_text(&r.workdir)}]}),
    ];
    messages.extend(r.history.clone());
    let mut p = json!({"messages": messages});
    if let Some(m) = &r.model {
        p["model"] = json!(m);
    }
    // tools no formato neutro do contrato (definições reais da união).
    let defs: Vec<Value> = r.tools.values().map(|(_, d)| d.clone()).collect();
    if !defs.is_empty() {
        p["tools"] = Value::Array(defs);
    }
    let mut cp = json!({"service": "ai.llm@0", "method": "complete", "params": p, "stream": true});
    if let Some(t) = &r.llm_target {
        cp["target"] = json!(t);
    }
    *next += 1;
    let id = format!("agent-{next}");
    pending.insert(id.clone(), json!("llm-immediate"));
    send(stdout, &req(id, "atom.call", cp));
}

fn end_run(
    stdout: &mut std::io::Stdout,
    sessions: &mut HashMap<String, Vec<Value>>,
    r: &mut Run,
    seq: &mut u64,
    failed: bool,
) {
    sessions.insert(r.session.clone(), r.history.clone());
    let text = r.final_message.clone().unwrap_or_default();
    if let Some(s) = &r.stream {
        let payload = if failed {
            json!({"error": {"code": -32603, "message": text}})
        } else {
            json!({"result": {"message": text}})
        };
        let mut obj = serde_json::Map::new();
        obj.insert("stream".to_string(), json!(s));
        if failed {
            obj.insert("error".to_string(), payload["error"].clone());
        } else {
            obj.insert("result".to_string(), payload["result"].clone());
        }
        send(stdout, &notif("atom.stream.end", Value::Object(obj)));
    }
    if failed {
        send(
            stdout,
            &json!({"jsonrpc": "2.0", "id": r.invoke_id,
                "error": {"code": -32603, "message": text}}),
        );
    } else {
        send(
            stdout,
            &json!({"jsonrpc": "2.0", "id": r.invoke_id, "result": {"message": text}}),
        );
    }
    let _ = seq;
}

fn finish_cancelled(
    stdout: &mut std::io::Stdout,
    sessions: &mut HashMap<String, Vec<Value>>,
    r: &mut Run,
    seq: &mut u64,
) {
    sessions.insert(r.session.clone(), r.history.clone());
    if let Some(s) = &r.stream {
        let mut obj = serde_json::Map::new();
        obj.insert("stream".to_string(), json!(s));
        obj.insert(
            "error".to_string(),
            json!({"code": -32800, "message": "cancelado"}),
        );
        send(stdout, &notif("atom.stream.end", Value::Object(obj)));
    }
    send(
        stdout,
        &json!({"jsonrpc": "2.0", "id": r.invoke_id,
            "error": {"code": -32800, "message": "cancelado"}}),
    );
    let _ = seq;
}
