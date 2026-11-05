# Spec: CLI bootstrap

A única interface embutida. Objetivo: operar o Atom sem nenhuma extensão instalada. Sem cores obrigatórias, saída texto simples; `--json` em todos os comandos para scripts/UIs.

Flags globais: `--profile/-p <p>` (ou env `ATOM_PROFILE`), `--log-level`, `--home <dir>` (ou env `ATOM_HOME`). `--json` em todos os comandos é objetivo da spec e **não está implementado** (2026-11-05).

O CLI conecta no daemon do perfil se estiver rodando; senão executa em processo (comandos de pacote/perfil) ou sobe o daemon (comandos que precisam de extensões).

## Comandos

| Comando | Descrição |
|---|---|
| `atom version` | versão do core, protocolo, plataforma |
| `atom serve [--ws <addr>] [--foreground]` | roda o daemon do perfil |
| `atom stop` | para o daemon |
| `atom status` | extensões e estados, bindings, sources (+ aviso `tampered` v1.1: `grants.toml`/`profile.toml` externos ignorados) |
| `atom profile list\|create <p> [--from <dist>]\|delete <p>\|use <p>\|export <p> <arquivo>\|import <arquivo> [--name <n>]\|reload` | perfis (`reload`: aceita campos de segurança externos; só cliente confiável, peer recebe `-32005`) |
| `atom ext search <q> [--kind <k>]` | busca nas sources (catalog/search) |
| `atom ext info <pkg>` | metadados, versões, capabilities (advisory separadas: aviso de não-enforcement `[C-UI-1]`) |
| `atom ext install <pkg>[@faixa] \| --dev <pasta> \| <arquivo.tar.gz> [--source <nome>] [--refresh]` | instala (pede grants; advisory com aviso de não-enforcement `[C-UI-1]`) |
| `atom ext remove <pkg>` | remove do perfil |
| `atom ext update [pkg]` | atualiza para a maior versão do mesmo requisito (`--check` de `updates.md §2`: não implementado; use `atom update --check`) |
| `atom ext rollback` | restaura o `atom.lock.prev` mais recente (`--to <ts>` de `updates.md §2`: não implementado) |
| `atom ext list` | instalados no perfil |
| `atom ext restart <pkg>` | reinicia extensão |
| `atom source add <nome> <url> [--keys <k>] [--priority <n>] [--root <sha256>] \| remove \| list` | lojas (`--root`: loja `store/v2` (TUF) — baixa `metadata/1.root.json`, confere o sha256, grava trust em `<cache>/tuf/<source>/`) |
| `atom source migrate <nome> --root <sha256>` | (v1.1) troca a URL p/ `/v2/` mantendo nome/prioridade (lock inalterado); sources v1 exibem aviso em `list`/`status` |
| `atom bind <contrato@major> <pkg>` / `atom unbind <contrato@major>` | bindings (também pontos de UI, ex. `ui.menu.items`) |
| `atom config get <pkg> [chave]` / `atom config set <pkg> <chave> <valor>` | config de pacote (`edit`: não implementado) |
| `atom grants list\|pending` / `atom grants approve\|deny <pkg> <cap>...` | permissões (advisory com aviso de não-enforcement `[C-UI-1]`) |
| `atom grants reload` | (v1.1) aceita alteração externa de `grants.toml` (só de cliente confiável; peer não-confiável recebe `-32005`) |
| `atom secret set\|delete\|list <nome>` | secrets (valor via stdin) |
| `atom call <contrato@major> <método> [json\|-] [--target <pkg>] [--stream]` | chamada direta no bus (debug e scripting) |
| `atom events tail [glob] [--from <epoch>:<offset>]` | (v1.1) cauda de eventos retidos (`--from`: retomada; `epoch` divergente ou cursor fora do anel imprime `gap` explícito) |
| `atom logs [pkg]` | logs |
| `atom run <comando> [args...]` | executa comando contribuído por extensão (ativação `onCommand`) |
| `atom ext pack <pasta> --key <arquivo> [--platform <p>] [--out-dir <d>]` | empacota e assina uma plataforma (padrão: a atual se executável; `any` se declarativo) → `.tar.gz` + snippet `.index.json` |
| `atom ext publish <snippets...> --store-dir <v1> --store-key <arquivo> [--artifacts-dir <d>] [--reindex] [--check-only] [--github]` | publica no layout `store/v1` (mescla índice, assina índices e catalog). `store/v2`: sem comando ainda (`store-v2.md §8–9`) |
| `atom ext keygen [--out <arquivo>]` | gera par ed25519 (publisher ou loja; privada 0600, imprime `ed25519:BASE64`) |
| `atom update [--check] [--channel <c>] [--url <base>] [--key <ed25519:…>]` | self-update (`--url`/`--key`: override de base e chave de release) |
| `atom store gc [--dry-run]` | limpa store |

Extensões contribuem comandos via `[contributes.commands]` no manifesto:
```toml
[contributes.commands]
chat = { description = "Chat no terminal", interactive = true }
```
Isso implica ativação `onCommand:chat`. `atom run chat` sobe o daemon se necessário, inicia a extensão e, se `interactive = true`, conecta stdin/stdout/stderr do terminal à extensão por um canal separado (a extensão recebe `command.run` `{name, args, tty: true, io: "<caminho de socket/pipe>"}` e conecta nele para o TTY). Assim `ui-cli` é uma extensão que você abre com `atom run chat`.

Atalho: se `<config>/atom.toml` tiver `default_command = "chat"`, `atom` sem argumentos executa `atom run chat`; sem isso, imprime ajuda.
