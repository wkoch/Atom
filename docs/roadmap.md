# Roadmap até v1.0

Regras: implementar na ordem; uma versão só termina quando **todos** os critérios passam em CI (Linux obrigatório; macOS/Windows obrigatórios a partir da 0.1 para o que já existir). Não adiantar escopo.

Legenda: `[ ]` pendente, `[x]` concluído.

---

## v0.1 — Kernel
Escopo:
- [x] Cargo workspace (`atom-protocol`, `atom-core`, `atom`), `rust-toolchain.toml`, CI Linux/macOS/Windows com fmt + clippy + test.
- [x] `atom-protocol`: tipos serde de mensagens `atom/1`, `$ctx`, códigos de erro, manifesto e lockfile.
- [x] `atom-core::profile`: diretórios por plataforma, `ATOM_HOME`, perfis (create/list/use/delete), `profile.toml` (sem sources ainda).
- [x] `atom-core::pkg`: ler/validar `atom.toml` (manifest.md); instalar de pasta (`--dev`) e de `.tar.gz` local para a store CAS; `atom.lock` com escrita atômica e `.prev`.
- [x] `atom-core::transport`: stdio NDJSON; unix socket / named pipe.
- [x] `atom-core::bus`: JSON-RPC 2.0, `$ctx` autoritativo, `atom.call` → `service.invoke`, erros padronizados.
- [x] `atom-core::host`: spawn, env filtrado, handshake `initialize/initialized/shutdown/exit`, captura de stderr em log, ativação `onStartup` e `onService`.
- [x] `atom-core::registry`: providers, bindings, resolução (target → binding → único).
- [x] `atom-core::storage`: KV por extensão (`atom.storage.get/set/delete/list`), `atom.config.get`, `atom.log`.
- [x] CLI: `version`, `serve`, `stop`, `status`, `profile *`, `ext install --dev|<tar.gz>`, `ext list|remove|restart`, `bind`, `config`, `call`, `logs`.
- [x] `testdata/echo`: extensão Rust mínima (crate do workspace, usando `atom-protocol`) provendo `test.echo@0`.

Aceite:
- [x] `atom ext install --dev testdata/echo && atom call test.echo@0 echo '{"x":1}'` retorna `{"x":1}`.
- [x] Mesma sequência com `--profile outro` é isolada (lock, KV).
- [x] Contexto chega ao provider com `tenant=local` mesmo se o chamador tentar forjar.
- [x] Core compila sem nenhuma referência a conceitos de IA (revisão manual + grep por `llm|prompt|agent|mcp|skill` em `crates/`).

## v0.2 — Dogfood (extensões, não core)
Escopo (tudo em `extensions/`, instalado com `--dev`):
- [x] Contratos `ai.llm@0`, `ai.tools@0`, `ai.agent@0`, `ui.prompt@0` (`atom-extensions/docs/specs/contracts.md`) como pacotes `kind=contract`.
- [x] `atom/llm-openai-compat` (cobre OpenAI, OpenRouter, Ollama, LM Studio) e `atom/llm-anthropic`. API key via secrets (CLI `secret set` entra aqui no core).
- [x] `atom/tools-fs`, `atom/tools-shell`.
- [x] `atom/agent-loop-basic` (loop simples: llm ↔ tools até `stop`).
- [x] `atom/ui-cli` como comando interativo `atom run chat`.
- Core: streams (`atom.stream.*`, cancelamento), `atom.secrets.*`, `contributes.commands` + `atom run`, `onCommand`.

Aceite:
- [x] Uma sessão de chat real editando arquivos de um projeto via `atom run chat`.
- [x] Trocar provider de LLM só com `atom bind ai.llm@0 ...`.
- [x] Ctrl+C cancela o stream de tokens fim a fim.
- [x] Lista de ajustes nos contratos `@0` registrada (é o objetivo desta versão).

## v0.3 — Hooks, eventos, config
- [x] Interceptors `before/after/around` + `atom.proceed`, prioridade, timeout.
- [x] Eventos pub/sub, eventos do core, `onEvent`.
- [x] `config_schema` validado; `atom.config.changed`; reload de `profile.toml`.
- [x] Validação opcional de contratos (`-32013`).
- [x] Clientes como providers temporários (`atom.register`) → `ui-cli` provê `ui.prompt@0`.
- [x] Extensão exemplo: `atom/guard-shell` intercepta `ai.tools@0/execute` e pede confirmação via `ui.prompt@0`.

