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

## ADR-018 — Spec verificável: cláusulas com id, schemas do kernel e oráculos externos
- **Contexto:** `atom/1` congelou como prosa + tipos serde. Revisão pós-1.0 achou cláusulas da spec sem implementação (janela de stream, propagação de `$/cancelRequest`) e referência a spec inexistente (`contracts.md`). As partes de maior risco (ciclo de vida, `around`/`proceed`, cancelamento, timeout, crash) são máquinas de estado que se cruzam e não têm modelo.
- **Decisão:**
  1. Toda cláusula normativa (deve/não deve/rejeita) recebe um id estável (`[P-STREAM-1]`, ver `specs/conformance.md`). Cada id tem ao menos um teste marcado `// spec: <id>`; CI falha se um id não tem teste ou se um teste cita id inexistente.
  2. O próprio `atom/1` ganha JSON Schemas legíveis por máquina (`docs/specs/schemas/atom-1/`), não só os contratos. `atom-protocol` e `sdks/atom-ts` são testados contra eles (round-trip dos exemplos). Sem gerador de código.
  3. Máquinas de estado críticas têm modelo executável (`stateright`, roda em `cargo test`) com invariantes listadas na spec.
  4. "Está certo" é decidido por oráculos fora do autor: suíte de conformidade executável contra qualquer implementação, fuzzers, mutation testing, extensão adversária e ao menos uma implementação independente escrita só a partir da spec.
- **Consequência:** spec e código não divergem silenciosamente; custo de manter ids e schemas.

## ADR-019 — Identidade por conexão; sem autoridade ambiente no socket local
- **Contexto:** extensões rodam como o mesmo usuário do SO e recebem `HOME`. O socket local 0600 era "cliente confiável com todas as capabilities" (protocol §10). Logo qualquer extensão contornava todos os grants conectando-se ao socket. Também podia editar `grants.toml`/`profile.toml` e o core recarregava.
- **Decisão:**
  1. O core verifica o par da conexão (pid do peer) e recusa como cliente confiável qualquer processo **descendente do daemon** (`-32005 UntrustedPeer`). Extensão só fala com o bus pelo próprio stdio, com seus grants. Linux: `SO_PEERCRED` + `/proc` + `PR_SET_CHILD_SUBREAPER` no daemon (órfãos continuam descendentes). Windows: cada extensão em Job Object (sem breakaway) + pid do cliente. macOS: `LOCAL_PEERPID` + árvore de processos; double-fork escapa (risco residual documentado).
  2. `grants.toml` e campos de segurança de `profile.toml` (`sources`, `bindings`, `validation`) só mudam por comando vindo de cliente confiável. Alteração externa do arquivo é ignorada, gera `atom.grants.tampered`/`atom.config.tampered` e exige `atom grants reload`/`atom profile reload` de um cliente confiável. Watch automático do `profile.toml` vale só para `[config.*]`.
  3. Capabilities não aplicadas pelo core (`fs.*`, `net`, `exec`, `ui.serve`) passam a se chamar **advisory** na spec e na UI de aprovação, com texto explícito: "não é bloqueado; confie no publisher".
- **Consequência:** bus enforcement deixa de ser contornável pelo atalho trivial. Segue não sendo sandbox: leitura direta de arquivos do usuário continua possível até o runtime WASM/sandbox de OS. Código de plataforma isolado (exceção a `forbid(unsafe_code)` com justificativa, AGENTS.md).

## ADR-020 — Backpressure por padrão sem ack; crédito opt-in; recursos sempre limitados
- **Contexto:** a spec v1 prometia janela de 256 chunks com `atom.stream.ack`, mas nenhum cliente nem SDK envia ack e o core o ignora; os canais internos são ilimitados. Impor o ack agora travaria todos os consumidores existentes.
- **Decisão:**
  1. Backpressure padrão = **fila de saída limitada por conexão**: se o consumidor não drena, a fila enche e o core para de ler daquele stream do produtor. Nenhum ack necessário (compatível com todos os clientes atuais).
  2. Controle explícito opt-in: consumidor envia `"flow": {"window": N}` em `atom.call` e passa a ackar; produtor que declara `"flow": "credit"` no `initialize` recebe `atom.stream.credit` e o core não precisa pausar a leitura do stdout inteiro (evita head-of-line entre streams da mesma extensão).
  3. Todo canal interno é limitado; limites de requests em voo, fila de saída e assinaturas são da spec (protocol §13). Excesso = `-32050 Overloaded`, nunca memória ilimitada.
  4. `$/cancelRequest` é propagado de fato (mapeamento de id por salto) e tem prazo de cortesia.
- **Consequência:** a cláusula de janela da v1 é reescrita como opt-in; isto é tratado como correção de spec nunca implementada, não como quebra (nenhuma implementação dependia dela).

## ADR-021 — `deadline` fim a fim no `$ctx`
- **Decisão:** campo opcional `$ctx.deadline` (epoch ms, UTC). O core limita ao `max_deadline` do perfil, propaga a todo salto (provider, interceptors, sub-chamadas via SDK) e ao estourar cancela a cadeia e responde `-32022 DeadlineExceeded`. Timeouts por salto (ex.: 30s do interceptor) viram `min(timeout, restante)`.
- **Consequência:** chamadas encadeadas não somam timeouts; servidor futuro consegue SLO por request. Aditivo em `atom/1`.

