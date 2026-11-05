# AGENTS.md — Instruções para quem implementa o Atom

Leia antes de escrever qualquer código: `docs/architecture.md`, `docs/decisions.md`, `docs/roadmap.md` e as specs da versão em andamento.

## Regra de ouro

**O core não contém conceitos de IA.** Nenhum tipo, método, campo ou dependência sobre LLM, prompt, chat, tool, sessão de agente, memória, MCP ou skill entra em `crates/atom`, `crates/atom-core` ou `crates/atom-protocol`. Se parece precisar, a resposta é: contrato + extensão. Qualquer exceção exige novo ADR em `docs/decisions.md`.

## Stack

- Core: **Rust** stable, edition 2024, versão fixada em `rust-toolchain.toml` (MSRV declarado em `Cargo.toml`).
- Binário único: Linux via `*-unknown-linux-musl` (estático); macOS e Windows nativos.
- Alvos: `x86_64-unknown-linux-musl` (primário), `aarch64-unknown-linux-musl`, `x86_64-apple-darwin`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`.
- Build cross: CI com runners nativos por OS (macOS só em runner macOS); `cargo-zigbuild` aceitável para Linux.
- **Sem dependências C** no build padrão (nada de OpenSSL, SQLite bundled etc.) para manter cross-compile simples.
- `#![forbid(unsafe_code)]` em todos os crates; exceção só em módulo de plataforma isolado, com justificativa.

### Dependências permitidas sem discussão

| Uso | Crate |
|---|---|
| Async, processos, sockets, named pipes, timers | `tokio` (features: `rt-multi-thread`, `macros`, `process`, `io-util`, `net`, `sync`, `time`, `signal`, `fs`) |
| JSON / TOML | `serde`, `serde_json`, `toml` |
| CLI | `clap` (derive) |
| Erros | `thiserror` (libs), `anyhow` (só no binário) |
| Logs | `tracing`, `tracing-subscriber` (formato JSON) |
| Diretórios por plataforma | `directories` |
| SemVer | `semver` |
| Ids | `ulid` |
| Glob (`match`, tópicos, capabilities) | `globset` |
| Hash / assinatura | `sha2`, `ed25519-dalek`, `base64` |
| Pacotes `tar.gz` | `tar`, `flate2` (backend `rust_backend`) |
| HTTP cliente (loja, updates) | `reqwest` com `rustls` (sem OpenSSL; provider `ring` se `aws-lc-rs` complicar cross-compile) |
| WebSocket (`atom serve --ws`, v0.8) | `tokio-tungstenite` |
| Keyring | `keyring` |
| Self-update (troca do executável, inclusive Windows) | `self-replace` |
| Validação de contratos (v0.3) | `jsonschema` |
| Testes | `tempfile`, `assert_cmd` |
| Verificação (v1.1, só dev-deps / CI) | `proptest`, `stateright`, `criterion`; ferramentas `cargo-fuzz` (job nightly isolado), `cargo-mutants`, `cargo-deny` |
| TUF (`store/v2`, v1.1) | `tough`, **só se** o build padrão continuar sem dependência C; senão verificação própria com `ed25519-dalek` + `sha2` |
| Peer/Job Object no Windows (v1.1) | `windows-sys`, só no módulo de plataforma isolado (exceção a `forbid(unsafe_code)` justificada em ADR-019) |

Qualquer outra: justificar no PR. Preferir std quando couber em poucas linhas.

## Estrutura do repositório (Cargo workspace)

```
Cargo.toml                 # [workspace]
rust-toolchain.toml
crates/
  atom-protocol/           # tipos serde do atom/1: mensagens, $ctx, erros, manifesto, lockfile, índice store/v1.
                           # Sem tokio. Reutilizado pelo SDK Rust de extensões.
  atom-core/               # lib: o kernel. Embutível (futuro cdylib/Tauri/servidor).
    src/bus/               # roteador JSON-RPC, interceptors, eventos, streams, cancelamento
    src/host/              # spawn/supervisão de extensões, handshake, ativação lazy
    src/registry/          # serviços, providers, bindings, kind handlers
    src/caps/              # capabilities: declaração, concessão, enforcement
    src/profile/           # perfis, layout de diretórios, config
    src/storage/           # trait Storage (KV + FS concedido) + backend filesystem
    src/secrets/           # trait Secrets + keyring + fallback
    src/pkg/               # store local (CAS), instalação, lockfile, resolver
    src/store/             # cliente store/v1, verificação de assinatura
    src/update/            # self-update do binário
    src/transport/         # stdio, unix socket / named pipe, websocket
  atom/                    # bin: CLI bootstrap (clap) + wiring. Lógica fica em atom-core.
  atom-sdk/                # (v0.9) SDK Rust para extensões, sobre atom-protocol
  sdks/atom-ts/            # SDK TypeScript (par do atom-sdk; mesma spec atom/1)
testdata/                  # crates de extensões de teste: echo, events, interceptor, crash, slow-stream
docs/
```

