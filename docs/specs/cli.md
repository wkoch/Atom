# Spec: CLI bootstrap

A única interface embutida. Objetivo: operar o Atom sem nenhuma extensão instalada. Sem cores obrigatórias, saída texto simples; `--json` em todos os comandos para scripts/UIs.

Flags globais: `--profile <p>` (ou env `ATOM_PROFILE`), `--json`, `--log-level`, `--home <dir>`.

O CLI conecta no daemon do perfil se estiver rodando; senão executa em processo (comandos de pacote/perfil) ou sobe o daemon (comandos que precisam de extensões).

## Comandos

| Comando | Descrição |
|---|---|
| `atom version` | versão do core, protocolo, plataforma |
| `atom serve [--ws <addr>] [--foreground]` | roda o daemon do perfil |
| `atom stop` | para o daemon |
| `atom status` | extensões e estados, bindings, sources |
| `atom profile list\|create <p> [--from <dist>]\|delete <p>\|use <p>\|export\|import\|reload` | perfis |
| `atom ext search <q>` | busca nas sources (catalog/search) |
| `atom ext info <pkg>` | metadados, versões, capabilities |
| `atom ext install <pkg>[@faixa] \| --dev <pasta> \| <arquivo.tar.gz>` | instala (pede grants) |
| `atom ext remove <pkg>` | remove do perfil |
| `atom ext update [--check] [pkg...]` | atualiza |
| `atom ext rollback [--to <ts>]` | restaura lock anterior |
| `atom ext list` | instalados no perfil |
| `atom ext restart <pkg>` | reinicia extensão |
| `atom source add <nome> <url> --key <k> \| remove \| list` | lojas |
| `atom source add <nome> <url-v2> --root <sha256>` / `atom source migrate <nome> --root <sha256>` | (v1.1) lojas `store/v2` (TUF) |
| `atom bind <contrato@major> <pkg>` / `atom unbind` | bindings |
| `atom config get\|set\|edit <pkg> [chave] [valor]` | config de pacote |
| `atom grants list\|approve\|revoke <pkg> [cap]` | permissões |
| `atom grants reload` | (v1.1) aceita alteração externa de `grants.toml` (só de cliente confiável) |
| `atom secret set\|delete\|list <nome>` | secrets (valor via stdin) |
| `atom call <contrato@major> <método> [json\|-] [--target <pkg>] [--stream]` | chamada direta no bus (debug e scripting) |
| `atom events tail [glob] [--from <epoch>:<offset>]` | stream de eventos (`--from`: retomada, v1.1) |
| `atom logs [pkg] [-f]` | logs |
| `atom run <comando> [args...]` | executa comando contribuído por extensão (ativação `onCommand`) |
| `atom pack [<pasta>] --key <arquivo>` | empacota e assina para todas as plataformas declaradas |
| `atom publish [--github <repo>] [--source <nome>]` | publica |
| `atom keygen` | gera par ed25519 de publisher |
| `atom update [--check] [--channel <c>]` | self-update |
| `atom store gc` | limpa store |

## `atom run` e UIs

Extensões contribuem comandos via `[contributes.commands]` no manifesto:
```toml
[contributes.commands]
chat = { description = "Chat no terminal", interactive = true }
```
Isso implica ativação `onCommand:chat`. `atom run chat` sobe o daemon se necessário, inicia a extensão e, se `interactive = true`, conecta stdin/stdout/stderr do terminal à extensão por um canal separado (a extensão recebe `command.run` `{name, args, tty: true, io: "<caminho de socket/pipe>"}` e conecta nele para o TTY). Assim `ui-cli` é uma extensão que você abre com `atom run chat`.

Atalho: se `<config>/atom.toml` tiver `default_command = "chat"`, `atom` sem argumentos executa `atom run chat`; sem isso, imprime ajuda.
