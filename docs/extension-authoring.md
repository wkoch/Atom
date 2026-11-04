# Tutorial: escrevendo uma extensão

Pré-requisito: `atom` instalado (`install.sh`) e perfil padrão criado.
Tudo abaixo usa `--dev` (pasta local, sem loja nem assinatura).

## 1. Extensão mínima (Rust)

Crie a pasta e o manifesto:

```toml
# hello/atom.toml
manifest = 1
name = "voce/hello"
version = "0.1.0"
kind = "extension"
description = "Eco com cumprimento"
license = "MIT"
engine = ">=0.1.0"
protocol = "atom/1"
entrypoint = "bin/hello"
activation = ["onService:test.hello@0"]

[[provides]]
contract = "test.hello@0"
version = "0.1.0"
```

Implemente com o SDK (`crates/atom-sdk`; mesma cobertura no TS em `sdks/atom-ts`):

```rust
struct Hello;
impl atom_sdk::Handler for Hello {
    fn declare(&self) -> atom_sdk::Declare {
        atom_sdk::Declare {
            provides: vec![atom_sdk::ProvidesEntry {
                contract: "test.hello@0".into(),
                version: "0.1.0".into(),
            }],
            ..Default::default()
        }
    }
    async fn on_invoke(&self, _c: &atom_sdk::Client, inv: atom_sdk::Invoke)
        -> Result<serde_json::Value, atom_sdk::RpcError> {
        Ok(serde_json::json!({"saudacao": format!("olá, {}!", inv.params)}))
    }
}
#[tokio::main] async fn main() { atom_sdk::run(Hello).await; }
```

Instale e chame:

```sh
atom ext install --dev ./hello
atom call test.hello@0 ola '"mundo"'
```

O SDK cuida de handshake, `service.invoke`, streams, interceptors e
eventos; `client.call("ai.llm@0", ...)` chama outros serviços e
`client.publish(...)` emite eventos. Ver `testdata/echo` (espelho mínimo)
e `sdks/atom-ts/examples/echo.ts`.

## 2. Prover um contrato

Contratos são pacotes `kind = "contract"` com schemas (`specs/contracts.md`).
Para prover um existente, declare `[[provides]]` (como acima) — o core
valida params/resposta contra o schema quando instalado (`-32013` se violar).
Para criar um contrato novo, copie `extensions/contracts/ai-llm/atom.toml`
e escreva `schemas/seu.json`; versione `nome@major` (quebras só em major novo).

## 3. Kind handler (categoria nova sem mexer no core)

Para carregar pacotes declarativos (ex.: `kind = "skill"`):

```toml
handles = ["skill"]
```

e no handler:

```rust
async fn on_kind_load(&self, c: &Client, pkg: atom_sdk::KindPackage)
    -> Result<(), atom_sdk::RpcError> {
    // pkg.manifest / pkg.dir (read-only) / pkg.config
    Ok(())
}
```

Exemplos: `extensions/skill-loader`, `extensions/mcp-bridge`.

## 4. Interceptar e ouvir

```toml
[[intercepts]]
match = "ai.tools@0/execute"
phase = "before"
priority = 50
```

`on_intercept` retorna `Continue`, `Modify*`, `Respond` ou `Reject`;
em `around`, `client.proceed(token)` executa o resto da cadeia (uso único).
Eventos: `subscribes = ["atom.config.changed"]` + `on_event`.
Exemplo: `extensions/guard-shell`.

## 5. Capabilities e config

Declare no manifesto o que a extensão usa (`capabilities.md`); o instalador
pede aprovação (`grants`) — sem grant, a chamada falha com `-32003`.
Config do usuário chega em `atom.initialize` e muda via `atom.config.changed`;
valide com `config_schema` (ver `extensions/tools-fs/atom.toml`).

## 6. Publicar e distribuir

```sh
atom ext keygen --out ~/.config/atom/keys/pub.ed25519
atom ext pack ./hello --key ~/.config/atom/keys/pub.ed25519 --out-dir ./out
atom ext publish --store-dir ./v1 --store-key store.ed25519 ./out/*.index.json
atom source add minha file:///caminho/v1/ --keys ed25519:...
atom ext install voce/hello
```

Para um perfil pronto: escreva um pacote `kind = "distribution"`
(ver `distributions/code-starter/atom.toml`) e crie com
`sources` da loja primeiro (sources são por perfil):

```sh
ATOM_PROFILE=code atom source add oficial <url> --keys ed25519:...
atom profile create code --from atom/code-starter
```

O `--from` aceita id da loja (`publisher/nome[@req]`) ou pasta local.
A distribution instala `packages`, aplica `bindings` + `config` e pode
trazer `sources` próprias.
