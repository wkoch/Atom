# Spec: Protocolo `atom/1`

Status: estável desde v1.0 (ver `docs/deprecation.md`). Adições compatíveis da v1.1 marcadas **(v1.1)**; ids de cláusula `[P-...]` conforme `conformance.md`. Schemas legíveis por máquina de cada método: `docs/specs/schemas/atom-1/` (ADR-018).

## 1. Transporte e framing

- **JSON-RPC 2.0**.
- Extensões (iniciadas pelo host): **stdin/stdout**, NDJSON (uma mensagem por linha, UTF-8, sem `\n` interno). **stderr** = logs livres (o core captura, linha a linha).
- Clientes externos: unix socket (`<runtime>/atom-<profile>.sock`, permissão 0600) em Linux/macOS/Windows (`AF_UNIX`, tokio; no Windows sem ACL — 0600 só vale em unix). WebSocket opcional (`atom serve --ws :port`) com token bearer. Mesmo framing NDJSON (no WS: uma mensagem por frame texto). (v1.1: spec dizia named pipe no Windows; código usa `AF_UNIX` em toda plataforma — spec alinhada ao código; named pipe + Job Object ficam p/ pós-1.1, ver ADR-019 e `platform::job`.)
- Tamanho máximo de mensagem: 16 MiB (configurável). Dados maiores: streams.
- **(v1.1)** `[P-CONN-1]` Cada conexão tem identidade fixa no momento da aceitação: extensão (stdio de processo iniciado pelo host) ou cliente (socket/pipe/WS). Capabilities são avaliadas pela identidade da conexão, nunca por campos da mensagem.
- **(v1.1)** `[P-CONN-2]` Conexão de socket local cujo peer é **descendente do daemon** não é cliente confiável: `atom.hello` falha com `-32005 UntrustedPeer` e a conexão é fechada (ADR-019). Detecção por plataforma: Linux `SO_PEERCRED` + `/proc/<pid>/stat` com o daemon como `PR_SET_CHILD_SUBREAPER`; macOS `LOCAL_PEERPID` + árvore de processos (melhor esforço: double-fork escapa); Windows: sem árvore Win32 neste escopo (código usa `AF_UNIX` em toda plataforma) — só o próprio pid do daemon é recusado, demais peers = desconhecido explícito (confiável + warn); Job Object por extensão (sem breakaway) fica p/ pós-1.1 com named pipe (`platform::job` documenta o contrato).

## 2. Variáveis de ambiente para extensões

| Var | Valor |
|---|---|
| `ATOM_PROTOCOL` | `atom/1` |
| `ATOM_EXTENSION` | `publisher/nome@versão` |
| `ATOM_PKG_DIR` | raiz do pacote (read-only) |
| `ATOM_LOG_LEVEL` | `debug|info|warn|error` |

Nada mais do ambiente do usuário é repassado, exceto `PATH`, `HOME`/`USERPROFILE`, `TMPDIR`/`TEMP`, `LANG` e variáveis concedidas via capability `env:<NOME>`.

## 3. Contexto (`$ctx`)

Todo request e notificação **roteado** carrega em `params` o campo reservado `$ctx`:

```json
{"$ctx": {"tenant": "local", "user": "local", "profile": "dev", "session": "s_01H...", "trace": "t_01H...", "caller": "atom/ui-cli@0.2.0"}}
```

- O core **sobrescreve** `tenant`, `user`, `profile`, `caller` sempre.
- `session` e `trace` são propagados se presentes; o core gera `trace` se ausente.
- **(v1.1)** `deadline` (opcional, inteiro, epoch ms UTC) — ADR-021:
  - `[P-DL-1]` O core propaga `deadline` a todo `service.invoke`, `interceptor.invoke` e chamadas de `atom.proceed` daquela cadeia.
  - `[P-DL-2]` O core aplica `deadline = min(deadline, agora + max_deadline)` (`[bus] max_deadline_ms` no perfil; ausente = sem teto). Sem `deadline` no request e sem `default_deadline_ms` = sem prazo (comportamento v1).
  - `[P-DL-3]` Ao estourar, o core cancela a cadeia (como §5.4) e responde ao chamador `-32022 DeadlineExceeded`. Para streams, o prazo vale até `atom.stream.end`.
  - `[P-DL-4]` Timeouts por salto (interceptor 30s, `shutdown` 5s) passam a ser `min(timeout, restante)`.
  - SDKs propagam `deadline` do request em tratamento para as chamadas feitas durante ele.
