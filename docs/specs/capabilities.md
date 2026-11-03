# Spec: Capabilities

## 1. Modelo

- Pacote declara `capabilities` no manifesto.
- Na instalação (e em update que adiciona capabilities) o usuário aprova. Aprovações viram **grants** no perfil (`grants.toml`).
- O core verifica grants em toda operação via bus. Negado → `-32003 CapabilityDenied`.
- Extensão pode pedir mais em runtime via `atom.grants.request` (o usuário aprova pelo cliente UI ativo ou `atom grants approve`).

> Limite v1: extensões executáveis são processos do usuário; capabilities de `fs`, `net`, `exec` são **declarativas/auditáveis**, não sandbox. O core só consegue bloquear o que passa pelo bus. Sandbox real: pós-1.0 (WASM / sandbox de OS). A UI de aprovação deve deixar isso claro.

## 2. Formato

`<nome>[:<escopo>]`. Escopo aceita glob `*`.

| Capability | Significado | Enforcement v1 |
|---|---|---|
| `service:<contrato@major>` | chamar um contrato | bus |
| `intercept:<contrato@major>` | interceptar um contrato | bus |
| `events.publish:<glob>` | publicar tópicos | bus |
| `events.subscribe:<glob>` | assinar tópicos (`atom.*` livres) | bus |
| `secrets:<nome>` / `secrets.write:<nome>` | ler/gravar secret | bus |
| `packages.manage` | instalar/remover/atualizar | bus |
| `fs.data` | diretório privado de dados | core cria/entrega |
| `fs.read:<path>` / `fs.write:<path>` | caminhos fora do privado (`workspace`, `pkg`, `home/...`, absoluto) | declarativo |
| `net:<host[:porta]>` | rede | declarativo |
| `exec:<comando>` | executar processos | declarativo |
| `env:<NOME>` | receber variável de ambiente | core (filtra env) |
| `ui.serve` | abrir porta HTTP local | declarativo |

Alias de caminhos: `pkg` (raiz do pacote), `data` (privado), `workspace` (diretório de trabalho da sessão, definido por contrato de IA, não pelo core).

## 3. Grants

`profiles/<p>/grants.toml`:
```toml
["atom/skill-loader"]
granted = ["fs.read:pkg", "service:ai.tools@0"]
denied = []
approved_at = "2025-01-01T00:00:00Z"
```
Grants são por pacote (todas as versões); update com capability nova exige nova aprovação antes de ativar a versão nova.

## 4. Modo dev

`atom ext install --dev <pasta>`: pacote local não assinado, grants pedidos interativamente, marcado `dev` em `atom.lock`.