Aceite: guard-shell bloqueia/permite comando shell sem nenhuma mudança em `tools-shell` ou no loop.

## v0.4 — Kind handlers
- [x] `handles`, `kind.load/unload`, `onKind`, estado `dormant`.
- [x] Kinds de bootstrap no core: `contract`, `runtime`, `distribution`.
- [x] `atom/skill-loader` (+ `ai.context@0`), `atom/mcp-bridge` (stdio e HTTP).
- [x] `atom/runtime-python` (via `uv`) e/ou `atom/runtime-node` (via `bun`), para skills com scripts.

Aceite: instalar um pacote `kind=skill` e um `kind=mcp` (declarativos) e usá-los no chat; um `kind=xyz` sem handler fica `dormant` sem erro.

## v0.5 — Loja
- [x] Cliente `store/v1` (store.md): config, índice esparso, catalog, cache/ETag, múltiplas sources.
- [x] Verificação completa de assinaturas (4 passos).
- [x] Resolver guloso + `engine` + plataforma + `yanked`.
- [x] `ext search/info/install/update/rollback`, `source *`, `keygen`, `pack`, `publish --github`.
- [x] Repo `atom-store` de referência com CI (validação + regenerar catalog + assinar índices).

Aceite: publicar `atom/echo` na loja GitHub e instalar em perfil limpo em outra máquina; adulterar 1 byte do artefato → instalação recusada.

## v0.6 — Capabilities e robustez
- [x] Grants (capabilities.md): aprovação na instalação, `grants *`, `atom.grants.request`, enforcement no bus, filtro `env:`.
- [x] Supervisão com backoff, `failed`, `ExtensionUnavailable`.
- [x] Hot reload por pacote; ativação lazy completa.
- [x] Journal opcional do bus.

Aceite: extensão `testdata/crash` reinicia com backoff e não derruba o core; chamada sem capability → `-32003`.

## v0.7 — Auto-update
- [x] Self-update do core (updates.md): canais, assinatura com chave embutida, troca atômica (Linux/macOS/Windows), health check e rollback.
- [x] Update automático de extensões por política; pausa em capability nova; rollback automático em falha de ativação.
- [x] Migrações versionadas de formatos de estado.
- [x] `atom store gc`.

Aceite: release N+1 com defeito proposital volta sozinho para N.

## v0.8 — Web
- [x] `atom serve --ws` com token.
- [x] `atom/ui-web` como **shell mínimo** (ADR-016): serve assets, ponte WS, loader de `kind="ui-mod"`, ponto raiz `ui.root`. Sem telas próprias.
- [x] Contratos de UI como pacotes `kind=contract` com pontos de contribuição semânticos (`ui.menu@0`, `ui.views@0`, `ui.settings@0`, `ui.status@0`); renderer `single` por ponto via perfil; aninhamento; estado por mod.
- [x] Mods oficiais mínimos: layout, menu superior, menu lateral, configurações em popup e em painel, chat (sobre `ai.agent@0`).

Aceite: (1) um `ui-mod` instalado adiciona uma view sem tocar em `ui-web` nem em outros mods; (2) trocar o renderer do menu (superior → lateral) e o das configurações (popup → painel lateral) mantém todos os itens de menu e seções/mods internos de configuração, sem alterar nenhum outro pacote.

## v0.9 — Portabilidade e ecossistema
- [x] Builds e releases para todas as plataformas no CI (`release.yml`: Linux amd64/arm64 musl estático via zigbuild, macOS arm64 + Intel cross, Windows msvc; publica na GitHub Release por tag `v*`); instaladores simples (`install.sh`, `install.ps1`).
- [x] SDKs mínimos para extensões (parcial): Rust (`atom-sdk`, sobre `atom-protocol`) e TypeScript (`sdks/atom-ts`), ambos com handshake, call, streams, interceptors e eventos. SDK Python adiado para pós-1.0.
- [x] Distributions `atom/code-starter` e `atom/cowork-starter` (`distributions/`, com `packages`/`bindings`); `profile create --from` aceita id da loja (`publisher/nome[@req]`, instalando distribution + packages via sources do perfil) além de pasta local; `export`/`import` já existiam.
- [x] Documentação de autor de extensão (`docs/extension-authoring.md`: extensão mínima nos SDKs, contrato, kind handler, interceptor/eventos, grants, publicar, distribution).