- Ids: ULID com prefixo.

Campos de `params` que começam com `$` são reservados ao protocolo.

## 4. Ciclo de vida da extensão

1. Core inicia o processo.
2. Core → `atom.initialize` (request):
   ```json
   {"protocol": "atom/1", "core": {"version": "0.1.0"}, "extension": {"name": "...", "version": "..."},
    "config": {...}, "grants": ["..."], "$ctx": {...}}
   ```
3. Extensão → resposta:
   ```json
   {"protocol": "atom/1",
    "provides": [{"contract": "ai.tools@0", "version": "0.3.0"}],
    "handles": ["skill"],
    "intercepts": [{"match": "ai.tools@0/execute", "phase": "before", "priority": 100}],
    "subscribes": ["atom.package.*"]}
   ```
   Campos devem ser subconjunto do manifesto (o core rejeita o que não foi declarado). Omitidos = valores do manifesto.
4. Core → `atom.initialized` (notificação). A partir daqui a extensão pode chamar o core.
5. Encerramento: core → `atom.shutdown` (request, timeout 5s) → core → `atom.exit` (notificação) → kill após 2s.

Timeout de `initialize`: 10s. Falha → estado `failed`, aplica supervisão.

### 4.1 Supervisão
- Estados: `installed → starting → running → stopping → stopped`, `failed`, `dormant`.
- Restart em crash: backoff exponencial 1s→60s, máx. 5 restarts em 5 min → `failed` (evento `atom.extension.failed`).
- Requests pendentes para extensão que caiu recebem erro `-32010 ExtensionUnavailable`.

## 5. Chamadas a serviços

### 5.1 Chamador → core: `atom.call`
```json
{"service": "ai.tools@0", "method": "execute", "params": {...}, "target": "opcional publisher/nome", "stream": false, "$ctx": {...}}
```
Resolução: `target` explícito → provider do binding do perfil → único provider instalado → erro `-32011 NoProvider` / `-32012 AmbiguousProvider`.
Requer capability `service:<contrato@major>` no chamador (clientes locais autenticados têm todas).

### 5.2 Core → provider: `service.invoke`
```json
{"service": "ai.tools@0", "method": "execute", "params": {...}, "stream": "st_01H... | null", "$ctx": {...}}
```
O resultado do provider volta ao chamador como resultado de `atom.call` (após interceptors `after`).

### 5.3 Streams
- Quem chama com `"stream": true` recebe na resposta imediata `{"stream": "st_..."}` e depois notificações.
- Produtor emite `atom.stream.chunk` `{"stream": "st_...", "seq": n, "data": any}` e termina com `atom.stream.end` `{"stream": "st_...", "result": any}` ou `{"stream": "st_...", "error": {...}}`.
- O core repassa chunks na ordem ao consumidor.
- **Backpressure (v1.1, substitui a janela por ack da v1.0, nunca implementada — ADR-020):**
  - `[P-STREAM-1]` Padrão, sem ack: a fila de saída de cada conexão é limitada (§13). Se o consumidor não drena, o core para de encaminhar chunks daquele stream e, se o buffer do stream (256 chunks) encher, para de ler o stdout do produtor. Nenhum chunk é descartado.
  - `[P-STREAM-2]` Opt-in do consumidor: `atom.call` com `"flow": {"window": N}` (1 ≤ N ≤ 4096). O core só envia chunks com `seq ≤ último_ack + N`; consumidor envia `atom.stream.ack` `{"stream", "seq"}` (cumulativo). Sem `flow`, ack é ignorado.
  - `[P-STREAM-3]` Opt-in do produtor: extensão que responde `"flow": "credit"` no `initialize` recebe `atom.stream.credit` `{"stream", "until": seq}` e não deve emitir `seq > until`. Com isso o core nunca pausa o stdout inteiro (sem head-of-line entre streams da mesma extensão). Produtor que excede o crédito: chunk excedente recusado e stream terminado com `-32050 Overloaded`.
  - `[P-STREAM-4]` `seq` começa em 0 e é contíguo; o core repassa na ordem.

