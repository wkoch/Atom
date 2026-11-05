# Spec: Protocolo da loja `store/v1`

Status: estável desde v1.0. **(v1.1)** Sucessor `store/v2` (TUF) em `store-v2.md`; o cliente suporta ambos (`docs/deprecation.md`). `store/v1` não protege contra *freeze attack* (índice antigo validamente assinado servido para sempre) nem tem limiar de chaves; o cliente mostra aviso para sources v1. A loja de referência (`atom-store`) mantém uma cópia desta spec em `docs/store.md`; a fonte da verdade é este arquivo.

Objetivo: uma loja deve ser servível como **arquivos estáticos** (repo GitHub + Pages/raw, S3, qualquer HTTP). Um servidor dinâmico implementa os mesmos caminhos e pode adicionar endpoints opcionais.

Todas as URLs são relativas à URL base da source (termina em `/v1/`). HTTPS obrigatório (exceto `file://` e `http://localhost` em dev).

## 1. `GET config.json`

```json
{
  "store": 1,
  "name": "Atom Oficial",
  "index_signature_keys": ["ed25519:BASE64..."],
  "artifacts_base": "https://github.com/org/atom-store/releases/download/",
  "features": ["catalog"]            // opcionais: "search", "publish", "auth"
}
```
O cliente valida que `index_signature_keys` contém ao menos uma das `keys` configuradas na source (chave raiz fixada pelo usuário). Mudança de chave exige ação do usuário.

## 2. `GET index/<publisher>/<nome>.json` (índice esparso)

```json
{
  "name": "atom/skill-loader",
  "publisher_keys": ["ed25519:BASE64..."],
  "versions": [
    {
      "version": "0.1.2",
      "kind": "extension",
      "engine": ">=0.4.0, <2.0.0",
      "protocol": "atom/1",
      "requires": {"packages": {"atom/contract-ai-tools": "^0.1"}, "contracts": {"ai.tools@0": "*"}},
      "provides": [{"contract": "ai.tools@0", "version": "0.3.0"}],
      "capabilities": ["fs.read:pkg", "service:ai.tools@0"],
      "artifacts": [
        {"platform": "linux-amd64", "url": "v0.1.2/atom-skill-loader-0.1.2-linux-amd64.tar.gz",
         "sha256": "...", "size": 123456, "signature": "BASE64"}
      ],
      "yanked": false,
      "published_at": "2025-01-01T00:00:00Z"
    }
  ]
}
```
- `url` relativa resolve contra `artifacts_base`; absoluta é usada como está.
- Metadados do índice devem bater com o `atom.toml` dentro do artefato; divergência = erro.

`GET index/<publisher>/<nome>.json.sig` → assinatura ed25519 (base64) dos bytes exatos do JSON, por uma `index_signature_keys`.

## 3. `GET catalog.json` (opcional, recomendado)

Lista leve para busca/listagem: `[{"name","description","kind","latest","tags"}]` + `catalog.json.sig`. Gerado pelo CI da loja.

## 4. Assinatura de artefato

Publisher assina, com sua chave ed25519 (listada em `publisher_keys`), a mensagem UTF-8:
```
atom-artifact-v1\n<name>\n<version>\n<platform>\n<sha256-hex>\n
```
Verificação no cliente (todas obrigatórias):
1. `.sig` do índice válida por chave da loja.
2. `sha256` do download confere.
3. `signature` do artefato válida por uma `publisher_keys`.
4. `atom.toml` interno confere com o índice (name, version, kind, capabilities, provides).

Papéis: **raiz da loja** (fixada no cliente) → **índice** → **publisher**. Podem ser a mesma chave no começo, mas o código trata separado (permite evoluir para TUF).

## 5. Cache e frescor

- Cliente usa `ETag`/`If-None-Match` e `Last-Modified`.
- Cache do índice em `<cache>/index/<source>/`; TTL padrão 1h; `atom ext update` força refresh.
- Proteção contra rollback de índice: cliente guarda maior `published_at` visto por pacote e recusa índice que "remove" versões já vistas sem `yanked`.

## 6. Múltiplas sources

- Ordem por `priority`. Primeiro que tem o pacote vence, a menos que o requisito fixe `source`.
- Namespace por publisher: um pacote `x/y` instalado de uma source não é substituído por outra source sem confirmação.
- Source local: `file:///caminho/v1/` com o mesmo layout (útil para lojas privadas e testes).

## 7. Endpoints opcionais (servidor dinâmico)

| Endpoint | Feature |
|---|---|
| `GET search?q=&kind=` → igual a `catalog.json` filtrado | `search` |
| `POST publish` (multipart: artefatos + assinaturas), auth bearer | `publish` |
| Auth bearer para lojas privadas: token em secret `store:<source>` | `auth` |

## 8. Loja em repo GitHub (implementação de referência)

```
atom-store/
  v1/config.json
  v1/catalog.json(+.sig)
  v1/index/<publisher>/<nome>.json(+.sig)
  publishers/<publisher>.toml       # chaves públicas aprovadas
```
- Publicar: `atom pack` gera artefatos + assinaturas → `atom publish --github` abre PR adicionando a versão ao índice e anexando artefatos a um Release.
- CI da loja: valida manifesto, schema, assinatura do publisher, unicidade de versão; ao fazer merge, regenera `catalog.json` e assina índices.
- Servido via GitHub Pages ou `raw.githubusercontent.com`.