Aceite: um amigo, em Windows ou macOS, instala o Atom e cria um perfil `code-starter` funcional seguindo só a doc.

## v1.0 — Congelamento
- [x] Congelar `atom/1`, manifesto `manifest = 1`, `store/v1`, formatos `profile = 1` / `lock = 1` (leitores rejeitam versão futura; specs marcadas estáveis).
- [x] Política de depreciação documentada (`docs/deprecation.md`: suporte a `atom/1` por pelo menos um major após `atom/2`).
- [x] Suite de conformidade do protocolo (`crates/atom-core/tests/conformance.rs`: handshake, envelope de erro, NDJSON inválido, notificações, invoke, encerramento; matriz echo + prompt-fake).
- [x] Revisão de segurança (`docs/security-review-v1.md`; corrigidos 0600 com janela legível e bearer sem tempo constante).

Os contratos de IA continuam podendo evoluir (`@0` → `@1`) sem release do core.

## v1.1 — Spec verificável e endurecimento (aditivo sobre `atom/1`)
Referências: ADR-018 a ADR-025, `specs/conformance.md`, `specs/store-v2.md`. Clientes e extensões v1.0 continuam funcionando sem mudança.

Escopo:
- [x] **Verificação (ADR-018):** ids `[..-..-n]` em todas as cláusulas normativas existentes; `scripts/spec-ids` no CI (`[CF-1/2]`); `docs/specs/schemas/atom-1/` + teste de round-trip em `atom-protocol` e `sdks/atom-ts` (`[CF-3]`); `atom-conformance` real (harness `conformance_common.rs` + binário `examples/atom-conformance.rs`, matriz SDK×core em CI); modelos `stateright` (`tests/models/`); `fuzz/` (6 alvos + corpus); `proptest` (`properties.rs`); `cargo-mutants` semanal (`mutants.yml` + `spec_coverage.rs [CF-5]`); soak 10⁶ (`stream_soak.rs`, job `soak`).
- [x] **Divergências spec ↔ código:** `$/cancelRequest` propagado (`[P-CANCEL-1..4]`); `atom.stream.ack` janela opt-in (`[P-STREAM-2]`); canais limitados (`[P-LIM-1]`); socket: spec alinhada ao código (AF_UNIX nas 3 plataformas; named pipe adiado).
- [x] **Identidade por conexão (ADR-019):** peer descendente recusado (Linux `SO_PEERCRED`+subreaper, macOS `LOCAL_PEERPID`, Windows só-próprio-pid; `platform.rs` isolado); `grants.toml.core` + detecção externa; `grants/profile reload` só confiável; advisory `[C-UI-1]`.
- [x] **Fluxo e limites (ADR-020):** fila limitada; `flow.window`; `flow: "credit"` + `atom.stream.credit`; limites §13; `-32050`. SDKs Rust+TS com crédito e window.
- [x] **Deadline (ADR-021):** `$ctx.deadline`, `[bus] max_deadline_ms/default_deadline_ms`, `-32022`; SDKs propagam.
- [x] **Eventos (ADR-022):** `epoch`/`offset`, ring retain, handover no re-exec, `from`, `atom.events.gap`; `atom events tail --from`.
- [x] **Storage (ADR-023):** `version`, `if_version`/`if_absent`, `-32041`, `list` paginado, `atom.storage.watch`; Filesystem passa.
- [x] **`store/v2` (ADR-024) — verificação:** `store::tuf` com verificação própria (ed25519+sha2, sem `tough`; perfil TUF do Atom em `specs/store-v2.md §1.1`), `source add --root`, `source migrate`, aviso v1 (`store_v2.rs`, `[ST2-1..5]`).
- [x] **`store/v2` — integração:** `ext install/search/info/update` despacham para `store::tuf` quando a source é v2; `check_expiry` compara `expires` RFC 3339 com o relógio; ferramenta de publicação v2 na CLI conforme `store-v2.md §8` (`atom ext publish --v2` com `--v2-dir`, `--publisher-key`, `--targets-key/--snapshot-key/--timestamp-key` repetíveis, `--publishers-dir` (default `<v2>/../publishers`), `--timestamp-only`, `--rotate-root` (+`--prev-root-key/--next-root-key`), `--extra-target <rel>=<arq>`, `--check-only`→check; v1 intacto sem `--v2`); canal de releases em `v2/` com root embutido + fetch v2 com fallback v1 (sem cache de versão TUF do canal — janela de freeze limitada à expiração, ver `updates.md`); externo: `atom-store` publica `v1/` + `v2/` com cron diário de `timestamp` (plano em `atom-store/docs/roadmap.md`).
- [x] **SDK cancel provider:** `atom-sdk` (Rust) e `@atom/sdk` (TS) expõem cancelamento ao `on_invoke`/`on_intercept` (token por request).
- [x] **Gaps CLI:** flag global `--json` (JSON em `ext search|info|list`, `source list`); `ext update --check`; `ext rollback --to <prefix>`; `config edit <pkg>` ($EDITOR).
- [x] **ADRs ui.session/serve-mods-asset:** ADR-026 (`atom.ui.serve`/`atom.ui.session` exigem `ui.serve` via `check_cap`, negado = `-32003`; token WS continua bearer de autoridade total — risco residual honesto); ADR-027 (serve/mods/asset ficam com semântica genérica documentada, sem rename); `ui.serve` deixou de ser advisory (`is_advisory_cap` removido no `main.rs`).
- [x] **Bump 1.1.0:** `atom-protocol` 0.1.0→1.1.0, `atom-sdk` 0.9.0→1.1.0, `@atom/sdk` 0.9.0→1.1.0 (histórico v0.9 no roadmap/CHANGELOG permanece; só campos de versão mudam).
- [ ] **UI dados primeiro (ADR-025):** em `atom-extensions` (ADR-E006, spec `ui-contributions.md`, roadmap `E0.2`) — fora deste repo.
- [x] **Achados da revisão cruzada (2026-11-05) — resolvidos na v1.1:**
  - [x] SDK cancel provider (acima).
  - [x] `atom.ui.session` gate por capability (ADR-026).
  - [x] `atom.ui.serve`/`packages.mods`/`packages.asset` ficam genéricos (ADR-027).
  - [x] Gaps CLI `--json`/`update --check`/`rollback --to`/`config edit` (acima).
  - [x] Bump p/ 1.1.0 nos 3 manifests (acima; tag v1.1 após commit — árvore suja, Main tagueia).
