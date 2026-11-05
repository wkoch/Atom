# Spec: Pacote e manifesto (`atom.toml`)

Status: estável desde v1.0 (ver `docs/deprecation.md`).

## 1. Identidade

- `name`: `<publisher>/<nome>`, regex `^[a-z0-9][a-z0-9-]{0,38}/[a-z0-9][a-z0-9-]{0,63}$`.
- `version`: SemVer 2.0.
- Id completo: `publisher/nome@versão`.

## 2. Estrutura do arquivo de pacote

`<publisher>-<nome>-<versão>-<os>-<arch>.tar.gz` (ou `-any` para pacotes sem binário). Conteúdo na raiz:

```
atom.toml          # obrigatório
README.md          # recomendado
bin/               # executáveis (pacotes executáveis)
schemas/           # JSON Schemas (contratos, config)
...                # qualquer dado (skills, assets, etc.)
```

## 3. Campos do `atom.toml`

```toml
manifest = 1                         # versão do formato do manifesto (obrigatório)
name = "atom/skill-loader"           # obrigatório
version = "0.1.0"                    # obrigatório
kind = "extension"                   # obrigatório; string livre, core não interpreta
description = "Carrega pacotes kind=skill"
license = "MIT"
homepage = "https://..."
engine = ">=0.1.0, <2.0.0"           # faixa de versão do core atom (obrigatório)
protocol = "atom/1"                  # protocolo do kernel (obrigatório se executável)

# Executável (omitir para pacotes declarativos)
entrypoint = "bin/skill-loader"      # relativo à raiz; no Windows o core tenta também ".exe"
runtime = ""                         # opcional: pacote runtime (ex.: "atom/runtime-python")
args = []                            # argumentos extras
activation = ["onStartup"]           # ver §4

# Dependências de pacotes
[requires.packages]
"atom/contract-ai-tools" = "^0.1"

# Contratos que este pacote consome (garantia de que há provider)
[requires.contracts]
"ai.tools@0" = "*"

# Contratos implementados
[[provides]]
contract = "ai.tools@0"              # contrato@major
version = "0.3.0"                    # versão do contrato implementada

# Kinds que esta extensão carrega
handles = ["skill"]

# Interceptors declarados (também podem ser registrados em runtime)
[[intercepts]]
match = "ai.tools@0/execute"         # glob sobre "<contrato>/<método>"
phase = "before"                     # before | after | around
priority = 100                       # menor executa primeiro

# Assinaturas de eventos
subscribes = ["atom.package.*"]

# Capabilities requeridas (ver capabilities.md)
capabilities = [
  "fs.read:pkg",
  "service:ai.tools@0",
]

# Schema de configuração do pacote (JSON Schema). Config fica no perfil.
config_schema = "schemas/config.json"

# Dados livres para kind handlers / outras extensões. O core repassa sem interpretar.
[contributes]
# ex.: para kind="skill": { file = "SKILL.md" }
```

### Regras
- Campos desconhecidos no topo: **erro** se `manifest` maior que o suportado; **aviso** caso contrário (compatibilidade futura).
- `contributes` é opaco para o core.
- Pacote declarativo (sem `entrypoint`) não pode ter `provides`, `handles`, `intercepts`, `subscribes`.
- `requires.contracts` é satisfeito se algum pacote instalado no perfil `provides` aquele contrato@major.

## 4. Ativação

| Valor | Quando o core inicia a extensão |
|---|---|
| `onStartup` | no boot do perfil |
| `onService:<contrato@major>` | na primeira chamada ao contrato |
| `onEvent:<tópico-glob>` | no primeiro evento casando |
| `onKind:<kind>` | quando existe pacote do kind a carregar |
| `onCommand:<nome>` | quando `atom run <nome>` é chamado (ver cli.md) |

Sem `activation` → `onStartup`. Handlers com `handles` recebem implicitamente `onKind:<kind>`. Interceptors declarados recebem implicitamente ativação na primeira chamada que casar.

## 5. Kinds convencionais (não interpretados pelo core)

| kind | Significado | Handler |
|---|---|---|
| `extension` | extensão genérica executável | — |
| `contract` | JSON Schemas de contrato em `schemas/` | core lê só para validação opcional |
| `runtime` | fornece interpretador | core usa para `runtime=` (ver §6) |
| `distribution` | template de perfil | core (cli `profile create --from`) |
| `skill`, `mcp`, `ui-mod`, ... | definidos por extensões | extensão com `handles` |

`contract`, `runtime` e `distribution` são as **únicas** exceções que o core entende, porque são necessárias para o bootstrap. Todo o resto é de extensões.

### 5.1 `kind = "contract"`
```toml
[contributes.contract]
id = "ai.tools@0"
version = "0.3.0"
schema = "schemas/ai.tools.json"     # formato: atom-extensions/docs/specs/contracts.md (ADR-017)
```

### 5.2 `kind = "runtime"`
```toml
[contributes.runtime]
# comando para rodar uma extensão; {entry} = caminho absoluto do entrypoint, {pkg} = raiz do pacote da extensão
command = ["bin/python3", "{entry}"]
```

### 5.3 `kind = "distribution"`
```toml
[contributes.distribution]
packages = { "atom/ui-cli" = "^0.2", "atom/agent-loop-basic" = "^0.2" }
bindings = { "ai.agent@0" = "atom/agent-loop-basic" }
sources = [{ name = "oficial", url = "https://...", key = "ed25519:..." }]
[contributes.distribution.config."atom/llm-openai"]
model = "gpt-x"
```