### 5.4 Cancelamento
`$/cancelRequest` `{"id": <id>}` (estilo LSP) ou `atom.stream.cancel` `{"stream"}`. O core propaga ao provider e a interceptors `around` ativos. Provider responde erro `-32800 RequestCancelled`.

- `[P-CANCEL-1]` `id` refere-se a um request do próprio remetente naquela conexão. Id desconhecido ou já respondido: ignorado.
- `[P-CANCEL-2]` O core mapeia o id para o request do próximo salto (`service.invoke`/`interceptor.invoke`/`atom.proceed` em andamento) e envia `$/cancelRequest` com o id **daquele** salto.
- `[P-CANCEL-3]` Se o provider não responder em 2s após o cancelamento (cortesia), o core responde ele mesmo `-32800` ao chamador e descarta a resposta tardia.
- `[P-CANCEL-4]` Toda chamada recebe **exatamente uma** resposta (resultado, erro ou `-32800`), inclusive com crash do provider (`-32010`) e estouro de `deadline` (`-32022`).
- `$/cancelRequest` é notificação; se vier com `id` JSON-RPC, o core responde `null`.

## 6. Interceptors

Core → interceptor: `interceptor.invoke`
```json
{"phase": "before|after|around", "service": "ai.tools@0", "method": "execute",
 "params": {...}, "result": "(só after)", "error": "(só after)", "$ctx": {...}}
```
Resposta:
```json
{"action": "continue"}
{"action": "modify", "params": {...}}       // before/around
{"action": "modify", "result": {...}}       // after
{"action": "reject", "error": {"code": -32020, "message": "..."}}
{"action": "respond", "result": {...}}      // before/around: curto-circuito
```
- Ordem: `before` por `priority` crescente; `after` em ordem reversa.
- `around`: o interceptor recebe a chamada e pode chamar `atom.proceed` `{"token": "..."}` (o token vem em `params.$proceed`) para executar o resto da cadeia, recebendo o resultado; depois responde com `modify/respond/continue`.
- Interceptor não pode interceptar chamadas feitas por ele mesmo (evita recursão).
- Timeout padrão por interceptor: 30s (configurável), estouro = `reject` com `-32021 InterceptorTimeout`.
- Métodos `atom.*` do core **não** são interceptáveis em v1.

## 7. Eventos

- Publicar: `atom.events.publish` `{"topic": "meu.topico", "data": any}`. Requer `events.publish:<glob>`.
- Assinar em runtime: `atom.events.subscribe` `{"topics": ["glob"]}` → `{"subscription": "sub_..."}`; `atom.events.unsubscribe`.
- Entrega: core → `event.deliver` (notificação) `{"topic", "data", "seq", "$ctx"}`.
- Entrega at-most-once, ordem garantida por publicador.
- **Retomada (v1.1, ADR-022):**
  - `[P-EV-1]` `event.deliver` inclui `epoch` (ULID `ep_...` da instância do log de eventos do perfil) e `offset` (inteiro, monotônico e contíguo dentro do `epoch`, global ao perfil).
  - `[P-EV-2]` O core retém os últimos `[events] retain` eventos (padrão 10000) em memória. No re-exec do self-update grava o ring em `<data>/profiles/<p>/events/handover.jsonl` (0600), recarrega no boot e apaga o arquivo; o `epoch` continua. Boot frio = novo `epoch`.
  - `[P-EV-3]` `atom.events.subscribe` aceita `from: {epoch, offset}`: o core reentrega eventos retidos com `offset > from.offset` que casam os tópicos, depois segue ao vivo. Se `epoch` difere ou o offset saiu do ring, envia antes `atom.events.gap` `{subscription, epoch, from, to}` (intervalo perdido; `from/to` nulos se desconhecidos).
  - `[P-EV-4]` Com `from`, entrega é at-least-once (consumidor deduplica por `(epoch, offset)`). Sem `from`, semântica v1.
  - `[P-EV-5]` Assinante lento não bloqueia o publicador: a fila por assinante é limitada (§13); ao estourar, eventos são descartados para aquele assinante e ele recebe `atom.events.gap` com o intervalo.

### Eventos do core
`atom.profile.started`, `atom.profile.stopping`, `atom.extension.{started,stopped,failed}`, `atom.package.{installed,updated,removed}`, `atom.config.changed` `{package}`, `atom.binding.changed` `{contract}`, `atom.update.available` `{component, version}`. **(v1.1)** `atom.grants.tampered` `{file}`, `atom.config.tampered` `{file, fields}` (alteração externa ignorada, ADR-019).