## ADR-022 — Eventos com cursor de retomada
- **Contexto:** entrega at-most-once sem replay; o self-update reinicia o daemon e os clientes perdem eventos do intervalo.
- **Decisão:** cada evento entregue ganha `epoch` (id da instância do log de eventos do perfil) e `offset` (monotônico no epoch). O core retém os últimos N eventos (ring) e os passa adiante no re-exec do self-update. `atom.events.subscribe` aceita `from: {epoch, offset}`; lacuna irrecuperável gera `atom.events.gap`. Sem `from`, semântica v1 (at-most-once).
- **Consequência:** clientes que reconectam viram at-least-once (dedupe por `offset`). Aditivo.

## ADR-023 — Semântica do `Storage` faz parte da spec; backends passam por suíte de conformidade
- **Contexto:** "backend trocável" (FS → Postgres/S3) só é verdade se o comportamento observável for definido. Hoje KV é get/set sem versão; consistência, ordem de `list` e durabilidade não estão escritas.
- **Decisão:** por chave, linearizável dentro de `(tenant, profile, extension)`; `set` durável antes de responder; `get` retorna `version` opaca; `set/delete` aceitam `if_version`/`if_absent` (`-32041 VersionConflict`); `list` em ordem de bytes UTF-8 com paginação; `atom.storage.watch` por prefixo. Sem transações multi-chave (não-objetivo). `Secrets`: read-your-writes. Uma suíte genérica sobre os traits `Storage`/`Secrets` é obrigatória para todo backend.
- **Consequência:** trocar backend não muda comportamento das extensões; Postgres/S3 pós-1.0 entram passando na mesma suíte.

## ADR-024 — `store/v2` sobre TUF (supersedes a evolução prevista no ADR-011)
- **Contexto:** `store/v1` está congelado sem expiração de metadados nem limiar de chaves: um mirror pode servir para sempre um índice antigo validamente assinado (freeze attack); a raiz não tem rotação com limiar. Corrigir isso muda o formato, logo é `store/v2`.
- **Decisão:** `store/v2` = repositório TUF estático (root/targets/snapshot/timestamp, delegação de `targets` por publisher). Continua servível como arquivos estáticos. Cliente via `tough` (ou subconjunto próprio de verificação se `tough` exigir dependência C). Core suporta `store/v1` e `store/v2` em paralelo pelo período de `docs/deprecation.md`; sources v1 exibem aviso. O canal de releases do core migra para o mesmo modelo (root embutido no binário).
- **Consequência:** proteção contra freeze, rollback, mix-and-match e comprometimento de uma chave; CI da loja precisa re-assinar `timestamp` diariamente (chave online de poder limitado).

## ADR-025 — Contribuições de UI são dados primeiro (emenda ADR-016 §3–4)
- **Contexto:** ADR-016 define contribuição como "dados + componente". Componente = JS do `ui-web`, o que amarra contribuições à web e impede TUI/Tauri/mobile de renderizar os mesmos pontos.
- **Decisão:** toda contribuição é um **descritor serializável** validado pelo schema do contrato `ui.*` (id, título, ícone, ordem, `when`, ação = chamada de serviço/comando/view, pontos filhos). Componentes são opcionais e **por superfície** (`surfaces.web`, `surfaces.tui`, ...). Renderer de uma superfície sem componente específico renderiza o descritor genericamente. Contratos `ui.*@0` mudam livremente (ADR-014); trabalho em `atom-extensions`.
- **Consequência:** frontends novos reaproveitam todas as contribuições existentes. Core intocado (ADR-012).

## ADR-026 — `atom.ui.session` exige capability `ui.serve` (gate no bus)
- **Contexto:** `atom.ui.session` entregava `wsUrl` + token bearer do `atom serve --ws` a qualquer extensão, sem capability. O token WS dá autoridade de cliente confiável (todas as capabilities), logo qualquer extensão contornava grants (ADR-019) e todo o enforcement do bus.
- **Decisão:** `atom.ui.serve` e `atom.ui.session` exigem a capability `ui.serve`, verificada via `check_cap` no bus; negado = `-32003 CapabilityDenied`, mesmo padrão do braço `atom.events.publish`. Com isso `ui.serve` deixa de ser advisory (`capabilities.md`, `is_advisory_cap` no `main.rs`) e passa a aplicada pelo bus.
- **Consequência:** risco residual honesto — o token WS continua bearer de autoridade total, então quem tem `ui.serve` obtém autoridade total. Só o shell `atom/ui-web` (e equivalentes confiáveis) deve receber `ui.serve`. Pós-1.1: tokens com escopo/menor autoridade.

## ADR-027 — `serve`/`mods`/`asset` ficam no core com semântica genérica
- **Contexto:** `protocol.md §9.0` listava `atom.ui.serve`, `atom.packages.mods` (filtra `kind = "ui-mod"` no core) e `atom.packages.asset` como "fora da spec, pendente de ADR": conceito de UI no core (ADR-012), core interpretando um `kind` fora de `contract`/`runtime`/`distribution` (ADR-004), leitura de pacote sem capability.
- **Decisão:** FICAM, sem rename (renomear quebraria `ui-web` e outros repos), com semântica genérica documentada: `atom.packages.mods` lista pacotes por kind (o filtro atual é só `ui-mod`; generalização total fica para pós-1.1); `atom.packages.asset` é leitura genérica de arquivo de pacote (máx 2 MiB, sem escapes). Não são removidos do bus.
- **Consequência:** texto da spec atualizado em `protocol.md §9.0`; sem quebra de compatibilidade com `ui-web`.
