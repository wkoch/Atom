//! `atom/ui-cli`: chat no terminal via `atom run chat`.
//! Fala com o bus no stdin/stdout e com o terminal no socket `io`.
//! Thread principal dirige tudo; feeders só empurram linhas para o canal.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

enum Ev {
    Bus(Option<Value>),
    Tty(Option<String>),
}

fn send(out: &mut std::io::Stdout, v: &Value) {
    writeln!(out, "{}", serde_json::to_string(v).unwrap()).unwrap();
    out.flush().unwrap();
}

fn main() {
    let (tx, rx) = std::sync::mpsc::channel::<Ev>();
    // feeder do bus (stdin)
    let txb = tx.clone();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            match line {
                Ok(l) if !l.trim().is_empty() => {
                    let v: Value = match serde_json::from_str(&l) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    if txb.send(Ev::Bus(Some(v))).is_err() {
                        break;
                    }
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        let _ = txb.send(Ev::Bus(None));
    });

    let mut out = std::io::stdout();
    let mut next: u64 = 0;
    let mut invoke_id = Value::Null;
    let mut tty: Option<UnixStream> = None;
    let mut streaming: Option<String> = None;
    let mut pending_stream = false;
    let mut model: Option<String> = None;
    let mut target: Option<String> = None;
    let mut workdir: Option<String> = None;
    let mut initial: Option<String> = None;
    let session = "chat".to_string();
    // Prompt pendente de `ui.prompt@0` (guard-shell): (id, método).
    let mut pending_prompt: Option<(Value, String)> = None;

    let prompt = |tty: &mut Option<UnixStream>| {
        if let Some(t) = tty {
            let _ = t.write_all(b"\n> ");
        }
    };
    let say = |tty: &mut Option<UnixStream>, s: &str| {
        if let Some(t) = tty {
            let _ = t.write_all(s.as_bytes());
        }
    };

    for ev in rx {
        match ev {
            Ev::Bus(None) => break,
            Ev::Bus(Some(v)) => {
                let method = v
                    .get("method")
                    .and_then(|m| m.as_str())
                    .unwrap_or("")
                    .to_string();
                let id = v.get("id").cloned();
                let params = v.get("params").cloned().unwrap_or(Value::Null);
                if method.is_empty() {
                    // resposta a `agent.run` (imediata {"stream": ...})
                    if pending_stream {
                        pending_stream = false;
                        streaming = v
                            .get("result")
                            .and_then(|r| r.get("stream"))
                            .and_then(|s| s.as_str())
                            .map(|s| s.to_string());
                    }
                    continue;
                }
                match method.as_str() {
                    "atom.exit" => break,
                    "atom.initialize" => {
                        if let Some(i) = id {
                            send(
                                &mut out,
                                &json!({"jsonrpc": "2.0", "id": i, "result":
                                {"protocol": "atom/1", "provides": [{"contract": "ui.prompt@0", "version": "0.1.0"}]}}),
                            );
                        }
                    }
                    "atom.shutdown" => {
                        if let Some(i) = id {
                            send(&mut out, &json!({"jsonrpc": "2.0", "id": i, "result": {}}));
                        }
                    }
                    "service.invoke" => {
                        let svc = params.get("service").and_then(|s| s.as_str()).unwrap_or("");
                        let m = params.get("method").and_then(|x| x.as_str()).unwrap_or("");
                        let p = params.get("params").cloned().unwrap_or(Value::Null);
                        if svc != "ui.prompt@0" {
                            if let Some(i) = id {
                                send(
                                    &mut out,
                                    &json!({"jsonrpc": "2.0", "id": i,
                                    "error": {"code": -32601, "message": "contrato desconhecido"}}),
                                );
                            }
                            continue;
                        }
                        match m {
                            "notify" => {
                                let text = p.get("text").and_then(|t| t.as_str()).unwrap_or("");
                                say(&mut tty, &format!("\n[aviso] {text}\n"));
                                if tty.is_none() {
                                    say(&mut tty, &format!("[notify] {text}\n"));
                                }
                                if let Some(i) = id {
                                    send(
                                        &mut out,
                                        &json!({"jsonrpc": "2.0", "id": i, "result": {}}),
                                    );
                                }
                            }
                            "confirm" | "ask" => {
                                let title = p
                                    .get("title")
                                    .and_then(|t| t.as_str())
                                    .unwrap_or("Confirmar?");
                                let detail = p.get("detail").and_then(|d| d.as_str()).unwrap_or("");
                                match tty.as_mut() {
                                    Some(t) => {
                                        let _ = t.write_all(
                                            format!("\n{title}\n{detail}\n[y/N]: ").as_bytes(),
                                        );
                                        pending_prompt = Some((
                                            id.clone().unwrap_or(Value::Null),
                                            m.to_string(),
                                        ));
                                    }
                                    None => {
                                        // Sem TTY (ex.: testes): fail-closed.
                                        if let Some(i) = id {
                                            let r = if m == "confirm" {
                                                json!({"ok": false})
                                            } else {
                                                json!({"value": Value::Null})
                                            };
                                            send(
                                                &mut out,
                                                &json!({"jsonrpc": "2.0", "id": i, "result": r}),
                                            );
                                        }
                                    }
                                }
                            }
                            _ => {
                                if let Some(i) = id {
                                    send(
                                        &mut out,
                                        &json!({"jsonrpc": "2.0", "id": i,
                                        "error": {"code": -32601, "message": format!("método {m} desconhecido")}}),
                                    );
                                }
                            }
                        }
                    }
                    "command.run" => {
                        let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                        let args: Vec<String> = params
                            .get("args")
                            .and_then(|a| a.as_array())
                            .cloned()
                            .unwrap_or_default()
                            .iter()
                            .filter_map(|x| x.as_str().map(|s| s.to_string()))
                            .collect();
                        let mut it = args.iter().peekable();
                        while let Some(a) = it.next() {
                            match a.as_str() {
                                "--model" => model = it.next().cloned(),
                                "--target" => target = it.next().cloned(),
                                "--workdir" => workdir = it.next().cloned(),
                                s => {
                                    if initial.is_none() {
                                        initial = Some(s.to_string());
                                    }
                                }
                            }
                        }
                        invoke_id = id.clone().unwrap_or(Value::Null);
                        let tty_flag = params.get("tty").and_then(|t| t.as_bool()).unwrap_or(false);
                        if !tty_flag || name != "chat" {
                            if let Some(i) = id {
                                send(
                                    &mut out,
                                    &json!({"jsonrpc": "2.0", "id": i, "result": {"ok": true}}),
                                );
                            }
                            break;
                        }
                        let path = params
                            .get("io")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();
                        match UnixStream::connect(&path) {
                            Ok(s) => {
                                tty = Some(s);
                                let txt = tx.clone();
                                let mut reader =
                                    BufReader::new(tty.as_ref().unwrap().try_clone().unwrap());
                                std::thread::spawn(move || {
                                    loop {
                                        let mut line = String::new();
                                        match reader.read_line(&mut line) {
                                            Ok(0) => {
                                                let _ = txt.send(Ev::Tty(None));
                                                break;
                                            }
                                            Ok(_) => {
                                                let t =
                                                    line.trim_end_matches(['\r', '\n']).to_string();
                                                if txt.send(Ev::Tty(Some(t))).is_err() {
                                                    break;
                                                }
                                            }
                                            Err(_) => {
                                                let _ = txt.send(Ev::Tty(None));
                                                break;
                                            }
                                        }
                                    }
                                });
                                say(&mut tty, "atom chat (digite `exit` para sair)\n");
                                if let Some(first) = initial.take() {
                                    send_agent(
                                        &mut out, &mut next, &session, &first, &model, &target,
                                        &workdir,
                                    );
                                    pending_stream = true;
                                } else {
                                    prompt(&mut tty);
                                }
                            }
                            Err(e) => {
                                if let Some(i) = id {
                                    send(
                                        &mut out,
                                        &json!({"jsonrpc": "2.0", "id": i,
                                        "error": {"code": -32603, "message": format!("io: {e}")}}),
                                    );
                                }
                                break;
                            }
                        }
                    }
                    "atom.stream.chunk" => {
                        let s = params.get("stream").and_then(|x| x.as_str()).unwrap_or("");
                        if streaming.as_deref() != Some(s) {
                            continue;
                        }
                        let data = params.get("data").cloned().unwrap_or(Value::Null);
                        render_chunk(&mut tty, &data);
                    }
                    "atom.stream.end" => {
                        let s = params.get("stream").and_then(|x| x.as_str()).unwrap_or("");
                        if streaming.as_deref() != Some(s) {
                            continue;
                        }
                        streaming = None;
                        if let Some(e) = params.get("error") {
                            say(
                                &mut tty,
                                &format!(
                                    "\n({})\n",
                                    e.get("message").and_then(|x| x.as_str()).unwrap_or("erro")
                                ),
                            );
                        }
                        prompt(&mut tty);
                    }
                    _ => {
                        if let Some(i) = id {
                            send(
                                &mut out,
                                &json!({"jsonrpc": "2.0", "id": i,
                                "error": {"code": -32601, "message": format!("método {method} desconhecido")}}),
                            );
                        }
                    }
                }
            }
            Ev::Tty(None) => {
                // EOF do terminal: cancela stream, responde e sai.
                if let Some(s) = streaming.take() {
                    send(
                        &mut out,
                        &json!({"jsonrpc": "2.0", "method": "atom.stream.cancel",
                        "params": {"stream": s}}),
                    );
                }
                send(
                    &mut out,
                    &json!({"jsonrpc": "2.0", "id": invoke_id, "result": {"goodbye": true}}),
                );
                break;
            }
            Ev::Tty(Some(line)) => {
                if let Some((pid, kind)) = pending_prompt.take() {
                    let ok = matches!(
                        line.trim().to_lowercase().as_str(),
                        "y" | "yes" | "s" | "sim"
                    );
                    let r = if kind == "confirm" {
                        json!({"ok": ok})
                    } else {
                        json!({"value": line})
                    };
                    send(&mut out, &json!({"jsonrpc": "2.0", "id": pid, "result": r}));
                    prompt(&mut tty);
                    continue;
                }
                if line == "exit" || line == "quit" {
                    if let Some(s) = streaming.take() {
                        send(
                            &mut out,
                            &json!({"jsonrpc": "2.0", "method": "atom.stream.cancel",
                            "params": {"stream": s}}),
                        );
                    }
                    send(
                        &mut out,
                        &json!({"jsonrpc": "2.0", "id": invoke_id, "result": {"goodbye": true}}),
                    );
                    break;
                }
                if streaming.is_some() || pending_stream {
                    say(&mut tty, "(aguardando resposta…)\n");
                    continue;
                }
                send_agent(
                    &mut out, &mut next, &session, &line, &model, &target, &workdir,
                );
                pending_stream = true;
            }
        }
    }
}

