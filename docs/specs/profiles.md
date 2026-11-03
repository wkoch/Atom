# Spec: Perfis, diretórios, storage e lockfile

## 1. Diretórios base (`ATOM_HOME`)

Override por env `ATOM_HOME` (tudo sob ele). Sem override:

| | Linux | macOS | Windows |
|---|---|---|---|
| config | `$XDG_CONFIG_HOME/atom` (`~/.config/atom`) | `~/Library/Application Support/atom` | `%APPDATA%\atom` |
| data | `$XDG_DATA_HOME/atom` (`~/.local/share/atom`) | `~/Library/Application Support/atom` | `%LOCALAPPDATA%\atom` |
| cache | `$XDG_CACHE_HOME/atom` | `~/Library/Caches/atom` | `%LOCALAPPDATA%\atom\cache` |
| runtime (sockets) | `$XDG_RUNTIME_DIR/atom` | `$TMPDIR/atom-<uid>` | named pipes |

## 2. Layout

```
<config>/
  atom.toml                    # config global: perfil padrão, canal de update, sources globais
  profiles/<perfil>/
    profile.toml               # bindings, config de pacotes, sources, política de update
    atom.lock                  # versões exatas instaladas
    atom.lock.prev/            # lockfiles anteriores (rollback), N=10
    grants.toml
<data>/
  store/<publisher>/<nome>/<versão>-<sha256[:16]>/   # pacotes extraídos, imutáveis (read-only)
  profiles/<perfil>/
    ext/<publisher>/<nome>/    # dados privados (atom.storage.dir)
    kv.db                      # KV (arquivo JSON por extensão na v0.x; SQLite opcional depois)
    journal/                   # journal do bus (opcional)
    secrets.enc                # fallback se keyring indisponível
    serve.token
  bin/
    atom-<versão>              # binários do core (self-update)
<cache>/
  downloads/                   # artefatos .tar.gz verificados
  index/<source>/              # cache do índice das lojas
```

Nome do perfil: `^[a-z0-9][a-z0-9-]{0,31}$`. Perfil `default` criado no primeiro uso.

## 3. `profile.toml`

```toml
profile = 1
description = "Desenvolvimento de apps"

[update]
core = "notify"          # herda global se omitido
extensions = "notify"    # auto | notify | pinned
pinned = ["atom/agent-loop-basic"]

[[sources]]
name = "oficial"
url = "https://atom-store.example.com/v1/"
keys = ["ed25519:BASE64..."]       # chaves raiz da loja
priority = 0                        # menor = preferida

[bindings]
"ai.agent@0" = "atom/agent-loop-basic"
"ai.memory@0" = "meu/memory-sqlite"

[config."atom/llm-openai"]
base_url = "https://api.openai.com/v1"
model = "gpt-x"

[validation]
contracts = true          # valida chamadas contra schemas de contrato
```

Config de cada pacote é validada contra o `config_schema` dele; inválida → pacote não ativa (erro claro). Mudanças em `profile.toml` são detectadas (watch ou `atom profile reload`) → evento `atom.config.changed`.

## 4. `atom.lock`

```toml
lock = 1
core = "0.5.0"                      # versão do core que gerou

[[package]]
name = "atom/skill-loader"
version = "0.1.2"
source = "oficial"                  # ou "dev:/caminho"
platform = "linux-amd64"            # ou "any"
sha256 = "..."
requested = "^0.1"                  # vazio se dependência transitiva
dependencies = ["atom/contract-ai-tools@0.1.0"]
```

Lockfile é a fonte da verdade do que roda. Toda alteração: grava `atom.lock.prev/<timestamp>.lock`, depois escreve atômico.

## 5. Resolver

- Uma versão por pacote por perfil.
- v0.x: guloso — para cada requisito, maior versão compatível com `engine` do core atual e plataforma; conflito → erro listando a cadeia. Upgrade para PubGrub só se conflitos reais aparecerem.
- Plataforma: preferir `<os>-<arch>`, senão `any`.
- `yanked` só é escolhido se já estiver no lockfile.

## 6. Interfaces internas (para multi-tenant futuro)

O core acessa estado somente via interfaces `Storage` (KV + blobs por `(tenant, profile, extension)`) e `Secrets` (`(tenant, profile, name)`). Backend v1: filesystem + keyring do OS (serviço `atom`, conta `<profile>/<name>`), com fallback `secrets.enc` (chave derivada de senha ou arquivo de chave 0600). Nenhum código fora dessas interfaces lê/escreve em `<data>/profiles`.

## 7. Export / import

- `atom profile export <p> [--with-data]` → `.tar.gz` com `profile.toml`, `atom.lock`, `grants.toml` (e dados opcionais). **Nunca** inclui secrets.
- `atom profile import <arquivo> [--name]` → reinstala pacotes do lockfile (verificando hashes) e pede aprovação de grants novamente.
- `atom profile create <p> --from <publisher/distribution>` aplica um pacote `kind=distribution`.
