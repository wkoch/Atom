/* Renderer do ponto ui.settings.sections: painel lateral sempre visível.
 * Mesmo contrato do popup — só muda a apresentação. */
"use strict";

function fieldRow(f) {
  const lab = document.createElement("label");
  const name = document.createElement("strong");
  name.textContent = f.label || f.key;
  lab.appendChild(name);
  let input;
  if (f.type === "check") {
    input = document.createElement("input");
    input.type = "checkbox";
    input.checked = !!f.default;
  } else if (f.type === "number") {
    input = document.createElement("input");
    input.type = "number";
    if (f.default !== undefined) input.value = f.default;
  } else if (f.type === "note") {
    const p = document.createElement("p");
    p.textContent = f.help || f.default || "";
    lab.appendChild(p);
    return lab;
  } else {
    input = document.createElement("input");
    input.type = "text";
    if (f.default !== undefined) input.value = f.default;
  }
  input.name = f.key;
  lab.appendChild(input);
  if (f.help) {
    const h = document.createElement("small");
    h.textContent = f.help;
    lab.appendChild(h);
  }
  return lab;
}

export function render(el, sections) {
  el.textContent = "";
  const panel = document.createElement("div");
  panel.className = "settings-side";
  const h = document.createElement("h2");
  h.textContent = "Configurações";
  panel.appendChild(h);
  for (const s of sections || []) {
    const sec = document.createElement("div");
    sec.className = "settings-section";
    const t = document.createElement("h3");
    t.textContent = s.title || s.id;
    sec.appendChild(t);
    for (const f of s.fields || []) sec.appendChild(fieldRow(f));
    panel.appendChild(sec);
  }
  if (!(sections || []).length) {
    const p = document.createElement("p");
    p.textContent = "(sem seções de configuração)";
    panel.appendChild(p);
  }
  el.appendChild(panel);
}
