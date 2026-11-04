/* Shell mínimo do Atom (ADR-016): só monta o ponto raiz.
 * Tudo o que aparece (menu, views, settings, status) vem de mods via
 * contratos. Contribuidor e renderer só conhecem o contrato. */
"use strict";

async function getJson(url) {
  const r = await fetch(url);
  if (!r.ok) throw new Error(url + ": " + r.status);
  return r.json();
}

function modFile(mods, pkg, file) {
  void mods;
  return "/mod/" + pkg + "/" + file;
}

/* Carrega um módulo de renderer e chama `render(el, items, ctx)`. */
async function loadRenderer(mods, point, fallbackPkgs) {
  const bindings = mods.bindings || {};
  let pkg = bindings[point];
  if (!pkg && fallbackPkgs) {
    for (const p of fallbackPkgs) {
      const m = (mods.mods || []).find((x) => x.name === p);
      if (m && m.contributes && m.contributes.renderers && m.contributes.renderers[point]) {
        pkg = p;
        break;
      }
    }
  }
  if (!pkg) return null;
  const mod = (mods.mods || []).find((x) => x.name === pkg);
  const file = mod && mod.contributes && mod.contributes.renderers && mod.contributes.renderers[point] && mod.contributes.renderers[point].module;
  if (!file) return null;
  const m = await import(modFile(mods, pkg, file));
  return { pkg, render: m.render };
}

async function main() {
  const root = document.getElementById("ui-root");
  let session = { wsUrl: null, token: null };
  try {
    session = await getJson("/api/session");
  } catch (e) {
    console.warn("sem sessão WS:", e);
  }
  const ctx = {
    wsUrl: session.wsUrl,
    token: session.token,
    fetchJson: getJson,
  };
  let mods;
  try {
    mods = await getJson("/api/mods");
  } catch (e) {
    root.textContent = "mods indisponíveis: " + e;
    return;
  }
  ctx.mods = mods;
  // Ponto raiz: um renderer single (layout).
  const layout = await loadRenderer(mods, "ui.root", ["atom/layout-basic"]);
  if (!layout) {
    root.textContent = "sem renderer p/ ui.root (bind ui.root)";
    return;
  }
  try {
    await layout.render(root, [], Object.assign({ mountPoint, loadRenderer, point: "ui.root" }, ctx));
  } catch (e) {
    root.textContent = "falha no layout: " + e;
    console.error(e);
  }
}

/* Monta um ponto: renderer do binding (ou primeiro que declarar). */
async function mountPoint(el, mods, point, items, ctx) {
  const r = await loadRenderer(mods, point, null);
  if (!r) {
    console.warn("sem renderer p/", point);
    return;
  }
  await r.render(el, items, Object.assign({ mountPoint, loadRenderer, point }, ctx));
}

window.addEventListener("DOMContentLoaded", main);
