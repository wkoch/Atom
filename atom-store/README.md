# atom-store (referência `store/v1`)

Loja de pacotes servida como **arquivos estáticos** (spec: `docs/specs/store.md`
do repo do Atom). Este diretório é o esqueleto de referência: copie para um
repo GitHub próprio e sirva `v1/` via Pages (ou `raw.githubusercontent.com`).

```
atom-store/
  README.md                  # este arquivo
  .github/workflows/store.yml# CI: valida índices + regenera catalog + assina
  publishers/<pub>.toml      # chaves públicas aprovadas (`key = "ed25519:…"`)
  v1/config.json(+.sig)      # nome, index_signature_keys, artifacts_base
  v1/catalog.json(+.sig)     # índice leve gerado pelo publish/CI
  v1/index/<pub>/<nome>.json(+.sig)  # índice esparso por pacote
  v1/artifacts/<pub>/<arq>.tar.gz    # artefatos (ou Releases, ver abaixo)
```

## Fluxo de publicação

Na máquina do publisher (com o CLI `atom` instalado):

```sh
# 1. gera par de chaves (uma vez)
atom ext keygen --out ~/.config/atom/keys/publisher.ed25519
#    → imprime `ed25519:BASE64` (pub); mande ao mantenedor da loja.

# 2. empacota (gera tarball + .sig + snippet do índice)
atom ext pack ./meu-pacote --key ~/.config/atom/keys/publisher.ed25519 --out-dir ./out

# 3. publica no checkout da loja (mescla índice, assina, regen catalog)
atom ext publish --store-dir ./v1 --store-key ~/.config/atom/keys/store.ed25519 ./out/*.index.json

# 4. GitHub: git add v1 publishers && git commit && git push
#    anexe os .tar.gz ao Release da versão e abra o PR.
#    Com --github o comando imprime esse lembrete.
```

A chave da loja (`store.ed25519`) fica com o mantenedor; a raiz é fixada
no cliente via `atom source add oficial <url> --keys ed25519:…`.
Mudança de chave exige ação do usuário (o cliente recusa sig de chave nova).

## Artefatos: no repo ou no Release

- Lojas pequenas/`file://`: commit dos `.tar.gz` em `v1/artifacts/` (o
  `publish` já copia para lá).
- GitHub com Releases: suba os `.tar.gz` como anexos do Release e edite o
  `url` do artefato no índice para a URL do anexo antes de assinar
  (`artifacts_base` em `config.json` cobre o prefixo comum).

## Verificação (o que o cliente checa, spec store §4)

1. `.sig` do índice válida por uma `index_signature_keys` da loja;
2. `sha256` do download confere;
3. assinatura do artefato válida por uma `publisher_keys` do índice;
4. `atom.toml` interno confere com o índice (name, version, kind,
   capabilities, provides).

Adulterar 1 byte do artefato → instalação recusada (passo 2); trocar o
`sha256` no índice sem a chave do publisher → recusada (passo 3);
re-assinar o índice sem a chave da loja → recusada (passo 1).

## CI

`.github/workflows/store.yml`: valida índices/catalog (PRs) e, no `main`
com o secret `STORE_PRIVKEY`, re-assina índices + regenera o `catalog`
via `atom ext publish --reindex`. Sem o secret, o job só verifica
(`--check-only`) e o mantenedor assina localmente.
