# Decisões de arquitetura (ADRs)

Formato: contexto → decisão → consequência. Novas decisões são adicionadas ao fim; decisões não são editadas, só substituídas por uma nova ("supersedes ADR-N").

## ADR-001 — Core é microkernel sem conceitos de IA
- **Contexto:** ferramentas de IA ficam obsoletas rápido; o domínio muda a cada poucos meses.
- **Decisão:** o core só gerencia pacotes, processos, mensagens, permissões, estado por perfil e updates. Todo domínio de IA vive em extensões e contratos.
- **Consequência:** core estável; custo de mais IPC e de definir contratos fora do core.

## ADR-002 — Core em Go  — **SUBSTITUÍDO por ADR-015**
- **Contexto:** binário único, cross-compile, desenvolvedor solo, iteração rápida.
- **Decisão:** Go, `CGO_ENABLED=0`.
- **Alternativa rejeitada:** Rust (melhor para WASM component model, porém iteração mais lenta). WASM entra como runtime-extensão pós-1.0, não depende da linguagem do core.

## ADR-003 — Extensões fora de processo, JSON-RPC 2.0 sobre stdio (NDJSON)
- **Contexto:** precisa ser poliglota, isolar crashes, permitir hot reload, funcionar em todo OS.
- **Decisão:** cada extensão executável é um processo; framing NDJSON (uma mensagem JSON por linha, UTF-8); stderr = logs. Mesmo estilo de MCP/LSP.
- **Consequência:** latência de IPC (aceitável, ordem de µs-ms); segurança depende de confiança/assinatura até existir sandbox.

## ADR-004 — `kind` livre + kind handlers
- **Contexto:** surgem novas categorias (skills, MCP, mods...).
- **Decisão:** o core não interpreta `kind`. Pacotes declarativos são carregados por extensões que declaram `handles`.
- **Consequência:** nova categoria nunca exige mudar o core.

## ADR-005 — Contratos são pacotes versionados por major
- **Decisão:** `kind="contract"` com JSON Schema; id `nome@major`; múltiplos providers; perfil escolhe via `bindings`.
- **Consequência:** trocar loop/memória/LLM = trocar binding. Contratos evoluem sem release do core.

## ADR-006 — Interceptors genéricos no bus como mecanismo de hooks
- **Decisão:** hooks não são pontos especiais; são interceptors `before/after/around` sobre qualquer `serviço/método`.
- **Consequência:** qualquer comportamento é alterável sem o provider original prever o hook.

## ADR-007 — Contexto multi-tenant obrigatório em toda mensagem
- **Decisão:** `ctx {tenant,user,profile,session,trace}` em todo request, autoritativo pelo core.
- **Consequência:** migração para servidor sem mudar protocolo/extensões.

## ADR-008 — Storage e secrets atrás de interfaces
- **Decisão:** extensões nunca tocam caminhos do perfil diretamente; usam `atom.storage.*`, `atom.secrets.*` ou diretórios concedidos.
- **Consequência:** backends trocáveis (FS → DB/objeto).

## ADR-009 — Store local endereçada por conteúdo + lockfile por perfil
- **Decisão:** pacotes extraídos em `store/<publisher>/<name>/<version>-<sha256[:16]>/`, imutáveis; perfis referenciam via `atom.lock`.
- **Consequência:** perfis com versões distintas sem duplicação; rollback trivial.

## ADR-010 — Loja como arquivos estáticos (`store/v1`)
- **Decisão:** protocolo implementável por repo GitHub/Pages/S3; servidor dinâmico implementa o mesmo + extras.
- **Consequência:** custo zero para começar; sem busca server-side na versão estática (usa `catalog.json`).

## ADR-011 — Assinatura ed25519 com papéis separados
- **Decisão:** publisher assina artefatos; mantenedor da loja assina o índice; chaves raiz da loja fixadas na config da source. Papéis separados desde já (mesmo que a mesma pessoa) para permitir evoluir para TUF.

## ADR-012 — UI é extensão/cliente; core é headless
- **Decisão:** o binário só tem CLI bootstrap. TUI, web e "mods de tela" são extensões.

## ADR-013 — Formatos
- Manifesto, config de perfil e lockfile: **TOML** (editável por humanos).
- Protocolo, índice da loja, schemas: **JSON**.
- Pacote: **tar.gz**.

## ADR-014 — Contratos de IA instáveis até uso real
- **Decisão:** contratos iniciais são `@0` e podem quebrar livremente até serem validados por dogfooding. Só o protocolo do kernel (`atom/1`), o manifesto e `store/v1` congelam na v1.0.