## 8. Kind handlers

- Core → handler: `kind.load` `{"package": {manifesto completo}, "dir": "<caminho read-only>", "config": {...}}` → `{}` ou erro.
- Core → handler: `kind.unload` `{"package": "publisher/nome@versão"}`.
- Chamado no boot do perfil, na instalação/atualização/remoção do pacote.

## 9. Serviços do core (`atom.*`)

| Método | Params | Capability |
|---|---|---|
| `atom.call` | ver §5 | `service:<id>` |
| `atom.proceed` | `{token}` | (interceptor ativo) |
| `atom.events.publish/subscribe/unsubscribe` | ver §7 | `events.*` |
| `atom.registry.list` | `{contract?}` → providers, bindings | — |
| `atom.storage.get/set/delete/list` | `{key, value?, prefix?}` — KV por extensão+perfil, valores JSON | — (namespace próprio) |
| `atom.storage.dir` | `{}` → `{path}` diretório de dados privado da extensão | `fs.data` |
| `atom.secrets.get` | `{name}` → `{value}` | `secrets:<name>` |
| `atom.secrets.set` | `{name, value}` | `secrets.write:<name>` |
| `atom.config.get` | `{}` → config validada do próprio pacote | — |
| `atom.log` | `{level, msg, attrs}` (notificação) | — |
| `atom.packages.list/search/install/update/remove` | ver cli.md | `packages.manage` |
| `atom.profile.info` | `{}` → nome, versão core, plataforma | — |
| `atom.grants.request` | `{capabilities[]}` → pede ao usuário via cliente UI ativo | — |

| `atom.storage.watch/unwatch` | ver §9.1 (v1.1) | — (namespace próprio) |

Só para clientes (socket/WS), nunca para extensões pelo stdio: `atom.hello` (§10), `atom.status` `{}` → estados/bindings/sources, `atom.stop` `{}`, **(v1.1)** `atom.grants.reload` `{}` e `atom.profile.reload` `{}` (só cliente confiável; `[C-AMB-2/3]`).

KV: chave string ≤ 512 bytes, valor ≤ 1 MiB.

### 9.0 Métodos existentes no código fora desta spec (pendente de ADR)
Implementados no bus para o shell `atom/ui-web` (v0.8), sem capability e sem cláusula normativa:

| Método | O que faz | Problema |
|---|---|---|
| `atom.ui.serve` | `{url}`: extensão anuncia a URL HTTP do shell (aparece em `status`) | conceito de UI no core (ADR-012) |
| `atom.ui.session` | `{}` → `{wsUrl, token}` do `atom serve --ws` | entrega o token bearer do WS (cliente com todas as capabilities) a **qualquer** extensão: contorna grants e ADR-019 |
| `atom.packages.mods` | lista pacotes `kind = "ui-mod"` + `contributes` + bindings | core interpretando um `kind` fora de `contract/runtime/distribution` (ADR-004, `manifest.md §5`) |
| `atom.packages.asset` | `{package, path}` → `{encoding: "base64", data}` (≤ 2 MiB) de arquivo do pacote | sem capability; lê de qualquer pacote instalado |

Até haver ADR, extensões novas não devem depender deles. Correção mínima esperada: `atom.ui.session` exige capability (ex.: `ui.serve`, que passaria de advisory a aplicada pelo bus) e o token do WS deixa de ser de autoridade total; `atom.packages.mods`/`asset` viram genéricos (`{kind}`, `packages.read`) ou saem do core.

### 9.1 Semântica do KV (v1.1, ADR-023)
Aditivo: chamadas v1 sem os campos novos seguem funcionando igual.

| Método | Params | Resultado |
|---|---|---|
| `atom.storage.get` | `{key}` | `{value, version}`; chave ausente = `{value: null}` sem `version` |
| `atom.storage.set` | `{key, value, if_version?, if_absent?}` | `{version}` |
| `atom.storage.delete` | `{key, if_version?}` | `{deleted: bool}` |
| `atom.storage.list` | `{prefix, cursor?, limit?}` | `{keys, cursor?}`; sem `limit` = tudo (v1) |
| `atom.storage.watch` | `{prefix}` | `{subscription}`; depois `storage.changed` `{subscription, key, version?, deleted}` |

