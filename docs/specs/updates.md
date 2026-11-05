# Spec: Atualizações

## 1. Core (binário `atom`)

### Manifesto de release
`GET <update_url>/v1/core/<channel>.json` (+ `.sig`), canais `stable`, `beta`, `nightly`:
```json
{
  "version": "0.7.0",
  "published_at": "2025-01-01T00:00:00Z",
  "min_upgrade_from": "0.5.0",
  "notes_url": "https://...",
  "artifacts": [{"platform": "linux-amd64", "url": "...", "sha256": "...", "size": 0, "signature": "BASE64"}]
}
```
- `update_url` padrão embutido no binário; override em `<config>/atom.toml`.
- Chave pública de release **embutida no binário** (lista, permite rotação: release N assina com chave nova listada em N-1).
- Assinatura: mesma mensagem de `store.md §4` com `name = "atom/core"`.

### (v1.1) Canal de releases sobre TUF (ADR-024)
- `<update_url>/v2/` é um repositório `store-v2.md` com targets `core/<channel>.json` (mesmo JSON acima) e os artefatos como targets.
- O binário embute o `root.json` inicial (substitui a lista `ATOM_RELEASE_KEYS`); rotação pela cadeia de roots do TUF, com limiar.
- `[U-1]` Metadados expirados ou com versão menor que a já vista → não atualiza e publica `atom.update.available` com `{error: "stale-metadata"}`.
- Transição: o core tenta `v2/` e cai para `v1/` enquanto `docs/deprecation.md` permitir.

### Fluxo
1. Checagem: no boot do daemon e a cada 24h (configurável); `atom update --check`.
2. Política global (`atom.toml [update] core = auto|notify|off`). `notify` publica `atom.update.available`.
3. Download → verificar sha256 + assinatura → salvar em `<data>/bin/atom-<versão>`.
4. Trocar: o executável chamado pelo usuário (`atom` no PATH) é substituído de forma atômica:
   - Linux/macOS: escrever ao lado, `rename` sobre o atual.
   - Windows: renomear o atual para `atom.old.exe`, mover o novo para o lugar; apagar `.old` no próximo boot.
   - Se o binário não for gravável (instalado por gerenciador de pacotes), só notificar.
5. Reiniciar daemon (`atom serve` faz re-exec), clientes reconectam.
6. **Health check**: novo binário grava marcador `pending-health`; se não chegar a `running` com o perfil em 60s ou crashar 2x, o próximo start restaura a versão anterior (guardada em `<data>/bin/`) e marca a versão como ruim.
7. Manter as 2 últimas versões.

Mudanças de formato de estado (lockfile, profile.toml) exigem migração explícita e versionada (`lock = N`), executada no boot com backup prévio.

## 2. Extensões (por perfil)

- Política por perfil e por pacote: `auto` | `notify` | `pinned`.
- Checagem: mesmo agendamento do core; `atom ext update [--check] [pkg]`.
- Fluxo:
  1. Resolver novo conjunto respeitando requisitos e `engine`.
  2. Baixar/verificar/extrair para a store (não afeta o que está rodando).
  3. Se alguma versão nova pede capabilities novas → **sempre** parar e pedir aprovação (mesmo em `auto`).
  4. Gravar backup do lock, escrever novo lock.
  5. Hot swap: para cada pacote alterado: `kind.unload`/`atom.shutdown` da antiga → iniciar a nova → `kind.load`. Em falha de ativação → rollback do lock automaticamente e evento `atom.extension.failed`.
- `atom ext rollback [--to <timestamp>]` restaura um lock anterior.
- GC: `atom store gc` remove da store versões não referenciadas por nenhum lock (inclui `.prev`) há mais de 30 dias.