Aceite:
- [x] Consumidor que não lê o socket durante stream de 10⁶ chunks: memória estável e sem perda — soak `stream_soak.rs` (`[P-LIM-2]`, job `soak` em CI: 10⁶ chunks reais de `testdata/flood` por canal de 1 slot, RSS estável, `seq` 0..N contíguo).
- [x] Suíte `atom-conformance` passa contra core e SDKs Rust/TS — binário real `examples/atom-conformance.rs` (matriz em CI: echo Rust + echo TS + core via socket); achado da matriz: fallthrough `atom.initialize`→`atom.shutdown` no SDK TS corrigido.
- [x] Ctrl+C chega ao provider como `$/cancelRequest` com o id do salto (`[P-CANCEL-2]`, `stream_cancel.rs`).
- [x] Reconexão com `from` recebe eventos ou `gap` explícito (`events_resume.rs [P-EV-3]`).
- [x] `store/v2` expirado → `expired`; snapshot antigo → `rollback`; v1 com aviso (`store_v2.rs [ST2-2]` + CLI `source list`). Integração CLI→v2 feita nesta v1.1 (instalação via CLI de loja v2 com `expires` futuro; `timestamp` expirado → `tuf expired`).
- [ ] Estável (§8): falta implementação independente, 7 dias de fuzz, revisão externa TUF; memória 10⁶ em CI verde (job `soak`), p99 ≤ 1 ms medido só no bench local (`call_latency.rs`, sem gate em CI).

---

## Pós-1.0 (fora de escopo agora)
Runtime WASM (wasmtime + Component Model; feature opcional do core ou extensão-runtime); sandbox de OS; servidor multi-tenant (auth, backends Postgres/S3 para `Storage`/`Secrets`, isolamento por tenant); loja dinâmica com busca/publicação; OpenTelemetry; resolver PubGrub se necessário. (TUF antecipado para v1.1 como `store/v2`, ADR-024.)
