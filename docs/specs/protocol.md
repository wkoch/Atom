# Spec: Protocolo `atom/1`

Status: estável desde v1.0 (ver `docs/deprecation.md`).

## 1. Transporte e framing

- **JSON-RPC 2.0**.
- Extensões (iniciadas pelo host): **stdin/stdout**, NDJSON (uma mensagem por linha, UTF-8, sem `\n` interno). **stderr** = logs livres (o core captura, linha a linha).
- Clientes externos: unix socket (`<runtime>/atom-<profile>.sock`, permissão 0600) no Linux/macOS; named pipe `\\.\pipe\atom-<user>-<profile>` no Windows; WebSocket opcional (`atom serve --ws :port`) com token bearer. Mesmo framing NDJSON (no WS: uma mensagem por frame texto).
- Tamanho máximo de mensagem: 16 MiB (configurável). Dados maiores: streams.

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
- Backpressure v1: janela de 256 chunks sem ack; consumidor envia `atom.stream.ack` `{"stream", "seq"}`. Excedido → core pausa leitura do produtor.

### 5.4 Cancelamento
`$/cancelRequest` `{"id": <id>}` (estilo LSP) ou `atom.stream.cancel` `{"stream"}`. O core propaga ao provider e a interceptors `around` ativos. Provider responde erro `-32800 RequestCancelled`.

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

### Eventos do core
`atom.profile.started`, `atom.profile.stopping`, `atom.extension.{started,stopped,failed}`, `atom.package.{installed,updated,removed}`, `atom.config.changed` `{package}`, `atom.binding.changed` `{contract}`, `atom.update.available` `{component, version}`.

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

KV: chave string ≤ 512 bytes, valor ≤ 1 MiB.

## 10. Clientes

Clientes conectam no socket e enviam `atom.hello` `{"protocol": "atom/1", "client": {"name", "version"}, "token?": "..."}` → `{"core": {...}, "profile": "..."}`. Socket local com permissão 0600 = confiável (`user=local`, todas as capabilities exceto `packages.manage` sem confirmação). WS exige token gerado em `atom serve` e guardado no perfil.

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
| -32010 | ExtensionUnavailable |
| -32011 | NoProvider |
| -32012 | AmbiguousProvider |
| -32013 | ContractValidationFailed |
| -32020 | Rejected (por interceptor) |
| -32021 | InterceptorTimeout |
| -32030 | PackageError |
| -32031 | SignatureInvalid |
| -32040 | StorageError |

`error.data` pode conter `{"trace": "...", "details": any}`.

## 12. Versionamento do protocolo

- `atom/1` é estável após v1.0: só adições compatíveis (novos métodos/campos opcionais).
- Campos desconhecidos devem ser ignorados por ambos os lados.
- Mudança incompatível = `atom/2`; o core deve suportar `atom/1` e `atom/2` simultaneamente por pelo menos um major.