- `[P-KV-1]` Por chave, linearizável dentro de `(tenant, profile, extension)`; read-your-writes na mesma conexão e entre conexões.
- `[P-KV-2]` `set`/`delete` só respondem após dado durável (sobrevive a crash do core).
- `[P-KV-3]` `version` é string opaca, muda a cada escrita; comparar só por igualdade.
- `[P-KV-4]` `if_version` diferente da atual, ou `if_absent: true` com chave existente → `-32041 VersionConflict`, nada escrito.
- `[P-KV-5]` `list` em ordem crescente de bytes UTF-8 da chave; `cursor` opaco; `limit` ≤ 1000.
- `[P-KV-6]` `watch` entrega mudanças do próprio namespace (inclusive de outras instâncias da mesma extensão no servidor futuro), em ordem por chave; fila limitada (§13), estouro → `storage.changed` `{subscription, overflow: true}` e o assinante deve reler.
- Sem transações multi-chave (não-objetivo).
- `[P-SEC-1]` `atom.secrets.*`: read-your-writes; `set` durável antes de responder.

## 10. Clientes

Clientes conectam no socket e enviam `atom.hello` `{"protocol": "atom/1", "client": {"name", "version"}, "token?": "..."}` → `{"core": {...}, "profile": "..."}`. Socket local com permissão 0600 = confiável (`user=local`, todas as capabilities exceto `packages.manage` sem confirmação), **desde que o peer não seja descendente do daemon** (`[P-CONN-2]`, v1.1). WS exige token gerado em `atom serve` e guardado no perfil. O token WS dá a mesma autoridade de cliente confiável; o `[P-CONN-2]` não se aplica ao WS, por isso quem obtém o token (ver `atom.ui.session`, §9.0) tem autoridade total.

Clientes podem também ser **providers temporários**: enviar `atom.register` `{provides, intercepts, subscribes}` e receber `service.invoke`/`interceptor.invoke`/`event.deliver` enquanto conectados. Isso permite UIs que contribuem serviços (ex.: `ui.prompt` para aprovações).

## 11. Códigos de erro

| Código | Nome |
|---|---|
| -32700..-32600 | padrão JSON-RPC |
| -32800 | RequestCancelled |
| -32001 | ProtocolMismatch |
| -32002 | NotInitialized |
| -32003 | CapabilityDenied |
| -32004 | InvalidContext |
| -32005 | UntrustedPeer (v1.1) |
| -32010 | ExtensionUnavailable |
| -32011 | NoProvider |
| -32012 | AmbiguousProvider |
| -32013 | ContractValidationFailed |
| -32020 | Rejected (por interceptor) |
| -32021 | InterceptorTimeout |
| -32022 | DeadlineExceeded (v1.1) |
| -32030 | PackageError |
| -32031 | SignatureInvalid |
| -32040 | StorageError |
| -32041 | VersionConflict (v1.1) |
| -32050 | Overloaded (v1.1) |

`error.data` pode conter `{"trace": "...", "details": any}`.

## 12. Versionamento do protocolo

- `atom/1` é estável após v1.0: só adições compatíveis (novos métodos/campos opcionais).
- Campos desconhecidos devem ser ignorados por ambos os lados.
- Mudança incompatível = `atom/2`; o core deve suportar `atom/1` e `atom/2` simultaneamente por pelo menos um major.
- Recursos v1.1 opt-in são negociados por presença de campo (`flow`, `from`, `if_version`, `deadline`); quem não os envia recebe exatamente o comportamento v1.0.

## 13. Limites (v1.1, ADR-020)

`[P-LIM-1]` Todo buffer do core é limitado; nenhum canal interno é ilimitado. Padrões (configuráveis em `[bus]` do perfil):

| Limite | Padrão | Excedido |
|---|---|---|
| Requests em voo por conexão | 1024 | `-32050 Overloaded` no request novo |
| Fila de saída por conexão | 1024 mensagens ou 64 MiB | pausa de streams (`[P-STREAM-1]`); eventos: `[P-EV-5]` |
| Buffer por stream | 256 chunks | pausa leitura do produtor (ou crédito) |
| Assinaturas por conexão | 256 | `-32050` |
| Tokens `around` pendentes | 1024 | `-32050` |

`[P-LIM-2]` Extensão ou cliente lento nunca faz a memória do core crescer além da soma desses limites.
