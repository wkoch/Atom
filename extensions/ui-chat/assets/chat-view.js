/* View de chat sobre ai.agent@0 (v0.8).
 * Exporta renderView(el, ctx): ctx traz {wsUrl, token}.
 * Fala com o daemon via WS: ai.agent@0/run com stream de eventos. */
"use strict";

function send(ws, obj) {
  ws.send(JSON.stringify(obj));
}

export async function renderView(el, ctx) {
  el.textContent = "";
  const log = document.createElement("div");
  log.className = "chat-log";
  const row = document.createElement("div");
  row.className = "chat-row";
  const input = document.createElement("input");
  input.placeholder = ctx.wsUrl ? "fale com o agente…" : "sem WS (rode com --ws)";
  input.disabled = !ctx.wsUrl;
  const btn = document.createElement("button");
  btn.textContent = "Enviar";
  btn.disabled = !ctx.wsUrl;
  row.appendChild(input);
  row.appendChild(btn);
  el.appendChild(log);
  el.appendChild(row);

  if (!ctx.wsUrl) {
    log.textContent = "chat indisponível: daemon sem --ws.";
    return;
  }
  let url = ctx.wsUrl;
  if (ctx.token) {
    url += (url.includes("?") ? "&" : "?") + "token=" + encodeURIComponent(ctx.token);
  }
  let ws;
  try {
    ws = new WebSocket(url);
  } catch (e) {
    log.textContent = "WS falhou: " + e;
    return;
  }
  let nextId = 0;
  const pending = new Map();
  ws.onmessage = (ev) => {
    let msg;
    try {
      msg = JSON.parse(ev.data);
    } catch (e) {
      return;
    }
    if (msg.method === "atom.stream.chunk") {
      const d = (msg.params && msg.params.data) || {};
      if (d.type === "text" && d.text) log.textContent += d.text;
      if (d.type === "tool_call") log.textContent += "\n[tool " + d.name + "]\n";
      if (d.type === "tool_result") log.textContent += "\n[" + d.name + ": done]\n";
      return;
    }
    if (msg.method === "atom.stream.end") {
      log.textContent += "\n";
      if (msg.params && msg.params.error) {
        log.textContent += "(" + (msg.params.error.message || "erro") + ")\n";
      }
      return;
    }
    if (msg.id != null && pending.has(String(msg.id))) {
      const cb = pending.get(String(msg.id));
      pending.delete(String(msg.id));
      cb(msg);
    }
  };
  const call = (service, method, params, stream) =>
    new Promise((resolve) => {
      nextId += 1;
      const id = "chat-" + nextId;
      pending.set(id, resolve);
      const p = { service, method, params, stream: !!stream };
      send(ws, { jsonrpc: "2.0", id, method: "atom.call", params: p });
    });
  async function ask(text) {
    log.textContent += "\n> " + text + "\n";
    const r = await call("ai.agent@0", "run", { session: "chat", input: text }, true);
    if (r.error) log.textContent += "(" + (r.error.message || "erro") + ")\n";
  }
  btn.addEventListener("click", () => {
    const t = input.value.trim();
    if (!t) return;
    input.value = "";
    ask(t);
  });
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") btn.click();
  });
}
