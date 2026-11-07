# Spec: Conformidade e verificação

Status: v1.1 (ADR-018). Define como saber, sem confiar no autor, que o código cumpre as specs.

## 1. Cláusulas com id

- Toda cláusula normativa (deve / não deve / rejeita / garante) tem id `[<ÁREA>-<TEMA>-<n>]`, estável e nunca reutilizado. Áreas: `P` protocol, `C` capabilities, `S` profiles/storage, `ST1`/`ST2` store, `U` updates, `M` manifest.
- `[CF-1]` Cada id tem ≥1 teste com o comentário `// spec: <id>` (Rust) ou `// spec: <id>` (TS).
- Definição = id no início de um item de lista ou parágrafo (ex.: item de lista que começa com o id entre crases); qualquer outra ocorrência é referência.
- `[CF-2]` CI (`scripts/spec-ids`, std-only) extrai os ids definidos de `docs/specs/*.md` e dos testes e falha se: id sem teste, teste ou referência citando id inexistente, id definido duas vezes.
- Cláusulas da v1.0 ainda sem id recebem id na v1.1 (tarefa do roadmap); o texto não muda.

## 2. Schemas do kernel

- `docs/specs/schemas/atom-1/<método>.json` (JSON Schema 2020-12) com `params`, `result` e `examples` para cada método e notificação de `protocol.md`, incluindo `$ctx`, `event.deliver`, `atom.stream.*`, erros.
- `[CF-3]` Teste em `atom-protocol`: todo exemplo desserializa no tipo serde e reserializa válido pelo schema (`jsonschema`). Mesmo teste em `sdks/atom-ts`.
- Sem gerador de código: os schemas são o oráculo, os tipos são conferidos contra eles.

## 3. Suíte de conformidade executável

- Harness compartilhado em `crates/atom-core/tests/conformance_common.rs`
  (usado por `tests/conformance.rs` e pelo binário dev-only
  `crates/atom-core/examples/atom-conformance.rs`) que roda contra **qualquer** implementação:
  - modo extensão: `cargo run -p atom-core --example atom-conformance -- ext -- <comando>` (o harness faz papel de core);
  - modo core: `... -- core --socket <caminho>` (o harness faz papel de extensão/cliente).
- Cobre todos os ids `P-*`. Saída: lista de ids passados/falhados (JSON).
- CI (`conformance-matrix`, ubuntu): binário contra extensão Rust (`testdata/echo`),
  extensão TS (`sdks/atom-ts/examples/echo.ts`) e core real via socket.

## 4. Modelos de estado

Modelos `stateright` em `crates/atom-core/tests/models/` (rodam em `cargo test`). Invariantes obrigatórias:

| Modelo | Invariantes |
|---|---|
| ciclo de vida da extensão | estados só pelas transições de `protocol.md §4.1`; `failed` após 5 mortes/5 min; pendentes recebem `-32010` |
| chamada + interceptors + `around`/`proceed` | `[P-CANCEL-4]` exatamente uma resposta; token `proceed` usado no máximo uma vez; interceptor nunca recebe chamada própria |
| cancelamento + deadline + crash | todo cancelamento termina em ≤ cortesia; nenhuma resposta tardia vaza ao chamador |
| stream com crédito | `seq` contíguo e em ordem; nunca `seq > ack + window`; nenhum chunk perdido sem `end` de erro |

## 5. Fuzz, propriedades, mutação

- `fuzz/` (`cargo-fuzz`, nightly só no job de fuzz): framing NDJSON, `atom.toml`, `atom.lock`, `profile.toml`, índice `store/v1`, metadados TUF, extração tar. `[CF-4]` CI noturno: 30 min por alvo, corpus versionado; crash = issue bloqueante.
- `proptest`: resolver (resultado sempre satisfaz requisitos ou erro com cadeia), lockfile (round-trip), roteamento (target → binding → único), `[P-KV-*]` contra modelo em memória.
- `cargo-mutants` (CI semanal): `[CF-5]` zero mutantes sobreviventes em `caps`, verificação de assinatura/TUF e `$ctx`; demais módulos reportados com tendência.

## 6. Extensão adversária

`testdata/evil` tenta, e cada tentativa tem teste que exige falha:

| Ataque | Esperado |
|---|---|
| conectar ao socket do daemon como cliente | `-32005` (`[P-CONN-2]`) |
| editar `grants.toml` e `profile.toml [bindings]` | ignorado + `atom.*.tampered` (`[C-AMB-2/3]`) |
| chamar serviço sem grant | `-32003` |
| forjar `$ctx.tenant/user/profile/caller` | sobrescrito |
| stream sem crédito / flood sem drenar | memória limitada (`[P-LIM-2]`), `-32050` |
| nunca responder `service.invoke` com `deadline` | `-32022` ao chamador |
| responder após cancelamento | descartado (`[P-CANCEL-3]`) |

## 7. Backends

`[S-BE-1]` (`profiles.md §6`): suítes `storage::conformance`/`secrets::conformance` parametrizadas pelo trait; todo backend passa.

## 8. Oráculos externos (critério de "estável")

Uma versão de spec só é marcada estável quando, além do CI verde:
1. ≥1 implementação **independente** (escrita por quem não escreveu o core, só a partir de `docs/specs/` e dos schemas) passa em `atom-conformance` — candidato: SDK Python;
2. fuzz noturno sem crash por 7 dias seguidos;
3. checklist de ataques do TUF (spec TUF §1.5.2) revisada por alguém fora do projeto para `store/v2`;
4. benchmarks (`criterion`) de latência por salto e memória sob stream longo publicados no PR com o orçamento: p99 de `atom.call` local ≤ 1 ms sem interceptors; memória do core estável (±10%) num stream de 10⁶ chunks com consumidor lento — coberto em CI pelo soak `stream_soak.rs` (`[P-LIM-2]`, job `soak`: 10⁶ chunks reais com canal de 1 slot, RSS estável, `seq` contíguo).
