# Arquitetura

## 1. Objetivos

1. Core mínimo e estável; toda evolução via extensões.
2. Suportar categorias de extensão que ainda não existem, sem mudar o core.
3. Loops, harness, memória, UI — tudo substituível.
4. Perfis isolados, cada um um "Atom vazio" personalizável.
5. Rodar hoje local no Linux; amanhã Windows/macOS, distribuição para amigos e servidor multi-tenant **sem reescrita**.
6. Auto-atualização do core e das extensões a partir de lojas (estáticas ou servidor).

## 2. Não-objetivos do core

Chat, prompts, modelos, tools, sessões de agente, memória, MCP, skills, UI, temas, telemetria de produto. Tudo isso é extensão.

## 3. Visão geral

```
                 +-------------------- atom (binário) --------------------+
 clientes        |                                                        |
 (ui-cli,  <---->|  transport (unix socket / named pipe / websocket)      |
  ui-web,        |        |                                               |
  scripts)       |        v                                               |
                 |  BUS (JSON-RPC 2.0) -- interceptors -- eventos -- streams
                 |    |         |            |             |              |
                 |  registry  caps       storage/secrets  host ----------+---> extensões (processos,
                 |  (serviços,           (por perfil)     (spawn,        |      stdio NDJSON)
                 |   bindings,                             supervisão)   |
                 |   kind handlers)                                      |
                 |  pkg (store CAS, lockfile, resolver) <-- store client  <---- lojas (store/v1)
                 |  update (self-update)                                 |
                 |  cli bootstrap                                        |
                 +-------------------------------------------------------+
```

## 4. Componentes do kernel

| Componente | Responsabilidade |
|---|---|
| **bus** | Roteia requests/responses, notificações, eventos pub/sub, streams e cancelamento. Aplica a cadeia de interceptors. Injeta contexto autoritativo. |
| **host** | Inicia extensões como processos, faz handshake `atom/1`, ativação lazy, supervisão (restart com backoff), shutdown gracioso, hot reload. |
| **registry** | Mantém serviços (contrato@major → providers), bindings do perfil, kind handlers, interceptors e assinaturas de eventos declarados. |
| **caps** | Capabilities declaradas no manifesto, concedidas pelo usuário, verificadas em todo acesso via bus. |
| **profile / storage / secrets** | Isolamento por perfil. Interfaces com backend filesystem/keyring hoje; trocáveis no futuro (Postgres/S3/Vault). |
| **pkg** | Store local endereçada por conteúdo, instalação, lockfile por perfil, resolver semver. |
| **store** | Cliente do protocolo `store/v1`, múltiplas sources, verificação de assinatura. |
| **update** | Self-update do binário com canais e rollback. |
| **cli** | Comandos mínimos para operar sem nenhuma extensão instalada. |

## 5. O modelo universal de extensão

### 5.1 Tudo é pacote
Um pacote tem um manifesto `atom.toml` com `kind` livre (o core **não interpreta** `kind`). Ver `specs/manifest.md`.

### 5.2 Duas formas
- **Executável**: tem `entrypoint`; o host o roda como processo.
- **Declarativo**: só dados (markdown, JSON, assets). Precisa de um **kind handler**.

### 5.3 Kind handlers
Uma extensão executável declara `handles = ["skill"]`. Ao ativar um pacote `kind="skill"`, o core chama `kind.load` no handler, passando o manifesto e o diretório (read-only) do pacote. O handler faz o que quiser (registrar tools, contexto, telas...).

> Nova categoria de extensão = nova extensão handler. O core nunca muda.

Pacote sem handler disponível fica instalado com estado `dormant` e aviso.

### 5.4 Contratos
Contratos (interfaces) são pacotes `kind="contract"` com JSON Schemas de métodos e eventos. Identificador: `nome@major` (ex.: `ai.llm@0`). O core valida chamadas contra o schema quando o contrato está instalado (validação pode ser desligada por perfil para performance). Múltiplos contratos concorrentes podem coexistir.

### 5.5 Providers e bindings
Várias extensões podem `provides` o mesmo contrato. O perfil define `bindings` (contrato → extensão). Chamada sem `target` vai para o provider do binding; com `target` vai para o provider explícito. Contratos "multi" (ex.: tools) são chamados por `target` ou por listagem.

### 5.6 Interceptors (hooks)
Qualquer chamada roteada pelo bus pode ser interceptada nas fases `before`, `after`, `around` por padrão `serviço/método` (glob), com `priority`. Interceptors podem continuar, modificar, rejeitar ou responder no lugar. Exige capability `intercept:<serviço>`.