## Modelo de concorrência

- Um runtime `tokio` multi-thread.
- **Atores + canais**, não locks compartilhados: o bus e o registry são tasks donas do seu estado, acessadas via `mpsc`/`oneshot`. Cada extensão tem uma task de leitura (stdout → bus) e uma de escrita (bus → stdin).
- Nunca segurar `Mutex` através de `.await`. Se precisar de estado compartilhado de leitura, `Arc` + `tokio::sync::RwLock` ou snapshot (`arc-swap` exige justificativa).
- Cancelamento via `tokio_util::sync::CancellationToken` (`tokio-util` liberado junto com `tokio`).
- Backends trocáveis (`Storage`, `Secrets`) são traits `async` (`async fn` em trait nativo; `dyn` via `Box<dyn ...>` com `async-trait` só se necessário).

## Convenções

- Todo método do bus carrega contexto `{tenant, user, profile, session, trace}` (ver `docs/specs/protocol.md`). Localmente `tenant="local"`, `user="local"`. Nunca omitir.
- Extensões **nunca** recebem caminhos absolutos do perfil, exceto diretórios concedidos via capability.
- Todo acesso a disco do core passa por `atom_core::storage` / `atom_core::profile` — nada de caminho hardcoded; usar diretórios por plataforma (ver `docs/specs/profiles.md`).
- Escritas de estado (lockfile, config, store) são **atômicas**: escrever em temp + fsync + rename.
- Erros: enums `thiserror` por módulo em `atom-core`, com contexto; `anyhow` só em `crates/atom`. Nada de `unwrap()`/`expect()` fora de testes e invariantes documentadas. No bus usar códigos de `docs/specs/protocol.md` (mapeamento centralizado em `atom-protocol`).
- Logs: `tracing` JSON, span por request com campo `trace`.
- Qualidade: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` em CI.
- Specs são a fonte da verdade. Se o código precisa divergir, atualize a spec no mesmo PR.
- (v1.1) Toda cláusula normativa nova tem id `[ÁREA-TEMA-n]` e teste marcado `// spec: <id>` no mesmo PR (`docs/specs/conformance.md`). Nenhum canal `unbounded` no core.

## Testes

- Cada módulo de `atom-core` com testes unitários (`#[cfg(test)]`).
- Testes de integração em `crates/atom-core/tests/` usam extensões de `testdata/` (crates do workspace, compilados pelo cargo antes dos testes): echo, eventos, interceptor, crash (para supervisão), stream lento (para cancelamento).
- CI roda em Linux, macOS e Windows desde a v0.1 (portabilidade não é "depois").

## Fluxo de trabalho

1. Implementar por versão do roadmap, na ordem. Não adiantar features de versões futuras.
2. Cada versão termina quando todos os critérios de aceite dela passam em CI.
3. Ao fim de cada versão, atualizar `docs/roadmap.md` (marcar concluído) e `CHANGELOG.md`.

<!-- trail:start -->
## Trail — repo context graph

This repo is indexed in `.trail/cache/`: small linked markdown nodes that explain each
system and carry exact file:line spans, kept in sync with the code through git.

For ANY task here — understanding how something works, finding where code lives,
or scoping a change — get context from the graph before grepping or opening
source files. Re-ask freely (it's cheap) and reuse literal identifiers you
already have (symbol, error string, file name) as the query. New to this repo?
Run `trail map` first — a token-budgeted orientation (dir clusters, hubs,
hotspots), no LLM, no key.

- Run `trail ask "<your question>" --source` → ranked nodes with the relevant
  code spans inlined (each hit's ≤8-line crux by default; `--full` for whole
  definitions when the crux isn't enough). Match the tool to the task shape:
  for understanding or editing, the top node IS the answer — cite its
  `covers:` file:line spans and edit straight from `--source`. For
  exhaustive tasks ("every occurrence / every caller of this pattern"), ranked
  results are top-N, not complete — run `trail grep "<literal>"` instead
  (exhaustive over indexed files, grouped by enclosing symbol), falling back
  to raw `grep -rn` only for unindexed files.
- `trail skeleton <file>` → every definition's signature + span, ~10× cheaper
  than reading the file; use it to skim an API surface.
- `trail callers <symbol>` gives precomputed, exact edges — who calls this.
  Add `--direction out` for what it calls, or `--depth N` to walk
  transitively for the full blast radius. For structural questions, skip
  ranking and use this directly.
- Or browse: `.trail/cache/INDEX.md` lists every node; follow the links.
- Monorepos and folders of multiple repos rank fairly across sub-projects —
  hits carry `[scope/]` labels naming which one they're from. Narrow with
  `trail ask "<task>" --in <scope>/` once you know where you're working.

If a returned span is truncated ("+N more lines"), open the file at that exact
range before finalizing. Only open source files when a node genuinely lacks a
needed detail, and then at the exact file:line the node points to — never
re-read whole files.

After big code changes, refresh the graph with `trail build` (deterministic,
no API key, $0).
<!-- trail:end -->
