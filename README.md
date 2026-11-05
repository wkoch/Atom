# Atom

Microkernel minimalista para trabalho com IA (Code e Cowork). O core é um binário headless que **não sabe nada de IA**: ele só instala, executa, conecta, protege e atualiza extensões. LLMs, loops de agente, memória, tools, MCP, skills, CLI, web UI, telas — tudo é extensão.

## Documentação

| Documento | Conteúdo |
|---|---|
| [AGENTS.md](AGENTS.md) | Instruções para a IA/dev que vai implementar |
| [docs/architecture.md](docs/architecture.md) | Visão geral, princípios, componentes do kernel |
| [docs/decisions.md](docs/decisions.md) | Decisões de arquitetura (ADRs) e porquês |
| [docs/roadmap.md](docs/roadmap.md) | Versões 0.1 → 1.0 com critérios de aceite |
| [docs/glossary.md](docs/glossary.md) | Termos |
| [docs/specs/manifest.md](docs/specs/manifest.md) | Formato do pacote e `atom.toml` |
| [docs/specs/protocol.md](docs/specs/protocol.md) | Protocolo `atom/1` (core ↔ extensões ↔ clientes) |
| [docs/specs/capabilities.md](docs/specs/capabilities.md) | Permissões |
| [docs/specs/profiles.md](docs/specs/profiles.md) | Perfis, layout de diretórios, storage, lockfile |
| [docs/specs/store.md](docs/specs/store.md) | Protocolo da loja `store/v1` e assinatura |
| [docs/specs/updates.md](docs/specs/updates.md) | Auto-update do core e das extensões |
| [docs/specs/cli.md](docs/specs/cli.md) | CLI bootstrap embutida |
| [atom-extensions](https://github.com/wkoch/atom-extensions) | Extensões, contratos, mods e distributions oficiais |
| [atom-store](https://github.com/wkoch/atom-store) | Loja `store/v1` oficial |

## Estado

Projeto em especificação. Nenhum código ainda. Comece por `AGENTS.md` e `docs/roadmap.md` (v0.1).
