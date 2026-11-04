/* Layout básico: renderer de ui.root (+ ui.views e ui.status.items).
 * Contrato: exporta render(el, items, ctx); ctx.point diz o ponto.
 * ctx traz { mods, bindings, mountPoint, loadRenderer, wsUrl, token }. */
"use strict";

function itemsOf(mods, point) {
  const out = [];
  for (const m of mods.mods || []) {
    const arr = m.contributes && m.contributes[point];
    if (Array.isArray(arr)) {
      for (const it of arr) out.push(Object.assign({ _mod: m.name }, it));
    }
  }
  out.sort((a, b) => (a.order || 0) - (b.order || 0));
  return out;
}

/* HTML de mods: sanitiza (remove scripts, handlers e URLs javascript:).
 * Mods são pacotes instalados/assinados, mas defesa em profundidade. */
function sanitize(html) {
  const tpl = document.createElement("template");
  const parsed = new DOMParser().parseFromString(html, "text/html");
  const drop = new Set(["script", "style", "iframe", "object", "embed", "link", "meta"]);
  const walk = (node, out) => {
    for (const child of Array.from(node.childNodes)) {
      if (child.nodeType !== 1) {
        out.appendChild(child.cloneNode(false));
        continue;
      }
      const tag = child.tagName.toLowerCase();
      if (drop.has(tag)) {
        continue;
      }
      const clean = document.createElement(tag);
      for (const attr of Array.from(child.attributes || [])) {
        const n = attr.name.toLowerCase();
        const v = String(attr.value || "").trim().toLowerCase();
        if (n.startsWith("on") || v.startsWith("javascript:")) {
          continue;
        }
        try {
          clean.setAttribute(attr.name, attr.value);
        } catch (_e) { /* nome inválido: ignora */ }
      }
      walk(child, clean);
      out.appendChild(clean);
    }
  };
  walk(parsed.body, tpl.content);
  return tpl.content;
}

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text != null) e.textContent = text;
  return e;
}

async function renderRoot(root, ctx) {
  const header = el("header");
  const body = el("div", "layout-body");
  const main = el("main", "views");
  main.id = "views";
  const aside = el("aside");
  aside.id = "settings";
  const footer = el("footer", "statusbar");
  footer.id = "status";
  body.appendChild(main);
  body.appendChild(aside);
  root.appendChild(header);
  root.appendChild(body);
  root.appendChild(footer);
  await ctx.mountPoint(header, ctx.mods, "ui.menu.items", itemsOf(ctx.mods, "ui.menu.items"), ctx);
  await renderViews(main, itemsOf(ctx.mods, "ui.views"), ctx);
  await ctx.mountPoint(aside, ctx.mods, "ui.settings.sections", itemsOf(ctx.mods, "ui.settings.sections"), ctx);
  renderStatus(footer, itemsOf(ctx.mods, "ui.status.items"));
  window.addEventListener("hashchange", () => showView(main));
  showView(main);
}

function showView(main) {
  const h = location.hash.match(/^#\/view\/(.+)$/);
  const id = h ? decodeURIComponent(h[1]) : null;
  let first = null;
  for (const s of main.querySelectorAll(".view")) {
    if (!first) first = s.dataset.view;
    s.classList.toggle("active", id ? s.dataset.view === id : s === main.firstElementChild);
  }
  if (!id && first) history.replaceState(null, "", "#/view/" + first);
}

async function renderViews(main, views, ctx) {
  main.textContent = "";
  if (!views.length) {
    main.appendChild(el("p", null, "nenhuma view instalada"));
    return;
  }
  for (const v of views) {
    const s = el("section", "view");
    s.dataset.view = v.id;
    s.appendChild(el("h2", null, v.title || v.id));
    const body = el("div", "view-body");
    s.appendChild(body);
    if (v.module && v._mod) {
      try {
        const m = await import("/mod/" + v._mod + "/" + v.module);
        const fn = m.renderView || m.render;
        if (typeof fn === "function") {
          await fn(body, Object.assign({}, ctx, { view: v }));
        } else {
          body.textContent = "view sem renderView: " + v.id;
        }
      } catch (e) {
        body.textContent = "falha ao carregar view " + v.id + ": " + e;
      }
    } else if (v.html) {
      body.appendChild(sanitize(v.html));
    } else {
      body.textContent = "(view vazia)";
    }
    main.appendChild(s);
  }
}

function renderStatus(footer, items) {
  footer.textContent = "";
  for (const it of items) {
    const s = el("span", "status-item", it.text);
    s.title = it.id;
    footer.appendChild(s);
  }
  if (!items.length) footer.appendChild(el("span", null, "atom"));
}

export async function render(elm, items, ctx) {
  const point = ctx && ctx.point;
  if (point === "ui.views") {
    await renderViews(elm, items, ctx);
    return;
  }
  if (point === "ui.status.items") {
    renderStatus(elm, items);
    return;
  }
  await renderRoot(elm, ctx);
}