### 5.7 Eventos
Pub/sub assíncrono por tópico (glob). O core publica eventos de ciclo de vida (`atom.*`). Extensões publicam seus próprios tópicos.

## 6. Clientes / UIs

- `atom serve` roda o core como daemon e escuta em socket local (e WS opcional).
- Clientes falam o **mesmo** protocolo `atom/1` que as extensões, com papel `client`.
- UIs são extensões que também atuam como clientes (ex.: `ui-cli` é executado pelo host e conversa com o bus; `ui-web` serve assets HTTP e faz ponte WS).
- UI web = **shell mínimo + mods** (ADR-016). `ui-web` só serve assets, faz a ponte WS e carrega pacotes `kind="ui-mod"`; menus, painéis, editor, chat, status etc. são mods, de um botão a uma tela inteira.
- Contribuições são **dados primeiro** (ADR-025): descritor serializável validado pelo contrato; componentes são opcionais e por superfície (web, TUI, Tauri). Qualquer frontend renderiza as mesmas contribuições.
- Mods contribuem em **pontos de contribuição semânticos** (`ui.menu.items`, `ui.settings.sections`, ...) definidos por pacotes `kind="contract"`, não pelo shell nem pelo core. Quem **contribui** e quem **renderiza** um ponto são mods diferentes que só conhecem o contrato: trocar o renderer (menu superior → lateral, popup → painel) preserva as contribuições, inclusive as aninhadas. O core não sabe nada disso.

## 7. Contexto multi-tenant desde o dia 1

Todo request carrega `ctx = {tenant, user, profile, session, trace}`, preenchido pelo core (extensões não podem forjar `tenant/user/profile`). Storage, secrets, bindings e caps são sempre resolvidos por `(tenant, profile)`. Localmente: `tenant=local`, `user=local`. O servidor futuro troca backends de storage, adiciona autenticação no transporte e runtime isolado (WASM/containers), sem mudar protocolo nem extensões.

## 8. Isolamento e segurança

- v1: extensões são processos do mesmo usuário do OS. Capabilities são aplicadas **no bus**; acesso direto ao OS pela extensão não é bloqueado (capabilities `fs/net/exec` são *advisory*). Logo: **assinatura obrigatória** (exceto pacotes locais em modo dev) e aprovação explícita de capabilities.
- Sem autoridade ambiente (v1.1, ADR-019): a autoridade vem da identidade da conexão. Processo descendente do daemon não vira cliente confiável no socket; alteração externa de `grants.toml` e de campos de segurança do perfil é ignorada até reload por cliente confiável.
- Cadeia de suprimentos (v1.1, ADR-024): `store/v2` e canal de releases sobre TUF (expiração, limiar, delegação por publisher).
- Pós-1.0: runtime WASM (sandbox real; wasmtime + Component Model/WIT, embutido no core como feature opcional ou como extensão-runtime) e sandbox de OS (Landlock/bwrap, sandbox-exec, AppContainer). Pré-requisito para multi-tenant.

## 9. Runtimes de linguagem

O core só executa binários nativos. Extensões em Python/Node/etc. declaram `runtime = "<pacote-runtime>"`; o pacote runtime (ex.: `atom/runtime-python`, kind `runtime`) fornece o interpretador e o comando de execução. Preferir extensões oficiais em binário único (Go/Rust).

## 10. Recursos e prazos (v1.1)

- Todo buffer do core é limitado (ADR-020, protocol §13); consumidor lento gera backpressure, nunca memória ilimitada.
- `$ctx.deadline` fim a fim (ADR-021); cancelamento propagado salto a salto.
- Semântica de `Storage`/`Secrets` é da spec, não do backend (ADR-023): trocar FS por Postgres/S3 exige passar a suíte de conformidade, e o comportamento visto pelas extensões não muda.

## 11. Verificação (v1.1)

Specs têm cláusulas com id ligadas a testes; `atom/1` tem JSON Schemas próprios; máquinas de estado têm modelos; "estar certo" é decidido por oráculos externos (ver `specs/conformance.md`, ADR-018).

## 12. Observabilidade

- `tracing` (saída JSON) com `trace` propagado em todas as mensagens.
- stderr das extensões capturado e anexado ao log com o id da extensão.
- Journal opcional do bus por sessão (append-only, JSONL) para debug/replay; extensões podem assiná-lo como evento.
- Eventos com `epoch`/`offset` e retomada após reconexão (ADR-022).