fn send_agent(
    out: &mut std::io::Stdout,
    next: &mut u64,
    session: &str,
    input: &str,
    model: &Option<String>,
    target: &Option<String>,
    workdir: &Option<String>,
) {
    *next += 1;
    let mut opts = json!({});
    if let Some(m) = model {
        opts["model"] = json!(m);
    }
    if let Some(w) = workdir {
        opts["workdir"] = json!(w);
    }
    let mut cp = json!({"service": "ai.agent@0", "method": "run",
        "params": {"session": session, "input": input, "options": opts}, "stream": true});
    if let Some(t) = target {
        cp["target"] = json!(t);
    }
    send(
        out,
        &json!({"jsonrpc": "2.0", "id": format!("ui-{next}"), "method": "atom.call", "params": cp}),
    );
}

fn render_chunk(tty: &mut Option<UnixStream>, data: &Value) {
    let t = match data.get("type").and_then(|x| x.as_str()) {
        Some("text") => data
            .get("text")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        Some("tool_call") => format!(
            "\n[tool {} {}]\n",
            data.get("name").and_then(|x| x.as_str()).unwrap_or("?"),
            serde_json::to_string(data.get("input").unwrap_or(&Value::Null)).unwrap_or_default()
        ),
        Some("tool_result") => format!(
            "\n[{}: {}]\n",
            data.get("name").and_then(|x| x.as_str()).unwrap_or("tool"),
            data.get("output").and_then(|x| x.as_str()).unwrap_or("")
        ),
        Some("status") => format!(
            "\n({})\n",
            data.get("text").and_then(|x| x.as_str()).unwrap_or("")
        ),
        Some("done") => String::new(),
        _ => serde_json::to_string(data).unwrap_or_default(),
    };
    if let Some(s) = tty {
        let _ = s.write_all(t.as_bytes());
    }
}
