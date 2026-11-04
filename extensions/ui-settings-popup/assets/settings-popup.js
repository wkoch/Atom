/* Renderer do ponto ui.settings.sections: popup sobre a tela.
 * Contrato ui.settings@0: render(el, sections) com sections
 * [{id, title, fields: [{key, label, type, default, help}]}]. */
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
  const open = document.createElement("button");
  open.textContent = "Configurações";
  open.addEventListener("click", () => {
    const overlay = document.createElement("div");
    overlay.className = "settings-overlay";
    const box = document.createElement("div");
    box.className = "settings-box";
    const close = document.createElement("button");
    close.textContent = "Fechar";
    close.addEventListener("click", () => overlay.remove());
    box.appendChild(close);
    for (const s of sections || []) {
      const sec = document.createElement("div");
      sec.className = "settings-section";
      const h = document.createElement("h3");
      h.textContent = s.title || s.id;
      sec.appendChild(h);
      for (const f of s.fields || []) sec.appendChild(fieldRow(f));
      box.appendChild(sec);
    }
    if (!(sections || []).length) {
      const p = document.createElement("p");
      p.textContent = "(sem seções de configuração)";
      box.appendChild(p);
    }
    overlay.appendChild(box);
    overlay.addEventListener("click", (e) => {
      if (e.target === overlay) overlay.remove();
    });
    document.body.appendChild(overlay);
  });
  el.appendChild(open);
}
