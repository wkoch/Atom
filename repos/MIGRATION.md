# Migração: monorepo `atom` → 3 repos

Estas pastas são cópias (o repo `atom` não foi alterado). Passos:

## 1. Criar os repos
```sh
mv repos/atom-extensions ~/Projetos/atom-extensions
mv repos/atom-store      ~/Projetos/atom-store
cd ~/Projetos/atom-extensions && git init && git add . && git commit -m "extração do monorepo atom v1.0"
cd ~/Projetos/atom-store      && git init && git add . && git commit -m "loja store/v1 inicial"
```
Substituir `<org>` em: `atom-extensions/Cargo.toml`, `atom-extensions/README.md`,
`atom-store/.github/workflows/store.yml`, `atom-store/docs/operations.md`, `atom-store/v1/config.json.example`.

## 2. GitHub
- `atom-extensions`: secrets `ATOM_PUBLISHER_KEY`, `ATOM_STORE_TOKEN`; vars `ATOM_STORE_REPO`, `ATOM_VERSION`.
- `atom-store`: Pages na raiz; secret `STORE_PRIVKEY` (opcional); proteção de branch com review obrigatório.
- `atom` (core): publicar `atom-sdk` (crates.io ou tag) — pré-requisito do roadmap E0.

## 3. Limpar o repo `atom` (depois que os novos repos estiverem verdes)
- Remover `extensions/`, `distributions/`, `atom-store/` e esses membros de `[workspace].members`
  (+ `reqwest` de `[workspace.dependencies]` se nada do core usar).
- **Testes do core que usam `extensions/*`** (precisam ir para `atom-extensions/tests/` ou trocar por `testdata/`):
  `crates/atom-core/tests/guard_e2e.rs`, `kind_e2e.rs` e outros — `grep -rn 'extensions/' crates`.
- Mover `docs/extension-authoring.md`, `docs/specs/contracts.md`, `docs/contract-adjustments-0.md`,
  `docs/dogfood-v0.2.md` (já copiados) e deixar links no core.
- ADR novo no core (ADR-017): "extensões oficiais e loja em repos próprios", referenciando ADR-E001.
- Roadmap do core: pós-1.0 aponta para `atom-extensions/docs/roadmap.md` no que for UI/mods.
- Considerar embutir a source oficial (URL + chave raiz) como padrão no binário.
