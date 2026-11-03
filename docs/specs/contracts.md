# Spec: Contratos (formato) e contratos iniciais de IA

Contratos **não fazem parte do core**. São pacotes `kind="contract"` publicados como qualquer outro. Este documento define (1) o formato do schema de contrato e (2) um ponto de partida para os contratos de IA, todos `@0` (instáveis, podem quebrar até serem validados por uso — ADR-014).

## 1. Formato do schema de contrato

Arquivo JSON referenciado em `[contributes.contract].schema`:

```json
{
  "contract": "ai.llm@0",
  "version": "0.1.0",
  "description": "Provedor de modelo de linguagem",
  "multi": false,
  "methods": {
    "complete": {
      "params": { "$ref": "#/$defs/CompleteParams" },
      "result": { "$ref": "#/$defs/CompleteResult" },
      "stream": { "$ref": "#/$defs/Chunk" }
    }
  },
  "events": {
    "ai.llm.usage": { "$ref": "#/$defs/Usage" }
  },
  "$defs": { }
}
```
- JSON Schema draft 2020-12 para `params`, `result`, `stream` (tipo de cada chunk, se o método suporta stream) e eventos.
- `multi: true` = contrato pensado para muitos providers simultâneos (chamados por `target` ou listados via `atom.registry.list`), sem binding único.
- Regras de evolução dentro do mesmo major: só adicionar métodos e campos opcionais. Quebra = novo major (`@1`), que pode coexistir.
- Validação (se `validation.contracts = true` no perfil): core valida `params` antes de `service.invoke` e `result` antes de devolver; falha → `-32013`.

## 2. Contratos iniciais de IA (rascunho, para v0.2+)

Mínimos para dogfooding. Defina-os como pacotes em `extensions/contracts/`. Descrições abaixo são de intenção; o schema exato é decidido ao implementar a primeira extensão de cada um.

### `ai.llm@0` (single, por binding)
- `complete({model?, messages[], tools?[], params?})` → `{message, usage, stop_reason}`; stream de chunks `{delta_text?, delta_tool_call?, usage?}`.
- `models()` → `[{id, context, capabilities}]`.
- `messages[]`: `{role: system|user|assistant|tool, content: [parts]}`; parts: `text`, `image`, `tool_call`, `tool_result`. Formato neutro; cada provider traduz.

### `ai.tools@0` (multi)
- `list()` → `[{name, description, input_schema}]`.
- `execute({name, input})` → `{output: [parts], is_error}`; stream opcional para progresso.
- Um "registry de tools" agregador é **uma extensão** (provider especial que lista todos os providers de `ai.tools@0` e expõe a união), não o core.

### `ai.agent@0` (single, por binding) — o loop/harness
- `run({session, input, options?})` → stream de eventos do agente `{type: text|tool_call|tool_result|status|done, ...}`.
- `sessions.list/get/delete`.
- O loop chama `ai.llm@0`, `ai.tools@0`, `ai.context@0`, `ai.memory@0` pelo bus — por isso tudo é interceptável e trocável.

### `ai.context@0` (multi)
- `collect({session, input})` → `{system?: [parts], messages?: []}`. Providers: skills, regras do projeto, arquivos. O agente agrega e ordena.

### `ai.memory@0` (single)
- `append({session, items[]})`, `query({session?, text, limit})` → `[items]`, `forget({ids})`.

### `ui.prompt@0` (single, normalmente fornecido por cliente UI conectado)
- `confirm({title, detail})` → `{ok}`; `ask({title, schema})` → `{value}`; `notify({level, text})`. Usado para aprovações (tools perigosas, grants).

### Contratos de UI web / mods (v0.8, regras em ADR-016)
Os schemas detalhados são definidos na v0.8. Regras fixas desde já:
- `atom/ui-web` é só um shell (assets, ponte WS, loader de `ui-mod`, ponto raiz `ui.root`); não contém telas.
- Pontos de contribuição são **semânticos** e vivem em pacotes `kind="contract"` próprios (ex.: `ui.menu@0` → ponto `ui.menu.items`; `ui.settings@0` → `ui.settings.sections`; `ui.views@0`; `ui.status@0`). Proibido nomear por posição/apresentação (`topbar`, `sidebar`, `popup`).
- Cada ponto tem: contribuições `multi` (mods de conteúdo declaram itens) e **um renderer** `single` escolhido no perfil (mods de layout). Contribuidor e renderer só conhecem o contrato.
- Qualquer mod pode declarar pontos próprios (aninhamento) com as mesmas regras.
- Estado de UI é chaveado pelo mod/contribuição, nunca pelo container.
- Exemplo de aceite: trocar o renderer de `ui.menu.items` (menu superior → menu lateral) ou de `ui.settings` (popup → painel lateral) mantém todos os itens e mods internos sem reinstalar ou alterar nada além do renderer.

## 3. Kinds declarativos iniciais (handlers são extensões)

| kind | Handler | Conteúdo (`contributes`) |
|---|---|---|
| `skill` | `atom/skill-loader` | `{file = "SKILL.md", scripts = "scripts/", runtime?}`; registra em `ai.context@0` (descrição) e `ai.tools@0` (se tiver scripts) |
| `mcp` | `atom/mcp-bridge` | `{transport = "stdio|http", command?, args?, url?, env_secrets?}`; sobe o servidor MCP e expõe suas tools como `ai.tools@0` (um target por servidor) |
| `prompt` | extensão de UI/agente | templates |
| `ui-mod` | `atom/ui-web` (shell) | assets + contribuições em pontos semânticos / renderer de pontos (ADR-016) |

Comportamentos de MCP que precisam de hooks = pacote `mcp` + extensão executável com `intercepts` (ou um pacote executável que faça ambos).
