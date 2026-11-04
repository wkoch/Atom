//! `atom/ui-web`: shell web mínimo (ADR-016, v0.8).
//!
//! - Serve HTTP em 127.0.0.1:0: shell estático (`assets/`) + `/api/*`.
//! - `/api/mods` = pacotes `kind="ui-mod"` + bindings (`atom.packages.mods`).
//! - `/api/session` = `{wsUrl, token}` (`atom.ui.session`; null sem `--ws`).
//! - `/mod/<pub>/<nome>/<arq>` = asset do mod (`atom.packages.asset`).
//! - Anuncia a base em `atom.ui.serve` (aparece no `status`).
//! - Sem telas próprias: tudo o que aparece vem de mods via contratos.
//!
//! Concorrência: loop único dirige o bus; threads HTTP enfileiram pedidos
//! (`Req`) e aguardam resposta. Uma chamada bus por vez; resto em fila.

use serde_json::{Value, json};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::mpsc;

enum BusMsg {
    Line(Option<String>),
    /// Acorda o loop p/ drenar pedidos HTTP (reqtx não acorda busrx).
    Wake,
}

enum ReqKind {
    Mods,
    Session,
    Asset { pkg: String, path: String },
}

struct Req {
    kind: ReqKind,
    reply: mpsc::Sender<Result<Value, String>>,
}

enum Pending {
    Bus {
        rid: String,
        reply: mpsc::Sender<Result<Value, String>>,
    },
    Announce {
        rid: String,
    },
}

fn send(out: &mut std::io::Stdout, v: &Value) {
    writeln!(out, "{}", serde_json::to_string(v).unwrap()).unwrap();
    out.flush().unwrap();
}

fn content_type(path: &str) -> &'static str {
    if path.ends_with(".html") {
        "text/html; charset=utf-8"
    } else if path.ends_with(".js") {
        "text/javascript; charset=utf-8"
    } else if path.ends_with(".css") {
        "text/css; charset=utf-8"
    } else if path.ends_with(".json") {
        "application/json"
    } else {
        "application/octet-stream"
    }
}

