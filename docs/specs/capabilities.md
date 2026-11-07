# Spec: Capabilities

## 1. Modelo

- Pacote declara `capabilities` no manifesto.
- Na instalação (e em update que adiciona capabilities) o usuário aprova. Aprovações viram **grants** no perfil (`grants.toml`).
- O core verifica grants em toda operação via bus. Negado → `-32003 CapabilityDenied`.
- Extensão pode pedir mais em runtime via `atom.grants.request` (o usuário aprova pelo cliente UI ativo ou `atom grants approve`).

> Limite v1: extensões executáveis são processos do usuário; capabilities de `fs`, `net` e `exec` são **advisory** (não aplicadas), não sandbox. O core só consegue bloquear o que passa pelo bus. Sandbox real: pós-1.0 (WASM / sandbox de OS).
>
> `[C-UI-1]` (v1.1) A aprovação lista capabilities advisory separadas das aplicadas, com o texto: "Não é bloqueado pelo Atom: a extensão já pode fazer isso como seu usuário. Aprove só se confia no publisher." Desde a v1.1, `ui.serve` NÃO é advisory: é aplicada pelo bus (ADR-026). A única barreira real contra extensão maliciosa na v1 é a assinatura.

### 1.1 Autoridade ambiente (v1.1, ADR-019)
- `[C-AMB-1]` Extensão só tem a autoridade dos seus grants. Conectar-se ao socket do daemon não dá autoridade extra: peer descendente do daemon → `-32005 UntrustedPeer` (`protocol.md [P-CONN-2]`).
- `[C-AMB-2]` `grants.toml` só é alterado pelo core em resposta a cliente confiável. O core guarda o sha256 do arquivo que escreveu; arquivo diferente no boot ou em runtime → conteúdo externo **ignorado** (vale o último estado escrito pelo core, guardado em `grants.toml.core`), evento `atom.grants.tampered`, aviso em `atom status`. `atom grants reload` (cliente confiável) aceita o arquivo atual.
- `[C-AMB-3]` Mesmo para os campos de segurança de `profile.toml` (`sources`, `bindings`, `validation`, `update`): mudança externa só vale após `atom profile reload` de cliente confiável. Watch automático aplica apenas `[config.*]`.
- Risco residual: extensão pode ler arquivos do usuário (inclusive secrets em fallback) e, no macOS, escapar da árvore de processos com double-fork. Fecha só com sandbox (pós-1.0).

## 2. Formato

`<nome>[:<escopo>]`. Escopo aceita glob `*`.

| Capability | Significado | Enforcement |
|---|---|---|
| `service:<contrato@major>` | chamar um contrato | bus |
| `intercept:<contrato@major>` | interceptar um contrato | bus |
| `events.publish:<glob>` | publicar tópicos | bus |
| `events.subscribe:<glob>` | assinar tópicos (`atom.*` livres) | bus |
| `secrets:<nome>` / `secrets.write:<nome>` | ler/gravar secret | bus |
| `packages.manage` | instalar/remover/atualizar | bus |
| `fs.data` | diretório privado de dados | core cria/entrega |
| `fs.read:<path>` / `fs.write:<path>` | caminhos fora do privado (`workspace`, `pkg`, `home/...`, absoluto) | advisory |
| `net:<host[:porta]>` | rede | advisory |
| `exec:<comando>` | executar processos | advisory |
| `env:<NOME>` | receber variável de ambiente | core (filtra env) |
| `ui.serve` | anunciar HTTP do shell (`atom.ui.serve`) e obter sessão WS (`atom.ui.session`) | bus (v1.1, ADR-026) |

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
