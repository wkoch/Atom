# Revisão de segurança (v1.0)

Escopo: assinaturas, sockets, token WS e env filtrado — os 4 itens do aceite
da v1.0. Modelo de ameaça: processos do mesmo usuário do SO; capabilities
aplicadas no bus; sem sandbox até pós-1.0 (ADR-003).

## Verificado

| Área | Estado |
|---|---|
| Assinatura da loja | Verificação em 4 passos (sig índice → sha → sig publisher → manifesto), chaves raiz fixadas na source; teste e2e adultera 1 byte e recusa |
| Releases do core | Mesma mensagem de assinatura (`atom/core`), chave embutida `ATOM_RELEASE_KEYS`, health check com rollback |
| Tarball | `tar` 0.4.46: `unpack` rejeita caminhos absolutos e `..` fora do destino; extração sempre em tempdir antes de mover à store |
| Socket local | Unix socket com 0600 (unix); trust-local = `user=local` com todas as capabilities |
| Token WS | 32 bytes de `OsRng`, arquivo 0600, comparação em tempo constante |
| Segredos | Keyring do SO; fallback em arquivo 0600 |
| Env de extensões | `env_clear` + allowlist (`PATH`, `HOME`, `USERPROFILE`, `TMPDIR`, `TEMP`, `LANG`); extras só via grant `env:<NOME>` |
| Capabilities | Enforcement no bus/host (`-32003`); grants aprovadas na instalação; teste e2e nega sem capability |
| Chaves `keygen` | Arquivo da seed 0600 |

## Corrigido nesta revisão

- **0600 com janela legível**: `serve.token`, `secrets.json` e chaves `keygen`
  eram escritos com permissão padrão e protegidos com chmod depois
  (visíveis no intervalo sob umask permissiva). Novo
  `profile::atomic_write_private` cria o temp já 0600 e é usado nos três.
- **Comparação do bearer em tempo constante**: `bearer_ok` usava `==`
  (early-exit por conteúdo). Novo `ct_eq` (fold XOR, std puro).

## Riscos residuais (aceitos, fora do escopo v1.0)

- Sem sandbox de OS: extensão maliciosa com grants amplas roda como o usuário.
  Mitigação atual: assinatura obrigatória (exceto `--dev`) + aprovação de grants.
- Fallback de segredos em claro (0600, só leitura do dono); criptografar
  (age/sodium) quando houver demanda — `shortcut` documentado em `secrets.rs`.
- Token WS também aceito via query `?token=` (pode vazar em logs); prefira
  `Authorization: Bearer`. WS sem `atom.register` (providers via WS ficam p/ depois).
- Socket no Windows usa AF_UNIX sem ACL explícita (0600 só em unix);
  herdada do diretório do usuário — suficiente no modelo mesmo-usuário.
- **Achado pós-revisão (corrigir na v1.1, ADR-019):** extensão podia se conectar ao socket local como cliente confiável e ignorar todos os grants, ou editar `grants.toml`/`profile.toml`. Também: `store/v1` sem proteção a freeze attack (ADR-024) e buffers ilimitados no bus (ADR-020).
- `PATH` repassado à extensão: binários do usuário podem influenciar
  `runtime=`/spawn; aceito (mesmo usuário).
