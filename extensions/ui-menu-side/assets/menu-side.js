/* Renderer do ponto ui.menu.items: barra lateral.
 * Mesmo contrato do menu superior — só muda a apresentação. */
"use strict";

export function render(el, items) {
  el.textContent = "";
  const nav = document.createElement("nav");
  nav.className = "menu vertical";
  const sorted = (items || []).slice().sort((a, b) => (a.order || 0) - (b.order || 0));
  for (const it of sorted) {
    const b = document.createElement("button");
    b.textContent = it.label || it.id;
    b.title = it.id;
    b.addEventListener("click", () => {
      if (it.target) location.hash = "#/view/" + encodeURIComponent(it.target);
    });
    nav.appendChild(b);
  }
  if (!sorted.length) nav.appendChild(document.createTextNode("(sem itens de menu)"));
  el.appendChild(nav);
}
