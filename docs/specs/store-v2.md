# Spec: Protocolo da loja `store/v2` (TUF)

Status: proposta para v1.1 (ADR-024). Sucessor de `store/v1`; ambos suportados pelo cliente durante o período de `docs/deprecation.md`.

Objetivo: o mesmo de `store/v1` (servível como arquivos estáticos: GitHub Pages, S3, `file://`), com as garantias do [TUF](https://theupdateframework.github.io/specification/latest/): expiração, versões monotônicas, limiar de chaves, delegação por publisher. Implementação de referência do cliente: crate `tough`; se ela exigir dependência C no build padrão, `atom-core::store::tuf` implementa só a **verificação** (ed25519-dalek + sha2) e a publicação usa `tuftool` no CI.

## 1. Layout

URL base da source termina em `/v2/`.

```
v2/
  metadata/
    1.root.json, 2.root.json, ...      # cadeia de roots (rotação)
    timestamp.json
    <N>.snapshot.json                  # consistent snapshots
    <N>.targets.json
    <N>.<publisher>.json               # papel delegado por publisher
  targets/
    catalog.json                       # = store/v1 §3, sem .sig próprio
    index/<publisher>/<nome>.json      # = store/v1 §2, sem publisher_keys/signature
    <publisher>/<nome>/<versão>/<arquivo>.tar.gz   # ou URL externa em custom.url
```

## 2. Papéis e chaves

| Papel | Quem detém | Limiar recomendado | Expiração | Assina |
|---|---|---|---|---|
| `root` | mantenedores, offline | 2 de 3 | 1 ano | chaves de todos os papéis de topo |
| `targets` | mantenedores, offline | 1 | 90 dias | delegações `paths: ["<publisher>/*", "index/<publisher>/*"]` → chaves do publisher |
| `<publisher>` (delegado) | publisher | 1 (publisher escolhe) | 90 dias | hashes/tamanhos dos artefatos e do índice dele |
| `snapshot` | CI da loja | 1 | 7 dias | versões de todos os `targets` |
| `timestamp` | CI da loja (online) | 1 | 1 dia | versão do snapshot |

- `catalog.json` é target do papel `targets` (CI regenera e mantenedor/CI assina conforme política da loja).
- A assinatura `atom-artifact-v1` de `store/v1 §4` é substituída pela assinatura do papel delegado do publisher sobre o hash do artefato.
- CI da loja re-assina `timestamp` diariamente (cron); sem isso a loja "expira" e clientes param de instalar, por design.

## 3. Configuração da source

```toml
[[sources]]
name = "oficial"
url = "https://atom-store.example.com/v2/"
root_sha256 = "..."          # sha256 do 1.root.json confiável (TOFU proibido)
priority = 0
```
`atom source add <nome> <url> --root <sha256>`: baixa `1.root.json`, confere o sha256, grava o root em `<cache>/tuf/<source>/` e daí em diante só aceita roots pela cadeia assinada.

## 4. Verificação no cliente (todas obrigatórias)

- `[ST2-1]` Workflow de atualização de metadados do TUF (§5 da spec TUF): root → timestamp → snapshot → targets → delegados, com limiar, expiração e versão ≥ última vista por papel.
- `[ST2-2]` Metadado expirado, versão regredida ou limiar não atingido → instalação/atualização recusada (`-32031 SignatureInvalid`, `details.reason` = `expired|rollback|threshold`).
- `[ST2-3]` Artefato: tamanho e sha256 conferem com o papel delegado do publisher dono do `paths`.
- `[ST2-4]` `atom.toml` interno confere com o índice (name, version, kind, capabilities, provides), como `store/v1` passo 4.
- `[ST2-5]` Publisher só pode assinar targets sob o próprio prefixo (`<publisher>/*`).

## 5. Cache e frescor

Metadados verificados ficam em `<cache>/tuf/<source>/`. A proteção contra rollback de `store/v1 §5` (`published_at`) é substituída pelas versões de metadado do TUF. `yanked` continua no índice.

## 6. Migração v1 → v2

- A loja publica `v1/` e `v2/` lado a lado durante a transição (mesmo CI gera os dois).
- `atom source migrate <nome> --root <sha256>` troca a URL para `/v2/`, mantendo nome e prioridade. Pacotes já instalados não mudam (lock tem sha256).
- Sources v1 continuam funcionando com aviso em `atom source list`/`atom status`.

## 7. Endpoints opcionais

Iguais a `store/v1 §7`. `POST publish` recebe artefatos + metadado delegado já assinado pelo publisher; o servidor assina snapshot/timestamp.
