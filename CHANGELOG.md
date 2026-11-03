# Changelog

Formato: [Keep a Changelog](https://keepachangelog.com/pt-BR/1.1.0/), versionamento SemVer.

## [Não lançado]
### Adicionado
- ADR-016: UI web como shell mínimo + mods componíveis por pontos de contribuição semânticos; architecture, contracts, roadmap v0.8 e glossário ajustados.
- ADR-015: core em Rust (substitui ADR-002, Go); AGENTS.md e roadmap atualizados.
- Especificação inicial: arquitetura, ADRs, roadmap, specs de manifesto, protocolo `atom/1`, capabilities, perfis, loja `store/v1`, atualizações, CLI e contratos iniciais.
- v0.1 — Kernel: workspace (`atom-protocol`, `atom-core`, `atom`), CI Linux/macOS/Windows (fmt + clippy `-D warnings` + test); perfis (`ATOM_HOME`, create/list/use/delete, `profile.toml`); pacotes (`--dev`, `.tar.gz`, `atom.lock` atômico + `.prev`); transporte NDJSON + socket unix (0600) com daemon (`serve`/`stop`/`status`, `call` via daemon ou em processo); bus JSON-RPC com `$ctx` autoritativo; host (spawn, env filtrado, handshake, `onStartup`/`onService`); registry (target → binding → único); KV por extensão + `atom.storage.*`, `atom.config.get`, `atom.log` no host; CLI completa da v0.1; `testdata/echo` + teste de integração do aceite (isolamento por perfil).
- v0.2 — Dogfood: 4 contratos (`ai.llm/tools/agent@0`, `ui.prompt@0`) como pacotes `kind=contract`; extensões `llm-openai-compat`, `llm-anthropic`, `tools-fs`, `tools-shell`, `agent-loop-basic`, `ui-cli` (`atom run chat`); core com streams (`atom.stream.*`, cancelamento fim a fim, `call --stream`), `atom.secrets.*` (keyring + fallback 0600) e CLI `secret`; `contributes.commands` + `atom run` (`onCommand`, TTY via socket); `atom.registry.list` para extensões; `test/fake-llm` + testes `stream_cancel` (cancelamento `-32800`); docs `dogfood-v0.2.md` e `contract-adjustments-0.md`.