## ADR-015 — Core em Rust (substitui ADR-002)
- **Contexto:** comparados Go, Rust, Goose e Mojo. Goose (sem subprocessos/rede na stdlib, ~2 meses de vida) e Mojo (sem Windows nativo, ecossistema de sistemas fraco) descartados para o core. Entre Go e Rust, pesaram: WASM Component Model (wasmtime) para sandbox e multi-tenant, core embutível como biblioteca (Tauri, C ABI, servidor), tipos soma/ausência de null num kernel que congela na v1.0, e o compilador barrando mais erros em código gerado por IA.
- **Decisão:** core em Rust stable, Cargo workspace com `atom-protocol` (tipos), `atom-core` (lib do kernel) e `atom` (bin CLI). tokio como runtime async. Sem dependências C no build padrão; Linux via musl estático.
- **Consequências:**
  - Iteração mais lenta e compilação mais demorada que Go; mitigado por modelo de atores + canais (evita brigas com lifetimes) e escopo mínimo do core.
  - Runtime WASM pós-1.0 pode ser embutido no core (wasmtime como feature opcional do Cargo) em vez de exigir uma extensão-runtime separada; ambos os caminhos continuam válidos.
  - `atom-protocol` vira a base do SDK Rust de extensões.
  - Extensões continuam em qualquer linguagem (ADR-003): a escolha não afeta o protocolo.

## ADR-016 — UI web: shell mínimo + mods componíveis por contrato
- **Contexto:** a UI web futura (v0.8) deve seguir a mesma filosofia do core: um shell quase vazio, e tudo o que aparece na tela (menus, painéis, editor, chat, status, um botão, um texto) é um **mod** instalável pela loja. O usuário precisa poder trocar a *forma* da interface (menu superior → menu lateral, popup → painel lateral) sem perder o *conteúdo* (itens de menu, seções de configuração e seus mods internos). Hoje os docs amarravam slots ao layout do shell (`panel`, `sidebar`) e punham o chat dentro do `ui-web`, o que tornaria essa troca impossível.
- **Decisão:**
  1. **`atom/ui-web` é só um shell:** serve assets, faz a ponte WS com o bus, carrega mods (`kind = "ui-mod"`, handler = o shell) e expõe um único ponto raiz (`ui.root`). Não contém chat, menu, layout, tema nem tela alguma.
  2. **Mod = unidade de UI de qualquer tamanho**, de um botão a uma tela inteira. Não há categorias distintas para "widget", "painel" ou "tela"; a regra é a mesma em qualquer nível.
  3. **Pontos de contribuição são semânticos e pertencem a contratos, não a mods.** Nomes descrevem *o quê* (`ui.menu.items`, `ui.settings.sections`, `ui.views`, `ui.status.items`), nunca *onde/como* (`topbar`, `sidebar`, `popup`). Os contratos de UI são pacotes `kind = "contract"` próprios (como `ai.llm@0`), versionados `nome@major` e independentes de qualquer mod ou do shell.
  4. **Contribuir ≠ renderizar.** Mods de conteúdo só *declaram* contribuições (dados + componente) em pontos de contribuição. Mods de layout/container *consomem* um ponto e decidem a apresentação. Os dois lados conhecem apenas o contrato, nunca um ao outro. Trocar o renderer (menu superior ↔ lateral, popup ↔ painel lateral) preserva todas as contribuições.
  5. **Um renderer ativo por ponto**, escolhido no perfil (mesma lógica de binding de contratos `single`). Contribuições são `multi`.
  6. **Aninhamento:** qualquer mod pode declarar seus próprios pontos de contribuição (ex.: o mod de configurações expõe `ui.settings.sections`), seguindo as mesmas regras. Quem renderiza o mod de configurações (popup ou painel) não afeta seus mods internos.
  7. **Estado pertence ao mod, não ao container:** chaveado pelo nome do mod (e id da contribuição), via storage do bus. Trocar layout não perde estado.
  8. **Mods não referenciam outros mods por nome** para montar UI; só pontos de contribuição e contratos de serviço do bus. Lógica pesada fica em extensões comuns acessadas por contrato (ex.: o mod de chat fala com `ai.agent@0`, nunca com o loop diretamente).
- **Consequências:**
  - O core continua sem nenhum conceito de UI (ADR-012); tudo isso vive em `ui-web` + contratos `ui.*` + mods.
  - A UI terminal (`ui-cli`) segue independente; contratos de *serviço* como `ui.prompt@0` são semânticos e servem a qualquer UI.
  - O formato exato (manifesto do `ui-mod`, API JS do shell, schemas dos pontos) é especificado na v0.8, obedecendo a estas regras. Contratos `ui.*` começam em `@0` (ADR-014).

## ADR-017 — Extensões oficiais e loja em repos próprios

- **Status:** aceito (ver ADR-E001 em `atom-extensions/docs/decisions.md`).
- **Decisão:** `extensions/`, `distributions/` e a loja de referência saem do repo `atom`. Ficam em `wkoch/atom-extensions` e `wkoch/atom-store`. O core não depende de nenhum deles.
- **Mantido no `atom`:** `sdks/atom-ts` (par do `atom-sdk`, mesma versão de spec) e `testdata/` (fixtures dos testes do core).
- **Consequências:** os e2e que exercitam extensões oficiais (`guard`, `kind`, `web`) saem do core e rodam em `atom-extensions`. Specs continuam aqui; docs de autor/dogfood ficam no repo de extensões.
