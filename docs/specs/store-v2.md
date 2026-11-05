# Spec: Protocolo da loja `store/v2` (TUF)

Status: v1.1 (ADR-024). Verificação implementada no cliente (`atom-core::store::tuf`); integração com a CLI e ferramenta de publicação pendentes (§9). Sucessor de `store/v1`; ambos suportados pelo cliente durante o período de `docs/deprecation.md`.

Objetivo: o mesmo de `store/v1` (servível como arquivos estáticos: GitHub Pages, S3, `file://`), com as garantias do [TUF](https://theupdateframework.github.io/specification/latest/): expiração, versões monotônicas, limiar de chaves, delegação por publisher.

Implementação: `tough` foi avaliado e **rejeitado** (risco de dependência C). O cliente faz verificação própria (`ed25519-dalek` + `sha2`) sobre o **perfil TUF do Atom** (§1.1), que não é byte-compatível com ferramentas TUF genéricas (`tuftool`, `python-tuf`): a publicação usa a ferramenta do próprio Atom (§8).

## 1. Layout

URL base da source termina em `/v2/` (é assim que o cliente reconhece uma source v2).

```
v2/
  metadata/
    1.root.json, 2.root.json, ...      # cadeia de roots (rotação); 1.root.json é o trust inicial
    timestamp.json                     # sempre sem prefixo de versão
    snapshot.json | <N>.snapshot.json  # <N>. só com consistent_snapshot = true
    targets.json  | <N>.targets.json
    publisher:<pub>.json | <N>.publisher:<pub>.json   # papel delegado por publisher
  targets/
    catalog.json                                  # = store/v1 §3 (lista), sem .sig próprio
    <pub>/<nome>/<versão>/<arquivo>.tar.gz        # artefatos
    index/<pub>/<nome>.json                       # opcional (o cliente lê o índice do metadado, ver §1.1)
```

### 1.1 Perfil TUF do Atom (formato exato verificado pelo cliente)

- **Envelope:** `{"signatures": [{"keyid", "sig"}], "signed": {...}}`. `sig` = ed25519 em **base64** (padrão) sobre os bytes canônicos de `signed`.
- **Forma canônica:** JSON com chaves de objeto ordenadas por bytes, sem espaços, strings e números como `serde_json` os serializa. (Não é o OLPC canonical JSON do TUF.)
- **Chave:** `{"keytype": "ed25519", "scheme": "ed25519", "keyval": {"public": "<hex de 32 bytes>"}}`. Só `ed25519`.
- **keyid:** primeiros 8 caracteres hex do sha256 dos 32 bytes da chave pública. O cliente recusa `keys` cujo id não confere. (Não é o keyid TUF padrão; ver §9.)
- **`_type`:** `root`, `timestamp`, `snapshot`, `targets` (delegados também usam `targets`).
- **`version`:** inteiro ≥ 1. **`expires`:** RFC 3339 UTC `AAAA-MM-DDTHH:MM:SSZ` (ver §9 sobre o estado do código).
- **Root:** `keys` (keyid → chave), `roles` com `root`, `targets`, `snapshot`, `timestamp` (`{keyids, threshold}`), `consistent_snapshot` (padrão `false`).
- **Timestamp:** `meta["snapshot.json"] = {version, length, hashes: {sha256}}`.
- **Snapshot:** `meta["targets.json"]` e `meta["publisher:<pub>.json"]` para cada delegado, cada um `{version, length, hashes: {sha256}}` dos bytes exatos servidos.
- **Targets (topo):** `targets["catalog.json"]` + `delegations: {keys, roles: [{name: "publisher:<pub>", keyids, threshold, paths: ["<pub>/*", "index/<pub>/*"], terminating: true}]}`. Papéis delegados com nome fora do formato `publisher:<pub>` são ignorados.
- **Delegado `publisher:<pub>`:** `targets` contém:
  - `index/<pub>/<nome>.json` → `{length, hashes, custom: <índice completo>}`. O **índice é lido de `custom`** (mesmo schema de `store/v1 §2`; `publisher_keys` informativo; `signature` de artefato vazia). O arquivo servido em `targets/index/...` é opcional.
  - `<pub>/<nome>/<versão>/<arquivo>` → `{length, hashes: {sha256}}` para **cada** artefato listado no índice. Artefato listado sem alvo correspondente = recusa.
- **URL do artefato no índice:** relativa = `<pub>/<nome>/<versão>/<arquivo>` (resolve para `<base>targets/<url>`); absoluta (`https://`, `file://`) é usada como está, mas o caminho do alvo continua sendo `<pub>/<nome>/<versão>/<último segmento da url>`.

## 2. Papéis e chaves

| Papel | Quem detém | Limiar recomendado | Expiração recomendada | Assina |
|---|---|---|---|---|
| `root` | mantenedores, offline | 2 de 3 | 1 ano | chaves de todos os papéis de topo |
| `targets` | mantenedores, offline | 1 | 90 dias | `catalog.json` + delegações `publisher:<pub>` |
| `publisher:<pub>` (delegado) | publisher | 1 (publisher escolhe) | 90 dias | índice e artefatos dele |
| `snapshot` | CI da loja | 1 | 7 dias | versões/hashes de `targets` e delegados |
| `timestamp` | CI da loja (online) | 1 | 1 dia | versão/hash do snapshot |

- `catalog.json` é target do papel `targets` (CI regenera; quem detém `targets` assina conforme a política da loja).
- A assinatura `atom-artifact-v1` de `store/v1 §4` é substituída pelo hash do artefato no papel delegado do publisher.
- **O publisher assina o próprio papel delegado**; a loja nunca recebe a chave privada do publisher. A loja só assina `targets` (delegação + catálogo), `snapshot` e `timestamp` (§7, §8).
- CI da loja re-assina `timestamp` diariamente (cron); sem isso a loja "expira" e clientes param de instalar, por design.

## 3. Configuração da source

```toml
[[sources]]
name = "oficial"
url = "https://atom-store.example.com/v2/"
root_sha256 = "..."          # sha256 do 1.root.json confiável (TOFU proibido)
priority = 0
```
`atom source add <nome> <url> --root <sha256>`: baixa `metadata/1.root.json`, confere o sha256, grava em `<cache>/tuf/<source>/` (`1.root.json` e `trusted-root.json`) e daí em diante só aceita roots pela cadeia assinada. URL terminada em `/v2/` sem `--root` é recusada. O sha256 do `1.root.json` é publicado pela loja **fora de banda** (README, release do core).

## 4. Verificação no cliente (todas obrigatórias)

- `[ST2-1]` Workflow de atualização de metadados do TUF (§5 da spec TUF): root (cadeia `N+1.root.json` assinada pelo root anterior e por ele mesmo) → timestamp → snapshot (hash/tamanho conforme timestamp) → targets (conforme snapshot) → delegados (conforme snapshot), com limiar, expiração e versão ≥ última vista por papel.
- `[ST2-2]` Metadado expirado, versão regredida (ou diferente da anunciada pelo papel acima) ou limiar não atingido → instalação/atualização recusada (`-32031 SignatureInvalid`, `error.data.details.reason` = `expired|rollback|threshold`; a mensagem traz `tuf <reason>:`).
- `[ST2-3]` Artefato: tamanho e sha256 conferem com o papel delegado do publisher dono do `paths`.
- `[ST2-4]` `atom.toml` interno confere com o índice (name, version, kind, capabilities, provides), como `store/v1` passo 4.
- `[ST2-5]` Publisher só pode assinar targets sob o próprio prefixo (`<pub>/*`, `index/<pub>/*`).

## 5. Cache e frescor

`<cache>/tuf/<source>/`: `trusted-root.json`, `versions.json` (maior versão vista por papel, inclusive delegados) e `verified/{timestamp,snapshot,targets}.json`. A proteção contra rollback de `store/v1 §5` (`published_at`) é substituída pelas versões de metadado do TUF. `yanked` continua no índice.

## 6. Migração v1 → v2

- A loja publica `v1/` e `v2/` lado a lado durante a transição, a partir dos **mesmos snippets** do `pack` (mesmo CI gera os dois; mesmas versões e artefatos byte a byte).
- `atom source migrate <nome> --root <sha256>` troca a URL para `/v2/`, mantendo nome e prioridade. Pacotes já instalados não mudam (lock tem sha256).
- Sources v1 continuam funcionando com aviso em `atom source list`/`atom status`.

## 7. Endpoints opcionais

Iguais a `store/v1 §7`. `POST publish` recebe artefatos + metadado delegado já assinado pelo publisher; o servidor assina targets/snapshot/timestamp.

## 8. Ferramenta de publicação (requisito)

A loja estática precisa de uma ferramenta que produza exatamente §1.1. Requisitos (a implementar no core, CLI `atom`; hoje só existe `store::tuf::publish_repo`, usada em teste — §9):

1. **Lado publisher:** a partir dos snippets `.index.json` do `atom ext pack`, gera/atualiza `publisher:<pub>.json` com o índice em `custom` e os alvos dos artefatos, assinado com a chave do publisher, incrementando `version` sobre a última publicada.
2. **Lado loja:** aceita o delegado assinado (verifica assinatura contra a chave registrada em `publishers/<pub>.toml` e o prefixo `[ST2-5]`), regenera `catalog.json`, atualiza `targets` (delegações), `snapshot` e `timestamp`, sempre com `version` incrementada e `expires` real (§2).
3. **Só timestamp:** re-assina `timestamp.json` (nova versão e expiração) sem tocar nos demais — usado pelo cron diário.
4. **Rotação de root:** gera `N+1.root.json` assinado pelo limiar do root N e do root N+1.
5. **Verificação:** modo só-leitura que roda `[ST2-1..5]` sobre o diretório (`--check-only` do v2), usado no CI de PR.
6. Chaves por papel em arquivos separados (seed `ed25519-seed:BASE64`, 0600); limiar > 1 aceita várias chaves por papel.

## 9. Estado da implementação (revisão de 2026-11-05)

Divergências entre esta spec e o código do core, para corrigir no código:

- `atom ext install/search/info/update` ainda usam só o caminho `store/v1`: `store::tuf::install_package`/`resolve_latest_version` não são chamados fora de `tests/store_v2.rs`. Uma source v2 aceita por `source add --root` não instala.
- `check_expiry` trata qualquer `expires` RFC 3339 com ano < 3000 como expirado (atalho de teste). Com datas reais (§2) toda loja seria recusada. Deve comparar com o relógio atual.
- `publish_repo` é só para teste: exige as seeds de todos os publishers, usa a chave root também para `targets`, grava `version = 1` e `expires` sentinela `"9999999999"` sempre. Não serve para a loja de referência (§8).
- keyid de 8 hex (32 bits) é suficiente para seleção de chave, mas não é o keyid TUF; registrar no checklist da revisão externa (`conformance.md §8`).
- Lixo de publicação: `publish_repo` cria um diretório vazio `targets/catalog/` ao lado de `targets/catalog.json`.

Verificado por execução em 2026-11-05 (binário `atom` do working tree, perfis limpos, loja `file://`): `source add --root` aceita a source v2, mas `ext search` e `ext install` falham com `config.json: No such file or directory`, e `ext info` com `sem provedor`. Com a mesma source, `store::tuf::install_package` (biblioteca) instala. `timestamp` re-assinado com `expires` `2027-06-01T00:00:00Z` ou `2999-01-01T00:00:00Z` → `tuf expired`; só passam o sentinela e epoch numérico em segundos (`1893456000`). A suíte `cargo test --workspace` do core passa inteira, o que mostra que nenhum teste cobre o caminho CLI → v2.
