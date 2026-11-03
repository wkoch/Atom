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
- [x] Contratos `ai.llm@0`, `ai.tools@0`, `ai.agent@0`, `ui.prompt@0` (contracts.md) como pacotes `kind=contract`.
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
- [ ] Interceptors `before/after/around` + `atom.proceed`, prioridade, timeout.
- [ ] Eventos pub/sub, eventos do core, `onEvent`.
- [ ] `config_schema` validado; `atom.config.changed`; reload de `profile.toml`.
- [ ] Validação opcional de contratos (`-32013`).
- [ ] Clientes como providers temporários (`atom.register`) → `ui-cli` provê `ui.prompt@0`.
- [ ] Extensão exemplo: `atom/guard-shell` intercepta `ai.tools@0/execute` e pede confirmação via `ui.prompt@0`.

Aceite: guard-shell bloqueia/permite comando shell sem nenhuma mudança em `tools-shell` ou no loop.

## v0.4 — Kind handlers
- [ ] `handles`, `kind.load/unload`, `onKind`, estado `dormant`.
- [ ] Kinds de bootstrap no core: `contract`, `runtime`, `distribution`.
- [ ] `atom/skill-loader` (+ `ai.context@0`), `atom/mcp-bridge` (stdio e HTTP).
- [ ] `atom/runtime-python` (via `uv`) e/ou `atom/runtime-node` (via `bun`), para skills com scripts.

Aceite: instalar um pacote `kind=skill` e um `kind=mcp` (declarativos) e usá-los no chat; um `kind=xyz` sem handler fica `dormant` sem erro.

## v0.5 — Loja
- [ ] Cliente `store/v1` (store.md): config, índice esparso, catalog, cache/ETag, múltiplas sources.
- [ ] Verificação completa de assinaturas (4 passos).
- [ ] Resolver guloso + `engine` + plataforma + `yanked`.
- [ ] `ext search/info/install/update/rollback`, `source *`, `keygen`, `pack`, `publish --github`.
- [ ] Repo `atom-store` de referência com CI (validação + regenerar catalog + assinar índices).

Aceite: publicar `atom/echo` na loja GitHub e instalar em perfil limpo em outra máquina; adulterar 1 byte do artefato → instalação recusada.

## v0.6 — Capabilities e robustez
- [ ] Grants (capabilities.md): aprovação na instalação, `grants *`, `atom.grants.request`, enforcement no bus, filtro `env:`.
- [ ] Supervisão com backoff, `failed`, `ExtensionUnavailable`.
- [ ] Hot reload por pacote; ativação lazy completa.
- [ ] Journal opcional do bus.

Aceite: extensão `testdata/crash` reinicia com backoff e não derruba o core; chamada sem capability → `-32003`.

## v0.7 — Auto-update
- [ ] Self-update do core (updates.md): canais, assinatura com chave embutida, troca atômica (Linux/macOS/Windows), health check e rollback.
- [ ] Update automático de extensões por política; pausa em capability nova; rollback automático em falha de ativação.
- [ ] Migrações versionadas de formatos de estado.
- [ ] `atom store gc`.

Aceite: release N+1 com defeito proposital volta sozinho para N.

## v0.8 — Web
- [ ] `atom serve --ws` com token.
- [ ] `atom/ui-web` como **shell mínimo** (ADR-016): serve assets, ponte WS, loader de `kind="ui-mod"`, ponto raiz `ui.root`. Sem telas próprias.
- [ ] Contratos de UI como pacotes `kind=contract` com pontos de contribuição semânticos (`ui.menu@0`, `ui.views@0`, `ui.settings@0`, `ui.status@0`); renderer `single` por ponto via perfil; aninhamento; estado por mod.
- [ ] Mods oficiais mínimos: layout, menu superior, menu lateral, configurações em popup e em painel, chat (sobre `ai.agent@0`).

Aceite: (1) um `ui-mod` instalado adiciona uma view sem tocar em `ui-web` nem em outros mods; (2) trocar o renderer do menu (superior → lateral) e o das configurações (popup → painel lateral) mantém todos os itens de menu e seções/mods internos de configuração, sem alterar nenhum outro pacote.

## v0.9 — Portabilidade e ecossistema
- [ ] Builds e releases para todas as plataformas no CI; instaladores simples (script sh/ps1, Homebrew tap opcional).
- [ ] SDKs mínimos para extensões: Rust (`atom-sdk`, sobre `atom-protocol`), TypeScript, Python (handshake, call, streams, interceptors, eventos).
- [ ] Distributions `atom/code-starter` e `atom/cowork-starter`; `profile create --from`, `export/import`.
- [ ] Documentação de autor de extensão (tutorial: extensão, contrato, kind handler).

Aceite: um amigo, em Windows ou macOS, instala o Atom e cria um perfil `code-starter` funcional seguindo só a doc.

## v1.0 — Congelamento
- [ ] Congelar `atom/1`, manifesto `manifest = 1`, `store/v1`, formatos `profile = 1` / `lock = 1`.
- [ ] Política de depreciação documentada (suporte a `atom/1` por pelo menos um major após `atom/2`).
- [ ] Suite de conformidade do protocolo (testa qualquer extensão/SDK).
- [ ] Revisão de segurança (assinaturas, sockets, token WS, env filtrado).

Os contratos de IA continuam podendo evoluir (`@0` → `@1`) sem release do core.

---

## Pós-1.0 (fora de escopo agora)
Runtime WASM (wasmtime + Component Model; feature opcional do core ou extensão-runtime); sandbox de OS; servidor multi-tenant (auth, backends Postgres/S3 para `Storage`/`Secrets`, isolamento por tenant); loja dinâmica com busca/publicação; OpenTelemetry; resolver PubGrub se necessário; evoluir assinaturas para TUF.