fn reply_http(stream: &mut std::net::TcpStream, code: &str, ctype: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {code}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

fn clean_rel(parts: &[&str]) -> Option<String> {
    let mut out = vec![];
    for p in parts {
        if p.is_empty() || *p == "." || *p == ".." {
            return None;
        }
        if p.contains('\\') || p.contains('\0') {
            return None;
        }
        out.push(*p);
    }
    if out.is_empty() {
        return None;
    }
    Some(out.join("/"))
}

fn handle_http(
    mut stream: std::net::TcpStream,
    pkg_dir: String,
    tx: mpsc::Sender<Req>,
    wake: mpsc::Sender<BusMsg>,
) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut head = String::new();
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if line == "\r\n" || line == "\n" || line.trim().is_empty() {
            break;
        }
        head.push_str(&line);
        if head.len() > 65536 {
            return;
        }
    }
    let first = head.lines().next().unwrap_or("");
    let mut it = first.split_whitespace();
    let method = it.next().unwrap_or("");
    let path = it.next().unwrap_or("/");
    if method != "GET" {
        reply_http(
            &mut stream,
            "405 Method Not Allowed",
            "text/plain",
            b"GET apenas",
        );
        return;
    }
    let path = path.split('?').next().unwrap_or("/");
    // Arquivos estáticos do shell.
    for name in ["index.html", "shell.js", "style.css"] {
        if path == format!("/{name}") || (name == "index.html" && path == "/") {
            let f = format!("{pkg_dir}/assets/{name}");
            match std::fs::read(&f) {
                Ok(b) => reply_http(&mut stream, "200 OK", content_type(name), &b),
                Err(_) => reply_http(&mut stream, "404 Not Found", "text/plain", b"ausente"),
            }
            return;
        }
    }
    let segs: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let mut answer = |v: Result<Value, String>| match v {
        Ok(body) => {
            let s = serde_json::to_string(&body).unwrap_or_default();
            reply_http(&mut stream, "200 OK", "application/json", s.as_bytes());
        }
        Err(e) => reply_http(&mut stream, "502 Bad Gateway", "text/plain", e.as_bytes()),
    };
    match segs.as_slice() {
        ["api", "mods"] => {
            let (txr, rxr) = mpsc::channel();
            let _ = tx.send(Req {
                kind: ReqKind::Mods,
                reply: txr,
            });
            let _ = wake.send(BusMsg::Wake);
            match rxr.recv() {
                Ok(v) => answer(v),
                Err(_) => reply_http(
                    &mut stream,
                    "502 Bad Gateway",
                    "text/plain",
                    b"shell ocupado",
                ),
            }
        }
        ["api", "session"] => {
            let (txr, rxr) = mpsc::channel();
            let _ = tx.send(Req {
                kind: ReqKind::Session,
                reply: txr,
            });
            let _ = wake.send(BusMsg::Wake);
            match rxr.recv() {
                Ok(v) => answer(v),
                Err(_) => reply_http(
                    &mut stream,
                    "502 Bad Gateway",
                    "text/plain",
                    b"shell ocupado",
                ),
            }
        }
        ["mod", pub_, name, rest @ ..] => {
            let pkg = format!("{pub_}/{name}");
            match clean_rel(rest) {
                Some(rel) => {
                    let ct = content_type(&rel);
                    let (txr, rxr) = mpsc::channel();
                    let _ = tx.send(Req {
                        kind: ReqKind::Asset {
                            pkg,
                            path: rel.clone(),
                        },
                        reply: txr,
                    });
                    let _ = wake.send(BusMsg::Wake);
                    match rxr.recv() {
                        Ok(Ok(v)) => {
                            let data = v.get("data").and_then(|d| d.as_str()).unwrap_or("");
                            let enc = v.get("encoding").and_then(|e| e.as_str()).unwrap_or("");
                            if enc != "base64" {
                                reply_http(
                                    &mut stream,
                                    "502 Bad Gateway",
                                    "text/plain",
                                    b"encoding?",
                                );
                                return;
                            }
                            // `shortcut: sem base64 no std; decodificação manual mínima.`
                            match b64_decode(data) {
                                Some(b) => reply_http(&mut stream, "200 OK", ct, &b),
                                None => reply_http(
                                    &mut stream,
                                    "502 Bad Gateway",
                                    "text/plain",
                                    b"base64?",
                                ),
                            }
                        }
                        Ok(Err(e)) => {
                            reply_http(&mut stream, "404 Not Found", "text/plain", e.as_bytes())
                        }
                        Err(_) => reply_http(
                            &mut stream,
                            "502 Bad Gateway",
                            "text/plain",
                            b"shell ocupado",
                        ),
                    }
                }
                None => reply_http(
                    &mut stream,
                    "400 Bad Request",
                    "text/plain",
                    "caminho inválido".as_bytes(),
                ),
            }
        }
        _ => reply_http(
            &mut stream,
            "404 Not Found",
            "text/plain",
            b"rota desconhecida",
        ),
    }
}

/// Decodifica base64 padrão (ignora whitespace). Suficiente p/ assets.
fn b64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let clean: Vec<u8> = s.bytes().filter(|b| !b" \t\r\n".contains(b)).collect();
    if !clean.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(clean.len() / 4 * 3);
    for q in clean.chunks(4) {
        let mut n = 0u32;
        let mut pad = 0;
        for (i, &c) in q.iter().enumerate() {
            if c == b'=' {
                pad += 1;
                n <<= 6;
                let _ = i;
            } else {
                n = (n << 6) | val(c)? as u32;
            }
        }
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    Some(out)
}

/// Uma chamada ao bus; retorna o id p/ casar a resposta.
fn bus_send(out: &mut std::io::Stdout, next: &mut u64, method: &str, params: Value) -> String {
    *next += 1;
    let rid = format!("ui-{next}");
    send(
        out,
        &json!({"jsonrpc": "2.0", "id": rid, "method": method, "params": params}),
    );
    rid
}

/// Inicia um pedido HTTP como chamada ao bus (métodos `atom.*` diretos).
fn start_req(out: &mut std::io::Stdout, next: &mut u64, req: Req) -> Pending {
    let (method, params) = match req.kind {
        ReqKind::Mods => ("atom.packages.mods", json!({})),
        ReqKind::Session => ("atom.ui.session", json!({})),
        ReqKind::Asset { pkg, path } => {
            ("atom.packages.asset", json!({"package": pkg, "path": path}))
        }
    };
    let rid = bus_send(out, next, method, params);
    let _ = method;
    Pending::Bus {
        rid,
        reply: req.reply,
    }
}

fn main() {
    let pkg_dir = std::env::var("ATOM_PKG_DIR").unwrap_or_else(|_| ".".to_string());
    let (bustx, busrx) = mpsc::channel::<BusMsg>();
    let txb = bustx.clone();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            match line {
                Ok(l) if !l.trim().is_empty() => {
                    let v: Value = match serde_json::from_str(&l) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    if txb.send(BusMsg::Line(Some(v.to_string()))).is_err() {
                        break;
                    }
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        let _ = txb.send(BusMsg::Line(None));
    });

    let mut out = std::io::stdout();
    let mut next: u64 = 0;
    let mut pending: Option<Pending> = None;
    let mut queue: VecDeque<Req> = VecDeque::new();
    // `announce_due`: anunciado quando o bus ficar livre (não atropela).
    let mut announce_due = false;

    // Sobe o HTTP antes de anunciar (porta efêmera em 127.0.0.1).
    let (reqtx, reqrx) = mpsc::channel::<Req>();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ui-web");
    let addr = listener.local_addr().expect("addr ui-web");
    let url = format!("http://{addr}");
    eprintln!("ui-web em {url}");
    {
        let pkg = pkg_dir.clone();
        let tx = reqtx.clone();
        let wake = bustx.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let pkg = pkg.clone();
                let tx = tx.clone();
                let wake = wake.clone();
                std::thread::spawn(move || handle_http(stream, pkg, tx, wake));
            }
        });
    }
    // Loop principal: uma chamada bus por vez; resto em fila.
    // HTTP acorda o loop via Wake (busrx bloquearia para sempre).
    for ev in busrx {
        match ev {
            BusMsg::Line(None) => break,
            // Acorda p/ drenar a fila (pedidos HTTP não chegam pelo bus).
            BusMsg::Wake => {}
            BusMsg::Line(Some(line)) => {
                let msg: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                // Resposta a uma chamada nossa?
                if msg.get("method").and_then(|m| m.as_str()).is_none() {
                    if let Some(Pending::Bus { rid, reply }) = pending.take() {
                        let got = msg.get("id").and_then(|i| i.as_str()).unwrap_or("");
                        if got != rid {
                            pending = Some(Pending::Bus { rid, reply });
                            continue;
                        }
                        if let Some(e) = msg.get("error") {
                            let m = e.get("message").and_then(|x| x.as_str()).unwrap_or("erro");
                            let _ = reply.send(Err(m.to_string()));
                        } else {
                            let _ =
                                reply.send(Ok(msg.get("result").cloned().unwrap_or(Value::Null)));
                        }
                        continue;
                    }
                    if let Some(Pending::Announce { rid }) = pending.take() {
                        let got = msg.get("id").and_then(|i| i.as_str()).unwrap_or("");
                        if got != rid {
                            pending = Some(Pending::Announce { rid });
                        }
                        continue;
                    }
                    continue;
                }
                let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
                let id = msg.get("id").cloned().unwrap_or(Value::Null);
                let has_id = msg.get("id").is_some();
                match method {
                    "atom.exit" => break,
                    "atom.initialize" => {
                        if has_id {
                            send(
                                &mut out,
                                &json!({"jsonrpc": "2.0", "id": id, "result": {"protocol": "atom/1", "provides": []}}),
                            );
                        }
                    }
                    "atom.shutdown" => {
                        if has_id {
                            send(&mut out, &json!({"jsonrpc": "2.0", "id": id, "result": {}}));
                        }
                    }
                    "atom.initialized" => {
                        // Anuncia a base HTTP assim que o bus ficar livre.
                        announce_due = true;
                    }
                    _ => {
                        if has_id {
                            send(
                                &mut out,
                                &json!({"jsonrpc": "2.0", "id": id,
                        "error": {"code": -32601, "message": format!("método {method} desconhecido")}}),
                            );
                        }
                    }
                }
            }
        }
        // Drena fila (primeiro) e anuncia base quando o bus ficar livre.
        while pending.is_none() {
            let req = queue.pop_front().or_else(|| reqrx.try_recv().ok());
            let Some(req) = req else { break };
            pending = Some(start_req(&mut out, &mut next, req));
        }
        if pending.is_none() && announce_due {
            announce_due = false;
            let rid = bus_send(&mut out, &mut next, "atom.ui.serve", json!({"url": url}));
            pending = Some(Pending::Announce { rid });
        }
    }
}
